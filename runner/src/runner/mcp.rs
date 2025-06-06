use crate::jobworkerp::runner::mcp_server_result;
use crate::jobworkerp::runner::mcp_server_result::BlobResourceContents;
use crate::jobworkerp::runner::mcp_server_result::TextResourceContents;
use crate::jobworkerp::runner::McpServerArgs;
use crate::jobworkerp::runner::McpServerResult;
use crate::runner::RunnerSpec;
use crate::runner::RunnerTrait;
use crate::{schema_to_json_string, schema_to_json_string_option};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::BoxStream;
use infra_utils::infra::trace::Tracing;
use jobworkerp_base::codec::ProstMessageCodec;
use jobworkerp_base::codec::UseProstCodec;
use jobworkerp_base::APP_WORKER_NAME;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::Context;
use proto::jobworkerp::data::ResultOutputItem;
use proto::jobworkerp::data::StreamingOutputType;
use proto::jobworkerp::function::data::McpTool;
use proto::jobworkerp::function::data::ToolAnnotations;
use proxy::McpServerProxy;
use rmcp::model::CallToolRequestParam;
use std::collections::HashMap;

pub mod config;
#[cfg(any(test, feature = "test-utils"))]
pub mod integration_tests;
pub mod proxy;

/**
 * PluginRunner wrapper
 * (for self mutability (run(), cancel()))
 */
#[derive(Debug)]
pub struct McpServerRunnerImpl {
    mcp_server: McpServerProxy,
}

impl McpServerRunnerImpl {
    pub fn new(server: McpServerProxy) -> Self {
        Self { mcp_server: server }
    }
    pub async fn tools(&self) -> Result<Vec<McpTool>> {
        self.mcp_server.load_tools().await.map(|list| {
            list.into_iter()
                .map(|tool| McpTool {
                    name: tool.name.into_owned(),
                    description: tool.description.map(|r| r.into_owned()),
                    input_schema: serde_json::to_string(&tool.input_schema)
                        .inspect_err(|e| {
                            tracing::error!("Failed to serialize tool input schema: {}", e)
                        })
                        .unwrap_or_default(),
                    annotations: tool.annotations.map(|an| ToolAnnotations {
                        title: an.title,
                        read_only_hint: an.read_only_hint,
                        destructive_hint: an.destructive_hint,
                        idempotent_hint: an.idempotent_hint,
                        open_world_hint: an.open_world_hint,
                    }),
                })
                .collect()
        })
    }
}

impl RunnerSpec for McpServerRunnerImpl {
    fn name(&self) -> String {
        self.mcp_server.name.clone()
    }
    fn runner_settings_proto(&self) -> String {
        "".to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/mcp_server_args.proto").to_string()
    }
    fn result_output_proto(&self) -> Option<String> {
        Some(include_str!("../../protobuf/jobworkerp/runner/mcp_server_result.proto").to_string())
    }
    fn output_type(&self) -> StreamingOutputType {
        StreamingOutputType::NonStreaming
    }
    fn settings_schema(&self) -> String {
        schema_to_json_string!(crate::jobworkerp::runner::Empty, "settings_schema")
    }
    fn arguments_schema(&self) -> String {
        schema_to_json_string!(McpServerArgs, "arguments_schema")
    }
    fn output_schema(&self) -> Option<String> {
        schema_to_json_string_option!(McpServerResult, "output_schema")
    }
}

impl Tracing for McpServerRunnerImpl {}

#[async_trait]
impl RunnerTrait for McpServerRunnerImpl {
    async fn load(&mut self, _settings: Vec<u8>) -> Result<()> {
        Ok(())
    }
    async fn run(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let result = async {
            let span = Self::otel_span_from_metadata(
                &metadata,
                APP_WORKER_NAME,
                "McpServerRunnerImpl::run",
            );
            let cx = Context::current_with_span(span);
            let mut metadata = metadata.clone();
            Self::inject_metadata_from_context(&mut metadata, &cx);
            // ref
            let span = cx.span();

            let arg = ProstMessageCodec::deserialize_message::<McpServerArgs>(args)?;
            span.set_attribute(opentelemetry::KeyValue::new(
                "input.tool_name",
                arg.tool_name.clone(),
            ));
            span.set_attribute(opentelemetry::KeyValue::new(
                "input.arg_json",
                arg.arg_json.clone(), // XXX clone
            ));
            let res = self
                .mcp_server
                .transport
                .call_tool(CallToolRequestParam {
                    name: std::borrow::Cow::Owned(arg.tool_name),
                    arguments: serde_json::from_str(arg.arg_json.as_str())
                        .inspect_err(|e| {
                            tracing::error!("Failed to parse arguments: {}", e);
                        })
                        .ok(),
                })
                .await?;

            if res.is_error.unwrap_or_default() {
                let error = anyhow!(
                    "Tool call failed: {}",
                    serde_json::json!(res.content).to_string()
                );
                span.record_error(error.as_ref());
            } else {
                span.set_attribute(opentelemetry::KeyValue::new(
                    "output",
                    serde_json::json!(res.content).to_string(),
                ));
            }
            // map res to McpServerResult and encode to Vec<u8>
            let mut mcp_contents = Vec::new();
            for content in res.content {
                match content.raw {
                    rmcp::model::RawContent::Text(rmcp::model::RawTextContent { text }) => {
                        mcp_contents.push(mcp_server_result::Content {
                            raw_content: Some(mcp_server_result::content::RawContent::Text(
                                mcp_server_result::TextContent { text },
                            )),
                        });
                    }
                    rmcp::model::RawContent::Image(rmcp::model::RawImageContent {
                        data,
                        mime_type,
                    }) => {
                        mcp_contents.push(mcp_server_result::Content {
                            raw_content: Some(mcp_server_result::content::RawContent::Image(
                                mcp_server_result::ImageContent { data, mime_type },
                            )),
                        });
                    }
                    // wait for raw audio content of raw content
                    // https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/src/model/content.rs#L55
                    rmcp::model::RawContent::Audio(_audio) => {
                        // mcp_contents.push(mcp_server_result::Content {
                        //     raw_content: Some(mcp_server_result::content::RawContent::Audio(
                        //         mcp_server_result::AudioContent { data, mime_type },
                        //     )),
                        // });
                        tracing::error!("Audio content not supported yet");
                    }
                    rmcp::model::RawContent::Resource(rmcp::model::RawEmbeddedResource {
                        resource:
                            rmcp::model::ResourceContents::TextResourceContents {
                                uri,
                                mime_type,
                                text,
                            },
                    }) => {
                        mcp_contents.push(mcp_server_result::Content {
                            raw_content: Some(mcp_server_result::content::RawContent::Resource(
                                mcp_server_result::EmbeddedResource {
                                    resource: Some(
                                        mcp_server_result::embedded_resource::Resource::Text(
                                            TextResourceContents {
                                                uri,
                                                mime_type,
                                                text,
                                            },
                                        ),
                                    ),
                                },
                            )),
                        });
                    }
                    rmcp::model::RawContent::Resource(rmcp::model::RawEmbeddedResource {
                        resource:
                            rmcp::model::ResourceContents::BlobResourceContents {
                                uri,
                                mime_type,
                                blob,
                            },
                    }) => {
                        mcp_contents.push(mcp_server_result::Content {
                            raw_content: Some(mcp_server_result::content::RawContent::Resource(
                                mcp_server_result::EmbeddedResource {
                                    resource: Some(
                                        mcp_server_result::embedded_resource::Resource::Blob(
                                            BlobResourceContents {
                                                uri,
                                                mime_type,
                                                blob,
                                            },
                                        ),
                                    ),
                                },
                            )),
                        });
                    }
                }
            }

            let mcp_result = McpServerResult {
                content: mcp_contents,
                is_error: res.is_error.unwrap_or(false),
            };

            // Encode the result as protobuf
            let encoded = ProstMessageCodec::serialize_message(&mcp_result)?;
            Ok(encoded)
        }
        .await;
        (result, metadata)
    }

    async fn run_stream(
        &mut self,
        _arg: &[u8],
        _metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        tracing::error!("run_stream not implemented");
        Err(anyhow!("run_stream not implemented"))
    }

    async fn cancel(&mut self) {
        tracing::debug!("cancel mcp server");
        // self.mcp_server.cancel().await.unwrap_or_default();
    }
}
