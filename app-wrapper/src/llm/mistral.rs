pub mod args;
pub mod core;
pub mod model;
pub mod result;
pub mod tracing;

pub use self::args::LLMRequestConverter;
pub use self::core::MistralCoreService;
use self::model::MistralModelLoader;
use anyhow::Result;
pub use args::DefaultLLMRequestConverter;
use async_stream::stream;
use futures::{
    stream::{BoxStream, StreamExt},
    SinkExt,
};
use jobworkerp_base::{
    codec::{ProstMessageCodec, UseProstCodec},
    error::JobWorkerError,
};
use jobworkerp_runner::jobworkerp::runner::llm::{
    llm_runner_settings::LocalRunnerSettings, LlmChatArgs, LlmCompletionArgs,
};
use mistralrs::{IsqType as MistralIsqType, Model};
use proto::jobworkerp::data::ResultOutputItem;
pub use result::{DefaultLLMResultConverter, LLMResultConverter};
use std::sync::Arc;

#[derive(Clone, Debug)]
enum ModelType {
    TextModel,
    GgufModel,
}

pub struct MistralLlmServiceImpl {
    // Model parameters and state
    model_type: ModelType,
    model_name: String,
    with_logging: bool,
    with_paged_attn: bool,
    isq_type: Option<MistralIsqType>,
    tok_model_id: Option<String>,
    gguf_files: Vec<String>,

    // The loaded model
    pub model: Arc<Model>,
}
impl MistralModelLoader for MistralLlmServiceImpl {}

impl MistralCoreService for MistralLlmServiceImpl {
    async fn request_chat(
        &self,
        request_builder: mistralrs::RequestBuilder,
    ) -> Result<mistralrs::ChatCompletionResponse> {
        let response = self.model.send_chat_request(request_builder).await?;
        Ok(response)
    }

    async fn stream_chat(
        &self,
        request_builder: mistralrs::RequestBuilder,
    ) -> Result<futures::stream::BoxStream<'static, mistralrs::Response>> {
        // For Phase 1, we'll implement a placeholder that returns a single response as a stream
        // This will be properly implemented in Phase 2 with full streaming support
        let response = self.model.send_chat_request(request_builder).await?;
        use async_stream::stream;
        let single_item_stream = stream! {
            // For Phase 1, create a compatible response using the chat completion format
            // This is a placeholder implementation - Phase 2 will implement proper streaming
            if response.choices.len() > 0 {
                yield mistralrs::Response::Done(response);
            }
        };
        Ok(single_item_stream.boxed())
    }

    fn model(&self) -> &Arc<Model> {
        &self.model
    }

    fn model_name(&self) -> &str {
        &self.model_name
    }
}

impl MistralLlmServiceImpl {
    // Store model metadata from the settings
    pub async fn new(settings: &LocalRunnerSettings) -> Result<Self> {
        let model = Arc::new(Self::load_model(settings).await?);

        match &settings.model_settings {
            Some(jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::local_runner_settings::ModelSettings::TextModel(
                text_model,
            )) => {
                let model_type = ModelType::TextModel;
                let model_name = text_model.model_name_or_path.clone();

                let isq_type = text_model.isq_type.map(Self::convert_isq_type);

                let with_logging = text_model.with_logging;
                let with_paged_attn = text_model.with_paged_attn;
                Ok(Self {
                    model,
                    model_type,
                    model_name,
                    with_logging,
                    with_paged_attn,
                    isq_type,
                    tok_model_id: None,
                    gguf_files: vec![],
                })
            }
            Some(jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::local_runner_settings::ModelSettings::GgufModel(
                gguf_model,
            )) => {
                let model_type = ModelType::GgufModel;
                let model_name = gguf_model.model_name_or_path.clone();
                let gguf_files = gguf_model.gguf_files.clone();

                let tok_id = gguf_model.tok_model_id.clone();

                let with_logging = gguf_model.with_logging;
                Ok(Self {
                    model,
                    model_type,
                    model_name,
                    with_logging,
                    with_paged_attn: false, // GGUF models don't use paged attention
                    isq_type: None,
                    tok_model_id: tok_id,
                    gguf_files,
                })
            }
            None => {
                Err(JobWorkerError::InvalidParameter("No model settings provided".to_string()).into())
            }
        }
    }

    pub async fn request_chat(
        &self,
        request_builder: mistralrs::RequestBuilder,
    ) -> Result<mistralrs::ChatCompletionResponse> {
        let response = self.model.send_chat_request(request_builder).await?;
        Ok(response)
    }

    async fn run(&mut self, args: &[u8]) -> Result<Vec<Vec<u8>>> {
        // Get model reference
        let model = &self.model;

        // Build request based on args type - no function app available in this context
        // TODO: This should be injected from the caller context for function calling
        let converter = DefaultLLMRequestConverter::new(None);
        let request_builder = if let Ok(chat_args) =
            ProstMessageCodec::deserialize_message::<LlmChatArgs>(args)
        {
            converter.build_request(&chat_args, false).await?
        } else if let Ok(completion_args) =
            ProstMessageCodec::deserialize_message::<LlmCompletionArgs>(args)
        {
            converter
                .build_completion_request(&completion_args, false)
                .await?
        } else {
            return Err(JobWorkerError::InvalidParameter("Invalid args type".to_string()).into());
        };

        // Send request to model and get response
        let response = model.send_chat_request(request_builder).await?;
        let result =
            if let Ok(_chat_args) = ProstMessageCodec::deserialize_message::<LlmChatArgs>(args) {
                DefaultLLMResultConverter::convert_chat_completion_result(&response)
            } else {
                // For completion requests, convert differently
                // TODO: Use appropriate completion conversion method
                DefaultLLMResultConverter::convert_chat_completion_result(&response)
            };

        Ok(vec![ProstMessageCodec::serialize_message(&result)?])
    }

    async fn run_stream(&mut self, args: &[u8]) -> Result<BoxStream<'static, ResultOutputItem>> {
        // Get model reference and clone it to move into the spawned task
        let model = self.model.clone();

        // Build request based on args type - no function app available in this context
        // TODO: This should be injected from the caller context for function calling
        let converter = DefaultLLMRequestConverter::new(None);
        let request_builder = if let Ok(chat_args) =
            ProstMessageCodec::deserialize_message::<LlmChatArgs>(args)
        {
            converter.build_request(&chat_args, true).await?
        } else if let Ok(completion_args) =
            ProstMessageCodec::deserialize_message::<LlmCompletionArgs>(args)
        {
            converter
                .build_completion_request(&completion_args, true)
                .await?
        } else {
            return Err(JobWorkerError::InvalidParameter("Invalid args type".to_string()).into());
        };

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
                                    ::tracing::error!("Failed to serialize chunk result: {}", e);
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
                                    ::tracing::error!("Failed to serialize chunk result: {}", e);
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
                        _ => {
                            ::tracing::warn!("Received unexpected response type from LLM");
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
                        proto::jobworkerp::data::Trailer {
                            metadata: std::collections::HashMap::new(),
                        },
                    )),
                };

                // Ignoring result - if this fails, the receiver is already gone
                let _ = tx.send(end_item).await;

                Ok::<_, anyhow::Error>(())
            }
            .await;

            if let Err(e) = result {
                ::tracing::error!("Error processing LLM stream: {}", e);
            }
        });

        // Convert the receiver to a BoxStream and return it
        Ok(rx.boxed())
    }

    async fn cancel(&mut self) {
        ::tracing::warn!("cannot cancel request until timeout")
    }
}

// TODO: Re-enable tests after integration completion
// Too heavy to run on the test environment
// cargo test --features test-env -- --ignored
#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use futures::stream::StreamExt;
    use jobworkerp_runner::jobworkerp::runner::llm::{
        llm_runner_settings::LocalRunnerSettings,
        LlmChatArgs, LlmCompletionArgs,
    };

    #[ignore]
    #[tokio::test]
    async fn test_completion_llm_runner() -> Result<()> {
        // Create settings
        let settings = create_mistral_settings()?;

        // Create service instance
        let service = MistralLlmServiceImpl::new(&settings).await?;

        // Create completion args
        let args = create_completion_args(false)?;

        // Convert to request builder
        let converter = DefaultLLMRequestConverter::new(None);
        let request_builder = converter.build_completion_request(&args, false).await?;

        // Send request
        let response = service.request_chat(request_builder).await?;

        // Convert to result
        let result = DefaultLLMResultConverter::convert_chat_completion_result(&response);

        // Verify response
        assert!(result.done || result.content.is_some());
        if let Some(content) = result.content {
            if let Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text)) = content.content {
                println!("Completion response: {}", text);
                assert!(!text.is_empty());
            } else {
                println!("No text content in completion response");
                assert!(false, "Expected text content in completion response");
            }
        } else {
            println!("No content in completion response");
            assert!(false, "Expected content in completion response");
        }

        Ok(())
    }

    #[ignore]
    #[tokio::test]
    async fn test_chat_llm_runner() -> Result<()> {
        // Create settings
        let settings = create_mistral_settings()?;

        // Create service instance
        let service = MistralLlmServiceImpl::new(&settings).await?;

        // Create chat args
        let args = create_chat_args(false)?;

        // Convert to request builder
        let request_builder = DefaultLLMRequestConverter::new(None)
            .build_request(&args, false)
            .await?;

        // Send request
        let response = service.request_chat(request_builder).await?;

        // Convert to result
        let result = DefaultLLMResultConverter::convert_chat_completion_result(&response);

        // Verify response
        assert!(result.done || result.content.is_some());
        if let Some(content) = result.content {
            if let Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text)) = content.content {
                assert!(!text.is_empty());
                println!("Chat response: {}", text);
            }
        }

        Ok(())
    }

    #[ignore]
    #[tokio::test]
    async fn test_gguf_model() -> Result<()> {
        // Create GGUF settings
        let settings = create_gguf_mistral_settings()?;

        // Create service instance
        let service = MistralLlmServiceImpl::new(&settings).await?;

        // Create chat args
        let args = create_chat_args(false)?;

        // Convert to request builder
        let request_builder = DefaultLLMRequestConverter::new(None)
            .build_request(&args, false)
            .await?;

        // Send request
        let response = service.request_chat(request_builder).await?;

        // Convert to result
        let result = DefaultLLMResultConverter::convert_chat_completion_result(&response);

        // Verify response
        assert!(result.done || result.content.is_some());
        if let Some(content) = result.content {
            if let Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text)) = content.content {
                assert!(!text.is_empty());
                println!("GGUF response: {}", text);
            }
        }

        Ok(())
    }

    #[ignore]
    #[tokio::test]
    async fn test_llm_runner_stream() -> Result<()> {
        // Create settings
        let settings = create_mistral_settings()?;

        // Create service instance
        let service = MistralLlmServiceImpl::new(&settings).await?;

        // Create chat args for streaming
        let args = create_chat_args(true)?;

        // Convert to request builder
        let converter = DefaultLLMRequestConverter::new(None);
        let request_builder = converter.build_request(&args, true).await?;

        // Use the internal streaming method directly
        let args_bytes = ProstMessageCodec::serialize_message(&args)?;
        let mut service_mut = service; // Make mutable for run_stream
        let stream = service_mut.run_stream(&args_bytes).await?;

        let mut stream = stream;
        let mut count = 0;
        let mut output = Vec::new();

        while let Some(item) = stream.next().await {
            match item.item {
                Some(proto::jobworkerp::data::result_output_item::Item::Data(data)) => {
                    // Deserialize the LLM result
                    let result = ProstMessageCodec::deserialize_message::<
                        jobworkerp_runner::jobworkerp::runner::llm::LlmChatResult,
                    >(&data)?;

                    println!("Stream item: {:?}", &result);

                    if let Some(content) = &result.content {
                        if let Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text)) = &content.content {
                            if !text.is_empty() {
                                output.push(text.clone());
                            }
                        }
                    }

                    count += 1;

                    if result.done {
                        break;
                    }
                }
                Some(proto::jobworkerp::data::result_output_item::Item::End(_)) => break,
                None => break,
            }
        }

        println!("Stream output: {:?}", output);
        assert!(count > 0, "Expected at least one stream item");

        Ok(())
    }

    #[ignore]
    #[tokio::test]
    async fn test_gguf_llm_runner_stream() -> Result<()> {
        // Create GGUF settings
        let settings = create_gguf_mistral_settings()?;

        // Create service instance
        let service = MistralLlmServiceImpl::new(&settings).await?;

        // Create chat args for streaming
        let args = create_chat_args(true)?;

        // Use the internal streaming method directly
        let args_bytes = ProstMessageCodec::serialize_message(&args)?;
        let mut service_mut = service; // Make mutable for run_stream
        let stream = service_mut.run_stream(&args_bytes).await?;

        let mut stream = stream;
        let mut count = 0;
        let mut output = Vec::new();

        while let Some(item) = stream.next().await {
            match item.item {
                Some(proto::jobworkerp::data::result_output_item::Item::Data(data)) => {
                    // Deserialize the LLM result
                    let result = ProstMessageCodec::deserialize_message::<
                        jobworkerp_runner::jobworkerp::runner::llm::LlmChatResult,
                    >(&data)?;

                    if let Some(content) = &result.content {
                        if let Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text)) = &content.content {
                            if !text.is_empty() {
                                output.push(text.clone());
                            }
                        }
                    }

                    count += 1;

                    if result.done {
                        break;
                    }
                }
                Some(proto::jobworkerp::data::result_output_item::Item::End(_)) => break,
                None => break,
            }
        }

        println!("GGUF stream output: {:?}", output);
        assert!(count > 0, "Expected at least one stream item");

        Ok(())
    }

    #[ignore]
    #[tokio::test]
    async fn test_llm_tool_runner() -> Result<()> {
        // Create tool-capable settings
        let settings = create_mistral_tool_settings()?;

        // Create service instance
        let service = MistralLlmServiceImpl::new(&settings).await?;

        // Create tool args
        let args = create_tool_args()?;

        // Convert to request builder
        let request_builder = DefaultLLMRequestConverter::new(None)
            .build_request(&args, false)
            .await?;

        // Send request
        let response = service.request_chat(request_builder).await?;

        // Convert to result
        let result = DefaultLLMResultConverter::convert_chat_completion_result(&response);

        println!("Tool result: {:#?}", &result);

        // Check if response contains tool calls or text
        assert!(result.content.is_some());
        if let Some(content) = result.content {
            match content.content {
                Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::ToolCalls(tool_calls)) => {
                    assert!(!tool_calls.calls.is_empty(), "Expected tool calls");
                    println!("Tool calls found: {}", tool_calls.calls.len());
                }
                Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text)) => {
                    assert!(!text.is_empty(), "Expected non-empty text response");
                    println!("Text response: {}", text);
                }
                _ => panic!("Expected either tool calls or text content"),
            }
        }

        Ok(())
    }

    fn create_mistral_settings() -> Result<LocalRunnerSettings> {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::local_runner_settings::*;
        let settings = LocalRunnerSettings {
            model_settings: Some(ModelSettings::TextModel(TextModelSettings {
                // model_name_or_path: "openai/gpt-oss-20b".to_string(),
                model_name_or_path: "Qwen/Qwen3-4B-Thinking-2507-FP8".to_string(),
                isq_type: None, //Some(IsqType::Q80 as i32),
                with_logging: true,
                with_paged_attn: true, //false, // false for mac metal
                chat_template: None,
            })),
            auto_device_map: None,
        };
        Ok(settings)
    }

    fn create_mistral_tool_settings() -> Result<LocalRunnerSettings> {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::local_runner_settings::*;
        let settings = LocalRunnerSettings {
            model_settings: Some(ModelSettings::TextModel(TextModelSettings {
                model_name_or_path: "Qwen/Qwen3-4B-Thinking-2507-FP8".to_string(),
                isq_type: None,
                with_logging: true,
                with_paged_attn: false,
                chat_template: None,
            })),
            auto_device_map: None,
        };
        Ok(settings)
    }

    fn create_gguf_mistral_settings() -> Result<LocalRunnerSettings> {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::local_runner_settings::*;
        let settings = LocalRunnerSettings {
            model_settings: Some(ModelSettings::GgufModel(GgufModelSettings {
                model_name_or_path: "bartowski/mistralai_Mistral-Small-3.1-24B-Instruct-2503-GGUF"
                    .to_string(),
                gguf_files: vec![
                    "mistralai_Mistral-Small-3.1-24B-Instruct-2503-Q4_K_M.gguf".to_string()
                ],
                // model_name_or_path: "bartowski/Qwen_Qwen3-30B-A3B-Instruct-2507-GGUF".to_string(),
                // gguf_files: vec!["Qwen_Qwen3-30B-A3B-Instruct-2507-Q4_K_L.gguf".to_string()],
                // model_name_or_path: "unsloth/Qwen3-32B-GGUF".to_string(),
                // gguf_files: vec!["Qwen3-32B-Q4_K_M.gguf".to_string()],
                // tok_model_id: Some("Qwen/Qwen3-32B".to_string()),
                tok_model_id: None,
                with_logging: true,
                chat_template: None,
            })),
            auto_device_map: None,
        };
        Ok(settings)
    }

    fn create_gguf_mistral_tool_settings() -> Result<LocalRunnerSettings> {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::local_runner_settings::*;
        let settings = LocalRunnerSettings {
            model_settings: Some(ModelSettings::GgufModel(GgufModelSettings {
                model_name_or_path: "bartowski/mistralai_Mistral-Small-3.1-24B-Instruct-2503-GGUF"
                    .to_string(),
                gguf_files: vec![
                    "mistralai_Mistral-Small-3.1-24B-Instruct-2503-Q4_K_M.gguf".to_string()
                ],
                // model_name_or_path: "bartowski/Qwen_Qwen3-30B-A3B-Instruct-2507-GGUF".to_string(),
                // gguf_files: vec!["Qwen_Qwen3-30B-A3B-Instruct-2507-Q4_K_L.gguf".to_string()],
                tok_model_id: None,
                with_logging: true,
                chat_template: Some("/workspace/github/chat_templates/mistral.jinja".to_string()),
            })),
            auto_device_map: None,
        };
        Ok(settings)
    }

    fn create_chat_args(_stream: bool) -> Result<LlmChatArgs> {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::*;
        let args = LlmChatArgs {
            messages: vec![ChatMessage {
                role: ChatRole::User as i32,
                content: Some(MessageContent {
                    content: Some(message_content::Content::Text("Hello, world!".to_string())),
                }),
            }],
            options: Some(LlmOptions {
                max_tokens: Some(1000),
                temperature: Some(0.5),
                ..Default::default()
            }),
            ..Default::default()
        };
        Ok(args)
    }

    fn create_completion_args(_stream: bool) -> Result<LlmCompletionArgs> {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_args::*;
        let args = LlmCompletionArgs {
            model: Some("test-model".to_string()),
            options: Some(LlmOptions {
                max_tokens: Some(1000),
                temperature: Some(0.5),
                ..Default::default()
            }),
            prompt: "Hello!".to_string(),
            ..Default::default()
        };
        Ok(args)
    }
    fn create_tool_args() -> Result<LlmChatArgs> {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::*;
        let args = LlmChatArgs {
            messages: vec![
                ChatMessage {
                    role: ChatRole::System as i32,
                    content: Some(MessageContent {
                        content: Some(message_content::Content::Text(
                            "You are an expert in composing functions. You are given a question and a set of possible functions. Based on the question, you will need to make one or more function/tool calls to achieve the purpose.
If none of the function can be used, point it out. If the given question lacks the parameters required by the function, also point it out.
You should only return the function call in tools call sections.
You SHOULD NOT include any other text in the response.
".to_string(),
                        )),
                    }),
                },
                ChatMessage {
                    role: ChatRole::User as i32,
                    content: Some(MessageContent {
                        content: Some(message_content::Content::Text(
                            "Find me the sales growth rate for company XYZ for the last 3 years and also the interest coverage ratio for the same duration.".to_string(),
                        )),
                    }),
                },
            ],
            options: Some(LlmOptions {
                temperature: Some(0.1),
                ..Default::default()
            }),
            function_options: Some(FunctionOptions {
                use_function_calling: true,
                ..Default::default()
            }),
            ..Default::default()
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
