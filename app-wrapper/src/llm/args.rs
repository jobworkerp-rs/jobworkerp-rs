use jobworkerp_runner::jobworkerp::runner::{llm_args, LlmArgs};
use anyhow::Result;
use jobworkerp_base::error::JobWorkerError;
use mistralrs::{
    Constraint, DrySamplingParams, Function, RequestBuilder, StopTokens, TextMessageRole, Tool,
    ToolChoice,
};
use serde_json;

/// Trait for converting protocol buffer messages to LLM request objects
pub trait LLMRequestConverter {
    fn build_request(args: &LlmArgs, need_stream: bool) -> Result<RequestBuilder> {
        let mut builder = RequestBuilder::new();

        match &args.request {
            // Handle text completion request
            Some(llm_args::Request::Completion(req)) => {
                if need_stream && !req.stream || !need_stream && req.stream {
                    return Err(JobWorkerError::InvalidParameter(
                        "illegal Stream mode for chat completion".to_string(),
                    )
                    .into());
                }
                // Add prompt as a user message
                builder = builder.add_message(TextMessageRole::User, &req.prompt);

                // Apply request options if provided
                if let Some(opts) = &req.options {
                    builder = Self::apply_options(builder, opts)?;
                }
            }

            // Handle chat completion request
            Some(llm_args::Request::ChatCompletion(req)) => {
                if need_stream && !req.stream || !need_stream && req.stream {
                    return Err(JobWorkerError::InvalidParameter(
                        "illegal Stream mode for chat completion".to_string(),
                    )
                    .into());
                }
                // Process based on message format
                match &req.messages_format {
                    // Handle string prompt as a user message
                    Some(llm_args::chat_completion_request::MessagesFormat::PromptString(
                        prompt,
                    )) => {
                        builder = builder.add_message(TextMessageRole::User, prompt);
                    }

                    // Handle chat messages array
                    Some(llm_args::chat_completion_request::MessagesFormat::ChatMessages(
                        chat_msgs,
                    )) => {
                        for msg in &chat_msgs.messages {
                            let role: Result<TextMessageRole> =
                                match llm_args::Role::try_from(msg.role)? {
                                    llm_args::Role::User => Ok(TextMessageRole::User),
                                    llm_args::Role::Assistant => Ok(TextMessageRole::Assistant),
                                    llm_args::Role::System => Ok(TextMessageRole::System),
                                    llm_args::Role::Tool => Ok(TextMessageRole::Tool),
                                    // Ok(llm_args::Role::Custom) => TextMessageRole::Custom(),
                                    llm_args::Role::Custom => {
                                        Err(JobWorkerError::InvalidParameter(
                                            "Custom role not supported".to_string(),
                                        )
                                        .into())
                                    }
                                };
                            let role = role?;

                            match &msg.content {
                                // Handle text content
                                Some(llm_args::chat_message::Content::Text(text)) => {
                                    builder = builder.add_message(role, text);
                                }

                                // Handle multiple content parts (text + tool calls)
                                Some(llm_args::chat_message::Content::ContentParts(parts)) => {
                                    // Current implementation only supports text input, not multimodal input (e.g., images)
                                    // Combine all text parts
                                    let mut combined_text = String::new();

                                    for part in &parts.items {
                                        if let Some(llm_args::message_content::Content::Text(
                                            text,
                                        )) = &part.content
                                        {
                                            if !combined_text.is_empty() {
                                                combined_text.push_str("\n");
                                            }
                                            combined_text.push_str(text);
                                        }
                                        // Tool call parts are not supported yet
                                    }

                                    if !combined_text.is_empty() {
                                        builder = builder.add_message(role, combined_text);
                                    }
                                }
                                None => {
                                    // Handle empty content as empty string
                                    builder = builder.add_message(role, "");
                                }
                            }
                        }
                    }
                    None => {
                        return Err(JobWorkerError::InvalidParameter(
                            "Chat messages format not specified".to_string(),
                        )
                        .into());
                    }
                }

                // Handle logprobs settings
                if req.logprobs {
                    builder = builder.return_logprobs(true);
                    if let Some(top_logprobs) = req.top_logprobs {
                        builder = builder.set_sampler_topn_logprobs(top_logprobs as usize);
                    }
                }

                // Apply request options
                if let Some(opts) = &req.options {
                    builder = Self::apply_options(builder, opts)?;
                }
            }
            None => {
                return Err(
                    JobWorkerError::InvalidParameter("Request not specified".to_string()).into(),
                );
            }
        }

        Ok(builder)
    }

    fn apply_options(
        mut builder: RequestBuilder,
        opts: &llm_args::LlmRequestOptions,
    ) -> Result<RequestBuilder> {
        // Set maximum token count
        if let Some(max_tokens) = opts.max_tokens {
            builder = builder.set_sampler_max_len(max_tokens as usize);
        }

        // Set temperature
        if let Some(temperature) = opts.temperature {
            builder = builder.set_sampler_temperature(temperature);
        }

        // Set top-p (nucleus sampling)
        if let Some(top_p) = opts.top_p {
            builder = builder.set_sampler_topp(top_p);
        }

        // Set top-k sampling
        if let Some(top_k) = opts.top_k {
            builder = builder.set_sampler_topk(top_k as usize);
        }

        // Set min-p sampling
        if let Some(min_p) = opts.min_p {
            builder = builder.set_sampler_minp(min_p);
        }

        // Set presence penalty
        if let Some(presence_penalty) = opts.presence_penalty {
            builder = builder.set_sampler_presence_penalty(presence_penalty);
        }

        // Set frequency penalty
        if let Some(frequency_penalty) = opts.frequency_penalty {
            builder = builder.set_sampler_frequency_penalty(frequency_penalty);
        }

        // Set stop sequences
        if !opts.stop_seqs.is_empty() {
            let stop_toks = StopTokens::Seqs(opts.stop_seqs.clone());
            builder = builder.set_sampler_stop_toks(stop_toks);
        }

        // Set logit bias
        if !opts.logit_bias.is_empty() {
            let bias_map = opts
                .logit_bias
                .iter()
                .map(|(k, v)| (*k as u32, *v))
                .collect();
            builder = builder.set_sampler_logits_bias(bias_map);
        }

        // Set number of generations
        if opts.n_choices > 1 {
            builder = builder.set_sampler_n_choices(opts.n_choices as usize);
        }

        // Set adapters if specified
        if !opts.adapters.is_empty() {
            builder = builder.set_adapters(opts.adapters.clone());
        }

        // Handle grammar configuration (using oneof)
        match &opts.grammar_config {
            Some(llm_args::llm_request_options::GrammarConfig::Json(grammar)) => {
                // Parse as a JSON schema
                match serde_json::from_str::<serde_json::Value>(grammar) {
                    Ok(json_schema) => {
                        builder = builder.set_constraint(Constraint::JsonSchema(json_schema));
                        tracing::info!("Applied JSON schema constraint");
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse JSON schema: {}", e);
                    }
                }
            }
            Some(llm_args::llm_request_options::GrammarConfig::Regex(grammar)) => {
                // Use simple regex constraint
                builder = builder.set_constraint(Constraint::Regex(grammar.clone()));
                tracing::info!("Applied regex constraint");
            }
            Some(llm_args::llm_request_options::GrammarConfig::Lark(grammar)) => {
                // Use Lark grammar directly
                builder = builder.set_constraint(Constraint::Lark(grammar.clone()));
                tracing::info!("Applied Lark grammar constraint");
            }
            None => {
                // No grammar specified
            }
        }

        // Handle DRY (Don't Repeat Yourself) sampling parameters if specified
        if let (Some(dry_multiplier), Some(dry_base)) = (opts.dry_multiplier, opts.dry_base) {
            let mut dry_params =
                DrySamplingParams::new_with_defaults(dry_multiplier, None, Some(dry_base), None)?;

            // Set allowed length if specified
            if let Some(dry_allowed_length) = opts.dry_allowed_length {
                dry_params.allowed_length = dry_allowed_length as usize;
            }

            // Set sequence breakers if specified
            if !opts.dry_sequence_breakers.is_empty() {
                dry_params.sequence_breakers = opts.dry_sequence_breakers.clone();
            }
            builder = builder.set_sampler_dry_params(dry_params);
        }

        // Handle tool schemas and tool choice
        if !opts.tool_schemas.is_empty() {
            // Parse tool schemas to create Tool objects
            let tools = opts
                .tool_schemas
                .iter()
                .filter_map(|schema| {
                    // Parse JSON schema to create a Tool
                    match serde_json::from_str::<Function>(schema) {
                        Ok(function) => Some(Tool {
                            tp: mistralrs::ToolType::Function,
                            function,
                        }),
                        Err(e) => {
                            tracing::warn!("Failed to parse tool schema: {}", e);
                            None
                        }
                    }
                })
                .collect::<Vec<_>>();

            if !tools.is_empty() {
                builder = builder.set_tools(tools);

                // Apply tool choice if specified
                if let Some(tool_choice) = &opts.tool_choice {
                    let choice = match llm_args::ToolChoiceOption::try_from(*tool_choice) {
                        Ok(llm_args::ToolChoiceOption::None) => ToolChoice::None,
                        Ok(llm_args::ToolChoiceOption::Auto) => ToolChoice::Auto,
                        Ok(llm_args::ToolChoiceOption::ForcedTool) => {
                            // Create a forced tool choice with the specified tool
                            // Convert HashMap<String, String> to HashMap<String, serde_json::Value>
                            if let Some(forced_tool) = &opts.forced_tool {
                                let parameters = if forced_tool.arguments.is_empty() {
                                    None
                                } else {
                                    Some(
                                        forced_tool
                                            .arguments
                                            .iter()
                                            .map(|(k, v)| {
                                                (k.clone(), serde_json::Value::String(v.clone()))
                                            })
                                            .collect(),
                                    )
                                };
                                ToolChoice::Tool(Tool {
                                    tp: mistralrs::ToolType::Function,
                                    function: mistralrs::Function {
                                        description: forced_tool.description.clone(),
                                        name: forced_tool.name.clone(),
                                        parameters,
                                    },
                                })
                            } else {
                                ToolChoice::Auto
                            }
                        }
                        _ => {
                            tracing::warn!("Invalid tool choice option specified: {}", tool_choice);
                            ToolChoice::Auto
                        }
                    };

                    builder = builder.set_tool_choice(choice);
                }
            }
        }

        Ok(builder)
    }
}

pub struct DefaultLLMRequestConverter;
impl LLMRequestConverter for DefaultLLMRequestConverter {}
