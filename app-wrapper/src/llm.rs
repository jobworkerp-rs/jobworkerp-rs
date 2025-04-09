use self::args::LLMRequestConverter;
use self::model::LLMModelLoader;
use anyhow::Result;
use args::DefaultLLMRequestConverter;
use async_trait::async_trait;
use futures::{
    stream::{BoxStream, StreamExt},
    SinkExt,
};
use jobworkerp_base::{
    codec::{ProstMessageCodec, UseProstCodec},
    error::JobWorkerError,
};
use jobworkerp_runner::{
    jobworkerp::runner::{LlmArgs, LlmResult, LlmRunnerSettings},
    runner::{
        llm::{LLMRunnerSpec, LLMRunnerSpecImpl},
        RunnerSpec, RunnerTrait,
    },
};
use mistralrs::{IsqType as MistralIsqType, Model};
use proto::jobworkerp::data::ResultOutputItem;
use result::{DefaultLLMResultConverter, LLMResultConverter};
use schemars::JsonSchema;
use std::sync::Arc;

pub mod args;
pub mod model;
pub mod result;

#[derive(Clone, Debug)]
enum ModelType {
    TextModel,
    GgufModel,
}

pub struct LLMRunnerImpl {
    // Model parameters and state
    model_type: Option<ModelType>,
    model_name: Option<String>,
    with_logging: bool,
    with_paged_attn: bool,
    isq_type: Option<MistralIsqType>,
    tok_model_id: Option<String>,
    gguf_files: Vec<String>,

    // The loaded model
    model: Option<Arc<Model>>,
}
impl LLMModelLoader for LLMRunnerImpl {}
impl LLMRunnerSpec for LLMRunnerImpl {}

#[derive(Debug, JsonSchema, serde::Deserialize, serde::Serialize)]
struct LlmRunnerInputSchema {
    settings: LlmRunnerSettings,
    args: LlmArgs,
}

impl RunnerSpec for LLMRunnerImpl {
    fn name(&self) -> String {
        LLMRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        LLMRunnerSpec::runner_settings_proto(self)
    }

    fn job_args_proto(&self) -> String {
        LLMRunnerSpec::job_args_proto(self)
    }

    fn result_output_proto(&self) -> Option<String> {
        LLMRunnerSpec::result_output_proto(self)
    }

    fn output_type(&self) -> proto::jobworkerp::data::StreamingOutputType {
        LLMRunnerSpec::output_type(self)
    }
    fn settings_schema(&self) -> String {
        LLMRunnerSpec::settings_schema(self)
    }
    fn arguments_schema(&self) -> String {
        LLMRunnerSpec::arguments_schema(self)
    }
    fn output_schema(&self) -> Option<String> {
        LLMRunnerSpec::output_schema(self)
    }
}

impl LLMRunnerImpl {
    pub fn new() -> Self {
        Self {
            model_type: None,
            model_name: None,
            with_logging: false,
            with_paged_attn: false,
            isq_type: None,
            tok_model_id: None,
            gguf_files: Vec::new(),
            model: None,
        }
    }
    // Store model metadata from the settings
    fn store_model_metadata(&mut self, settings: &LlmRunnerSettings) {
        if let Some(model_settings) = &settings.model_settings {
            match model_settings {
                jobworkerp_runner::jobworkerp::runner::llm_runner_settings::ModelSettings::TextModel(
                    text_model,
                ) => {
                    self.model_type = Some(ModelType::TextModel);
                    self.model_name = Some(text_model.model_name_or_path.clone());

                    if let Some(isq_type) = text_model.isq_type {
                        self.isq_type = Some(Self::convert_isq_type(isq_type));
                    }

                    self.with_logging = text_model.with_logging;
                    self.with_paged_attn = text_model.with_paged_attn;
                }
                jobworkerp_runner::jobworkerp::runner::llm_runner_settings::ModelSettings::GgufModel(
                    gguf_model,
                ) => {
                    self.model_type = Some(ModelType::GgufModel);
                    self.model_name = Some(gguf_model.model_name_or_path.clone());
                    self.gguf_files = gguf_model.gguf_files.clone();

                    if let Some(tok_id) = &gguf_model.tok_model_id {
                        self.tok_model_id = Some(tok_id.clone());
                    }

                    self.with_logging = gguf_model.with_logging;
                }
            }
        }
    }
}

impl Default for LLMRunnerImpl {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RunnerTrait for LLMRunnerImpl {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let settings = ProstMessageCodec::deserialize_message::<LlmRunnerSettings>(&settings)?;

        // Store metadata for reference
        self.store_model_metadata(&settings);

        // Load the model
        let model = self.load_model(&settings).await?;
        self.model = Some(Arc::new(model));

        Ok(())
    }

    async fn run(&mut self, args: &[u8]) -> Result<Vec<Vec<u8>>> {
        let args = ProstMessageCodec::deserialize_message::<LlmArgs>(args)?;

        // Get model reference
        let model = self
            .model
            .as_ref()
            .ok_or_else(|| JobWorkerError::OtherError("Model not initialized".to_string()))?;

        // Build request
        let request_builder = DefaultLLMRequestConverter::build_request(&args, false)?;

        // Send request to model and get response
        let response = model.send_chat_request(request_builder).await?;
        let result = DefaultLLMResultConverter::convert_chat_completion_result(&response);

        Ok(vec![ProstMessageCodec::serialize_message(&result)?])
    }

    async fn run_stream(&mut self, args: &[u8]) -> Result<BoxStream<'static, ResultOutputItem>> {
        let args = ProstMessageCodec::deserialize_message::<LlmArgs>(args)?;

        // Get model reference and clone it to move into the spawned task
        let model = self
            .model
            .as_ref()
            .ok_or_else(|| JobWorkerError::OtherError("Model not initialized".to_string()))?
            .clone();

        // Build request
        let request_builder = DefaultLLMRequestConverter::build_request(&args, true)?;

        // Create a channel to send processed chunks through
        let (mut tx, rx) = futures::channel::mpsc::channel(10);

        // Spawn a task to handle the streaming and channel sending
        tokio::spawn(async move {
            // Send request to model and get response stream
            let result = async {
                let mut stream = model.stream_chat_request(request_builder).await?;

                // Process chunks until the stream is exhausted
                while let Some(response) = stream.next().await {
                    match response {
                        mistralrs::Response::Chunk(chunk_response) => {
                            // Convert chunk to LlmResult
                            let llm_result =
                                DefaultLLMResultConverter::convert_chat_completion_chunk_result(
                                    &chunk_response,
                                );
                            // Serialize to bytes
                            match ProstMessageCodec::serialize_message(&llm_result) {
                                Ok(bytes) => {
                                    let item = ResultOutputItem {
                                        item: Some(
                                            proto::jobworkerp::data::result_output_item::Item::Data(
                                                bytes,
                                            ),
                                        ),
                                    };
                                    // Send the item through the channel
                                    if tx.send(item).await.is_err() {
                                        // Channel closed, receiver dropped
                                        break;
                                    }
                                    if DefaultLLMResultConverter::is_chat_finished(&chunk_response)
                                    {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("Failed to serialize chunk result: {}", e);
                                    // Send empty data
                                    let item = ResultOutputItem {
                                        item: Some(
                                            proto::jobworkerp::data::result_output_item::Item::Data(
                                                Vec::new(),
                                            ),
                                        ),
                                    };
                                    if tx.send(item).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        mistralrs::Response::CompletionChunk(chunk_response) => {
                            // Convert chunk to LlmResult
                            let llm_result =
                                DefaultLLMResultConverter::convert_completion_chunk_result(
                                    &chunk_response,
                                );
                            // Serialize to bytes
                            match ProstMessageCodec::serialize_message(&llm_result) {
                                Ok(bytes) => {
                                    let item = ResultOutputItem {
                                        item: Some(
                                            proto::jobworkerp::data::result_output_item::Item::Data(
                                                bytes,
                                            ),
                                        ),
                                    };
                                    // Send the item through the channel
                                    if tx.send(item).await.is_err() {
                                        // Channel closed, receiver dropped
                                        break;
                                    }
                                    if DefaultLLMResultConverter::is_completion_finished(
                                        &chunk_response,
                                    ) {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("Failed to serialize chunk result: {}", e);
                                    // Send empty data
                                    let item = ResultOutputItem {
                                        item: Some(
                                            proto::jobworkerp::data::result_output_item::Item::Data(
                                                Vec::new(),
                                            ),
                                        ),
                                    };
                                    if tx.send(item).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        e => {
                            tracing::error!(
                                "Received unexpected response type from LLM: {:?}",
                                e.as_result()
                            );
                            // Send empty data
                            let item = ResultOutputItem {
                                item: Some(
                                    proto::jobworkerp::data::result_output_item::Item::Data(
                                        Vec::new(),
                                    ),
                                ),
                            };
                            if tx.send(item).await.is_err() {
                                break;
                            }
                        }
                    }
                }

                // Send end marker
                let end_item = ResultOutputItem {
                    item: Some(proto::jobworkerp::data::result_output_item::Item::End(
                        proto::jobworkerp::data::Empty {},
                    )),
                };

                // Ignoring result - if this fails, the receiver is already gone
                let _ = tx.send(end_item).await;

                Ok::<_, anyhow::Error>(())
            }
            .await;

            if let Err(e) = result {
                tracing::error!("Error processing LLM stream: {}", e);
            }
        });

        // Convert the receiver to a BoxStream and return it
        Ok(rx.boxed())
    }

    async fn cancel(&mut self) {
        tracing::warn!("cannot cancel request until timeout")
    }
}

// Too heavy to run on the test environment
// cargo test --features test-env -- --ignored
#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use futures::stream::StreamExt;
    use jobworkerp_runner::jobworkerp::runner::llm_args;
    use jobworkerp_runner::jobworkerp::runner::llm_result;
    use jobworkerp_runner::jobworkerp::runner::llm_result::FinishReason;
    use jobworkerp_runner::jobworkerp::runner::llm_runner_settings;
    use jobworkerp_runner::jobworkerp::runner::LlmResult;
    use prost::Message;

    #[ignore]
    #[tokio::test]
    async fn test_completion_llm_runner() -> Result<()> {
        let mut runner = LLMRunnerImpl::new();

        // Load settings
        let settings = create_settings()?;
        let settings_bytes = ProstMessageCodec::serialize_message(&settings)?;

        // Load model
        runner.load(settings_bytes).await?;

        // Load args
        let args = create_completion_args(false)?;
        let args_bytes = ProstMessageCodec::serialize_message(&args)?;

        // Run
        let result = runner.run(&args_bytes).await?;
        let result: LlmResult = Message::decode(&result[0][..])?;

        match result.result {
            Some(llm_result::Result::ChatCompletion(res)) => {
                assert_eq!(res.choices.len(), 1);
                assert_eq!(res.choices[0].finish_reason, FinishReason::Stop as i32);
                assert!(res.choices[0]
                    .message
                    .as_ref()
                    .unwrap()
                    .content
                    .as_ref()
                    .unwrap()
                    .starts_with("Hello, world!"));
            }
            e => panic!("No result found: {:?}", e),
        }

        Ok(())
    }

    #[ignore]
    #[tokio::test]
    async fn test_completion_llm_runner_stream() -> Result<()> {
        let mut runner = LLMRunnerImpl::new();

        // Load settings
        let settings = create_settings()?;
        let settings_bytes = ProstMessageCodec::serialize_message(&settings)?;

        // Load model
        runner.load(settings_bytes).await?;

        // Load args
        let args = create_completion_args(true)?;
        let args_bytes = ProstMessageCodec::serialize_message(&args)?;

        // Run
        let stream = runner.run_stream(&args_bytes).await?;
        let mut stream = stream;

        let mut count = 0;
        let mut output = Vec::new();
        while let Some(item) = stream.next().await {
            let item = item;
            match item.item {
                Some(proto::jobworkerp::data::result_output_item::Item::Data(data)) => {
                    let result: LlmResult = Message::decode(&data[..])?;
                    match result.result {
                        // XXX not CompletionChunk...
                        Some(llm_result::Result::ChatCompletionChunk(res)) => {
                            println!("{:?}", &res);
                            assert_eq!(res.choices.len(), 1);
                            assert_eq!(
                                res.choices[0].delta.as_ref().unwrap().role.as_str(),
                                "assistant"
                            );
                            let content = res.choices[0]
                                .delta
                                .as_ref()
                                .unwrap()
                                .content
                                .as_ref()
                                .unwrap();
                            assert!(
                                !content.as_str().is_empty()
                                    || res.choices[0].finish_reason.is_some()
                            );
                            output.push(content.clone());
                        }
                        e => panic!("No result found: {:?}", e),
                    }
                    count += 1;
                }
                Some(proto::jobworkerp::data::result_output_item::Item::End(_)) | None => break,
            }
        }
        println!("{:?}", output);
        assert!(count > 1);

        Ok(())
    }

    #[ignore]
    #[tokio::test]
    async fn test_llm_runner() -> Result<()> {
        let mut runner = LLMRunnerImpl::new();

        // Load settings
        let settings = create_settings()?;
        let settings_bytes = ProstMessageCodec::serialize_message(&settings)?;

        // Load model
        runner.load(settings_bytes).await?;

        // Load args
        let args = create_chat_args(false)?;
        let args_bytes = ProstMessageCodec::serialize_message(&args)?;

        // Run
        let result = runner.run(&args_bytes).await?;
        let result: LlmResult = Message::decode(&result[0][..])?;

        match result.result {
            Some(llm_result::Result::ChatCompletion(res)) => {
                assert_eq!(res.choices.len(), 1);
                assert!(res.choices[0]
                    .message
                    .as_ref()
                    .unwrap()
                    .content
                    .as_ref()
                    .unwrap()
                    .starts_with("Hello, world!"));
            }
            e => panic!("No result found: {:?}", e),
        }

        Ok(())
    }

    #[ignore]
    #[tokio::test]
    async fn test_llm_runner_stream() -> Result<()> {
        let mut runner = LLMRunnerImpl::new();

        // Load settings
        let settings = create_settings()?;
        let settings_bytes = ProstMessageCodec::serialize_message(&settings)?;

        // Load model
        runner.load(settings_bytes).await?;

        // Load args
        let args = create_chat_args(true)?;
        let args_bytes = ProstMessageCodec::serialize_message(&args)?;

        // Run
        let stream = runner.run_stream(&args_bytes).await?;
        let mut stream = stream;

        let mut count = 0;
        let mut output = Vec::new();
        while let Some(item) = stream.next().await {
            let item = item;
            match item.item {
                Some(proto::jobworkerp::data::result_output_item::Item::Data(data)) => {
                    let result: LlmResult = Message::decode(&data[..])?;
                    match result.result {
                        Some(llm_result::Result::ChatCompletionChunk(res)) => {
                            println!("{:?}", &res);
                            assert_eq!(res.choices.len(), 1);
                            assert_eq!(
                                res.choices[0].delta.as_ref().unwrap().role.as_str(),
                                "assistant"
                            );
                            let content = res.choices[0]
                                .delta
                                .as_ref()
                                .unwrap()
                                .content
                                .as_ref()
                                .unwrap();
                            assert!(
                                !content.as_str().is_empty()
                                    || res.choices[0].finish_reason.is_some()
                            );
                            output.push(content.clone());
                        }
                        e => panic!("No result found: {:?}", e),
                    }
                    count += 1;
                }
                Some(proto::jobworkerp::data::result_output_item::Item::End(_)) | None => break,
            }
        }
        println!("{:?}", output);
        assert!(count > 1);

        Ok(())
    }

    #[ignore]
    #[tokio::test]
    async fn test_gguf_llm_runner_stream() -> Result<()> {
        let mut runner = LLMRunnerImpl::new();

        // Load settings
        let settings = create_gguf_settings()?;
        let settings_bytes = ProstMessageCodec::serialize_message(&settings)?;

        // Load model
        runner.load(settings_bytes).await?;

        // Load args
        let args = create_chat_args(true)?;
        let args_bytes = ProstMessageCodec::serialize_message(&args)?;

        // Run
        let stream = runner.run_stream(&args_bytes).await?;
        let mut stream = stream;

        let mut count = 0;
        let mut output = Vec::new();
        while let Some(item) = stream.next().await {
            let item = item;
            match item.item {
                Some(proto::jobworkerp::data::result_output_item::Item::Data(data)) => {
                    let result: LlmResult = Message::decode(&data[..])?;
                    match result.result {
                        Some(llm_result::Result::ChatCompletionChunk(res)) => {
                            println!("{:?}", &res);
                            assert_eq!(res.choices.len(), 1);
                            assert_eq!(
                                res.choices[0].delta.as_ref().unwrap().role.as_str(),
                                "assistant"
                            );
                            let content = res.choices[0]
                                .delta
                                .as_ref()
                                .unwrap()
                                .content
                                .as_ref()
                                .unwrap();
                            assert!(
                                !content.as_str().is_empty()
                                    || res.choices[0].finish_reason.is_some()
                            );
                            output.push(content.clone());
                        }
                        e => panic!("No result found: {:?}", e),
                    }
                    count += 1;
                }
                Some(proto::jobworkerp::data::result_output_item::Item::End(_)) | None => break,
            }
        }
        println!("{:?}", output);
        assert!(count > 1);

        Ok(())
    }

    #[ignore]
    #[tokio::test]
    async fn test_llm_tool_runner() -> Result<()> {
        let mut runner = LLMRunnerImpl::new();

        // Load settings
        let settings = create_gguf_tool_settings()?; //create_settings_for_tool()?;
        let settings_bytes = ProstMessageCodec::serialize_message(&settings)?;

        // Load model
        runner.load(settings_bytes).await?;

        // Load args
        let args = create_tool_args()?;
        let args_bytes = ProstMessageCodec::serialize_message(&args)?;

        // Run
        let result = runner.run(&args_bytes).await?;
        let result: LlmResult = Message::decode(&result[0][..])?;

        match result.result {
            Some(llm_result::Result::ChatCompletion(res)) => {
                println!("RES: {:#?}", &res);
                assert_eq!(res.choices.len(), 1);
                assert!(res.choices[0].message.as_ref().unwrap().tool_calls.len() > 0);
            }
            e => panic!("No result found: {:?}", e),
        }
        Ok(())
    }

    fn create_settings() -> Result<LlmRunnerSettings> {
        let settings = LlmRunnerSettings {
            model_settings: Some(llm_runner_settings::ModelSettings::TextModel(
                llm_runner_settings::TextModelSettings {
                    model_name_or_path: "microsoft/Phi-4-mini-instruct".to_string(),
                    isq_type: Some(llm_runner_settings::IsqType::Q80 as i32),
                    with_logging: true,
                    with_paged_attn: false, // false for mac metal
                    chat_template: None,
                },
            )),
            auto_device_map: None,
            // auto_device_map: Some(llm_runner_settings::AutoDeviceMapParams {
            //     max_seq_len: 4096,
            //     max_batch_size: 2,
            // }),
        };
        Ok(settings)
    }

    fn create_settings_for_tool() -> Result<LlmRunnerSettings> {
        let settings = LlmRunnerSettings {
            model_settings: Some(llm_runner_settings::ModelSettings::TextModel(
                llm_runner_settings::TextModelSettings {
                    // model_name_or_path: "watt-ai/watt-tool-8B".to_string(),
                    // model_name_or_path: "deepseek-ai/DeepSeek-R1-Distill-Qwen-7B".to_string(),
                    model_name_or_path: "Qwen/Qwen2.5-7B-Instruct".to_string(),
                    isq_type: None, //Some(llm_runner_settings::IsqType::Q80 as i32),
                    with_logging: true,
                    with_paged_attn: false,
                    // chat_template: Some("/workspace/github/chat_templates/llama3.3.json".to_string()),
                    chat_template: None,
                },
            )),
            auto_device_map: None,
            // auto_device_map: Some(llm_runner_settings::AutoDeviceMapParams {
            //     max_seq_len: 4096,
            //     max_batch_size: 2,
            // }),
        };
        Ok(settings)
    }

    fn create_gguf_settings() -> Result<LlmRunnerSettings> {
        let settings = LlmRunnerSettings {
            model_settings: Some(llm_runner_settings::ModelSettings::GgufModel(
                llm_runner_settings::GgufModelSettings {
                    model_name_or_path: "bartowski/google_gemma-3-12b-it-GGUF".to_string(),
                    gguf_files: vec!["google_gemma-3-12b-it-Q4_K_M.gguf".to_string()],
                    //                    model_name_or_path: "microsoft/phi-4-gguf".to_string(),
                    //                    gguf_files: vec!["phi-4-q4.gguf".to_string()],
                    // tok_model_id: Some("Qwen/Qwen2.5-32B".to_string()),
                    tok_model_id: None,
                    with_logging: true,
                    chat_template: None,
                },
            )),
            // auto_device_map: None,
            auto_device_map: Some(llm_runner_settings::AutoDeviceMapParams {
                max_seq_len: 4096,
                max_batch_size: 2,
            }),
        };
        Ok(settings)
    }

    fn create_gguf_tool_settings() -> Result<LlmRunnerSettings> {
        let settings = LlmRunnerSettings {
            model_settings: Some(llm_runner_settings::ModelSettings::GgufModel(
                llm_runner_settings::GgufModelSettings {
                    model_name_or_path:
                        "bartowski/mistralai_Mistral-Small-3.1-24B-Instruct-2503-GGUF".to_string(),
                    gguf_files: vec![
                        "mistralai_Mistral-Small-3.1-24B-Instruct-2503-Q4_K_M.gguf".to_string()
                    ],
                    //                    model_name_or_path: "microsoft/phi-4-gguf".to_string(),
                    //                    gguf_files: vec!["phi-4-q4.gguf".to_string()],
                    // tok_model_id: Some("Qwen/Qwen2.5-32B".to_string()),
                    tok_model_id: None,
                    with_logging: true,
                    chat_template: Some(
                        "/workspace/github/chat_templates/mistral.jinja".to_string(),
                    ),
                },
            )),
            auto_device_map: None,
            // auto_device_map: Some(llm_runner_settings::AutoDeviceMapParams {
            //     max_seq_len: 4096,
            //     max_batch_size: 2,
            // }),
        };
        Ok(settings)
    }

    fn create_chat_args(stream: bool) -> Result<LlmArgs> {
        let args = LlmArgs {
            request: Some(llm_args::Request::ChatCompletion(
                llm_args::ChatCompletionRequest {
                    logprobs: false,
                    top_logprobs: None,
                    stream,
                    options: Some(llm_args::LlmRequestOptions {
                        max_tokens: Some(1000),
                        temperature: Some(0.5),
                        // https://github.com/oobabooga/text-generation-webui/pull/5677
                        dry_allowed_length: Some(5),
                        dry_base: Some(1.75),
                        dry_multiplier: Some(0.8),
                        dry_sequence_breakers: vec![
                            "\n".to_string(),
                            ":".to_string(),
                            "\"".to_string(),
                            "*".to_string(),
                        ],
                        ..Default::default()
                    }),
                    messages_format: Some(
                        llm_args::chat_completion_request::MessagesFormat::ChatMessages(
                            llm_args::ChatMessages {
                                messages: vec![llm_args::ChatMessage {
                                    role: llm_args::Role::User as i32,
                                    content: Some(llm_args::chat_message::Content::Text(
                                        "Hello, world!".to_string(),
                                    )),
                                }],
                            },
                        ),
                    ),
                },
            )),
        };
        Ok(args)
    }

    fn create_completion_args(stream: bool) -> Result<LlmArgs> {
        let args = LlmArgs {
            request: Some(llm_args::Request::Completion(llm_args::CompletionRequest {
                prompt: "Hello, world!".to_string(),
                best_of: Some(1),
                stream,
                echo_prompt: false,
                suffix: None,
                options: None,
            })),
        };
        Ok(args)
    }
    fn create_tool_args() -> Result<LlmArgs> {
        let args = LlmArgs {
            request: Some(llm_args::Request::ChatCompletion(
                llm_args::ChatCompletionRequest {
                    logprobs: false,
                    top_logprobs: None,
                    stream: false,
                    options: Some(llm_args::LlmRequestOptions {
                        // max_tokens: Some(1000),
                        tool_schemas: tool_schemas(),
                        tool_choice: Some(llm_args::ToolChoiceOption::Auto as i32),
                        temperature: Some(0.1),
                        // https://github.com/oobabooga/text-generation-webui/pull/5677
                        dry_allowed_length: Some(8),
                        dry_base: Some(1.75),
                        dry_multiplier: Some(0.8),
                        // dry_sequence_breakers: vec![
                        //     "\n".to_string(),
                        //     ":".to_string(),
                        //     "\"".to_string(),
                        //     "*".to_string(),
                        // ],
                        ..Default::default()
                    }),
                    messages_format: Some(
                        llm_args::chat_completion_request::MessagesFormat::ChatMessages(
                            llm_args::ChatMessages {
                                messages: vec![
                                    llm_args::ChatMessage {
                                        role: llm_args::Role::System as i32,
                                        content: Some(llm_args::chat_message::Content::Text(
                                            "You are an expert in composing functions. You are given a question and a set of possible functions. Based on the question, you will need to make one or more function/tool calls to achieve the purpose.
If none of the function can be used, point it out. If the given question lacks the parameters required by the function, also point it out.
You should only return the function call in tools call sections.
You SHOULD NOT include any other text in the response.
".to_string(),
                                        )),
                                    },
                                    llm_args::ChatMessage {
                                        role: llm_args::Role::User as i32,
                                        content: Some(llm_args::chat_message::Content::Text(
                                            "Find me the sales growth rate for company XYZ for the last 3 years and also the interest coverage ratio for the same duration.".to_string(),
                                        )),
                                    },
                                ],
                            },
                        ),
                    ),
                },
            )),
        };
        Ok(args)
    }
    fn tool_schemas() -> Vec<String> {
        vec![
            serde_json::json!({
                "name": "financial_ratios.interest_coverage", "description": "Calculate a company's interest coverage ratio given the company name and duration",
                "parameters": {
                    "type": "dict",
                    "properties": {
                        "company_name": {
                            "type": "string",
                            "description": "The name of the company."
                        },
                        "years": {
                            "type": "integer",
                            "description": "Number of past years to calculate the ratio."
                        }
                    },
                    "required": ["company_name", "years"]
                }
            }).to_string(),
            serde_json::json!({
                "name": "sales_growth.calculate",
                "description": "Calculate a company's sales growth rate given the company name and duration",
                "parameters": {
                    "type": "dict",
                    "properties": {
                        "company": {
                            "type": "string",
                            "description": "The company that you want to get the sales growth rate for."
                        },
                        "years": {
                            "type": "integer",
                            "description": "Number of past years for which to calculate the sales growth rate."
                        }
                    },
                    "required": ["company", "years"]
                }
            }).to_string(),
            serde_json::json!({
                "name": "weather_forecast",
                "description": "Retrieve a weather forecast for a specific location and time frame.",
                "paramenters": {
                    "type": "dict",
                    "properties": {
                        "location": {
                            "type": "string",
                            "description": "The city that you want to get the weather for."
                        },
                        "days": {
                            "type": "integer",
                            "description": "Number of days for the forecast."
                        }
                    },
                    "required": ["location", "days"]
                }
            }).to_string(),
        ]
    }
}
