use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use genai::GenaiCompletionService;
use infra_utils::infra::trace::Tracing;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::APP_WORKER_NAME;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmCompletionArgs, LlmRunnerSettings};
use jobworkerp_runner::runner::llm::LLMCompletionRunnerSpec;
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use ollama::OllamaService;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::Context;
use prost::Message;
use proto::jobworkerp::data::{result_output_item, ResultOutputItem, RunnerType};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;

pub mod genai;
pub mod ollama;

pub struct LLMCompletionRunnerImpl {
    pub ollama: Option<OllamaService>,
    pub genai: Option<GenaiCompletionService>,
}

impl LLMCompletionRunnerImpl {
    pub fn new() -> Self {
        Self {
            ollama: None,
            genai: None,
        }
    }
}

impl Tracing for LLMCompletionRunnerImpl {}

impl Default for LLMCompletionRunnerImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl LLMCompletionRunnerSpec for LLMCompletionRunnerImpl {}
impl RunnerSpec for LLMCompletionRunnerImpl {
    fn name(&self) -> String {
        LLMCompletionRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        LLMCompletionRunnerSpec::runner_settings_proto(self)
    }

    fn job_args_proto(&self) -> String {
        LLMCompletionRunnerSpec::job_args_proto(self)
    }

    fn result_output_proto(&self) -> Option<String> {
        LLMCompletionRunnerSpec::result_output_proto(self)
    }

    fn output_type(&self) -> proto::jobworkerp::data::StreamingOutputType {
        LLMCompletionRunnerSpec::output_type(self)
    }

    fn settings_schema(&self) -> String {
        LLMCompletionRunnerSpec::settings_schema(self)
    }

    fn arguments_schema(&self) -> String {
        LLMCompletionRunnerSpec::arguments_schema(self)
    }

    fn output_schema(&self) -> Option<String> {
        LLMCompletionRunnerSpec::output_schema(self)
    }
}

#[async_trait]
impl RunnerTrait for LLMCompletionRunnerImpl {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let settings = LlmRunnerSettings::decode(&mut Cursor::new(settings))
            .map_err(|e| anyhow!("decode error: {}", e))?;
        match settings.settings {
            Some(
                jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Ollama(
                    settings,
                ),
            ) => {
                let ollama = OllamaService::new(settings).await?;
                tracing::info!("{} loaded(ollama)", RunnerType::LlmCompletion.as_str_name());
                self.ollama = Some(ollama);
                Ok(())
            }
            Some(
                jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Genai(
                    settings,
                ),
            ) => {
                let genai = GenaiCompletionService::new(settings).await?;
                tracing::info!("{} loaded(genai)", RunnerType::LlmCompletion.as_str_name());
                self.genai = Some(genai);
                Ok(())
            }
            _ => Err(anyhow!("model_settings is not set")),
        }
    }

    async fn run(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let metadata_clone = metadata.clone();
        let result = async {
            let span =
                Self::otel_span_from_metadata(&metadata, APP_WORKER_NAME, "llm_completion_run");
            let cx = Context::current_with_span(span);

            let args = LlmCompletionArgs::decode(&mut Cursor::new(arg))
                .map_err(|e| anyhow!("decode error: {}", e))?;
            if let Some(ollama) = self.ollama.as_mut() {
                let res = ollama.request_generation(args, cx, metadata_clone).await?;
                let mut buf = Vec::with_capacity(res.encoded_len());
                res.encode(&mut buf)
                    .map_err(|e| anyhow!("encode error: {}", e))?;
                Ok(buf)
            } else if let Some(genai) = self.genai.as_mut() {
                //XXX chat only
                let res = genai.request_chat(args, cx, metadata_clone).await?;
                let mut buf = Vec::with_capacity(res.encoded_len());
                res.encode(&mut buf)
                    .map_err(|e| anyhow!("encode error: {}", e))?;
                Ok(buf)
            } else {
                Err(anyhow!("llm is not initialized"))
            }
        }
        .await;
        (result, metadata)
    }

    async fn run_stream(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let args = LlmCompletionArgs::decode(args).map_err(|e| anyhow!("decode error: {}", e))?;

        if let Some(ollama) = self.ollama.as_mut() {
            // Get streaming responses from ollama service
            let stream = ollama.request_stream_generation(args).await?;

            let req_meta = Arc::new(metadata.clone());
            // Transform each LlmCompletionResult into ResultOutputItem
            let output_stream = stream
                .flat_map(move |completion_result| {
                    let mut result_items = Vec::new();

                    // Encode the completion result to binary if there is content
                    if completion_result
                        .content
                        .as_ref()
                        .is_some_and(|c| c.content.is_some())
                    {
                        let buf = ProstMessageCodec::serialize_message(&completion_result);
                        // Add content data item
                        if let Ok(buf) = buf {
                            result_items.push(ResultOutputItem {
                                item: Some(result_output_item::Item::Data(buf)),
                            });
                        } else {
                            tracing::error!("Failed to serialize LLM completion result");
                        }
                    }

                    // If this is the final result (done=true), add an end marker
                    if completion_result.done {
                        result_items.push(ResultOutputItem {
                            item: Some(proto::jobworkerp::data::result_output_item::Item::End(
                                proto::jobworkerp::data::Trailer {
                                    metadata: (*req_meta).clone(),
                                },
                            )),
                        });
                    }

                    futures::stream::iter(result_items)
                })
                .boxed();

            Ok(output_stream)
        } else if let Some(genai) = self.genai.as_mut() {
            // Get streaming responses from genai service
            let stream = genai.request_chat_stream(args, metadata).await?;
            Ok(stream)
        } else {
            Err(anyhow!("llm is not initialized"))
        }
    }

    async fn cancel(&mut self) {
        tracing::warn!("OllamaPromptRunner cancel: not implemented!");
    }
}
