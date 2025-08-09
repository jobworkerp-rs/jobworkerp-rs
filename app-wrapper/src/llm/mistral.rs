pub mod args;
pub mod core;
pub mod model;
pub mod result;
pub mod tracing;

pub use self::args::LLMRequestConverter;
pub use self::core::MistralCoreService;
use self::model::MistralModelLoader;
use anyhow::Result;
// DefaultLLMRequestConverter removed - LLMRequestConverter used as mixin
// async_stream::stream removed as it's not used
use futures::stream::StreamExt;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::llm::{
    llm_runner_settings::LocalRunnerSettings, LlmChatArgs,
};
use mistralrs::Model;
pub use result::{DefaultLLMResultConverter, LLMResultConverter};
use std::sync::Arc;

// Phase 1: 新規型定義（設計書211-237行）
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole as TextMessageRole;

/// MistralRS用のメッセージ型
#[derive(Debug, Clone)]
pub struct MistralRSMessage {
    pub role: TextMessageRole,
    pub content: String,
    pub tool_call_id: Option<String>, // Tool messageで必要
}

/// MistralRS用のツール呼び出し型
#[derive(Debug, Clone)]
pub struct MistralRSToolCall {
    pub id: String,
    pub function_name: String,
    pub arguments: String, // JSON文字列
}

/// ツール実行エラーハンドリング強化（段階的移行用）
#[derive(Debug, thiserror::Error)]
pub enum ToolExecutionError {
    #[error("Tool function not found: {function_name}")]
    FunctionNotFound { function_name: String },
    #[error("Invalid tool arguments: {reason}")]
    InvalidArguments { reason: String },
    #[error("Tool execution timeout: {function_name} (timeout: {timeout_sec}s)")]
    Timeout { function_name: String, timeout_sec: u32 },
    #[error("Tool execution failed: {function_name} - {source}")]
    ExecutionFailed { function_name: String, source: anyhow::Error },
}


pub struct MistralLlmServiceImpl {
    // Model name for identification
    model_name: String,
    
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
        // Clone the model to avoid lifetime issues
        let model = self.model.clone();
        
        // Use channel-based streaming to handle lifetime issues
        let (tx, rx) = futures::channel::mpsc::unbounded();
        
        // Spawn task to handle MistralRS streaming
        tokio::spawn(async move {
            let result = async {
                let mut stream = model.stream_chat_request(request_builder).await?;
                
                loop {
                    match stream.next().await {
                        Some(response) => {
                            if tx.unbounded_send(response).is_err() {
                                // Receiver dropped, stop streaming
                                break;
                            }
                        },
                        None => break,
                    }
                }
                
                anyhow::Result::<()>::Ok(())
            }.await;
            
            if let Err(e) = result {
                ::tracing::error!("Error in MistralRS stream: {}", e);
            }
        });
        
        // Convert receiver to BoxStream
        Ok(rx.boxed())
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
                let model_name = text_model.model_name_or_path.clone();
                Ok(Self {
                    model,
                    model_name,
                })
            }
            Some(jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::local_runner_settings::ModelSettings::GgufModel(
                gguf_model,
            )) => {
                let model_name = gguf_model.model_name_or_path.clone();
                Ok(Self {
                    model,
                    model_name,
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

}

// Phase 1: 変換ロジック実装（設計書241-308行）
impl MistralLlmServiceImpl {
    /// プロトコルメッセージをMistralRS形式に変換
    pub fn convert_proto_messages(&self, args: &LlmChatArgs) -> Result<Vec<MistralRSMessage>> {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{ChatRole, message_content};
        
        args.messages.iter().map(|msg| {
            let role = match ChatRole::try_from(msg.role)? {
                ChatRole::User => TextMessageRole::User,
                ChatRole::Assistant => TextMessageRole::Assistant,
                ChatRole::System => TextMessageRole::System,
                ChatRole::Tool => TextMessageRole::Tool,
                ChatRole::Unspecified => TextMessageRole::User,
            };
            
            match &msg.content {
                Some(content) => match &content.content {
                    Some(message_content::Content::Text(text)) => Ok(MistralRSMessage {
                        role,
                        content: text.clone(),
                        tool_call_id: None,
                    }),
                    Some(message_content::Content::ToolCalls(_tool_calls)) => {
                        // Assistant role with tool calls - content is typically empty
                        Ok(MistralRSMessage {
                            role: TextMessageRole::Assistant,
                            content: String::new(),
                            tool_call_id: None,
                        })
                    },
                    _ => Ok(MistralRSMessage {
                        role,
                        content: String::new(),
                        tool_call_id: None,
                    }),
                },
                None => Ok(MistralRSMessage {
                    role,
                    content: String::new(),
                    tool_call_id: None,
                }),
            }
        }).collect()
    }

    /// MistralRS APIからTool calls抽出
    pub fn extract_tool_calls_from_response(&self, response: &mistralrs::ChatCompletionResponse) -> Result<Vec<MistralRSToolCall>> {
        if let Some(first_choice) = response.choices.first() {
            if let Some(tool_calls) = &first_choice.message.tool_calls {
                return Ok(tool_calls.iter().map(|tc| MistralRSToolCall {
                    id: tc.id.clone(),
                    function_name: tc.function.name.clone(),
                    arguments: tc.function.arguments.clone(),
                }).collect());
            }
        }
        Ok(vec![])
    }

    /// ストリーミング用Tool calls抽出
    pub fn extract_tool_calls_from_chunk_response(&self, response: &mistralrs::ChatCompletionChunkResponse) -> Result<Vec<MistralRSToolCall>> {
        if let Some(first_choice) = response.choices.first() {
            if let Some(tool_calls) = &first_choice.delta.tool_calls {
                return Ok(tool_calls.iter().map(|tc| MistralRSToolCall {
                    id: tc.id.clone(),
                    function_name: tc.function.name.clone(),
                    arguments: tc.function.arguments.clone(),
                }).collect());
            }
        }
        Ok(vec![])
    }
}

// TODO: Re-enable tests after integration completion
// Too heavy to run on the test environment
// cargo test --features test-env -- --ignored
#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use app::app::function::{FunctionAppImpl, UseFunctionApp};
    use app::module::test::create_hybrid_test_app;
    use futures::stream::StreamExt;
    // removed ProstMessageCodec import as it's not used anymore
    use jobworkerp_runner::jobworkerp::runner::llm::{
        llm_runner_settings::LocalRunnerSettings,
        LlmChatArgs, LlmChatResult, LlmCompletionArgs,
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

        // Create function app for converter
        let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

        // Create test service with mixin
        struct TestLLMService {
            function_app: Arc<FunctionAppImpl>,
        }
        impl UseFunctionApp for TestLLMService {
            fn function_app(&self) -> &FunctionAppImpl {
                &self.function_app
            }
        }
        impl LLMRequestConverter for TestLLMService {}
        
        let test_service = TestLLMService { function_app: app_module.function_app.clone() };
        let request_builder = test_service.build_completion_request(&args, false).await?;

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

        // Create function app for converter
        let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

        // Create test service with mixin
        struct TestChatService {
            function_app: Arc<FunctionAppImpl>,
        }
        impl UseFunctionApp for TestChatService {
            fn function_app(&self) -> &FunctionAppImpl {
                &self.function_app
            }
        }
        impl LLMRequestConverter for TestChatService {}
        
        let test_service = TestChatService { function_app: app_module.function_app.clone() };
        let request_builder = test_service.build_request(&args, false).await?;

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

        // Create function app for converter
        let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

        // Create test service with mixin
        struct TestGGUFService {
            function_app: Arc<FunctionAppImpl>,
        }
        impl UseFunctionApp for TestGGUFService {
            fn function_app(&self) -> &FunctionAppImpl {
                &self.function_app
            }
        }
        impl LLMRequestConverter for TestGGUFService {}
        
        let test_service = TestGGUFService { function_app: app_module.function_app.clone() };
        let request_builder = test_service.build_request(&args, false).await?;

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

        // Create function app for converter
        let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

        // Create test service with mixin for streaming
        struct TestLLMStreamService {
            function_app: Arc<FunctionAppImpl>,
        }
        impl UseFunctionApp for TestLLMStreamService {
            fn function_app(&self) -> &FunctionAppImpl {
                &self.function_app
            }
        }
        impl LLMRequestConverter for TestLLMStreamService {}
        
        let test_service = TestLLMStreamService { function_app: app_module.function_app.clone() };
        let request_builder = test_service.build_request(&args, true).await?;

        // Use MistralCoreService stream_chat method
        let stream = service.stream_chat(request_builder).await?;

        let mut stream = stream;
        let mut count = 0;
        let mut output = Vec::new();

        while let Some(response) = stream.next().await {
            // Convert MistralRS response to LlmChatResult based on response type
            let result = match response {
                mistralrs::Response::Chunk(chunk) => {
                    DefaultLLMResultConverter::convert_chat_completion_chunk_result(&chunk)
                }
                mistralrs::Response::Done(completion) => {
                    DefaultLLMResultConverter::convert_chat_completion_result(&completion)
                }
                _ => {
                    // Handle other response types
                    LlmChatResult {
                        content: None,
                        reasoning_content: None,
                        done: true,
                        usage: None,
                    }
                }
            };

            println!("Stream item done: {}", result.done);

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

        // Create function app for converter
        let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

        // Create test service with mixin for GGUF streaming
        struct TestGGUFStreamService {
            function_app: Arc<FunctionAppImpl>,
        }
        impl UseFunctionApp for TestGGUFStreamService {
            fn function_app(&self) -> &FunctionAppImpl {
                &self.function_app
            }
        }
        impl LLMRequestConverter for TestGGUFStreamService {}
        
        let test_service = TestGGUFStreamService { function_app: app_module.function_app.clone() };
        let request_builder = test_service.build_request(&args, true).await?;

        // Use MistralCoreService stream_chat method
        let stream = service.stream_chat(request_builder).await?;

        let mut stream = stream;
        let mut count = 0;
        let mut output = Vec::new();

        while let Some(response) = stream.next().await {
            // Convert MistralRS response to LlmChatResult based on response type
            let result = match response {
                mistralrs::Response::Chunk(chunk) => {
                    DefaultLLMResultConverter::convert_chat_completion_chunk_result(&chunk)
                }
                mistralrs::Response::Done(completion) => {
                    DefaultLLMResultConverter::convert_chat_completion_result(&completion)
                }
                _ => {
                    // Handle other response types
                    LlmChatResult {
                        content: None,
                        reasoning_content: None,
                        done: true,
                        usage: None,
                    }
                }
            };

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

        println!("GGUF stream output: {:?}", output);
        assert!(count > 0, "Expected at least one stream item");

        Ok(())
    }

    #[ignore]
    #[tokio::test]
    async fn test_llm_tool_runner() -> Result<()> {
        // NOTE: このテストは現状失敗する想定（Phase 2でtool calling機能完全実装後に成功予定）
        // Phase 1では基本的なLLM応答確認のみ、実際のtool実行統合は未実装
        
        // Create tool-capable settings
        let settings = create_mistral_tool_settings()?;

        // Create service instance
        let service = MistralLlmServiceImpl::new(&settings).await?;

        // Create tool args with financial data query
        let args = create_tool_args()?;

        // function_app will automatically resolve available tools from function_sets
        println!("Using function_app to resolve tools from function_set: HTTP_REQUEST");

        // Create function app for converter
        let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

        // Create test service with mixin for tool testing
        struct TestToolService {
            function_app: Arc<FunctionAppImpl>,
        }
        impl UseFunctionApp for TestToolService {
            fn function_app(&self) -> &FunctionAppImpl {
                &self.function_app
            }
        }
        impl LLMRequestConverter for TestToolService {}
        
        let test_service = TestToolService { function_app: app_module.function_app.clone() };
        let request_builder = test_service.build_request(&args, false).await?;

        // Send request to LLM with tool schemas
        let response = service.request_chat(request_builder).await?;

        // Convert to result
        let result = DefaultLLMResultConverter::convert_chat_completion_result(&response);

        println!("Tool execution test result: {:#?}", &result);

        // Phase 1: 基本的な応答確認（tool calling統合は未実装なので失敗想定）
        assert!(result.content.is_some(), "Expected some content from LLM");
        
        if let Some(content) = result.content {
            match content.content {
                Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::ToolCalls(tool_calls)) => {
                    // LLMがtool callsを返した場合（Phase 2で実装予定）
                    assert!(!tool_calls.calls.is_empty(), "Expected non-empty tool calls");
                    println!("LLM requested {} tool calls:", tool_calls.calls.len());
                    for (i, call) in tool_calls.calls.iter().enumerate() {
                        println!("  Tool call {}: {} with args: {}", i + 1, call.fn_name, call.fn_arguments);
                    }
                    // TODO: Phase 2でtool実行とレスポンス統合を実装
                    println!("NOTE: Actual tool execution not implemented in Phase 1");
                }
                Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text)) => {
                    // LLMがテキスト応答を返した場合
                    assert!(!text.is_empty(), "Expected non-empty text response");
                    println!("LLM text response: {}", text);
                    println!("NOTE: LLM did not use available tools (expected in Phase 1)");
                }
                _ => {
                    panic!("Expected either tool calls or text content from LLM");
                }
            }
        }

        Ok(())
    }

    #[ignore]
    #[tokio::test]
    async fn test_gguf_llm_tool_runner() -> Result<()> {
        // NOTE: このテストは現状失敗する想定（Phase 2でtool calling機能完全実装後に成功予定）
        // GGUF版でのtool calling統合テスト
        
        // Create GGUF tool-capable settings
        let settings = create_gguf_mistral_tool_settings()?;

        // Create service instance
        let service = MistralLlmServiceImpl::new(&settings).await?;

        // Create tool args with financial data query
        let args = create_tool_args()?;

        // function_app will automatically resolve available tools from function_sets
        println!("Using function_app to resolve tools from function_set: HTTP_REQUEST for GGUF LLM");

        // Create function app for converter
        let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

        // Create test service with mixin for GGUF tool testing
        struct TestGGUFToolService {
            function_app: Arc<FunctionAppImpl>,
        }
        impl UseFunctionApp for TestGGUFToolService {
            fn function_app(&self) -> &FunctionAppImpl {
                &self.function_app
            }
        }
        impl LLMRequestConverter for TestGGUFToolService {}
        
        let test_service = TestGGUFToolService { function_app: app_module.function_app.clone() };
        let request_builder = test_service.build_request(&args, false).await?;

        // Send request to GGUF LLM with tool schemas
        let response = service.request_chat(request_builder).await?;

        // Convert to result
        let result = DefaultLLMResultConverter::convert_chat_completion_result(&response);

        println!("GGUF Tool execution test result: {:#?}", &result);

        // Phase 1: 基本的な応答確認（tool calling統合は未実装なので失敗想定）
        assert!(result.content.is_some(), "Expected some content from GGUF LLM");
        
        if let Some(content) = result.content {
            match content.content {
                Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::ToolCalls(tool_calls)) => {
                    // GGUF LLMがtool callsを返した場合（Phase 2で実装予定）
                    assert!(!tool_calls.calls.is_empty(), "Expected non-empty tool calls from GGUF");
                    println!("GGUF LLM requested {} tool calls:", tool_calls.calls.len());
                    for (i, call) in tool_calls.calls.iter().enumerate() {
                        println!("  GGUF Tool call {}: {} with args: {}", i + 1, call.fn_name, call.fn_arguments);
                    }
                    // TODO: Phase 2でtool実行とレスポンス統合を実装
                    println!("NOTE: Actual tool execution not implemented in Phase 1");
                }
                Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text)) => {
                    // GGUF LLMがテキスト応答を返した場合
                    assert!(!text.is_empty(), "Expected non-empty text response from GGUF");
                    println!("GGUF LLM text response: {}", text);
                    println!("NOTE: GGUF LLM did not use available tools (expected in Phase 1)");
                }
                _ => {
                    panic!("Expected either tool calls or text content from GGUF LLM");
                }
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
                function_set_name: Some("HTTP_REQUEST".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        Ok(args)
    }

    // tool_schemas() function removed - function_app now resolves tools automatically
    // from function_set_name specified in FunctionOptions
}
