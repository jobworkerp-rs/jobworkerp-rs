use crate::llm::generic_tracing_helper::GenericLLMTracingHelper;
use crate::llm::mistral::core::MistralCoreService;
use crate::llm::mistral::tracing::MistralTracingService;
use crate::llm::mistral::{
    MistralLlmServiceImpl, MistralRSMessage, MistralRSToolCall, SerializableChatResponse,
    ToolCallingConfig, ToolExecutionError,
};
use crate::llm::tracing::mistral_helper::MistralTracingHelper;
use anyhow::Result;
use app::app::function::{FunctionApp, FunctionAppImpl, UseFunctionApp};
use async_stream::stream;
use futures::future;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole as TextMessageRole;
use jobworkerp_runner::jobworkerp::runner::llm::{
    llm_runner_settings::LocalRunnerSettings, LlmChatArgs, LlmChatResult,
};
use mistralrs::{RequestBuilder, Tool};
use net_utils::trace::attr::{OtelSpanAttributes, OtelSpanBuilder, OtelSpanType};
use net_utils::trace::impls::GenericOtelClient;
use net_utils::trace::otel_span::GenAIOtelClient;
use opentelemetry::Context;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use tokio::time::{timeout, Duration};

/// Phase 2: MistralRSToolCallingService - Ollamaパターン採用
#[derive(Clone)]
pub struct MistralRSToolCallingService {
    pub core_service: Arc<MistralLlmServiceImpl>, // 具体的型を直接使用
    pub function_app: Arc<FunctionAppImpl>,       // Tool実行機能（必須）
    pub otel_client: Option<Arc<GenericOtelClient>>, // トレーシング
    pub config: ToolCallingConfig,                // Tool calling設定
}

impl MistralRSToolCallingService {
    /// コンストラクター（Composition pattern）
    pub async fn new_with_function_app(
        settings: LocalRunnerSettings,
        function_app: Arc<FunctionAppImpl>,
    ) -> Result<Self> {
        let core_service = Arc::new(MistralLlmServiceImpl::new(&settings).await?);

        Ok(Self {
            core_service,
            function_app,
            otel_client: Some(Arc::new(GenericOtelClient::new(
                "mistralrs.tool_calling_service",
            ))),
            config: ToolCallingConfig::default(),
        })
    }

    /// メインエントリーポイント - Ollamaパターン準拠 (Phase 3: トレーシング統合)
    pub async fn request_chat(
        &self,
        args: LlmChatArgs,
        cx: Context,
        metadata: HashMap<String, String>,
    ) -> Result<LlmChatResult> {
        // Phase 3: メインリクエストレベルのトレーシング
        let span_name = "mistral_chat_request";
        let mut main_span_builder = OtelSpanBuilder::new(span_name)
            .span_type(OtelSpanType::Span)
            .level("INFO");

        // メタデータ情報を追加
        let mut span_metadata = std::collections::HashMap::new();
        span_metadata.insert(
            "mistral.request.messages.count".to_string(),
            serde_json::Value::Number(serde_json::Number::from(args.messages.len())),
        );
        span_metadata.insert(
            "mistral.request.has_function_calling".to_string(),
            serde_json::Value::Bool(
                args.function_options
                    .as_ref()
                    .map_or(false, |opts| opts.use_function_calling),
            ),
        );

        // メタデータから重要な属性を抽出
        if let Some(job_id) = metadata.get("job_id") {
            main_span_builder = main_span_builder.session_id(job_id.clone());
        }
        if let Some(user_id) = metadata.get("user_id") {
            main_span_builder = main_span_builder.user_id(user_id.clone());
        }

        let main_attributes = main_span_builder.metadata(span_metadata).build();

        // 必要なデータを事前に処理（selfの借用エラーを回避）
        let messages = self.convert_proto_messages(&args).map_err(|e| {
            JobWorkerError::OtherError(format!("Convert proto messages failed: {}", e))
        })?;

        let tools = if let Some(function_opts) = &args.function_options {
            tracing::debug!(
                "Function options found: use_function_calling={}",
                function_opts.use_function_calling
            );
            if function_opts.use_function_calling {
                tracing::debug!(
                    "Creating tools from function options: function_set_name={:?}",
                    function_opts.function_set_name
                );
                let created_tools = self
                    .create_tools_from_options(function_opts)
                    .await
                    .map_err(|e| {
                        JobWorkerError::OtherError(format!("Create tools failed: {}", e))
                    })?;
                tracing::debug!("Created {} tools successfully", created_tools.len());
                Arc::new(created_tools)
            } else {
                tracing::info!("Function calling disabled in options");
                Arc::new(vec![])
            }
        } else {
            tracing::debug!("No function options provided");
            Arc::new(vec![])
        };

        // OpenTelemetryコンテキストのクローンを事前に作成
        let cx_clone = cx.clone();
        let self_arc = Arc::new(self.clone());
        let self_arc_clone = Arc::clone(&self_arc);
        let main_action = async move {
            // 再帰的tool calling処理開始（Arcで包む）
            let final_response = self_arc
                .request_chat_internal_with_tracing(messages, tools, Some(cx), Arc::new(metadata))
                .await
                .map_err(|e| JobWorkerError::OtherError(format!("Internal chat failed: {}", e)))?;

            // 最終結果をprotobuf形式に変換
            Ok(self_arc_clone.convert_mistral_response_to_final_result(&final_response))
        };

        // OpenTelemetryスパンで実行（wrapper構造体使用）
        let (result, _) = self
            .execute_with_span(span_name, main_attributes, Some(cx_clone), main_action)
            .await?;
        Ok(result)
    }

    /// ストリーミング版のチャット処理 - Tool calling戦略1採用
    pub async fn request_stream_chat(
        &self,
        args: LlmChatArgs,
    ) -> Result<futures::stream::BoxStream<'static, LlmChatResult>> {
        use futures::stream::{self, StreamExt};

        // Tool callingが必要かチェック
        let has_tools = args
            .function_options
            .as_ref()
            .map_or(false, |opts| opts.use_function_calling);

        if !has_tools {
            // Tool callingなし - 直接ストリーミング
            return Arc::new(self.clone())
                .request_direct_stream_chat(args)
                .await;
        }

        // Tool callingあり - 戦略1: Non-streaming tool calling + 最終結果streaming
        tracing::debug!("Tool calling detected, using non-streaming execution + final streaming");

        // 1. Tool callingを非ストリーミングで完全実行
        let final_result = self
            .request_chat(
                args,
                opentelemetry::Context::current(),
                std::collections::HashMap::new(),
            )
            .await?;

        // 2. 最終結果を単一streamアイテムとして返却
        let result_stream = stream::once(async move { final_result });
        Ok(result_stream.boxed())
    }

    /// Tool callingなしの直接ストリーミング処理
    async fn request_direct_stream_chat(
        self: Arc<Self>,
        args: LlmChatArgs,
    ) -> Result<futures::stream::BoxStream<'static, LlmChatResult>> {
        use async_stream::stream;
        use futures::stream::StreamExt;

        // プロトコルメッセージ変換
        let messages = self.convert_proto_messages(&args)?;
        let request_builder = self.build_request_from_messages(&messages, &[]).await?;

        // MistralCoreServiceのstream_chatを使用
        let mistral_stream: futures::stream::BoxStream<'static, mistralrs::Response> =
            self.core_service.stream_chat(request_builder).await?;

        // MistralRS Response -> LlmChatResult変換ストリーム
        let result_stream = stream! {
            tokio::pin!(mistral_stream);
            while let Some(response) = mistral_stream.next().await {
                match response {
                    mistralrs::Response::Chunk(chunk) => {
                        use crate::llm::mistral::result::{DefaultLLMResultConverter, LLMResultConverter};
                        let result =DefaultLLMResultConverter::convert_chat_completion_chunk_result(&chunk);
                        // let result = self.convert_mistral_chunk_to_result(&chunk);
                        yield result;
                    }
                    mistralrs::Response::Done(completion) => {
                        let result = self.convert_mistral_response_to_final_result(&completion);
                        yield result;
                        break; // Done response で終了
                    }
                    _ => {
                        // その他のレスポンスタイプ（必要に応じて処理を追加）
                        tracing::debug!("Received other response type in stream");
                    }
                }
            }
        };

        Ok(result_stream.boxed())
    }

    /// 再帰的tool calling処理（関数型アプローチ）
    async fn request_chat_internal_with_tracing(
        self: Arc<Self>,
        messages: Vec<MistralRSMessage>, // イミュータブル管理
        tools: Arc<Vec<Tool>>,
        parent_context: Option<Context>,
        metadata: Arc<HashMap<String, String>>,
    ) -> Result<mistralrs::ChatCompletionResponse> {
        self.request_chat_internal_with_iteration_count(
            messages,
            tools,
            parent_context,
            metadata,
            0,
        )
        .await
    }

    /// 反復回数制御付きの内部実装（Phase 3: 階層化トレーシング統合）
    async fn request_chat_internal_with_iteration_count(
        self: Arc<Self>,
        mut messages: Vec<MistralRSMessage>,
        tools: Arc<Vec<Tool>>,
        parent_context: Option<Context>,
        metadata: Arc<HashMap<String, String>>,
        iteration_count: usize,
    ) -> Result<mistralrs::ChatCompletionResponse> {
        // 最大反復チェック
        if iteration_count >= self.config.max_iterations {
            tracing::error!(
                "Maximum tool calling iterations ({}) exceeded",
                self.config.max_iterations
            );
            return Err(anyhow::Error::from(
                ToolExecutionError::MaxIterationsExceeded {
                    max_iterations: self.config.max_iterations,
                },
            ));
        }

        tracing::info!(
            "Tool calling iteration {}/{}",
            iteration_count + 1,
            self.config.max_iterations
        );

        // // 同じツール呼び出しの繰り返しを検出
        // if iteration_count > 0 {
        //     // 直前のメッセージでエラーが発生している場合、同じツールの繰り返しを避ける
        //     if let Some(last_msg) = messages.last() {
        //         if last_msg.content.contains("Error:") || last_msg.content.contains("Failed") {
        //             tracing::warn!("Previous tool call failed, iteration: {}", iteration_count);
        //             if iteration_count >= 2 { // 2回失敗したら停止
        //                 tracing::error!("Tool calling failed multiple times, stopping to prevent infinite loop");
        //                 return Err(anyhow::Error::from(
        //                     ToolExecutionError::MaxIterationsExceeded {
        //                         max_iterations: iteration_count,
        //                     },
        //                 ));
        //             }
        //         }
        //     }
        // }

        // デバッグ: 会話履歴をログ出力
        tracing::debug!("Building request with {} messages:", messages.len());
        for (i, msg) in messages.iter().enumerate() {
            tracing::debug!(
                "Message {}: {:?} - '{}' (tool_call_id: {:?}, tool_calls: {})",
                i,
                msg.role,
                if msg.content.len() > 100 {
                    &msg.content[..100]
                } else {
                    &msg.content
                },
                msg.tool_call_id,
                msg.tool_calls.as_ref().map_or(0, |tc| tc.len())
            );
        }

        // Phase 3: 階層化トレーシング - tool calling iteration span
        let iteration_attributes = self.create_tool_calling_iteration_attributes(
            iteration_count,
            messages.len(),
            tools.len(),
            &metadata,
        );

        // APIリクエストを1回だけ実行して結果をキャッシュ
        let request_builder = self.build_request_from_messages(&messages, &tools).await?;
        let response = self.core_service.request_chat(request_builder).await?;

        // トレーシング用のwrapper作成
        let serializable_response = SerializableChatResponse::from(&response);
        let iteration_action =
            async move { Ok::<SerializableChatResponse, JobWorkerError>(serializable_response) };

        let (_serializable_response, current_context) = self
            .execute_with_span(
                &format!("mistral_tool_calling_iteration_{}", iteration_count + 1),
                iteration_attributes,
                parent_context.clone(),
                iteration_action,
            )
            .await?;

        // MistralRS APIから直接tool calls抽出
        let tool_calls = self.extract_tool_calls_from_response(&response)?;

        // debug output
        if let Some(first_choice) = response.choices.first() {
            if let Some(content) = &first_choice.message.content {
                tracing::debug!("Response content: {}", content);
            }
            tracing::debug!("Response finish_reason: {}", first_choice.finish_reason);
        }

        if tool_calls.is_empty() {
            tracing::debug!("No tool calls found, returning final response");
            Ok(response)
        } else {
            tracing::debug!("Processing {} tool calls", tool_calls.len());
            for (i, call) in tool_calls.iter().enumerate() {
                tracing::debug!(
                    "Tool call {}: {} with args: {}",
                    i,
                    call.function_name,
                    call.arguments
                );
            }

            // // Assistant messageをtool callsと共に会話履歴に追加
            // let assistant_message =
            //     self.create_assistant_message_with_tool_calls(&response, &tool_calls)?;
            // messages.push(assistant_message);

            // 並列Tool実行 (Phase 3: 階層化トレーシング統合)
            let tool_results = if self.config.parallel_execution {
                self.execute_tool_calls_parallel_with_tracing(
                    &tool_calls,
                    metadata.clone(),
                    Some(current_context.clone()),
                )
                .await?
            } else {
                self.execute_tool_calls_sequential_with_tracing(
                    &tool_calls,
                    metadata.clone(),
                    Some(current_context.clone()),
                )
                .await?
            };

            // 🔧 修正2: Tool結果メッセージを順序保持で追加（extend）
            messages.extend(tool_results);

            // 再帰呼び出し (Phase 3: Context継承)
            Box::pin(self.request_chat_internal_with_iteration_count(
                messages, // 更新されたメッセージを渡す
                tools,
                parent_context, // parent contextを継承
                metadata,
                iteration_count + 1, // 反復回数をインクリメント
            ))
            .await
        }
    }

    // /// Assistant messageとtool calls変換
    // fn create_assistant_message_with_tool_calls(
    //     &self,
    //     response: &mistralrs::ChatCompletionResponse,
    //     tool_calls: &[MistralRSToolCall],
    // ) -> Result<MistralRSMessage> {
    //     // Assistant messageの内容（通常は空文字列またはreasoning）
    //     let content = response
    //         .choices
    //         .first()
    //         .and_then(|choice| choice.message.content.as_ref())
    //         .cloned()
    //         .unwrap_or_default();

    //     Ok(MistralRSMessage {
    //         role: TextMessageRole::Assistant,
    //         content,
    //         tool_call_id: None, // AssistantメッセージはtoolCallIdなし
    //         tool_calls: Some(tool_calls.to_vec()),
    //     })
    // }

    /// 並列Tool実行（spawn + join pattern） - Phase 3: トレーシング統合版
    async fn execute_tool_calls_parallel_with_tracing(
        &self,
        tool_calls: &[MistralRSToolCall],
        metadata: Arc<HashMap<String, String>>,
        parent_context: Option<Context>,
    ) -> Result<Vec<MistralRSMessage>> {
        if tool_calls.is_empty() {
            return Ok(vec![]);
        }

        let span_name = "mistral_parallel_tool_execution";
        let span_builder = OtelSpanBuilder::new(span_name)
            .span_type(OtelSpanType::Span)
            .level("INFO");

        let mut span_metadata = std::collections::HashMap::new();
        span_metadata.insert(
            "tool.execution.mode".to_string(),
            serde_json::Value::String("parallel".to_string()),
        );
        span_metadata.insert(
            "tool.calls.count".to_string(),
            serde_json::Value::Number(serde_json::Number::from(tool_calls.len())),
        );

        let attributes = span_builder.metadata(span_metadata).build();

        // parent_contextをクローンしてmove semanticsに備える
        let parent_context_clone = parent_context.clone();
        let tool_calls_vec = tool_calls.to_vec(); // 借用回避のためベクタークローン
                                                  // selfの必要なフィールドを事前にクローン
        let function_app = self.function_app.clone();
        let config = self.config.clone();
        let otel_client = self.otel_client.clone();
        let parallel_action = async move {
            // 全tool callを並列実行
            let handles: Vec<_> = tool_calls_vec
                .iter()
                .enumerate()
                .map(|(idx, call)| {
                    let function_app = function_app.clone();
                    let metadata = metadata.clone();
                    let call = call.clone();
                    let config = config.clone();
                    let otel_client = otel_client.clone();
                    let parent_ctx = parent_context_clone.clone();

                    tokio::spawn(async move {
                        Self::execute_single_tool_call_with_enhanced_tracing(
                            call,
                            function_app,
                            metadata,
                            config,
                            otel_client,
                            parent_ctx,
                            idx,
                        )
                        .await
                    })
                })
                .collect();

            // 全タスクの完了を待つ
            let results = future::join_all(handles).await;

            // 結果を順序保持でまとめる（正しいtool_call_idを使用）
            let mut messages = Vec::new();
            for (i, result) in results.into_iter().enumerate() {
                let tool_call_id = tool_calls_vec
                    .get(i)
                    .map(|call| call.id.clone())
                    .unwrap_or_else(|| format!("unknown_{}", i));

                match result {
                    Ok(message) => {
                        tracing::debug!("Tool call {} succeeded", i);
                        messages.push(message);
                    }
                    Err(join_error) => {
                        tracing::error!("Tool call {} task failed: {}", i, join_error);
                        messages.push(MistralRSMessage {
                            role: TextMessageRole::Tool,
                            content: format!("Task execution failed: {}", join_error),
                            tool_call_id: Some(tool_call_id),
                            tool_calls: None,
                        });
                    }
                }
            }

            Ok::<Vec<MistralRSMessage>, JobWorkerError>(messages)
        };

        let (result, _) = self
            .execute_with_span(span_name, attributes, parent_context, parallel_action)
            .await?;
        Ok(result)
    }

    /// 順次Tool実行 - Phase 3: トレーシング統合版
    async fn execute_tool_calls_sequential_with_tracing(
        &self,
        tool_calls: &[MistralRSToolCall],
        metadata: Arc<HashMap<String, String>>,
        parent_context: Option<Context>,
    ) -> Result<Vec<MistralRSMessage>> {
        if tool_calls.is_empty() {
            return Ok(vec![]);
        }

        let span_name = "mistral_sequential_tool_execution";
        let span_builder = OtelSpanBuilder::new(span_name)
            .span_type(OtelSpanType::Span)
            .level("INFO");

        let mut span_metadata = std::collections::HashMap::new();
        span_metadata.insert(
            "tool.execution.mode".to_string(),
            serde_json::Value::String("sequential".to_string()),
        );
        span_metadata.insert(
            "tool.calls.count".to_string(),
            serde_json::Value::Number(serde_json::Number::from(tool_calls.len())),
        );

        let attributes = span_builder.metadata(span_metadata).build();

        // parent_contextをクローンしてmove semanticsに備える
        let parent_context_clone = parent_context.clone();
        let tool_calls_vec = tool_calls.to_vec(); // 借用回避のためベクタークローン
                                                  // selfの必要なフィールドを事前にクローン
        let function_app = self.function_app.clone();
        let config = self.config.clone();
        let otel_client = self.otel_client.clone();
        let sequential_action = async move {
            let mut messages = Vec::new();

            for (idx, call) in tool_calls_vec.iter().enumerate() {
                let res = Self::execute_single_tool_call_with_enhanced_tracing(
                    call.clone(),
                    function_app.clone(),
                    metadata.clone(),
                    config.clone(),
                    otel_client.clone(),
                    parent_context_clone.clone(),
                    idx,
                )
                .await;
                messages.push(res);
            }

            Ok::<Vec<MistralRSMessage>, JobWorkerError>(messages)
        };

        let (result, _) = self
            .execute_with_span(span_name, attributes, parent_context, sequential_action)
            .await?;
        Ok(result)
    }

    /// 個別Tool実行（Phase 3: 強化トレーシング統合）
    async fn execute_single_tool_call_with_enhanced_tracing(
        call: MistralRSToolCall,
        function_app: Arc<FunctionAppImpl>,
        metadata: Arc<HashMap<String, String>>,
        config: ToolCallingConfig,
        otel_client: Option<Arc<GenericOtelClient>>,
        parent_context: Option<Context>,
        call_index: usize,
    ) -> MistralRSMessage {
        // OpenTelemetryトレーシング統合
        if let Some(client) = &otel_client {
            let mut span_builder =
                OtelSpanBuilder::new(&format!("tool_execution_{}", call.function_name))
                    .span_type(OtelSpanType::Span)
                    .level("INFO");

            let mut span_metadata = std::collections::HashMap::new();
            span_metadata.insert(
                "tool.call.id".to_string(),
                serde_json::Value::String(call.id.clone()),
            );
            span_metadata.insert(
                "tool.function.name".to_string(),
                serde_json::Value::String(call.function_name.clone()),
            );
            span_metadata.insert(
                "tool.arguments.length".to_string(),
                serde_json::Value::Number(serde_json::Number::from(call.arguments.len())),
            );
            span_metadata.insert(
                "tool.call.index".to_string(),
                serde_json::Value::Number(serde_json::Number::from(call_index)),
            );

            // メタデータから重要な属性を抽出
            if let Some(job_id) = metadata.get("job_id") {
                span_builder = span_builder.session_id(job_id.clone());
            }

            let attributes = span_builder.metadata(span_metadata).build();

            let parent_ctx = parent_context.unwrap_or_else(opentelemetry::Context::current);
            let _span_name = format!("tool_execution_{}", call.function_name);

            let tool_action = async move {
                let result =
                    Self::execute_tool_call_core(call, function_app, metadata, config).await;
                Ok::<MistralRSMessage, JobWorkerError>(result)
            };

            // span内でtool実行
            match client
                .with_span_result(attributes, Some(parent_ctx), tool_action)
                .await
            {
                Ok(result) => result,
                Err(e) => {
                    tracing::error!("Tool execution failed with error: {}", e);
                    MistralRSMessage {
                        role: TextMessageRole::Tool,
                        content: format!("Tool execution failed: {}", e),
                        tool_call_id: Some(call_index.to_string()),
                        tool_calls: None,
                    }
                }
            }
        } else {
            // トレーシングなしの従来実行
            Self::execute_tool_call_core(call, function_app, metadata, config).await
        }
    }

    /// コアのtool実行ロジック（トレーシング分離）
    async fn execute_tool_call_core(
        call: MistralRSToolCall,
        function_app: Arc<FunctionAppImpl>,
        metadata: Arc<HashMap<String, String>>,
        config: ToolCallingConfig,
    ) -> MistralRSMessage {
        // 引数パース
        let arguments_obj = serde_json::from_str(&call.arguments).map_err(|e| {
            ToolExecutionError::InvalidArguments {
                reason: format!("JSON parse error: {}", e),
            }
        });
        if let Err(e) = arguments_obj {
            tracing::warn!(
                "Invalid arguments for tool call '{}': {}",
                call.function_name,
                e
            );
            return MistralRSMessage {
                role: TextMessageRole::Tool,
                content: format!(
                    "Error parsing arguments for tool '{}': {}",
                    call.function_name, e
                ),
                tool_call_id: Some(call.id),
                tool_calls: None,
            };
        } else {
            tracing::debug!(
                "Parsed arguments for tool call '{}': {:?}",
                call.function_name,
                arguments_obj
            );
        }
        let arguments_obj: serde_json::Map<String, serde_json::Value> = arguments_obj.unwrap();

        // タイムアウト付きでtool実行
        let tool_execution = async {
            function_app
                .call_function_for_llm(
                    metadata,
                    &call.function_name,
                    Some(arguments_obj),
                    config.tool_timeout_sec,
                )
                .await
        };

        match timeout(
            Duration::from_secs(config.tool_timeout_sec as u64),
            tool_execution,
        )
        .await
        {
            Ok(Ok(result)) => {
                tracing::debug!("Tool call executed successfully: {}", call.function_name);
                MistralRSMessage {
                    role: TextMessageRole::Tool,
                    content: result.to_string(),
                    tool_call_id: Some(call.id),
                    tool_calls: None,
                }
            }
            Ok(Err(e)) => {
                tracing::warn!("Tool call failed: {}", e);
                MistralRSMessage {
                    role: TextMessageRole::Tool,
                    content: format!("Error executing tool '{}': {}", call.function_name, e),
                    tool_call_id: Some(call.id),
                    tool_calls: None,
                }
            }
            Err(_) => {
                tracing::warn!(
                    "Tool execution timed out after {} seconds",
                    config.tool_timeout_sec
                );
                MistralRSMessage {
                    role: TextMessageRole::Tool,
                    content: format!(
                        "Tool execution timed out after {} seconds",
                        config.tool_timeout_sec
                    ),
                    tool_call_id: Some(call.id),
                    tool_calls: None,
                }
            }
        }
    }

    /// Phase 3: 階層化tracing用のspan attributes作成
    fn create_tool_calling_iteration_attributes(
        &self,
        iteration_count: usize,
        messages_count: usize,
        tools_count: usize,
        metadata: &HashMap<String, String>,
    ) -> OtelSpanAttributes {
        let mut span_builder = OtelSpanBuilder::new(&format!(
            "mistral_tool_calling_iteration_{}",
            iteration_count + 1
        ))
        .span_type(OtelSpanType::Span)
        .level("INFO");

        let mut span_metadata = std::collections::HashMap::new();
        span_metadata.insert(
            "mistral.tool_calling.iteration".to_string(),
            serde_json::Value::Number(serde_json::Number::from(iteration_count)),
        );
        span_metadata.insert(
            "mistral.messages.count".to_string(),
            serde_json::Value::Number(serde_json::Number::from(messages_count)),
        );
        span_metadata.insert(
            "mistral.tools.count".to_string(),
            serde_json::Value::Number(serde_json::Number::from(tools_count)),
        );

        // メタデータから重要な属性を抽出
        if let Some(job_id) = metadata.get("job_id") {
            span_builder = span_builder.session_id(job_id.clone());
        }
        if let Some(user_id) = metadata.get("user_id") {
            span_builder = span_builder.user_id(user_id.clone());
        }

        span_builder.metadata(span_metadata).build()
    }

    fn create_tool_execution_attributes(
        &self,
        tool_call: &MistralRSToolCall,
        metadata: &HashMap<String, String>,
    ) -> OtelSpanAttributes {
        let mut span_builder =
            OtelSpanBuilder::new(&format!("tool_execution_{}", tool_call.function_name))
                .span_type(OtelSpanType::Span)
                .level("INFO");

        let mut span_metadata = std::collections::HashMap::new();
        span_metadata.insert(
            "tool.call.id".to_string(),
            serde_json::Value::String(tool_call.id.clone()),
        );
        span_metadata.insert(
            "tool.function.name".to_string(),
            serde_json::Value::String(tool_call.function_name.clone()),
        );
        span_metadata.insert(
            "tool.arguments.length".to_string(),
            serde_json::Value::Number(serde_json::Number::from(tool_call.arguments.len())),
        );

        // メタデータから重要な属性を抽出
        if let Some(job_id) = metadata.get("job_id") {
            span_builder = span_builder.session_id(job_id.clone());
        }

        span_builder.metadata(span_metadata).build()
    }

    // Helper methods that delegate to existing implementations
    fn convert_proto_messages(&self, args: &LlmChatArgs) -> Result<Vec<MistralRSMessage>> {
        // Direct access to MistralLlmServiceImpl
        self.core_service.convert_proto_messages(args)
    }

    fn extract_tool_calls_from_response(
        &self,
        response: &mistralrs::ChatCompletionResponse,
    ) -> Result<Vec<MistralRSToolCall>> {
        // Direct access to MistralLlmServiceImpl
        self.core_service.extract_tool_calls_from_response(response)
    }

    async fn create_tools_from_options(
        &self,
        function_opts: &jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::FunctionOptions,
    ) -> Result<Vec<Tool>> {
        use crate::llm::mistral::args::LLMRequestConverter;
        LLMRequestConverter::create_tools_from_options(self, function_opts).await
    }

    async fn build_request_from_messages(
        &self,
        messages: &[MistralRSMessage],
        tools: &[Tool],
    ) -> Result<RequestBuilder> {
        use mistralrs::{TextMessageRole as MistralTextRole, ToolChoice};
        let mut builder = RequestBuilder::new();

        // MistralRSMessage → MistralRS RequestBuilder変換
        for msg in messages {
            match msg.role {
                TextMessageRole::Tool => {
                    // Tool messageは結果として追加
                    if let Some(tool_call_id) = &msg.tool_call_id {
                        tracing::debug!(
                            "Adding tool message: '{}' with call_id: {}",
                            msg.content,
                            tool_call_id
                        );
                        builder =
                            builder.add_tool_message(msg.content.clone(), tool_call_id.clone());
                    } else {
                        tracing::warn!(
                            "Tool message without tool_call_id, adding as regular message"
                        );
                        let role = match msg.role {
                            TextMessageRole::User => MistralTextRole::User,
                            TextMessageRole::Assistant => MistralTextRole::Assistant,
                            TextMessageRole::System => MistralTextRole::System,
                            TextMessageRole::Tool => MistralTextRole::Tool,
                            _ => MistralTextRole::User,
                        };
                        builder = builder.add_message(role, &msg.content);
                    }
                }
                TextMessageRole::Assistant => {
                    // Assistant messageにtool callsが含まれている場合の特別処理
                    if let Some(tool_calls) = &msg.tool_calls {
                        if !tool_calls.is_empty() {
                            tracing::debug!(
                                "Adding Assistant message with {} tool calls: '{}'",
                                tool_calls.len(),
                                msg.content
                            );
                        } else {
                            tracing::debug!("Adding Assistant message: '{}'", msg.content);
                        }
                        builder = builder.add_message(MistralTextRole::Assistant, &msg.content);
                    } else {
                        tracing::debug!("Adding Assistant message: '{}'", msg.content);
                        builder = builder.add_message(MistralTextRole::Assistant, &msg.content);
                    }
                }
                _ => {
                    let role = match msg.role {
                        TextMessageRole::User => MistralTextRole::User,
                        TextMessageRole::System => MistralTextRole::System,
                        _ => MistralTextRole::User,
                    };
                    builder = builder.add_message(role, &msg.content);
                }
            }
        }

        // Tools追加
        if !tools.is_empty() {
            tracing::debug!("Adding {} tools to MistralRS request", tools.len());
            for (i, tool) in tools.iter().enumerate() {
                tracing::debug!(
                    "Tool {}: {} - {}\n schema: {:#?}",
                    i,
                    tool.function.name,
                    tool.function
                        .description
                        .as_deref()
                        .unwrap_or("No description"),
                    tool.function.parameters.as_ref()
                );
            }
            builder = builder.set_tools(tools.to_vec());
            builder = builder.set_tool_choice(ToolChoice::Auto);
        } else {
            tracing::debug!("No tools provided to MistralRS request");
        }

        Ok(builder)
    }

    fn convert_mistral_response_to_final_result(
        &self,
        response: &mistralrs::ChatCompletionResponse,
    ) -> LlmChatResult {
        use crate::llm::mistral::result::{DefaultLLMResultConverter, LLMResultConverter};
        DefaultLLMResultConverter::convert_chat_completion_result(response)
    }
}

impl UseFunctionApp for MistralRSToolCallingService {
    fn function_app(&self) -> &FunctionAppImpl {
        &self.function_app
    }
}

// Phase 3: MistralTracingService完全実装
impl MistralTracingService for MistralRSToolCallingService {
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>> {
        self.otel_client.as_ref()
    }

    async fn execute_with_tracing<F, T>(&self, action: F, context: Option<Context>) -> Result<T>
    where
        F: Future<Output = Result<T, anyhow::Error>> + Send,
        T: Send,
    {
        // 簡易実装：OpenTelemetryトレーシングはexecute_with_spanで処理
        let _ = context; // contextは他のメソッドで使用
        action.await
    }
}

// GenericLLMTracingHelperトレイトの実装
impl GenericLLMTracingHelper for MistralRSToolCallingService {
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>> {
        self.otel_client.as_ref()
    }

    fn convert_messages_to_input(
        &self,
        messages: &[impl crate::llm::generic_tracing_helper::LLMMessage],
    ) -> serde_json::Value {
        serde_json::json!(messages
            .iter()
            .map(|m| serde_json::json!({
                "role": m.get_role(),
                "content": m.get_content()
            }))
            .collect::<Vec<_>>())
    }

    fn get_provider_name(&self) -> &str {
        "mistralrs"
    }
}

// MistralTracingHelperトレイトの実装
impl MistralTracingHelper for MistralRSToolCallingService {}

// LLMRequestConverterトレイトの実装
impl crate::llm::mistral::args::LLMRequestConverter for MistralRSToolCallingService {}
