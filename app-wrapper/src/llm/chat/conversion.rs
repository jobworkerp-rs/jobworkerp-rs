use app::app::function::helper::McpNameConverter;
use command_utils::util::json_schema::SchemaCombiner;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::llm::LlmChatArgs;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::ToolExecutionRequest;
use proto::jobworkerp::function::data::FunctionSpecs;
use rmcp::ErrorData as McpError;
use rmcp::model::{ListToolsResult, Tool};
use serde_json;
use std::collections::HashSet;
use tracing;

/// Abstracts tool name access across different LLM provider ToolCall types.
///
/// Each LLM provider's ToolCall type must implement this trait in its own module
/// (e.g., genai.rs, ollama.rs) to enable shared selector-tool filtering logic.
pub trait ToolCallName {
    fn tool_name(&self) -> &str;
}

impl ToolCallName for ToolExecutionRequest {
    fn tool_name(&self) -> &str {
        &self.fn_name
    }
}

/// Result of auto-select evaluation.
#[derive(Debug)]
pub struct AutoSelectResult {
    pub selected_set_name: String,
    pub second_args: LlmChatArgs,
}

/// A `ToolResult` after fn_name resolution and call_id validation. Provider
/// adapters receive this instead of the raw proto type so they can trust that
/// every field is populated and references a real ASSISTANT ToolCall.
#[derive(Debug, Clone)]
pub struct ResolvedToolResult {
    pub call_id: String,
    pub fn_name: String,
    pub content: String,
    pub is_error: bool,
}

impl ResolvedToolResult {
    /// Render `content` with the cross-provider error marker policy applied.
    ///
    /// Providers that lack a native `is_error` field (OpenAI / Gemini /
    /// llama-cpp via the OAI-shaped jinja templates) must encode failure into
    /// the content text itself. We prepend `[ERROR] ` so the model still sees
    /// the textual outcome and can route around it. Providers with a native
    /// field (e.g. Anthropic) should bypass this helper and pass `is_error`
    /// through their own SDK path once that surfaces in `genai`.
    pub fn display_content(&self) -> std::borrow::Cow<'_, str> {
        if self.is_error {
            std::borrow::Cow::Owned(format!("[ERROR] {}", self.content))
        } else {
            std::borrow::Cow::Borrowed(&self.content)
        }
    }
}

pub struct ToolConverter;
impl McpNameConverter for ToolConverter {}

impl ToolConverter {
    /// Convert FunctionSpecs to MCP Tools.
    ///
    /// The tool input schema shape depends on whether the function is backed by a
    /// pre-configured Worker or a Runner invoked directly:
    /// - Worker (`worker_id` set): settings are already fixed at worker creation
    ///   time, so the tool exposes the method arguments directly at the top level
    ///   (no `settings`/`arguments` wrapper). For WORKFLOW workers this means the
    ///   workflow's own `input` schema is surfaced as-is, and callers pass the
    ///   input fields directly.
    /// - Runner (`worker_id` absent): direct execution still needs both the
    ///   runner init `settings` and the call `arguments`, so they are wrapped into
    ///   the unified `{ settings, arguments }` structure.
    pub fn convert_normal_function(tool: &FunctionSpecs) -> Vec<Tool> {
        let is_worker = tool.worker_id.is_some();
        tool.methods
            .as_ref()
            .map(|methods| {
                methods
                    .schemas
                    .iter()
                    .filter_map(|(method_name, method_schema)| {
                        let schema = if is_worker {
                            Self::build_worker_tool_schema(method_schema)
                        } else {
                            // Runner schema generation can fail; skip the method if so.
                            Self::build_runner_tool_schema(tool, method_schema)?
                        };

                        // Default method uses runner name only for simpler tool naming.
                        // Non-default methods combine runner and method names to avoid conflicts.
                        let tool_name = if method_name == proto::DEFAULT_METHOD_NAME {
                            tool.name.clone()
                        } else {
                            Self::combine_names(&tool.name, method_name)
                        };

                        Some(Tool::new(
                            tool_name,
                            method_schema
                                .description
                                .clone()
                                .unwrap_or_else(|| tool.description.clone()),
                            schema,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_else(|| {
                tracing::error!("error: no methods found for runner: {:?}", &tool);
                vec![]
            })
    }

    /// Build a Worker tool schema: the method arguments schema is exposed directly
    /// (no `settings`/`arguments` wrapper). Falls back to an empty object schema
    /// when the arguments schema is empty or unparseable.
    fn build_worker_tool_schema(
        method_schema: &proto::jobworkerp::function::data::MethodSchema,
    ) -> serde_json::Map<String, serde_json::Value> {
        match serde_json::from_str::<serde_json::Value>(&method_schema.arguments_schema) {
            Ok(serde_json::Value::Object(mut obj)) => {
                // MCP requires every tool inputSchema to be an object schema with
                // `"type": "object"`. A worker's arguments schema (e.g. a WORKFLOW
                // worker with `input: {}`, or any schema that omits the top-level
                // `type`) may not carry it, so default it here. We do not overwrite an
                // existing `type` to avoid corrupting an intentionally-typed schema.
                obj.entry("type")
                    .or_insert_with(|| serde_json::Value::String("object".to_string()));
                obj
            }
            Ok(other) => {
                // A non-object JSON Schema (e.g. `true`/`false`) cannot be used as an
                // MCP input schema object; fall back to an empty object schema.
                tracing::warn!(
                    "worker arguments schema is not a JSON object, using empty object: {:?}",
                    other
                );
                Self::empty_object_schema()
            }
            Err(e) => {
                tracing::error!("Failed to parse worker arguments schema: {}", e);
                Self::empty_object_schema()
            }
        }
    }

    /// Build a Runner tool schema: combine init `settings` and call `arguments`
    /// into the unified structure expected for direct runner execution.
    fn build_runner_tool_schema(
        tool: &FunctionSpecs,
        method_schema: &proto::jobworkerp::function::data::MethodSchema,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        let mut schema_combiner = SchemaCombiner::new();

        if !tool.settings_schema.is_empty() {
            schema_combiner
                .add_schema_from_string(
                    "settings",
                    &tool.settings_schema,
                    Some("Tool init settings".to_string()),
                )
                .ok();
        }

        schema_combiner
            .add_schema_from_string(
                "arguments",
                &method_schema.arguments_schema,
                Some("Tool arguments".to_string()),
            )
            .inspect_err(|e| tracing::error!("Failed to parse arguments schema: {}", e))
            .ok();

        match schema_combiner.generate_combined_schema() {
            Ok(schema) => Some(schema),
            Err(e) => {
                tracing::error!("Failed to generate combined schema: {}", e);
                None
            }
        }
    }

    /// An empty object JSON Schema (`{"type":"object","properties":{}}`) for tools
    /// that take no arguments or whose arguments schema could not be used.
    fn empty_object_schema() -> serde_json::Map<String, serde_json::Value> {
        match serde_json::json!({"type": "object", "properties": {}}) {
            serde_json::Value::Object(map) => map,
            _ => unreachable!("json! object literal is always a Value::Object"),
        }
    }

    pub fn convert_functions_to_mcp_tools(
        functions: Vec<FunctionSpecs>,
    ) -> Result<ListToolsResult, McpError> {
        let tool_list = functions
            .into_iter()
            .flat_map(|tool| Self::convert_normal_function(&tool))
            .collect::<Vec<_>>();

        Ok(ListToolsResult {
            tools: tool_list,
            next_cursor: None,
            meta: None,
        })
    }

    /// Convert MCP Tool to Ollama ToolInfo
    fn mcp_tool_to_ollama(tool: &Tool) -> Option<ollama_rs::generation::tools::ToolInfo> {
        use ollama_rs::generation::tools::{ToolFunctionInfo, ToolInfo, ToolType};
        let params_schema: Option<schemars::Schema> =
            serde_json::from_value(serde_json::Value::Object((*tool.input_schema).clone())).ok();
        params_schema.map(|parameters| ToolInfo {
            tool_type: ToolType::Function,
            function: ToolFunctionInfo {
                name: tool.name.to_string(),
                description: tool
                    .description
                    .as_ref()
                    .map(|d| d.to_string())
                    .unwrap_or_default(),
                parameters,
            },
        })
    }

    /// Build a `genai::chat::Tool` with the canonical name/schema/optional
    /// description shape. Centralised so any future genai builder change
    /// (renamed setters, ToolName variant tweaks, etc.) lands in one place.
    fn build_genai_tool(
        name: &str,
        schema: serde_json::Value,
        description: Option<&str>,
    ) -> genai::chat::Tool {
        let mut t = genai::chat::Tool::new(name).with_schema(schema);
        if let Some(desc) = description {
            t = t.with_description(desc);
        }
        t
    }

    /// Convert MCP Tool to GenAI Tool
    fn mcp_tool_to_genai(tool: &Tool) -> genai::chat::Tool {
        Self::build_genai_tool(
            tool.name.as_ref(),
            serde_json::Value::Object((*tool.input_schema).clone()),
            tool.description.as_deref(),
        )
    }

    /// Convert a list of FunctionSpecs to Vec<ToolInfo> for Ollama FunctionCalling
    pub fn convert_functions_to_ollama_tools(
        functions: Vec<FunctionSpecs>,
    ) -> Vec<ollama_rs::generation::tools::ToolInfo> {
        functions
            .into_iter()
            .flat_map(|tool| {
                Self::convert_normal_function(&tool)
                    .into_iter()
                    .filter_map(|mcp_tool| Self::mcp_tool_to_ollama(&mcp_tool))
            })
            .collect()
    }

    /// Convert a list of FunctionSpecs to Vec<genai::chat::Tool> for genai FunctionCalling
    pub fn convert_functions_to_genai_tools(
        functions: Vec<FunctionSpecs>,
    ) -> Vec<genai::chat::Tool> {
        functions
            .into_iter()
            .flat_map(|tool| {
                Self::convert_normal_function(&tool)
                    .into_iter()
                    .map(|mcp_tool| Self::mcp_tool_to_genai(&mcp_tool))
            })
            .collect()
    }

    /// Parse the client-supplied OpenAI Chat Completions `tools` JSON
    /// string into genai `Tool`s.
    ///
    /// Schema is forwarded **verbatim** — no envelope, no
    /// additionalProperties, no key filtering. The client owns the tool
    /// surface, which lets it expose a narrowed schema even when the
    /// runner-side worker would otherwise demand a wider one. Anything
    /// the runner re-wraps here would silently change the contract the
    /// model is told about.
    ///
    /// Validation is intentionally narrow: parseable as JSON, top-level
    /// array, each element shaped like
    /// `{"type":"function","function":{"name":<string>,"parameters":<object>}}`.
    /// `description` is optional.
    pub fn parse_client_tools_json(json: &str) -> anyhow::Result<Vec<genai::chat::Tool>> {
        use anyhow::{Context, bail};

        let value: serde_json::Value = serde_json::from_str(json)
            .with_context(|| format!("invalid client_tools_json: not valid JSON ({json:?})"))?;
        let arr = match value {
            serde_json::Value::Array(a) => a,
            _ => bail!("invalid client_tools_json: top-level value must be an array"),
        };

        let mut tools = Vec::with_capacity(arr.len());
        for (i, entry) in arr.into_iter().enumerate() {
            let ctx = |msg: &str| anyhow::anyhow!("invalid client_tools_json[{i}]: {msg}");
            let obj = entry
                .as_object()
                .ok_or_else(|| ctx("element must be an object"))?;

            let ty = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if ty != "function" {
                bail!(
                    "invalid client_tools_json[{i}]: only type=\"function\" is supported, got {ty:?}"
                );
            }
            let function = obj
                .get("function")
                .and_then(|v| v.as_object())
                .ok_or_else(|| ctx("missing or non-object `function`"))?;
            let name = function
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| ctx("function.name is required and must be a non-empty string"))?;
            let parameters = function
                .get("parameters")
                .ok_or_else(|| ctx("function.parameters is required"))?;
            if !parameters.is_object() {
                bail!("invalid client_tools_json[{i}]: function.parameters must be an object");
            }

            let description = function.get("description").and_then(|v| v.as_str());
            tools.push(Self::build_genai_tool(
                name,
                parameters.clone(),
                description,
            ));
        }
        Ok(tools)
    }

    /// Translate the client-supplied OpenAI `tool_choice` field into the
    /// genai `ToolChoice` enum. Returns `None` for values the runner does
    /// not recognise (object form for non-function types, malformed JSON,
    /// unknown keywords); the caller is expected to log and fall back to
    /// `Auto`. genai converts the enum to each provider's native form
    /// downstream, so we only have to surface intent here.
    pub fn parse_tool_choice(raw: &str) -> Option<genai::chat::ToolChoice> {
        let trimmed = raw.trim();
        match trimmed {
            "auto" => return Some(genai::chat::ToolChoice::Auto),
            "none" => return Some(genai::chat::ToolChoice::None),
            "required" => return Some(genai::chat::ToolChoice::Required),
            _ => {}
        }
        // Object form: {"type":"function","function":{"name":"X"}}
        let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
        let obj = value.as_object()?;
        if obj.get("type").and_then(|v| v.as_str()) != Some("function") {
            return None;
        }
        let name = obj
            .get("function")
            .and_then(|f| f.as_object())
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            .filter(|s| !s.is_empty())?;
        Some(genai::chat::ToolChoice::tool(name))
    }

    /// Human-readable names of the server-driven tool selection knobs on
    /// `FunctionOptions`. The list is the single source of truth for both
    /// the mutual-exclusion check below and the error message that
    /// surfaces it; if a new knob is added to FunctionOptions, append it
    /// here once and every caller stays consistent.
    pub const SERVER_DRIVEN_TOOL_KNOBS: &'static [&'static str] = &[
        "function_set_name",
        "use_runners_as_function",
        "use_workers_as_function",
        "auto_select_function_set",
    ];

    /// Returns true if any of the server-driven tool-selection knobs are
    /// engaged. Used to enforce mutual exclusion with `client_tools_json`
    /// at the request boundary.
    pub fn server_driven_tool_selection_set(
        fo: &jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::FunctionOptions,
    ) -> bool {
        fo.function_set_name.is_some()
            || fo.use_runners_as_function.is_some()
            || fo.use_workers_as_function.is_some()
            || fo.auto_select_function_set.unwrap_or(false)
    }

    /// Effective `is_auto_calling` flag after applying the client-driven
    /// override: when `client_tools_json` is set the runner must not
    /// execute tools server-side (the schemas belong to the client), so
    /// auto-calling is forced off and a warning is logged when the caller
    /// asked for it explicitly. Lives here because the invariant is a
    /// property of `FunctionOptions`, not of any particular LLM backend.
    pub fn effective_is_auto_calling(args: &LlmChatArgs) -> bool {
        let Some(fo) = args.function_options.as_ref() else {
            return false;
        };
        let requested = fo.is_auto_calling.unwrap_or(false);
        let client_driven = fo
            .client_tools_json
            .as_deref()
            .is_some_and(|s| !s.is_empty());
        if client_driven {
            if requested {
                tracing::warn!(
                    "client_tools_json is set; ignoring is_auto_calling=true and forcing manual (client-driven) mode"
                );
            }
            false
        } else {
            requested
        }
    }

    /// Regex for validating FunctionSet names used as tool names in auto-select mode.
    fn is_valid_function_set_name(name: &str) -> bool {
        if name.is_empty() {
            return false;
        }
        // Must not contain the MCP name delimiter
        if name.contains(Self::DELIMITER) {
            tracing::warn!(
                "FunctionSet name '{}' contains '{}' (McpNameConverter delimiter), skipping",
                name,
                Self::DELIMITER
            );
            return false;
        }
        // Recommended pattern for LLM provider compatibility
        let valid = name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !valid {
            tracing::warn!(
                "FunctionSet name '{}' contains characters outside [a-zA-Z0-9_-], skipping",
                name
            );
        }
        valid
    }

    /// Prefix added to FunctionSet names when used as pseudo-tool names in auto-select mode.
    pub const SELECTOR_TOOL_PREFIX: &'static str = "select_toolset_";

    /// Check if a tool name is a selector pseudo-tool (not a real runner/worker).
    pub fn is_selector_tool(name: &str) -> bool {
        name.starts_with(Self::SELECTOR_TOOL_PREFIX)
    }

    /// Convert a FunctionSet to a pseudo-tool for auto-select mode.
    /// The tool name is prefixed with `select_toolset_` to make it clearly
    /// identifiable as a selection action rather than a direct tool invocation.
    pub fn convert_function_set_to_selector_tool(
        set_name: &str,
        set_description: &str,
        tool_summaries: &[(String, String)],
    ) -> Option<Tool> {
        if !Self::is_valid_function_set_name(set_name) {
            return None;
        }

        let tools_list = tool_summaries
            .iter()
            .map(|(name, desc)| format!("- {name}: {desc}"))
            .collect::<Vec<_>>()
            .join("\n");

        let description = if tools_list.is_empty() {
            format!(
                "Activate the '{set_name}' toolset. {set_description}. \
                 Call this function to load its tools so you can use them."
            )
        } else {
            format!(
                "Activate the '{set_name}' toolset. {set_description}. \
                 Call this function to load the following tools:\n{tools_list}"
            )
        };

        // Empty input schema (no arguments needed to select a FunctionSet)
        let schema = Self::empty_object_schema();

        let tool_name = format!("{}{}", Self::SELECTOR_TOOL_PREFIX, set_name);
        Some(Tool::new(tool_name, description, schema))
    }

    /// Convert FunctionSet selector tools to Ollama ToolInfo list.
    pub fn convert_function_set_selector_tools_to_ollama(
        selector_tools: &[Tool],
    ) -> Vec<ollama_rs::generation::tools::ToolInfo> {
        selector_tools
            .iter()
            .filter_map(Self::mcp_tool_to_ollama)
            .collect()
    }

    /// Convert FunctionSet selector tools to GenAI Tool list.
    pub fn convert_function_set_selector_tools_to_genai(
        selector_tools: &[Tool],
    ) -> Vec<genai::chat::Tool> {
        selector_tools.iter().map(Self::mcp_tool_to_genai).collect()
    }

    /// Replace a tool execution request message with a text result in the message list.
    /// Only removes the matching request; other requests in the same message are preserved.
    pub fn replace_tool_execution_with_result(
        messages: &mut Vec<jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatMessage>,
        call_id: &str,
        result: &str,
    ) {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatMessage as ProtoChatMessage;
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::MessageContent;
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content as ProtoContent;

        for i in 0..messages.len() {
            if messages[i].role() != ChatRole::Tool {
                continue;
            }
            if let Some(ref mut content) = messages[i].content
                && let Some(ProtoContent::ToolExecutionRequests(reqs)) = &mut content.content
                && reqs.requests.iter().any(|r| r.call_id == call_id)
            {
                reqs.requests.retain(|r| r.call_id != call_id);
                if reqs.requests.is_empty() {
                    messages[i].content = Some(MessageContent {
                        content: Some(ProtoContent::Text(result.to_string())),
                    });
                } else {
                    messages.insert(
                        i + 1,
                        ProtoChatMessage {
                            role: ChatRole::Tool.into(),
                            content: Some(MessageContent {
                                content: Some(ProtoContent::Text(result.to_string())),
                            }),
                        },
                    );
                }
                return;
            }
        }
    }

    /// Resolve and validate a `ToolResults` payload attached to a TOOL-role
    /// message. See `ai-docs/tool-result-message-content-spec.md` for the
    /// full contract. Returns owned `ResolvedToolResult`s ready for the
    /// provider adapter.
    ///
    /// `prefix` MUST be the message slice **strictly before** the TOOL
    /// message that carries `results`. Every `call_id` must originate
    /// from the **immediately preceding** ASSISTANT `ToolCalls` block —
    /// i.e. the closest non-TOOL message in `prefix` walking backwards.
    /// Consecutive TOOL messages in `prefix` are skipped (this happens
    /// when parallel results are split across multiple TOOL messages),
    /// but anything else (USER, ASSISTANT/Text) terminates the search.
    /// This matches what OpenAI and Gemini require: a tool result block
    /// must answer the most-recent assistant tool call turn, not an
    /// arbitrary older one.
    ///
    /// Rules:
    /// 1. Empty `results` → bail.
    /// 2. Empty `call_id` on any entry → bail.
    /// 3. Each `call_id` must appear in the immediately-preceding
    ///    ASSISTANT `ToolCalls`. Otherwise the provider would reject the
    ///    conversation, so we fail fast here.
    /// 4. Empty `fn_name` is filled from the matching `ToolCall.fn_name`
    ///    (Gemini's `functionResponse.name` and similar fields require it).
    pub fn resolve_tool_results(
        prefix: &[jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatMessage],
        results: &jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::ToolResults,
    ) -> anyhow::Result<Vec<ResolvedToolResult>> {
        use anyhow::bail;
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content as ProtoContent;

        if results.results.is_empty() {
            bail!("ToolResults.results must not be empty");
        }

        // Locate the immediately-preceding non-TOOL message. Consecutive
        // TOOL messages (sibling tool_results split across messages) are
        // transparent. The next message back MUST be ASSISTANT with
        // ToolCalls content; otherwise the conversation is malformed.
        let last_tool_calls = prefix
            .iter()
            .rev()
            .find(|m| m.role() != ChatRole::Tool)
            .and_then(|m| m.content.as_ref())
            .and_then(|c| c.content.as_ref())
            .and_then(|c| match c {
                ProtoContent::ToolCalls(tc) => Some(tc),
                _ => None,
            });

        let Some(last_tool_calls) = last_tool_calls else {
            bail!(
                "tool_results must follow an ASSISTANT message with ToolCalls, but the preceding non-TOOL message is missing or has different content"
            );
        };

        let mut resolved = Vec::with_capacity(results.results.len());
        for tr in &results.results {
            if tr.call_id.is_empty() {
                bail!("ToolResult.call_id must not be empty");
            }

            let Some(matched) = last_tool_calls
                .calls
                .iter()
                .find(|c| c.call_id == tr.call_id)
            else {
                bail!(
                    "tool_result call_id not found in immediately-preceding ASSISTANT ToolCalls: {}",
                    tr.call_id
                );
            };

            // Explicit fn_name from the client wins; otherwise reuse the
            // matched ASSISTANT ToolCall's name.
            let fn_name = if tr.fn_name.is_empty() {
                matched.fn_name.clone()
            } else {
                tr.fn_name.clone()
            };

            resolved.push(ResolvedToolResult {
                call_id: tr.call_id.clone(),
                fn_name,
                content: tr.content.clone(),
                is_error: tr.is_error,
            });
        }

        Ok(resolved)
    }

    /// Validate every `ToolResults` payload in `args.messages`. Provider
    /// adapters call this at the entry of request_chat / request_chat_stream
    /// to fail fast on malformed payloads before any provider call is made.
    ///
    /// Role check is keyed off the content variant, not the role: any
    /// `ToolResults` payload riding on a non-TOOL message is rejected
    /// here. Otherwise the genai / ollama adapters would happily expand
    /// it under the message's actual role (USER / ASSISTANT) and the
    /// provider would either drop the response or surface a confusing
    /// API-level error.
    pub fn validate_all_tool_results(args: &LlmChatArgs) -> anyhow::Result<()> {
        use anyhow::bail;
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content as ProtoContent;

        for (idx, msg) in args.messages.iter().enumerate() {
            let Some(content) = msg.content.as_ref() else {
                continue;
            };
            let Some(ProtoContent::ToolResults(tr)) = content.content.as_ref() else {
                continue;
            };
            if msg.role() != ChatRole::Tool {
                bail!(
                    "tool_results must ride on a TOOL-role message, got role={:?} at message index {}",
                    msg.role(),
                    idx
                );
            }
            Self::resolve_tool_results(&args.messages[..idx], tr)?;
        }
        Ok(())
    }

    /// Check if a tool execution request is a selector pseudo-tool, and if so,
    /// replace it with a completion message. Returns true if skipped.
    pub fn skip_selector_tool_execution(
        req: &ToolExecutionRequest,
        messages: &mut Vec<jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatMessage>,
    ) -> bool {
        if Self::is_selector_tool(&req.fn_name) {
            tracing::warn!(
                tool = %req.fn_name,
                "Skipping selector pseudo-tool in tool execution"
            );
            Self::replace_tool_execution_with_result(
                messages,
                &req.call_id,
                "Tool selection completed",
            );
            true
        } else {
            false
        }
    }

    /// Filter out selector pseudo-tools from a list, logging warnings.
    pub fn filter_selector_tools<T: ToolCallName>(mut tool_calls: Vec<T>) -> Vec<T> {
        Self::retain_non_selector_tools(&mut tool_calls);
        tool_calls
    }

    /// Retain only non-selector tools in place.
    pub fn retain_non_selector_tools<T: ToolCallName>(tool_calls: &mut Vec<T>) {
        tool_calls.retain(|tc| {
            if Self::is_selector_tool(tc.tool_name()) {
                tracing::warn!(
                    tool = %tc.tool_name(),
                    "Filtering out selector pseudo-tool from pending tool calls"
                );
                false
            } else {
                true
            }
        });
    }

    /// Evaluate auto-select tool calls and prepare Phase 2 arguments.
    /// Returns Ok(AutoSelectResult) if a selector tool was found,
    /// or Err if no selector was called.
    pub fn evaluate_auto_select<T: ToolCallName>(
        tool_calls: &[T],
        auto_select_names: &HashSet<String>,
        original_args: LlmChatArgs,
        context_label: &str,
    ) -> anyhow::Result<AutoSelectResult> {
        let selected = tool_calls
            .iter()
            .find(|tc| auto_select_names.contains(tc.tool_name()));

        if let Some(selected) = selected {
            let selected_name = selected.tool_name();
            let selected_set_name = selected_name
                .strip_prefix(Self::SELECTOR_TOOL_PREFIX)
                .unwrap_or(selected_name)
                .to_string();

            tracing::info!(
                function_set = %selected_set_name,
                "Auto-select{}: LLM selected FunctionSet",
                context_label
            );

            let extra_selectors: Vec<&str> = tool_calls
                .iter()
                .filter(|tc| {
                    auto_select_names.contains(tc.tool_name()) && tc.tool_name() != selected_name
                })
                .map(|tc| tc.tool_name())
                .collect();
            if !extra_selectors.is_empty() {
                tracing::warn!(
                    selected = %selected_name,
                    ?extra_selectors,
                    "Auto-select{}: LLM called multiple selector tools, using the first one",
                    context_label
                );
            }

            let mut second_args = original_args;
            match second_args.function_options {
                Some(ref mut fo) => {
                    fo.function_set_name = Some(selected_set_name.clone());
                    fo.auto_select_function_set = Some(false);
                }
                None => {
                    use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::FunctionOptions;
                    second_args.function_options = Some(FunctionOptions {
                        use_function_calling: true,
                        function_set_name: Some(selected_set_name.clone()),
                        auto_select_function_set: Some(false),
                        ..Default::default()
                    });
                }
            }

            Ok(AutoSelectResult {
                selected_set_name,
                second_args,
            })
        } else {
            let attempted_names: Vec<&str> = tool_calls.iter().map(|tc| tc.tool_name()).collect();
            tracing::warn!(
                ?attempted_names,
                "Auto-select{}: LLM did not call any selector tool",
                context_label
            );
            Err(JobWorkerError::OtherError(format!(
                "Auto-select failed: LLM called {:?} instead of selector tools",
                attempted_names
            ))
            .into())
        }
    }

    /// Extract tool name/description summaries from FunctionSpecs.
    /// Shared by genai, ollama, and function_set_selector.
    pub fn get_tool_summaries(specs: &[FunctionSpecs]) -> Vec<(String, String)> {
        specs
            .iter()
            .flat_map(|spec| {
                if let Some(methods) = &spec.methods {
                    methods
                        .schemas
                        .iter()
                        .map(|(method_name, method_schema)| {
                            let tool_name = if method_name == proto::DEFAULT_METHOD_NAME {
                                spec.name.clone()
                            } else {
                                Self::combine_names(&spec.name, method_name)
                            };
                            let desc = method_schema
                                .description
                                .clone()
                                .unwrap_or_else(|| spec.description.clone());
                            (tool_name, desc)
                        })
                        .collect::<Vec<_>>()
                } else {
                    vec![(spec.name.clone(), spec.description.clone())]
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::jobworkerp::function::data::FunctionSpecs;
    use serde_json::{Value, json};

    fn make_single_schema_spec() -> FunctionSpecs {
        let mut method_schemas = std::collections::HashMap::new();
        method_schemas.insert(
            proto::DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: Some("desc_single".to_string()),
                arguments_schema:
                    json!({"type": "object", "properties": {"arg_a": {"type": "boolean"}}})
                        .to_string(),
                result_schema: Some(
                    json!({"type": "object", "properties": {"result": {"type": "string"}}})
                        .to_string(),
                ),
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                annotations: None,
            },
        );

        FunctionSpecs {
            name: "test_single".to_string(),
            description: "desc_single".to_string(),
            settings_schema:
                json!({"type": "object", "properties": {"setting_a": {"type": "string"}}})
                    .to_string(),
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
            runner_type: proto::jobworkerp::data::RunnerType::Command as i32,
            worker_id: None,
            ..Default::default()
        }
    }

    fn make_reusable_workflow_spec() -> FunctionSpecs {
        let mut method_schemas = std::collections::HashMap::new();
        method_schemas.insert(
            proto::DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: Some("desc_workflow".to_string()),
                arguments_schema:
                    json!({"type": "object", "properties": {"arg_b": {"type": "number"}}})
                        .to_string(),
                result_schema: Some(
                    json!({"type": "object", "properties": {"result": {"type": "string"}}})
                        .to_string(),
                ),
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                annotations: None,
            },
        );

        FunctionSpecs {
            name: "test_workflow".to_string(),
            description: "desc_workflow".to_string(),
            settings_schema:
                json!({"type": "object", "properties": {"setting_b": {"type": "integer"}}})
                    .to_string(),
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
            runner_type: proto::jobworkerp::data::RunnerType::Workflow as i32,
            worker_id: None,
            ..Default::default()
        }
    }

    fn make_mcp_tools_spec() -> FunctionSpecs {
        let mut method_schemas = std::collections::HashMap::new();
        method_schemas.insert(
            "inner".to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: Some("desc_inner".to_string()),
                arguments_schema:
                    json!({"type": "object", "properties": {"c": {"type": "boolean"}}}).to_string(),
                result_schema: None,
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                annotations: None,
            },
        );

        FunctionSpecs {
            name: "test_mcp".to_string(),
            description: "desc_mcp".to_string(),
            settings_schema: String::new(), // MCP Server typically doesn't have settings
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
            runner_type: proto::jobworkerp::data::RunnerType::McpServer as i32,
            worker_id: None,
            ..Default::default()
        }
    }

    fn make_multi_method_plugin_spec() -> FunctionSpecs {
        let mut method_schemas = std::collections::HashMap::new();

        method_schemas.insert(
            "method_a".to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: Some("First method".to_string()),
                arguments_schema:
                    json!({"type": "object", "properties": {"param_a": {"type": "string"}}})
                        .to_string(),
                result_schema: Some(
                    json!({"type": "object", "properties": {"output_a": {"type": "string"}}})
                        .to_string(),
                ),
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                annotations: None,
            },
        );

        method_schemas.insert(
            "method_b".to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: Some("Second method".to_string()),
                arguments_schema:
                    json!({"type": "object", "properties": {"param_b": {"type": "integer"}}})
                        .to_string(),
                result_schema: Some(
                    json!({"type": "object", "properties": {"output_b": {"type": "integer"}}})
                        .to_string(),
                ),
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                annotations: None,
            },
        );

        FunctionSpecs {
            name: "test_plugin".to_string(),
            description: "Multi-method plugin".to_string(),
            settings_schema:
                json!({"type": "object", "properties": {"api_key": {"type": "string"}}}).to_string(),
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
            runner_type: proto::jobworkerp::data::RunnerType::Plugin as i32,
            worker_id: None,
            ..Default::default()
        }
    }

    /// WORKFLOW worker spec: arguments_schema already holds the workflow's own
    /// `input` schema (extracted in app crate). The MCP tool must expose this
    /// directly at the top level without a `settings`/`arguments` wrapper.
    fn make_workflow_worker_spec() -> FunctionSpecs {
        let mut method_schemas = std::collections::HashMap::new();
        method_schemas.insert(
            proto::DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: Some("wf worker summary".to_string()),
                arguments_schema: json!({
                    "type": "object",
                    "properties": {
                        "owner": {"type": "string"},
                        "repo": {"type": "string"}
                    }
                })
                .to_string(),
                result_schema: Some(json!({"type": "object"}).to_string()),
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                annotations: None,
            },
        );

        FunctionSpecs {
            name: "wf_worker".to_string(),
            description: "wf worker description".to_string(),
            // Worker settings are fixed at creation time; settings_schema is empty.
            settings_schema: String::new(),
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
            runner_type: proto::jobworkerp::data::RunnerType::Workflow as i32,
            worker_id: Some(proto::jobworkerp::data::WorkerId { value: 42 }),
            ..Default::default()
        }
    }

    /// Non-WORKFLOW worker (e.g. COMMAND) backed by a pre-configured worker.
    /// Like the workflow worker, its tool must expose arguments directly without
    /// the `settings`/`arguments` wrapper.
    fn make_command_worker_spec() -> FunctionSpecs {
        let mut method_schemas = std::collections::HashMap::new();
        method_schemas.insert(
            proto::DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: Some("cmd worker".to_string()),
                arguments_schema: json!({
                    "type": "object",
                    "properties": {"command": {"type": "string"}}
                })
                .to_string(),
                result_schema: None,
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                annotations: None,
            },
        );

        FunctionSpecs {
            name: "cmd_worker".to_string(),
            description: "cmd worker description".to_string(),
            settings_schema: String::new(),
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
            runner_type: proto::jobworkerp::data::RunnerType::Command as i32,
            worker_id: Some(proto::jobworkerp::data::WorkerId { value: 7 }),
            ..Default::default()
        }
    }

    /// Helper function to verify schema has required fields
    fn assert_schema_required_fields(
        schema: &serde_json::Map<String, Value>,
        required_fields: &[&str],
    ) {
        let required = schema.get("required").and_then(|r| r.as_array());
        assert!(required.is_some(), "Schema should have 'required' field");

        let required_values: Vec<String> = required
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();

        for field in required_fields {
            assert!(
                required_values.contains(&field.to_string()),
                "Field '{field}' should be in required list: {required_values:?}"
            );
        }
    }

    /// Helper function to verify schema has specific properties with expected structure
    fn assert_schema_property(
        schema: &serde_json::Map<String, Value>,
        property_name: &str,
        expected_property: &Value,
    ) {
        let properties = schema.get("properties").and_then(|p| p.as_object());
        assert!(
            properties.is_some(),
            "Schema should have 'properties' field"
        );

        let properties = properties.unwrap();
        assert!(
            properties.contains_key(property_name),
            "Schema should contain property '{property_name}'"
        );

        let actual_property = &properties[property_name];
        assert_json_subset(expected_property, actual_property);
    }

    /// Verifies that expected is a subset of actual (all keys in expected exist in actual with the same values)
    fn assert_json_subset(expected: &Value, actual: &Value) {
        match (expected, actual) {
            (Value::Object(exp_obj), Value::Object(act_obj)) => {
                for (key, exp_val) in exp_obj {
                    assert!(
                        act_obj.contains_key(key),
                        "Expected key '{key}' not found in actual"
                    );
                    assert_json_subset(exp_val, &act_obj[key]);
                }
            }
            (Value::Array(exp_arr), Value::Array(act_arr)) => {
                assert!(
                    exp_arr.len() <= act_arr.len(),
                    "Expected array length {} but got {}",
                    exp_arr.len(),
                    act_arr.len()
                );
                for (exp_val, act_val) in exp_arr.iter().zip(act_arr.iter()) {
                    assert_json_subset(exp_val, act_val);
                }
            }
            (exp, act) => {
                assert_eq!(exp, act, "Expected value {exp:?} but got {act:?}");
            }
        }
    }

    #[test]
    fn test_convert_functions_to_mcp_tools() {
        let specs = vec![
            make_single_schema_spec(),
            make_reusable_workflow_spec(),
            make_mcp_tools_spec(),
            make_multi_method_plugin_spec(),
        ];
        let result = ToolConverter::convert_functions_to_mcp_tools(specs).unwrap();

        // SingleSchema
        let tool = result
            .tools
            .iter()
            .find(|t| t.name == "test_single")
            .unwrap();
        assert_eq!(tool.name, "test_single");
        assert_eq!(tool.description.as_ref().unwrap(), "desc_single");

        assert_eq!(
            tool.input_schema.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        assert_schema_required_fields(&tool.input_schema, &["arguments", "settings"]);

        let arguments_expected = json!({
            "type": "object",
            "properties": {
                "arg_a": {
                    "type": "boolean"
                }
            },
            "description": "Tool arguments"
        });
        assert_schema_property(&tool.input_schema, "arguments", &arguments_expected);

        let settings_expected = json!({
            "type": "object",
            "properties": {
                "setting_a": {
                    "type": "string"
                }
            },
            "description": "Tool init settings"
        });
        assert_schema_property(&tool.input_schema, "settings", &settings_expected);

        // ReusableWorkflow
        let tool = result
            .tools
            .iter()
            .find(|t| t.name == "test_workflow")
            .unwrap();
        assert_eq!(tool.name, "test_workflow");
        assert_eq!(tool.description.as_ref().unwrap(), "desc_workflow");

        // ReusableWorkflow now uses settings/arguments structure like other runners
        assert_schema_required_fields(&tool.input_schema, &["arguments"]);

        let expected_settings = json!({
            "type": "object",
            "properties": {
                "setting_b": {
                    "type": "integer"
                }
            },
            "description": "Tool init settings"
        });
        assert_schema_property(&tool.input_schema, "settings", &expected_settings);

        let expected_arguments = json!({
            "type": "object",
            "properties": {
                "arg_b": {
                    "type": "number"
                }
            },
            "description": "Tool arguments"
        });
        assert_schema_property(&tool.input_schema, "arguments", &expected_arguments);

        // McpTools
        let tool = result
            .tools
            .iter()
            .find(|t| t.name == "test_mcp___inner")
            .unwrap();
        assert_eq!(tool.name, "test_mcp___inner");
        assert_eq!(tool.description.as_ref().unwrap(), "desc_inner");

        // McpTools should have arguments with property c (settings/arguments structure)
        assert_schema_required_fields(&tool.input_schema, &["arguments"]);

        let expected_arguments = json!({
            "type": "object",
            "properties": {
                "c": {
                    "type": "boolean"
                }
            },
            "description": "Tool arguments"
        });
        assert_schema_property(&tool.input_schema, "arguments", &expected_arguments);

        // Multi-method Plugin - should generate 2 tools
        let plugin_tools: Vec<_> = result
            .tools
            .iter()
            .filter(|t| t.name.starts_with("test_plugin"))
            .collect();
        assert_eq!(
            plugin_tools.len(),
            2,
            "Multi-method plugin should generate 2 tools"
        );

        let tool_a = result
            .tools
            .iter()
            .find(|t| t.name == "test_plugin___method_a")
            .expect("Should have test_plugin___method_a tool");
        assert_eq!(tool_a.name, "test_plugin___method_a");
        assert_eq!(tool_a.description.as_ref().unwrap(), "First method");

        assert_schema_required_fields(&tool_a.input_schema, &["arguments", "settings"]);

        let expected_settings = json!({
            "type": "object",
            "properties": {
                "api_key": {
                    "type": "string"
                }
            },
            "description": "Tool init settings"
        });
        assert_schema_property(&tool_a.input_schema, "settings", &expected_settings);

        let expected_arguments_a = json!({
            "type": "object",
            "properties": {
                "param_a": {
                    "type": "string"
                }
            },
            "description": "Tool arguments"
        });
        assert_schema_property(&tool_a.input_schema, "arguments", &expected_arguments_a);

        let tool_b = result
            .tools
            .iter()
            .find(|t| t.name == "test_plugin___method_b")
            .expect("Should have test_plugin___method_b tool");
        assert_eq!(tool_b.name, "test_plugin___method_b");
        assert_eq!(tool_b.description.as_ref().unwrap(), "Second method");

        assert_schema_required_fields(&tool_b.input_schema, &["arguments", "settings"]);

        let expected_arguments_b = json!({
            "type": "object",
            "properties": {
                "param_b": {
                    "type": "integer"
                }
            },
            "description": "Tool arguments"
        });
        assert_schema_property(&tool_b.input_schema, "arguments", &expected_arguments_b);
        assert_schema_property(&tool_b.input_schema, "settings", &expected_settings);
    }

    /// WORKFLOW worker: the tool input schema must surface the workflow's `input`
    /// schema directly (no `settings`/`arguments` wrapper), so callers pass input
    /// fields at the top level.
    #[test]
    fn test_workflow_worker_tool_schema_is_unwrapped() {
        let result =
            ToolConverter::convert_functions_to_mcp_tools(vec![make_workflow_worker_spec()])
                .unwrap();
        let tool = result
            .tools
            .iter()
            .find(|t| t.name == "wf_worker")
            .expect("workflow worker tool should exist");

        // Description comes from the method (workflow summary), not the generic description.
        assert_eq!(tool.description.as_ref().unwrap(), "wf worker summary");

        let props = tool
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("input schema should have properties");

        // No wrapper keys: input fields are exposed directly at the top level.
        assert!(
            !props.contains_key("arguments"),
            "worker tool schema must not wrap input under 'arguments': {props:?}"
        );
        assert!(
            !props.contains_key("settings"),
            "worker tool schema must not contain 'settings': {props:?}"
        );
        assert!(
            props.contains_key("owner") && props.contains_key("repo"),
            "workflow input fields should be top-level properties: {props:?}"
        );
        assert_eq!(
            tool.input_schema.get("type").and_then(|v| v.as_str()),
            Some("object")
        );
    }

    /// Non-WORKFLOW worker: same unwrapped shape as a workflow worker. The
    /// distinction is worker-vs-runner, not the runner type.
    #[test]
    fn test_command_worker_tool_schema_is_unwrapped() {
        let result =
            ToolConverter::convert_functions_to_mcp_tools(vec![make_command_worker_spec()])
                .unwrap();
        let tool = result
            .tools
            .iter()
            .find(|t| t.name == "cmd_worker")
            .expect("command worker tool should exist");

        let props = tool
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("input schema should have properties");

        assert!(
            !props.contains_key("arguments") && !props.contains_key("settings"),
            "worker tool schema must be unwrapped: {props:?}"
        );
        assert!(
            props.contains_key("command"),
            "arguments fields should be top-level: {props:?}"
        );
    }

    /// Runner direct execution (worker_id absent) keeps the wrapped
    /// `{ settings, arguments }` structure, including for WORKFLOW runners.
    #[test]
    fn test_runner_tool_schema_stays_wrapped() {
        let result =
            ToolConverter::convert_functions_to_mcp_tools(vec![make_reusable_workflow_spec()])
                .unwrap();
        let tool = result
            .tools
            .iter()
            .find(|t| t.name == "test_workflow")
            .expect("runner tool should exist");

        let props = tool
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("input schema should have properties");
        assert!(
            props.contains_key("arguments"),
            "runner tool schema must keep the 'arguments' wrapper: {props:?}"
        );
    }

    /// A worker whose arguments schema omits the top-level `type` (e.g. a WORKFLOW
    /// worker defined with `input: {}`) must still produce an MCP-valid inputSchema
    /// with `"type": "object"`, otherwise clients reject the tool list.
    #[test]
    fn test_worker_tool_schema_defaults_missing_type_to_object() {
        let mut method_schemas = std::collections::HashMap::new();
        method_schemas.insert(
            proto::DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: Some("empty input".to_string()),
                // Mimics `input: {}` — a JSON object schema without a `type` key.
                arguments_schema: "{}".to_string(),
                result_schema: None,
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                annotations: None,
            },
        );
        let spec = FunctionSpecs {
            name: "empty_input_worker".to_string(),
            description: "desc".to_string(),
            settings_schema: String::new(),
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
            runner_type: proto::jobworkerp::data::RunnerType::Workflow as i32,
            worker_id: Some(proto::jobworkerp::data::WorkerId { value: 99 }),
            ..Default::default()
        };

        let result = ToolConverter::convert_functions_to_mcp_tools(vec![spec]).unwrap();
        let tool = result
            .tools
            .iter()
            .find(|t| t.name == "empty_input_worker")
            .expect("worker tool should exist");
        assert_eq!(
            tool.input_schema.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "missing top-level 'type' must default to 'object' for MCP validity"
        );
    }

    /// An existing top-level `type` on the worker arguments schema must be preserved
    /// (not overwritten with "object").
    #[test]
    fn test_worker_tool_schema_preserves_existing_type() {
        let mut method_schemas = std::collections::HashMap::new();
        method_schemas.insert(
            proto::DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: Some("typed".to_string()),
                arguments_schema:
                    json!({"type": "object", "properties": {"a": {"type": "string"}}}).to_string(),
                result_schema: None,
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                annotations: None,
            },
        );
        let spec = FunctionSpecs {
            name: "typed_worker".to_string(),
            description: "desc".to_string(),
            settings_schema: String::new(),
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
            runner_type: proto::jobworkerp::data::RunnerType::Command as i32,
            worker_id: Some(proto::jobworkerp::data::WorkerId { value: 100 }),
            ..Default::default()
        };

        let result = ToolConverter::convert_functions_to_mcp_tools(vec![spec]).unwrap();
        let tool = result
            .tools
            .iter()
            .find(|t| t.name == "typed_worker")
            .unwrap();
        assert_eq!(
            tool.input_schema.get("type").and_then(|v| v.as_str()),
            Some("object")
        );
        assert!(
            tool.input_schema
                .get("properties")
                .and_then(|p| p.as_object())
                .is_some_and(|p| p.contains_key("a")),
            "existing properties must be preserved"
        );
    }

    #[test]
    fn test_convert_functions_to_ollama_tools() {
        let specs = vec![
            make_single_schema_spec(),
            make_reusable_workflow_spec(),
            make_mcp_tools_spec(),
            make_multi_method_plugin_spec(),
        ];
        let result = ToolConverter::convert_functions_to_ollama_tools(specs);

        // SingleSchema
        let tool = result
            .iter()
            .find(|t| t.function.name == "test_single")
            .unwrap();
        println!("single Tool: {tool:#?}");
        assert_eq!(tool.function.name, "test_single");
        assert_eq!(tool.function.description, "desc_single");

        let schema_value = tool.function.parameters.clone().to_value();

        let schema_obj = schema_value.as_object().unwrap();
        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            props.contains_key("settings"),
            "Schema should contain settings property"
        );
        assert!(
            props.contains_key("arguments"),
            "Schema should contain arguments property"
        );

        let settings = props.get("settings").and_then(|s| s.as_object()).unwrap();
        let settings_props = settings
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            settings_props.contains_key("setting_a"),
            "Settings should contain setting_a property"
        );

        let setting_a = settings_props
            .get("setting_a")
            .and_then(|s| s.as_object())
            .unwrap();
        assert_eq!(
            setting_a.get("type").and_then(|t| t.as_str()),
            Some("string"),
            "setting_a should have type string"
        );

        let arguments = props.get("arguments").and_then(|s| s.as_object()).unwrap();
        let args_props = arguments
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props.contains_key("arg_a"),
            "Arguments should contain arg_a property"
        );

        let arg_a = args_props.get("arg_a").and_then(|s| s.as_object()).unwrap();
        assert_eq!(
            arg_a.get("type").and_then(|t| t.as_str()),
            Some("boolean"),
            "arg_a should have type boolean"
        );

        // ReusableWorkflow
        let tool = result
            .iter()
            .find(|t| t.function.name == "test_workflow")
            .unwrap();
        println!("workflow Tool: {tool:#?}");
        assert_eq!(tool.function.name, "test_workflow");
        assert_eq!(tool.function.description, "desc_workflow");

        let schema_value = tool.function.parameters.clone().to_value();
        let schema_obj = schema_value.as_object().unwrap();

        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        // ReusableWorkflow now uses settings/arguments structure
        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        assert!(
            props.contains_key("settings"),
            "Schema should contain settings property"
        );
        let settings = props.get("settings").and_then(|s| s.as_object()).unwrap();
        let settings_props = settings
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            settings_props.contains_key("setting_b"),
            "Settings should contain setting_b property"
        );
        let setting_b = settings_props
            .get("setting_b")
            .and_then(|s| s.as_object())
            .unwrap();
        assert_eq!(
            setting_b.get("type").and_then(|t| t.as_str()),
            Some("integer"),
            "setting_b should have type integer"
        );

        assert!(
            props.contains_key("arguments"),
            "Schema should contain arguments property"
        );
        let arguments = props.get("arguments").and_then(|a| a.as_object()).unwrap();
        let args_props = arguments
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props.contains_key("arg_b"),
            "Arguments should contain arg_b property"
        );
        let arg_b = args_props.get("arg_b").and_then(|a| a.as_object()).unwrap();
        assert_eq!(
            arg_b.get("type").and_then(|t| t.as_str()),
            Some("number"),
            "arg_b should have type number"
        );

        // McpTools
        let tool = result
            .iter()
            .find(|t| t.function.name == "test_mcp___inner")
            .unwrap();
        println!("mcp Tool: {tool:#?}");
        assert_eq!(tool.function.name, "test_mcp___inner");
        assert_eq!(tool.function.description, "desc_inner");

        let schema_value = tool.function.parameters.clone().to_value();
        let schema_obj = schema_value.as_object().unwrap();

        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            props.contains_key("arguments"),
            "Schema should contain arguments property"
        );

        let arguments = props.get("arguments").and_then(|a| a.as_object()).unwrap();
        let args_props = arguments
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props.contains_key("c"),
            "Arguments should contain c property"
        );
        let prop_c = args_props.get("c").and_then(|s| s.as_object()).unwrap();
        assert_eq!(
            prop_c.get("type").and_then(|t| t.as_str()),
            Some("boolean"),
            "c should have type boolean"
        );

        // Multi-method Plugin - should generate 2 tools
        let plugin_tools: Vec<_> = result
            .iter()
            .filter(|t| t.function.name.starts_with("test_plugin"))
            .collect();
        assert_eq!(
            plugin_tools.len(),
            2,
            "Multi-method plugin should generate 2 tools"
        );

        let tool_a = result
            .iter()
            .find(|t| t.function.name == "test_plugin___method_a")
            .expect("Should have test_plugin___method_a tool");
        assert_eq!(tool_a.function.name, "test_plugin___method_a");
        assert_eq!(tool_a.function.description, "First method");

        let schema_value_a = tool_a.function.parameters.clone().to_value();
        let schema_obj_a = schema_value_a.as_object().unwrap();
        let props_a = schema_obj_a
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        assert!(
            props_a.contains_key("settings"),
            "Schema should contain settings property"
        );
        assert!(
            props_a.contains_key("arguments"),
            "Schema should contain arguments property"
        );

        let settings_a = props_a.get("settings").and_then(|s| s.as_object()).unwrap();
        let settings_props_a = settings_a
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            settings_props_a.contains_key("api_key"),
            "Settings should contain api_key"
        );

        let arguments_a = props_a
            .get("arguments")
            .and_then(|a| a.as_object())
            .unwrap();
        let args_props_a = arguments_a
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props_a.contains_key("param_a"),
            "Arguments should contain param_a"
        );
        let param_a = args_props_a
            .get("param_a")
            .and_then(|p| p.as_object())
            .unwrap();
        assert_eq!(
            param_a.get("type").and_then(|t| t.as_str()),
            Some("string"),
            "param_a should have type string"
        );

        let tool_b = result
            .iter()
            .find(|t| t.function.name == "test_plugin___method_b")
            .expect("Should have test_plugin___method_b tool");
        assert_eq!(tool_b.function.name, "test_plugin___method_b");
        assert_eq!(tool_b.function.description, "Second method");

        let schema_value_b = tool_b.function.parameters.clone().to_value();
        let schema_obj_b = schema_value_b.as_object().unwrap();
        let props_b = schema_obj_b
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        let arguments_b = props_b
            .get("arguments")
            .and_then(|a| a.as_object())
            .unwrap();
        let args_props_b = arguments_b
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props_b.contains_key("param_b"),
            "Arguments should contain param_b"
        );
        let param_b = args_props_b
            .get("param_b")
            .and_then(|p| p.as_object())
            .unwrap();
        assert_eq!(
            param_b.get("type").and_then(|t| t.as_str()),
            Some("integer"),
            "param_b should have type integer"
        );
    }

    #[test]
    fn test_command_runner_schema_generation() {
        let mut method_schemas = std::collections::HashMap::new();
        method_schemas.insert(
            proto::DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::function::data::MethodSchema {
                description: Some("Run shell commands".to_string()),
                arguments_schema: r#"{"type": "object", "properties": {"command": {"type": "string", "description": "The command to execute"}, "args": {"type": "array", "items": {"type": "string"}, "description": "Command arguments"}, "with_memory_monitoring": {"type": "boolean", "description": "Enable memory monitoring"}}, "required": ["command", "args"]}"#.to_string(),
                result_schema: Some(r#"{"type": "object", "properties": {"exit_code": {"type": "integer"}, "stdout": {"type": "string"}, "stderr": {"type": "string"}}}"#.to_string()),
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                annotations: None,
            },
        );

        let command_spec = FunctionSpecs {
            name: "COMMAND".to_string(),
            description: "Run shell commands".to_string(),
            settings_schema: r#"{"type": "object", "properties": {}}"#.to_string(),
            methods: Some(proto::jobworkerp::function::data::MethodSchemaMap {
                schemas: method_schemas,
            }),
            runner_type: proto::jobworkerp::data::RunnerType::Command as i32,
            runner_id: None,
            worker_id: None,
        };

        let ollama_tools = ToolConverter::convert_functions_to_ollama_tools(vec![command_spec]);

        assert_eq!(ollama_tools.len(), 1, "Should generate exactly one tool");

        let tool = &ollama_tools[0];
        assert_eq!(tool.function.name, "COMMAND");
        assert_eq!(tool.function.description, "Run shell commands");

        let schema_value = tool.function.parameters.clone().to_value();
        let schema_obj = schema_value.as_object().unwrap();

        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        assert!(
            props.contains_key("settings"),
            "Schema should contain settings"
        );
        assert!(
            props.contains_key("arguments"),
            "Schema should contain arguments"
        );

        let arguments = props.get("arguments").and_then(|a| a.as_object()).unwrap();
        let arg_props = arguments
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        assert!(
            arg_props.contains_key("command"),
            "Arguments should contain 'command' field"
        );
        assert!(
            arg_props.contains_key("args"),
            "Arguments should contain 'args' field"
        );
        assert!(
            arg_props.contains_key("with_memory_monitoring"),
            "Arguments should contain 'with_memory_monitoring' field"
        );

        let command_field = arg_props
            .get("command")
            .and_then(|c| c.as_object())
            .unwrap();
        assert_eq!(
            command_field.get("type").and_then(|t| t.as_str()),
            Some("string"),
            "Command field should be a string"
        );

        let args_field = arg_props.get("args").and_then(|a| a.as_object()).unwrap();
        assert_eq!(
            args_field.get("type").and_then(|t| t.as_str()),
            Some("array"),
            "Args field should be an array"
        );
    }

    #[test]
    fn test_convert_functions_to_genai_tools() {
        let specs = vec![
            make_single_schema_spec(),
            make_reusable_workflow_spec(),
            make_mcp_tools_spec(),
            make_multi_method_plugin_spec(),
        ];
        let result = ToolConverter::convert_functions_to_genai_tools(specs);

        // SingleSchema
        let tool = result
            .iter()
            .find(|t| t.name.as_str() == "test_single")
            .unwrap();
        assert_eq!(tool.name.as_str(), "test_single");
        assert_eq!(tool.description.as_ref().unwrap(), "desc_single");

        let schema = tool.schema.as_ref().unwrap();
        let schema_obj = schema.as_object().unwrap();

        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            props.contains_key("settings"),
            "Schema should contain settings property"
        );
        assert!(
            props.contains_key("arguments"),
            "Schema should contain arguments property"
        );

        let settings = props.get("settings").and_then(|s| s.as_object()).unwrap();
        let settings_props = settings
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            settings_props.contains_key("setting_a"),
            "Settings should contain setting_a property"
        );

        let setting_a = settings_props
            .get("setting_a")
            .and_then(|s| s.as_object())
            .unwrap();
        assert_eq!(
            setting_a.get("type").and_then(|t| t.as_str()),
            Some("string"),
            "setting_a should have type string"
        );

        let arguments = props.get("arguments").and_then(|s| s.as_object()).unwrap();
        let args_props = arguments
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props.contains_key("arg_a"),
            "Arguments should contain arg_a property"
        );

        let arg_a = args_props.get("arg_a").and_then(|s| s.as_object()).unwrap();
        assert_eq!(
            arg_a.get("type").and_then(|t| t.as_str()),
            Some("boolean"),
            "arg_a should have type boolean"
        );

        // ReusableWorkflow
        let tool = result
            .iter()
            .find(|t| t.name.as_str() == "test_workflow")
            .unwrap();
        assert_eq!(tool.name.as_str(), "test_workflow");
        assert_eq!(tool.description.as_ref().unwrap(), "desc_workflow");

        let schema = tool.schema.as_ref().unwrap();
        let schema_obj = schema.as_object().unwrap();

        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        // ReusableWorkflow now uses settings/arguments structure
        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        assert!(
            props.contains_key("settings"),
            "Schema should contain settings property"
        );
        let settings = props.get("settings").and_then(|s| s.as_object()).unwrap();
        let settings_props = settings
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            settings_props.contains_key("setting_b"),
            "Settings should contain setting_b property"
        );
        let setting_b = settings_props
            .get("setting_b")
            .and_then(|s| s.as_object())
            .unwrap();
        assert_eq!(
            setting_b.get("type").and_then(|t| t.as_str()),
            Some("integer"),
            "setting_b should have type integer"
        );

        assert!(
            props.contains_key("arguments"),
            "Schema should contain arguments property"
        );
        let arguments = props.get("arguments").and_then(|a| a.as_object()).unwrap();
        let args_props = arguments
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props.contains_key("arg_b"),
            "Arguments should contain arg_b property"
        );
        let arg_b = args_props.get("arg_b").and_then(|a| a.as_object()).unwrap();
        assert_eq!(
            arg_b.get("type").and_then(|t| t.as_str()),
            Some("number"),
            "arg_b should have type number"
        );

        // McpTools
        let tool = result
            .iter()
            .find(|t| t.name.as_str() == "test_mcp___inner")
            .unwrap();
        assert_eq!(tool.name.as_str(), "test_mcp___inner");
        assert_eq!(tool.description.as_ref().unwrap(), "desc_inner");

        let schema = tool.schema.as_ref().unwrap();
        let schema_obj = schema.as_object().unwrap();

        assert_eq!(
            schema_obj.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "Schema should have type 'object'"
        );

        let props = schema_obj
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            props.contains_key("arguments"),
            "Schema should contain arguments property"
        );

        let arguments = props.get("arguments").and_then(|a| a.as_object()).unwrap();
        let args_props = arguments
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props.contains_key("c"),
            "Arguments should contain c property"
        );
        let prop_c = args_props.get("c").and_then(|s| s.as_object()).unwrap();
        assert_eq!(
            prop_c.get("type").and_then(|t| t.as_str()),
            Some("boolean"),
            "c should have type boolean"
        );

        // Multi-method Plugin - should generate 2 tools
        let plugin_tools: Vec<_> = result
            .iter()
            .filter(|t| t.name.as_str().starts_with("test_plugin"))
            .collect();
        assert_eq!(
            plugin_tools.len(),
            2,
            "Multi-method plugin should generate 2 tools"
        );

        let tool_a = result
            .iter()
            .find(|t| t.name.as_str() == "test_plugin___method_a")
            .expect("Should have test_plugin___method_a tool");
        assert_eq!(tool_a.name.as_str(), "test_plugin___method_a");
        assert_eq!(tool_a.description.as_ref().unwrap(), "First method");

        let schema_a = tool_a.schema.as_ref().unwrap();
        let schema_obj_a = schema_a.as_object().unwrap();
        let props_a = schema_obj_a
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        assert!(
            props_a.contains_key("settings"),
            "Schema should contain settings property"
        );
        assert!(
            props_a.contains_key("arguments"),
            "Schema should contain arguments property"
        );

        let settings_a = props_a.get("settings").and_then(|s| s.as_object()).unwrap();
        let settings_props_a = settings_a
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            settings_props_a.contains_key("api_key"),
            "Settings should contain api_key"
        );

        let arguments_a = props_a
            .get("arguments")
            .and_then(|a| a.as_object())
            .unwrap();
        let args_props_a = arguments_a
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props_a.contains_key("param_a"),
            "Arguments should contain param_a"
        );
        let param_a = args_props_a
            .get("param_a")
            .and_then(|p| p.as_object())
            .unwrap();
        assert_eq!(
            param_a.get("type").and_then(|t| t.as_str()),
            Some("string"),
            "param_a should have type string"
        );

        let tool_b = result
            .iter()
            .find(|t| t.name.as_str() == "test_plugin___method_b")
            .expect("Should have test_plugin___method_b tool");
        assert_eq!(tool_b.name.as_str(), "test_plugin___method_b");
        assert_eq!(tool_b.description.as_ref().unwrap(), "Second method");

        let schema_b = tool_b.schema.as_ref().unwrap();
        let schema_obj_b = schema_b.as_object().unwrap();
        let props_b = schema_obj_b
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();

        let arguments_b = props_b
            .get("arguments")
            .and_then(|a| a.as_object())
            .unwrap();
        let args_props_b = arguments_b
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap();
        assert!(
            args_props_b.contains_key("param_b"),
            "Arguments should contain param_b"
        );
        let param_b = args_props_b
            .get("param_b")
            .and_then(|p| p.as_object())
            .unwrap();
        assert_eq!(
            param_b.get("type").and_then(|t| t.as_str()),
            Some("integer"),
            "param_b should have type integer"
        );
    }

    #[test]
    fn test_mcp_tool_to_genai_preserves_name() {
        // Regression guard: ensure mcp_tool_to_genai keeps the bare tool name even
        // after ToolName became an enum in genai 0.6 (Custom variant must serialise
        // as a plain string for OTel / API compatibility).
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"x": {"type": "string"}}
        });
        let input_schema = schema.as_object().unwrap().clone();
        let mcp_tool = Tool::new(
            "my_tool".to_string(),
            "tool description".to_string(),
            input_schema,
        );
        let genai_tool = ToolConverter::mcp_tool_to_genai(&mcp_tool);
        assert_eq!(genai_tool.name.as_str(), "my_tool");
        assert_eq!(genai_tool.description.as_deref(), Some("tool description"));

        // OTel/API compatibility: name must serialize as a bare JSON string, not
        // a tagged enum (e.g. not {"Custom": "my_tool"}).
        let serialised = serde_json::to_value(&genai_tool).unwrap();
        assert_eq!(serialised["name"], serde_json::json!("my_tool"));
    }

    #[test]
    fn test_function_set_name_validation() {
        assert!(ToolConverter::is_valid_function_set_name("web-tools"));
        assert!(ToolConverter::is_valid_function_set_name("data_processing"));
        assert!(ToolConverter::is_valid_function_set_name("CodeExec123"));
        assert!(!ToolConverter::is_valid_function_set_name("bad___name"));
        assert!(!ToolConverter::is_valid_function_set_name("has spaces"));
        assert!(!ToolConverter::is_valid_function_set_name("has.dots"));
    }

    #[test]
    fn test_convert_function_set_to_selector_tool() {
        let tool = ToolConverter::convert_function_set_to_selector_tool(
            "web-tools",
            "Tools for web scraping",
            &[
                ("fetch_html".to_string(), "Fetch HTML from URL".to_string()),
                ("screenshot".to_string(), "Take screenshot".to_string()),
            ],
        );
        assert!(tool.is_some());
        let tool = tool.unwrap();
        assert_eq!(tool.name, "select_toolset_web-tools");
        let desc = tool.description.as_ref().unwrap();
        assert!(desc.contains("web-tools"), "Should contain set name");
        assert!(
            desc.contains("Tools for web scraping"),
            "Should contain description"
        );
        assert!(desc.contains("fetch_html"), "Should list tools");
        assert!(desc.contains("screenshot"), "Should list tools");
        assert!(
            desc.contains("Activate") || desc.contains("Call this"),
            "Should instruct LLM to call this function"
        );

        // Should have empty properties schema
        let props = tool.input_schema.get("properties").unwrap();
        assert!(props.as_object().unwrap().is_empty());
    }

    #[test]
    fn test_convert_function_set_to_selector_tool_no_tools() {
        let tool =
            ToolConverter::convert_function_set_to_selector_tool("empty-set", "An empty set", &[]);
        assert!(tool.is_some());
        let tool = tool.unwrap();
        assert_eq!(tool.name, "select_toolset_empty-set");
        let desc = tool.description.as_ref().unwrap();
        assert!(
            desc.contains("Activate") || desc.contains("Call this"),
            "Should instruct LLM to call this function even with no tools"
        );
    }

    #[test]
    fn test_convert_function_set_to_selector_tool_invalid_name() {
        let tool = ToolConverter::convert_function_set_to_selector_tool("bad___name", "desc", &[]);
        assert!(tool.is_none());
    }

    #[test]
    fn test_convert_function_set_selector_tools_to_providers() {
        let tools = vec![
            ToolConverter::convert_function_set_to_selector_tool(
                "set-a",
                "Description A",
                &[("tool1".to_string(), "desc1".to_string())],
            )
            .unwrap(),
        ];

        let ollama_tools = ToolConverter::convert_function_set_selector_tools_to_ollama(&tools);
        assert_eq!(ollama_tools.len(), 1);
        assert_eq!(ollama_tools[0].function.name, "select_toolset_set-a");

        let genai_tools = ToolConverter::convert_function_set_selector_tools_to_genai(&tools);
        assert_eq!(genai_tools.len(), 1);
        assert_eq!(genai_tools[0].name.as_str(), "select_toolset_set-a");
    }

    #[test]
    fn test_is_selector_tool() {
        assert!(ToolConverter::is_selector_tool("select_toolset_web-tools"));
        assert!(ToolConverter::is_selector_tool("select_toolset_"));
        assert!(!ToolConverter::is_selector_tool("http_request"));
        assert!(!ToolConverter::is_selector_tool("select_toolset"));
        assert!(!ToolConverter::is_selector_tool(""));
    }

    // -- Tests for shared selector filtering helpers --

    /// Simple struct implementing ToolCallName for testing
    struct MockToolCall {
        name: String,
    }
    impl ToolCallName for MockToolCall {
        fn tool_name(&self) -> &str {
            &self.name
        }
    }

    #[test]
    fn test_tool_call_name_for_tool_execution_request() {
        let req = ToolExecutionRequest {
            call_id: "id1".to_string(),
            fn_name: "my_tool".to_string(),
            fn_arguments: "{}".to_string(),
        };
        assert_eq!(req.tool_name(), "my_tool");
    }

    #[test]
    fn test_filter_selector_tools_removes_selectors() {
        let calls = vec![
            MockToolCall {
                name: "select_toolset_web".to_string(),
            },
            MockToolCall {
                name: "http_request".to_string(),
            },
            MockToolCall {
                name: "select_toolset_data".to_string(),
            },
            MockToolCall {
                name: "command".to_string(),
            },
        ];
        let filtered = ToolConverter::filter_selector_tools(calls);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].tool_name(), "http_request");
        assert_eq!(filtered[1].tool_name(), "command");
    }

    #[test]
    fn test_filter_selector_tools_all_selectors_returns_empty() {
        let calls = vec![
            MockToolCall {
                name: "select_toolset_a".to_string(),
            },
            MockToolCall {
                name: "select_toolset_b".to_string(),
            },
        ];
        let filtered = ToolConverter::filter_selector_tools(calls);
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_filter_selector_tools_empty_input() {
        let calls: Vec<MockToolCall> = vec![];
        let filtered = ToolConverter::filter_selector_tools(calls);
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_retain_non_selector_tools() {
        let mut calls = vec![
            MockToolCall {
                name: "select_toolset_web".to_string(),
            },
            MockToolCall {
                name: "http_request".to_string(),
            },
        ];
        ToolConverter::retain_non_selector_tools(&mut calls);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name(), "http_request");
    }

    #[test]
    fn test_evaluate_auto_select_finds_selector() {
        let calls = vec![
            MockToolCall {
                name: "select_toolset_web-tools".to_string(),
            },
            MockToolCall {
                name: "other_tool".to_string(),
            },
        ];
        let mut names = std::collections::HashSet::new();
        names.insert("select_toolset_web-tools".to_string());

        let args = LlmChatArgs::default();
        let result = ToolConverter::evaluate_auto_select(&calls, &names, args, "").unwrap();
        assert_eq!(result.selected_set_name, "web-tools");
        // Even when original args have no function_options, selected_set_name must be propagated
        let fo = result.second_args.function_options.unwrap();
        assert_eq!(fo.function_set_name, Some("web-tools".to_string()));
        assert_eq!(fo.auto_select_function_set, Some(false));
    }

    #[test]
    fn test_evaluate_auto_select_with_function_options() {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::FunctionOptions;

        let calls = vec![MockToolCall {
            name: "select_toolset_my-set".to_string(),
        }];
        let mut names = std::collections::HashSet::new();
        names.insert("select_toolset_my-set".to_string());

        let args = LlmChatArgs {
            function_options: Some(FunctionOptions {
                auto_select_function_set: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let result =
            ToolConverter::evaluate_auto_select(&calls, &names, args, " (stream)").unwrap();
        assert_eq!(result.selected_set_name, "my-set");
        let fo = result.second_args.function_options.unwrap();
        assert_eq!(fo.function_set_name, Some("my-set".to_string()));
        assert_eq!(fo.auto_select_function_set, Some(false));
    }

    #[test]
    fn test_evaluate_auto_select_no_selector_returns_error() {
        let calls = vec![MockToolCall {
            name: "http_request".to_string(),
        }];
        let mut names = std::collections::HashSet::new();
        names.insert("select_toolset_web".to_string());

        let args = LlmChatArgs::default();
        let result = ToolConverter::evaluate_auto_select(&calls, &names, args, "");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Auto-select failed"));
    }

    #[test]
    fn test_skip_selector_tool_execution_with_selector() {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
            ChatMessage as ProtoChatMessage, MessageContent,
            message_content::Content as ProtoContent,
        };

        let req = ToolExecutionRequest {
            call_id: "call-1".to_string(),
            fn_name: "select_toolset_web".to_string(),
            fn_arguments: "{}".to_string(),
        };

        let mut messages = vec![ProtoChatMessage {
            role: ChatRole::Tool.into(),
            content: Some(MessageContent {
                content: Some(ProtoContent::ToolExecutionRequests(
                    jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::ToolExecutionRequests {
                        requests: vec![req.clone()],
                    },
                )),
            }),
        }];

        assert!(ToolConverter::skip_selector_tool_execution(
            &req,
            &mut messages
        ));
        // Message should have been replaced with text
        if let Some(ref content) = messages[0].content {
            match &content.content {
                Some(ProtoContent::Text(t)) => assert_eq!(t, "Tool selection completed"),
                other => panic!("Expected Text content, got {:?}", other),
            }
        }
    }

    #[test]
    fn test_skip_selector_tool_execution_with_normal_tool() {
        let req = ToolExecutionRequest {
            call_id: "call-1".to_string(),
            fn_name: "http_request".to_string(),
            fn_arguments: "{}".to_string(),
        };
        let mut messages = vec![];
        assert!(!ToolConverter::skip_selector_tool_execution(
            &req,
            &mut messages
        ));
    }

    #[test]
    fn test_replace_tool_execution_preserves_other_requests() {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
            ChatMessage as ProtoChatMessage, MessageContent,
            message_content::Content as ProtoContent,
        };

        let req1 = ToolExecutionRequest {
            call_id: "call-1".to_string(),
            fn_name: "tool_a".to_string(),
            fn_arguments: "{}".to_string(),
        };
        let req2 = ToolExecutionRequest {
            call_id: "call-2".to_string(),
            fn_name: "tool_b".to_string(),
            fn_arguments: "{}".to_string(),
        };

        let mut messages = vec![ProtoChatMessage {
            role: ChatRole::Tool.into(),
            content: Some(MessageContent {
                content: Some(ProtoContent::ToolExecutionRequests(
                    jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::ToolExecutionRequests {
                        requests: vec![req1, req2],
                    },
                )),
            }),
        }];

        // Replace only call-1
        ToolConverter::replace_tool_execution_with_result(&mut messages, "call-1", "result-1");

        // Should now have 2 messages: original with only call-2, and new Text with result-1
        assert_eq!(messages.len(), 2);

        // First message should still have call-2
        if let Some(ref content) = messages[0].content {
            match &content.content {
                Some(ProtoContent::ToolExecutionRequests(reqs)) => {
                    assert_eq!(reqs.requests.len(), 1);
                    assert_eq!(reqs.requests[0].call_id, "call-2");
                }
                other => panic!("Expected ToolExecutionRequests, got {:?}", other),
            }
        }

        // Second message should be the text result
        if let Some(ref content) = messages[1].content {
            match &content.content {
                Some(ProtoContent::Text(t)) => assert_eq!(t, "result-1"),
                other => panic!("Expected Text content, got {:?}", other),
            }
        }

        // Now replace call-2 — should collapse to Text
        ToolConverter::replace_tool_execution_with_result(&mut messages, "call-2", "result-2");

        // First message should now be Text
        assert_eq!(messages.len(), 2);
        if let Some(ref content) = messages[0].content {
            match &content.content {
                Some(ProtoContent::Text(t)) => assert_eq!(t, "result-2"),
                other => panic!("Expected Text content, got {:?}", other),
            }
        }
    }

    #[test]
    fn test_evaluate_auto_select_none_function_options_propagates() {
        let calls = vec![MockToolCall {
            name: "select_toolset_api-tools".to_string(),
        }];
        let mut names = std::collections::HashSet::new();
        names.insert("select_toolset_api-tools".to_string());

        // function_options is None
        let args = LlmChatArgs {
            function_options: None,
            ..Default::default()
        };
        let result = ToolConverter::evaluate_auto_select(&calls, &names, args, "").unwrap();
        assert_eq!(result.selected_set_name, "api-tools");

        let fo = result
            .second_args
            .function_options
            .expect("function_options should be Some");
        assert_eq!(fo.function_set_name, Some("api-tools".to_string()));
        assert_eq!(fo.auto_select_function_set, Some(false));
        assert!(fo.use_function_calling);
    }

    // ====== resolve_tool_results tests ======

    fn assistant_with_tool_calls(
        calls: Vec<(&str, &str)>,
    ) -> jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatMessage {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
            ChatMessage as ProtoChatMessage, MessageContent,
            message_content::{Content as ProtoContent, ToolCall, ToolCalls},
        };
        ProtoChatMessage {
            role: ChatRole::Assistant.into(),
            content: Some(MessageContent {
                content: Some(ProtoContent::ToolCalls(ToolCalls {
                    calls: calls
                        .into_iter()
                        .map(|(call_id, fn_name)| ToolCall {
                            call_id: call_id.to_string(),
                            fn_name: fn_name.to_string(),
                            fn_arguments: "{}".to_string(),
                        })
                        .collect(),
                })),
            }),
        }
    }

    fn tool_msg_with_results(
        results: Vec<(&str, &str, &str, bool)>,
    ) -> jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatMessage {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
            ChatMessage as ProtoChatMessage, MessageContent,
            message_content::{Content as ProtoContent, ToolResult, ToolResults},
        };
        ProtoChatMessage {
            role: ChatRole::Tool.into(),
            content: Some(MessageContent {
                content: Some(ProtoContent::ToolResults(ToolResults {
                    results: results
                        .into_iter()
                        .map(|(call_id, fn_name, content, is_error)| ToolResult {
                            call_id: call_id.to_string(),
                            fn_name: fn_name.to_string(),
                            content: content.to_string(),
                            is_error,
                        })
                        .collect(),
                })),
            }),
        }
    }

    /// Pull the embedded `ToolResults` payload out of a TOOL-role test
    /// message. Panics if the variant is wrong — fine for tests.
    fn extract_tool_results(
        msg: &jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatMessage,
    ) -> jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::ToolResults
    {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content as ProtoContent;
        match msg.content.as_ref().unwrap().content.as_ref().unwrap() {
            ProtoContent::ToolResults(r) => r.clone(),
            other => panic!("expected ToolResults, got {other:?}"),
        }
    }

    #[test]
    fn test_resolve_tool_results_explicit_fn_name() {
        let messages = [
            assistant_with_tool_calls(vec![("c1", "tool_a")]),
            tool_msg_with_results(vec![("c1", "tool_a", "ok", false)]),
        ];
        let results = extract_tool_results(&messages[1]);
        let resolved = ToolConverter::resolve_tool_results(&messages[..1], &results).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].call_id, "c1");
        assert_eq!(resolved[0].fn_name, "tool_a");
        assert_eq!(resolved[0].content, "ok");
        assert!(!resolved[0].is_error);
    }

    #[test]
    fn test_resolve_tool_results_fn_name_reverse_resolved() {
        // fn_name is empty in the ToolResult — should be filled from
        // the matching ASSISTANT ToolCall via reverse scan.
        let messages = [
            assistant_with_tool_calls(vec![("c1", "tool_a"), ("c2", "tool_b")]),
            tool_msg_with_results(vec![("c2", "", "result-b", false)]),
        ];
        let results = extract_tool_results(&messages[1]);
        let resolved = ToolConverter::resolve_tool_results(&messages[..1], &results).unwrap();
        assert_eq!(resolved[0].fn_name, "tool_b");
    }

    #[test]
    fn test_resolve_tool_results_reverse_scan_picks_latest_assistant() {
        // When the same call_id appears in multiple ASSISTANT messages,
        // the most-recent declaration must win.
        let messages = [
            assistant_with_tool_calls(vec![("c1", "old_name")]),
            assistant_with_tool_calls(vec![("c1", "new_name")]),
            tool_msg_with_results(vec![("c1", "", "ok", false)]),
        ];
        let results = extract_tool_results(&messages[2]);
        let resolved = ToolConverter::resolve_tool_results(&messages[..2], &results).unwrap();
        assert_eq!(resolved[0].fn_name, "new_name");
    }

    #[test]
    fn test_resolve_tool_results_empty_results_bails() {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::ToolResults;
        let messages = [assistant_with_tool_calls(vec![("c1", "tool_a")])];
        let empty = ToolResults { results: vec![] };
        let err = ToolConverter::resolve_tool_results(&messages, &empty).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn test_resolve_tool_results_empty_call_id_bails() {
        let messages = [
            assistant_with_tool_calls(vec![("c1", "tool_a")]),
            tool_msg_with_results(vec![("", "tool_a", "ok", false)]),
        ];
        let results = extract_tool_results(&messages[1]);
        let err = ToolConverter::resolve_tool_results(&messages[..1], &results).unwrap_err();
        assert!(err.to_string().contains("call_id must not be empty"));
    }

    #[test]
    fn test_resolve_tool_results_unmatched_call_id_bails() {
        // Explicit fn_name is irrelevant — the call_id-not-found check fires
        // first now, so the error is deterministic.
        let messages = [
            assistant_with_tool_calls(vec![("c1", "tool_a")]),
            tool_msg_with_results(vec![("c-nonexistent", "tool_a", "ok", false)]),
        ];
        let results = extract_tool_results(&messages[1]);
        let err = ToolConverter::resolve_tool_results(&messages[..1], &results).unwrap_err();
        assert!(err.to_string().contains("call_id not found"));
    }

    #[test]
    fn test_resolve_tool_results_no_assistant_history_bails() {
        // No prior ASSISTANT messages at all → must bail.
        let messages = [tool_msg_with_results(vec![("c1", "", "ok", false)])];
        let results = extract_tool_results(&messages[0]);
        let err = ToolConverter::resolve_tool_results(&[], &results).unwrap_err();
        assert!(
            err.to_string()
                .contains("tool_results must follow an ASSISTANT"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn test_resolve_tool_results_user_in_between_bails() {
        // ASSISTANT(ToolCalls) → USER → TOOL(ToolResults) is invalid:
        // tool result must directly answer the most recent ASSISTANT
        // tool call turn (OpenAI / Gemini contract).
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
            ChatMessage as ProtoChatMessage, MessageContent,
            message_content::Content as ProtoContent,
        };
        let messages = [
            assistant_with_tool_calls(vec![("c1", "tool_a")]),
            ProtoChatMessage {
                role: ChatRole::User.into(),
                content: Some(MessageContent {
                    content: Some(ProtoContent::Text("interruption".to_string())),
                }),
            },
            tool_msg_with_results(vec![("c1", "tool_a", "ok", false)]),
        ];
        let results = extract_tool_results(&messages[2]);
        let err = ToolConverter::resolve_tool_results(&messages[..2], &results).unwrap_err();
        assert!(
            err.to_string()
                .contains("tool_results must follow an ASSISTANT"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn test_resolve_tool_results_assistant_without_tool_calls_bails() {
        // Direct predecessor is ASSISTANT, but with Text content — still
        // invalid because there is no tool_calls block to answer.
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
            ChatMessage as ProtoChatMessage, MessageContent,
            message_content::Content as ProtoContent,
        };
        let messages = [
            assistant_with_tool_calls(vec![("c1", "tool_a")]),
            ProtoChatMessage {
                role: ChatRole::Assistant.into(),
                content: Some(MessageContent {
                    content: Some(ProtoContent::Text("plain reply".to_string())),
                }),
            },
            tool_msg_with_results(vec![("c1", "tool_a", "ok", false)]),
        ];
        let results = extract_tool_results(&messages[2]);
        let err = ToolConverter::resolve_tool_results(&messages[..2], &results).unwrap_err();
        assert!(
            err.to_string()
                .contains("tool_results must follow an ASSISTANT"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn test_resolve_tool_results_allows_consecutive_tool_messages() {
        // ASSISTANT(ToolCalls{c1, c2}) → TOOL(c1) → TOOL(c2) is valid:
        // siblings carrying parallel results may be split across TOOL
        // messages, and the scan transparently skips over them.
        let messages = [
            assistant_with_tool_calls(vec![("c1", "tool_a"), ("c2", "tool_b")]),
            tool_msg_with_results(vec![("c1", "tool_a", "first", false)]),
            tool_msg_with_results(vec![("c2", "tool_b", "second", false)]),
        ];
        let results = extract_tool_results(&messages[2]);
        let resolved = ToolConverter::resolve_tool_results(&messages[..2], &results).unwrap();
        assert_eq!(resolved[0].call_id, "c2");
        assert_eq!(resolved[0].fn_name, "tool_b");
    }

    #[test]
    fn test_resolve_tool_results_preserves_order_and_is_error() {
        let messages = [
            assistant_with_tool_calls(vec![("c1", "tool_a"), ("c2", "tool_b")]),
            tool_msg_with_results(vec![
                ("c1", "tool_a", "first", false),
                ("c2", "tool_b", "second-failed", true),
            ]),
        ];
        let results = extract_tool_results(&messages[1]);
        let resolved = ToolConverter::resolve_tool_results(&messages[..1], &results).unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].call_id, "c1");
        assert!(!resolved[0].is_error);
        assert_eq!(resolved[1].call_id, "c2");
        assert!(resolved[1].is_error);
        assert_eq!(resolved[1].content, "second-failed");
    }

    #[test]
    fn test_validate_all_tool_results_passes_on_valid_input() {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
            ChatMessage as ProtoChatMessage, MessageContent,
            message_content::Content as ProtoContent,
        };
        let messages = vec![
            ProtoChatMessage {
                role: ChatRole::User.into(),
                content: Some(MessageContent {
                    content: Some(ProtoContent::Text("hello".to_string())),
                }),
            },
            assistant_with_tool_calls(vec![("c1", "tool_a")]),
            tool_msg_with_results(vec![("c1", "tool_a", "ok", false)]),
        ];
        let args = LlmChatArgs {
            messages,
            ..Default::default()
        };
        ToolConverter::validate_all_tool_results(&args).unwrap();
    }

    #[test]
    fn test_validate_all_tool_results_bails_on_invalid_input() {
        // Same setup as above but with a bad call_id, to confirm the entry
        // point propagates resolve_tool_results errors.
        let messages = vec![
            assistant_with_tool_calls(vec![("c1", "tool_a")]),
            tool_msg_with_results(vec![("c-unknown", "tool_a", "ok", false)]),
        ];
        let args = LlmChatArgs {
            messages,
            ..Default::default()
        };
        let err = ToolConverter::validate_all_tool_results(&args).unwrap_err();
        assert!(err.to_string().contains("call_id not found"));
    }

    #[test]
    fn test_resolved_tool_result_display_content() {
        let ok = ResolvedToolResult {
            call_id: "c1".to_string(),
            fn_name: "tool_a".to_string(),
            content: "fine".to_string(),
            is_error: false,
        };
        assert_eq!(ok.display_content(), "fine");

        let err = ResolvedToolResult {
            call_id: "c2".to_string(),
            fn_name: "tool_b".to_string(),
            content: "boom".to_string(),
            is_error: true,
        };
        assert_eq!(err.display_content(), "[ERROR] boom");
    }

    #[test]
    fn test_validate_all_tool_results_bails_on_non_tool_role() {
        // A ToolResults payload riding on a USER (or any non-TOOL) role
        // would otherwise be silently expanded by the genai/ollama
        // adapters under that wrong role, producing an invalid provider
        // message. Validation must reject it here.
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{
            ChatMessage as ProtoChatMessage, MessageContent,
            message_content::{Content as ProtoContent, ToolResult, ToolResults},
        };

        let bad = ProtoChatMessage {
            role: ChatRole::User.into(),
            content: Some(MessageContent {
                content: Some(ProtoContent::ToolResults(ToolResults {
                    results: vec![ToolResult {
                        call_id: "c1".to_string(),
                        fn_name: "tool_a".to_string(),
                        content: "ok".to_string(),
                        is_error: false,
                    }],
                })),
            }),
        };
        let args = LlmChatArgs {
            messages: vec![assistant_with_tool_calls(vec![("c1", "tool_a")]), bad],
            ..Default::default()
        };
        let err = ToolConverter::validate_all_tool_results(&args).unwrap_err();
        assert!(
            err.to_string().contains("must ride on a TOOL-role message"),
            "unexpected error: {}",
            err
        );
    }

    // ====== parse_client_tools_json / parse_tool_choice ======

    #[test]
    fn parse_client_tools_json_single_tool() {
        let json = r#"[
            {"type":"function","function":{
                "name":"lookback_recall",
                "description":"Search past conversations",
                "parameters":{"type":"object","properties":{"query":{"type":"string"}}}
            }}
        ]"#;
        let tools = ToolConverter::parse_client_tools_json(json).unwrap();
        assert_eq!(tools.len(), 1);
        // genai 0.6 stores `name` as ToolName; comparing via debug projection
        // would couple to upstream layout. Round-tripping through the public
        // API surface is enough — name is what we assert on through tool_choice.
        let json_value = serde_json::to_value(&tools[0]).unwrap();
        assert_eq!(json_value["name"], "lookback_recall");
        assert_eq!(json_value["description"], "Search past conversations");
        // Parameters must be forwarded byte-equivalent (FR-EXT-3 / AC-EXT-4).
        assert_eq!(
            json_value["schema"]["properties"]["query"]["type"],
            "string"
        );
    }

    #[test]
    fn parse_client_tools_json_multiple_tools_preserve_order() {
        let json = r#"[
            {"type":"function","function":{"name":"a","parameters":{"type":"object"}}},
            {"type":"function","function":{"name":"b","parameters":{"type":"object"}}}
        ]"#;
        let tools = ToolConverter::parse_client_tools_json(json).unwrap();
        assert_eq!(tools.len(), 2);
        let a = serde_json::to_value(&tools[0]).unwrap();
        let b = serde_json::to_value(&tools[1]).unwrap();
        assert_eq!(a["name"], "a");
        assert_eq!(b["name"], "b");
    }

    #[test]
    fn parse_client_tools_json_invalid_json_bails() {
        let err = ToolConverter::parse_client_tools_json("not json").unwrap_err();
        assert!(err.to_string().contains("invalid client_tools_json"));
    }

    #[test]
    fn parse_client_tools_json_top_level_not_array_bails() {
        let err = ToolConverter::parse_client_tools_json("{}").unwrap_err();
        assert!(err.to_string().contains("top-level value must be an array"));
    }

    #[test]
    fn parse_client_tools_json_non_function_type_bails() {
        let json = r#"[{"type":"web_search","function":{"name":"x","parameters":{}}}]"#;
        let err = ToolConverter::parse_client_tools_json(json).unwrap_err();
        assert!(err.to_string().contains("type=\"function\""));
    }

    #[test]
    fn parse_client_tools_json_missing_name_bails() {
        let json = r#"[{"type":"function","function":{"parameters":{}}}]"#;
        let err = ToolConverter::parse_client_tools_json(json).unwrap_err();
        assert!(err.to_string().contains("function.name is required"));
    }

    #[test]
    fn parse_client_tools_json_missing_parameters_bails() {
        let json = r#"[{"type":"function","function":{"name":"x"}}]"#;
        let err = ToolConverter::parse_client_tools_json(json).unwrap_err();
        assert!(err.to_string().contains("function.parameters is required"));
    }

    #[test]
    fn parse_client_tools_json_non_object_parameters_bails() {
        let json = r#"[{"type":"function","function":{"name":"x","parameters":42}}]"#;
        let err = ToolConverter::parse_client_tools_json(json).unwrap_err();
        assert!(err.to_string().contains("must be an object"));
    }

    #[test]
    fn parse_tool_choice_keywords() {
        assert!(matches!(
            ToolConverter::parse_tool_choice("auto"),
            Some(genai::chat::ToolChoice::Auto)
        ));
        assert!(matches!(
            ToolConverter::parse_tool_choice("none"),
            Some(genai::chat::ToolChoice::None)
        ));
        assert!(matches!(
            ToolConverter::parse_tool_choice("required"),
            Some(genai::chat::ToolChoice::Required)
        ));
    }

    #[test]
    fn parse_tool_choice_object_form() {
        let json = r#"{"type":"function","function":{"name":"lookback_recall"}}"#;
        match ToolConverter::parse_tool_choice(json) {
            Some(genai::chat::ToolChoice::Tool { name }) => assert_eq!(name, "lookback_recall"),
            other => panic!("expected Tool variant, got {other:?}"),
        }
    }

    #[test]
    fn parse_tool_choice_unknown_keyword_returns_none() {
        assert!(ToolConverter::parse_tool_choice("never_heard_of_it").is_none());
    }

    #[test]
    fn parse_tool_choice_invalid_json_returns_none() {
        assert!(ToolConverter::parse_tool_choice("{not json").is_none());
    }

    #[test]
    fn parse_tool_choice_object_without_function_name_returns_none() {
        assert!(ToolConverter::parse_tool_choice(r#"{"type":"function"}"#).is_none());
    }
}
