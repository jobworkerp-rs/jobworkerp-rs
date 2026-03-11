use crate::llm::chat::conversion::ToolConverter;
use anyhow::{Result, anyhow};
use app::app::function::function_set::FunctionSetApp;
use app::module::AppModule;
use futures::stream::BoxStream;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::function_set_selector::{
    FunctionSetSelectorArgs, FunctionSetSelectorResult, FunctionSetSelectorSettings,
};
use jobworkerp_runner::runner::RunnerSpec;
use jobworkerp_runner::runner::RunnerTrait;
use jobworkerp_runner::runner::cancellation_helper::{
    CancelMonitoringHelper, UseCancelMonitoringHelper,
};
use jobworkerp_runner::runner::function_set_selector::FunctionSetSelectorSpecImpl;
use proto::jobworkerp::data::{ResultOutputItem, result_output_item};
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tonic::async_trait;

const DEFAULT_ENTRY_TEMPLATE: &str =
    "## {{ name }}\n{{ description }}\n\nAvailable tools ({{ tool_count }}):\n{{ tools }}";
const DEFAULT_RESULT_TEMPLATE: &str = "The following FunctionSets are available. Select the most appropriate one for the user's task by name.\n\n{{ entries }}";
const DEFAULT_TOOL_ENTRY_TEMPLATE: &str = "- {{ tool_name }}: {{ tool_description }}";

#[derive(Clone)]
pub struct FunctionSetSelectorImpl {
    app: Arc<AppModule>,
    entry_template: Option<Arc<liquid::Template>>,
    result_template: Option<Arc<liquid::Template>>,
    tool_entry_template: Option<Arc<liquid::Template>>,
    spec: FunctionSetSelectorSpecImpl,
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl std::fmt::Debug for FunctionSetSelectorImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FunctionSetSelectorImpl")
            .field("spec", &self.spec)
            .finish()
    }
}

impl FunctionSetSelectorImpl {
    pub fn new_with_cancel_monitoring(
        app_module: Arc<AppModule>,
        cancel_helper: CancelMonitoringHelper,
    ) -> Self {
        Self {
            app: app_module,
            entry_template: None,
            result_template: None,
            tool_entry_template: None,
            spec: FunctionSetSelectorSpecImpl::new(),
            cancel_helper: Some(cancel_helper),
        }
    }

    async fn get_cancellation_token(&self) -> CancellationToken {
        if let Some(helper) = &self.cancel_helper {
            helper.get_cancellation_token().await
        } else {
            CancellationToken::new()
        }
    }

    fn parse_template(template_str: &str) -> Result<liquid::Template> {
        let parser = liquid::ParserBuilder::with_stdlib()
            .build()
            .map_err(|e| anyhow!("Failed to create liquid parser: {e}"))?;
        parser
            .parse(template_str)
            .map_err(|e| anyhow!("Failed to parse liquid template: {e}"))
    }

    fn render_tool_entry(&self, tool_name: &str, tool_description: &str) -> String {
        let template = self.tool_entry_template.as_ref();
        if let Some(tmpl) = template {
            let globals = liquid::object!({
                "tool_name": tool_name,
                "tool_description": tool_description,
            });
            match tmpl.render(&globals) {
                Ok(rendered) => rendered,
                Err(e) => {
                    tracing::warn!("Tool entry template render failed: {e}");
                    format!("- {tool_name}: {tool_description}")
                }
            }
        } else {
            format!("- {tool_name}: {tool_description}")
        }
    }

    fn render_entry(
        &self,
        name: &str,
        description: &str,
        category: i32,
        tools: &str,
        tool_count: usize,
    ) -> String {
        let template = self.entry_template.as_ref();
        if let Some(tmpl) = template {
            let globals = liquid::object!({
                "name": name,
                "description": description,
                "category": category,
                "tools": tools,
                "tool_count": tool_count as i64,
            });
            match tmpl.render(&globals) {
                Ok(rendered) => rendered,
                Err(e) => {
                    tracing::warn!("Entry template render failed for {name}: {e}");
                    format!("## {name}\n{description}\n\nAvailable tools ({tool_count}):\n{tools}")
                }
            }
        } else {
            format!("## {name}\n{description}\n\nAvailable tools ({tool_count}):\n{tools}")
        }
    }

    fn render_result(&self, entries: &str, total_count: i64) -> String {
        let template = self.result_template.as_ref();
        if let Some(tmpl) = template {
            let globals = liquid::object!({
                "entries": entries,
                "total_count": total_count,
            });
            match tmpl.render(&globals) {
                Ok(rendered) => rendered,
                Err(e) => {
                    tracing::warn!("Result template render failed: {e}");
                    format!(
                        "The following FunctionSets are available. Select the most appropriate one for the user's task by name.\n\n{entries}"
                    )
                }
            }
        } else {
            format!(
                "The following FunctionSets are available. Select the most appropriate one for the user's task by name.\n\n{entries}"
            )
        }
    }
}

impl RunnerSpec for FunctionSetSelectorImpl {
    fn name(&self) -> String {
        self.spec.name()
    }

    fn runner_settings_proto(&self) -> String {
        self.spec.runner_settings_proto()
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        self.spec.method_proto_map()
    }

    fn settings_schema(&self) -> String {
        self.spec.settings_schema()
    }
}

#[async_trait]
impl RunnerTrait for FunctionSetSelectorImpl {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let parsed =
            ProstMessageCodec::deserialize_message::<FunctionSetSelectorSettings>(&settings)?;

        let entry_str = parsed
            .entry_template
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_ENTRY_TEMPLATE.to_string());
        self.entry_template = Some(Arc::new(Self::parse_template(&entry_str)?));

        let result_str = parsed
            .result_template
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_RESULT_TEMPLATE.to_string());
        self.result_template = Some(Arc::new(Self::parse_template(&result_str)?));

        let tool_entry_str = parsed
            .tool_entry_template
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_TOOL_ENTRY_TEMPLATE.to_string());
        self.tool_entry_template = Some(Arc::new(Self::parse_template(&tool_entry_str)?));

        Ok(())
    }

    async fn run(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let cancellation_token = self.get_cancellation_token().await;

        let result = async {
            let args = ProstMessageCodec::deserialize_message::<FunctionSetSelectorArgs>(arg)
                .map_err(|e| {
                    JobWorkerError::InvalidParameter(format!(
                        "cannot deserialize FunctionSetSelectorArgs: {e:?}"
                    ))
                })?;

            // Check cancellation
            if cancellation_token.is_cancelled() {
                return Err(JobWorkerError::CancelledError(
                    "FunctionSet selector was cancelled".to_string(),
                )
                .into());
            }

            // Always fetch all and apply filters/paging in-memory for consistent total_count
            let function_sets = self
                .app
                .function_set_app
                .find_function_set_all_list(None)
                .await?;

            // Apply in-memory filters
            let mut filtered: Vec<_> = function_sets
                .into_iter()
                .filter(|fs| {
                    if let Some(data) = &fs.data {
                        let cat_ok = args.category.is_none_or(|c| data.category == c);
                        let name_ok = args
                            .name_filter
                            .as_ref()
                            .is_none_or(|f| data.name.to_lowercase().contains(&f.to_lowercase()));
                        cat_ok && name_ok
                    } else {
                        false
                    }
                })
                .collect();

            let total_count = filtered.len() as i64;

            // Apply limit/offset in-memory
            let offset = args.offset.unwrap_or(0).max(0) as usize;
            if offset > 0 {
                if offset < filtered.len() {
                    filtered.drain(..offset);
                } else {
                    filtered.clear();
                }
            }
            if let Some(limit) = args.limit {
                let limit = limit.max(0) as usize;
                if limit > 0 && limit < filtered.len() {
                    filtered.truncate(limit);
                }
            }

            // Build formatted entries
            let mut entries = Vec::new();
            for fs in &filtered {
                if cancellation_token.is_cancelled() {
                    return Err(JobWorkerError::CancelledError(
                        "FunctionSet selector was cancelled during entry building".to_string(),
                    )
                    .into());
                }

                let data = fs.data.as_ref().expect("data was verified Some by filter");

                // Fetch tool summaries
                let tools_result = self
                    .app
                    .function_set_app
                    .find_functions_by_set(&data.name)
                    .await;

                let tool_entries: Vec<String> = match tools_result {
                    Ok(specs) => ToolConverter::get_tool_summaries(&specs)
                        .iter()
                        .map(|(name, desc)| self.render_tool_entry(name, desc))
                        .collect(),
                    Err(e) => {
                        tracing::warn!(
                            "Failed to fetch tools for FunctionSet '{}': {e}",
                            data.name
                        );
                        vec![]
                    }
                };

                let tools_str = tool_entries.join("\n");
                let entry = self.render_entry(
                    &data.name,
                    &data.description,
                    data.category,
                    &tools_str,
                    tool_entries.len(),
                );
                entries.push(entry);
            }

            let entries_str = entries.join("\n\n");
            let formatted_output = self.render_result(&entries_str, total_count);

            let result = FunctionSetSelectorResult {
                formatted_output,
                total_count,
            };
            ProstMessageCodec::serialize_message(&result).map_err(|e| anyhow!("{e}"))
        }
        .await;

        (result, metadata)
    }

    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let (result, metadata) = self.run(arg, metadata.clone(), using).await;
        let data = result?;
        let trailer = proto::jobworkerp::data::Trailer { metadata };

        use async_stream::stream;
        let stream = stream! {
            yield ResultOutputItem {
                item: Some(result_output_item::Item::Data(data)),
            };
            yield ResultOutputItem {
                item: Some(result_output_item::Item::End(trailer)),
            };
        };
        Ok(Box::pin(stream))
    }
}

#[async_trait]
impl jobworkerp_runner::runner::cancellation::CancelMonitoring for FunctionSetSelectorImpl {
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: proto::jobworkerp::data::JobId,
        job_data: &proto::jobworkerp::data::JobData,
    ) -> Result<Option<proto::jobworkerp::data::JobResult>> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, job_data).await
        } else {
            Ok(None)
        }
    }

    async fn cleanup_cancellation_monitoring(&mut self) -> Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.cleanup_monitoring_impl().await
        } else {
            Ok(())
        }
    }

    async fn request_cancellation(&mut self) -> Result<()> {
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
            }
        }
        Ok(())
    }

    async fn reset_for_pooling(&mut self) -> Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.reset_for_pooling_impl().await?;
        }
        Ok(())
    }
}

impl UseCancelMonitoringHelper for FunctionSetSelectorImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_templates_parse() {
        FunctionSetSelectorImpl::parse_template(DEFAULT_ENTRY_TEMPLATE).unwrap();
        FunctionSetSelectorImpl::parse_template(DEFAULT_RESULT_TEMPLATE).unwrap();
        FunctionSetSelectorImpl::parse_template(DEFAULT_TOOL_ENTRY_TEMPLATE).unwrap();
    }

    #[test]
    fn test_template_rendering() {
        let entry_tmpl = FunctionSetSelectorImpl::parse_template(DEFAULT_ENTRY_TEMPLATE).unwrap();
        let result_tmpl = FunctionSetSelectorImpl::parse_template(DEFAULT_RESULT_TEMPLATE).unwrap();
        let tool_tmpl =
            FunctionSetSelectorImpl::parse_template(DEFAULT_TOOL_ENTRY_TEMPLATE).unwrap();

        // Test tool entry rendering
        let globals = liquid::object!({
            "tool_name": "test_tool",
            "tool_description": "A test tool",
        });
        let rendered = tool_tmpl.render(&globals).unwrap();
        assert_eq!(rendered, "- test_tool: A test tool");

        // Test entry rendering
        let globals = liquid::object!({
            "name": "my-set",
            "description": "A test set",
            "category": 1,
            "tools": "- test_tool: A test tool",
            "tool_count": 1_i64,
        });
        let entry = entry_tmpl.render(&globals).unwrap();
        assert!(entry.contains("## my-set"));
        assert!(entry.contains("A test set"));
        assert!(entry.contains("Available tools (1)"));

        // Test result rendering
        let globals = liquid::object!({
            "entries": entry,
            "total_count": 1_i64,
        });
        let result = result_tmpl.render(&globals).unwrap();
        assert!(result.contains("The following FunctionSets are available"));
        assert!(result.contains("## my-set"));
    }

    #[test]
    fn test_custom_template_parse() {
        let custom = "**{{ name }}** ({{ tool_count }} tools): {{ description }}";
        FunctionSetSelectorImpl::parse_template(custom).unwrap();
    }

    #[test]
    fn test_invalid_template_fails() {
        let invalid = "{{ unclosed";
        assert!(FunctionSetSelectorImpl::parse_template(invalid).is_err());
    }
}
