use super::super::StreamTaskExecutorTrait;
use super::do_::DoTaskStreamExecutor;
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self, DoTask, Input, TryTaskCatch},
    },
    execute::{
        context::{TaskContext, WorkflowContext, WorkflowStreamEvent},
        expression::UseExpression,
        task::ExecutionId,
    },
};
use app::app::job::execute::JobExecutorWrapper;
use async_stream::stream;
use debug_stub_derive::DebugStub;
use futures::StreamExt;
use std::{collections::HashMap, pin::Pin, sync::Arc, time::Duration};
use tokio::{sync::RwLock, time::Instant};

type CheckPointRepo =
    Arc<dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId>;

#[derive(DebugStub, Clone)]
pub struct TryStreamTaskExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    default_timeout: Duration,
    task: workflow::TryTask,
    #[debug_stub = "JobExecutorWrapper"]
    job_executors: Arc<JobExecutorWrapper>,
    #[debug_stub = "Option<CheckPointRepo>"]
    checkpoint_repository: Option<CheckPointRepo>,
    execution_id: Option<Arc<ExecutionId>>,
    #[debug_stub = "HashMap<String, String>"]
    metadata: Arc<HashMap<String, String>>,
    emit_streaming_data: bool,
}

impl TryStreamTaskExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workflow_context: Arc<RwLock<WorkflowContext>>,
        default_timeout: Duration,
        task: workflow::TryTask,
        job_executors: Arc<JobExecutorWrapper>,
        checkpoint_repository: Option<CheckPointRepo>,
        execution_id: Option<Arc<ExecutionId>>,
        metadata: Arc<HashMap<String, String>>,
        emit_streaming_data: bool,
    ) -> Self {
        Self {
            workflow_context,
            default_timeout,
            task,
            job_executors,
            checkpoint_repository,
            execution_id,
            metadata,
            emit_streaming_data,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_try_stream(
        workflow_context: Arc<RwLock<WorkflowContext>>,
        default_timeout: Duration,
        metadata: Arc<HashMap<String, String>>,
        task: workflow::TryTask,
        job_executors: Arc<JobExecutorWrapper>,
        checkpoint_repository: Option<CheckPointRepo>,
        execution_id: Option<Arc<ExecutionId>>,
        emit_streaming_data: bool,
        cx: Arc<opentelemetry::Context>,
        task_name: Arc<String>,
        task_context: TaskContext,
    ) -> Pin<
        Box<dyn futures::Stream<Item = Result<WorkflowStreamEvent, Box<workflow::Error>>> + Send>,
    > {
        Box::pin(stream! {
            let mut task_context = task_context;
            task_context.add_position_name("try".to_string()).await;
            let retry = task.catch.retry.clone();
            let start_time = Instant::now();

            let mut retry_count: i64 = 0;
            let mut error_caught = false;
            let mut last_context: Option<TaskContext> = None;

            // Main try-retry loop
            let final_result: Result<TaskContext, Box<workflow::Error>> = loop {
                // Execute try.do block
                if task.try_.is_empty() {
                    tracing::warn!("Try task list is empty, nothing to execute");
                    break Ok(task_context.clone());
                }

                let try_position = {
                    task_context
                        .position
                        .read()
                        .await
                        .as_error_instance()
                        .clone()
                };

                let do_tasks = DoTask {
                    do_: task.try_.clone(),
                    input: Some(Input {
                        from: Some(workflow::InputFrom::Variant0("${.}".to_string())),
                        ..Default::default()
                    }),
                    output: Some(workflow::Output {
                        as_: Some(workflow::OutputAs::Variant0("${.}".to_string())),
                        schema: None,
                    }),
                    metadata: task.metadata.clone(),
                    ..Default::default()
                };

                let do_stream_executor = DoTaskStreamExecutor::new(
                    workflow_context.clone(),
                    default_timeout,
                    metadata.clone(),
                    do_tasks,
                    job_executors.clone(),
                    checkpoint_repository.clone(),
                    execution_id.clone(),
                    emit_streaming_data,
                );

                let mut try_stream = do_stream_executor
                    .execute_stream(cx.clone(), Arc::new(task_name.to_string()), task_context.clone())
                    .boxed();

                let mut try_last_context = None;
                let mut try_error = None;

                while let Some(result) = try_stream.next().await {
                    match result {
                        Ok(event) => {
                            if let Some(ctx) = event.context() {
                                try_last_context = Some(ctx.clone());
                            }
                            yield Ok(event);
                        }
                        Err(e) => {
                            tracing::debug!("Try task failed with error: {:?}", e);
                            try_error = Some(e);
                            break;
                        }
                    }
                }

                match try_error {
                    None => {
                        // Try succeeded
                        error_caught = false;
                        match try_last_context {
                            Some(ctx) => {
                                last_context = Some(ctx.clone());
                                break Ok(ctx);
                            }
                            None => {
                                break Err(Box::new(workflow::Error {
                                    type_: workflow::UriTemplate("task-execution-error".to_string()),
                                    status: 400,
                                    detail: Some("No task context returned from try task list execution".to_string()),
                                    title: None,
                                    instance: Some(try_position),
                                }));
                            }
                        }
                    }
                    Some(error) => {
                        // Try failed - execute catch block
                        let catch_context = try_last_context.unwrap_or(task_context.clone());
                        let catch_config = task.catch.clone();
                        match Self::execute_catch_block(
                            &catch_config,
                            workflow_context.clone(),
                            catch_context,
                            error,
                        )
                        .await
                        {
                            Ok(catch_ctx) => {
                                error_caught = true;
                                if let Some(ref retry_config) = retry {
                                    if Self::eval_retry_condition(retry_config, &start_time, retry_count).await {
                                        retry_count += 1;
                                        task_context = catch_ctx;
                                        continue;
                                    } else {
                                        last_context = Some(catch_ctx.clone());
                                        break Ok(catch_ctx);
                                    }
                                } else {
                                    tracing::debug!("No retry configured, continuing with catch context");
                                    last_context = Some(catch_ctx.clone());
                                    break Ok(catch_ctx);
                                }
                            }
                            Err(catch_error) => {
                                tracing::error!("Catch block failed with error: {:?}", catch_error);
                                break Err(catch_error);
                            }
                        }
                    }
                }
            };

            // Handle result and execute catch.do if needed
            match final_result {
                Ok(res) => {
                    if error_caught {
                        if let Some(catch_do_tasks) = &task.catch.do_ {
                            // Remove position before executing catch.do to avoid inheriting try position
                            res.remove_position().await;

                            // Execute catch.do block
                            let do_tasks = DoTask {
                                do_: catch_do_tasks.clone(),
                                input: Some(Input {
                                    from: Some(workflow::InputFrom::Variant0("${.}".to_string())),
                                    ..Default::default()
                                }),
                                output: Some(workflow::Output {
                                    as_: Some(workflow::OutputAs::Variant0("${.}".to_string())),
                                    schema: None,
                                }),
                                metadata: task.metadata.clone(),
                                ..Default::default()
                            };

                            let catch_do_executor = DoTaskStreamExecutor::new(
                                workflow_context.clone(),
                                default_timeout,
                                metadata.clone(),
                                do_tasks,
                                job_executors.clone(),
                                checkpoint_repository.clone(),
                                execution_id.clone(),
                                emit_streaming_data,
                            );

                            let mut catch_do_stream = catch_do_executor
                                .execute_stream(cx.clone(), Arc::new("[catch_do]".to_string()), res.clone())
                                .boxed();

                            while let Some(result) = catch_do_stream.next().await {
                                match result {
                                    Ok(event) => {
                                        if let Some(ctx) = event.context() {
                                            last_context = Some(ctx.clone());
                                        }
                                        yield Ok(event);
                                    }
                                    Err(e) => {
                                        yield Err(e);
                                        return;
                                    }
                                }
                            }

                            // If no context was returned from catch.do stream, use the cleaned res
                            if last_context.is_none() {
                                last_context = Some(res);
                            }
                        } else {
                            res.remove_position().await;
                            last_context = Some(res);
                        }
                    } else {
                        res.remove_position().await;
                        last_context = Some(res);
                    }
                }
                Err(e) => {
                    yield Err(e);
                    return;
                }
            }

            // Final completed event with position
            if let Some(ctx) = last_context {
                let pos_str = ctx.position.read().await.as_json_pointer();
                yield Ok(WorkflowStreamEvent::task_completed_with_position(
                    "tryTask",
                    &task_name,
                    &pos_str,
                    ctx,
                ));
            }
        })
    }

    async fn eval_retry_condition(
        retry: &workflow::TryTaskCatchRetry,
        start_time: &Instant,
        retry_count: i64,
    ) -> bool {
        match retry {
            workflow::TryTaskCatchRetry::Variant0(workflow::RetryPolicy {
                backoff,
                delay,
                limit,
            }) => {
                tracing::debug!("Retry policy: {:?}", retry);
                if let Some(workflow::RetryLimit {
                    attempt: Some(workflow::RetryLimitAttempt { count, duration }),
                }) = limit.as_ref()
                {
                    if count.as_ref().is_some_and(|c| retry_count >= *c) {
                        tracing::debug!(
                            "Retry limit reached: retry_count={} >= count={:?}",
                            retry_count,
                            count
                        );
                        return false;
                    }
                    if let Some(duration) = duration {
                        if start_time.elapsed().as_secs_f64() > duration.to_millis() as f64 / 1000.0
                        {
                            tracing::debug!("Retry duration reached: {:?}", duration);
                            return false;
                        }
                    }
                    tracing::debug!("Retry count: ++{:?}", retry_count);
                } else {
                    tracing::warn!("Set retry without limit, so retry infinite");
                }

                if let Some(backoff) = backoff {
                    tracing::warn!(
                        "Backoff: {:?}, but unimplemented now. consider as constant 1 second",
                        backoff
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
                if let Some(delay) = delay {
                    tracing::debug!("Delay: {:?}", delay);
                    tokio::time::sleep(tokio::time::Duration::from_millis(delay.to_millis())).await;
                }
                true
            }
            workflow::TryTaskCatchRetry::Variant1(label) => {
                tracing::error!("Retry policy label not implemented: {:?}, no retry", label);
                false
            }
        }
    }

    async fn execute_catch_block(
        catch_config: &TryTaskCatch,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
        error: Box<workflow::Error>,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        let should_filter = Self::eval_error_filter(catch_config, &error);
        if !should_filter {
            return Err(error);
        }

        task_context.add_position_name("catch".to_string()).await;
        let default_error_name = "error".to_string();
        let error_name = catch_config.as_.as_ref().unwrap_or(&default_error_name);
        task_context
            .add_context_value(
                error_name.clone(),
                serde_json::to_value(&error)
                    .inspect_err(|e| tracing::warn!("Failed to serialize error: {:#?}", e))
                    .unwrap_or(serde_json::Value::Null),
            )
            .await;

        // Only build expression if when/except_when conditions are present
        // to avoid unnecessary RwLock access that can cause deadlocks in stream context
        if catch_config.when.is_some() || catch_config.except_when.is_some() {
            let expression = Self::expression(
                &*workflow_context.read().await,
                Arc::new(task_context.clone()),
            )
            .await;
            let expression = match expression {
                Ok(expression) => expression,
                Err(mut e) => {
                    tracing::error!("Failed to evaluate expression: {:#?}", e);
                    let pos = task_context.position.read().await;
                    e.position(&pos);
                    return Err(e);
                }
            };

            if let Some(when) = &catch_config.when {
                let eval_when = match Self::execute_transform_as_bool(
                    task_context.raw_input.clone(),
                    when,
                    &expression,
                ) {
                    Ok(value) => value,
                    Err(mut e) => {
                        tracing::error!("Failed to evaluate when condition: {:#?}", e);
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("when".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };

                if !eval_when {
                    return Err(error);
                }
            }

            if let Some(except_when) = &catch_config.except_when {
                let eval_except_when = match Self::execute_transform_as_bool(
                    task_context.raw_input.clone(),
                    except_when,
                    &expression,
                ) {
                    Ok(value) => value,
                    Err(mut e) => {
                        tracing::error!("Failed to evaluate except_when condition: {:#?}", e);
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("except_when".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };
                if eval_except_when {
                    return Err(error);
                }
            }
        }
        task_context.remove_position().await;
        Ok(task_context)
    }

    fn eval_error_filter(catch_config: &TryTaskCatch, error: &workflow::Error) -> bool {
        if let Some(errors) = &catch_config.errors {
            if let Some(filter) = &errors.with {
                let workflow::Error {
                    type_,
                    status,
                    detail,
                    title,
                    instance,
                } = &error;

                let should_filter_status = filter
                    .status
                    .as_ref()
                    .map(|st| st == status)
                    .unwrap_or(true);
                let should_filter_type = filter
                    .type_
                    .as_ref()
                    .map(|tp| type_.is_match(tp))
                    .unwrap_or(true);
                let should_filter_details = filter
                    .details
                    .as_ref()
                    .map(|ds| {
                        detail
                            .as_ref()
                            .is_some_and(|detail| ds == &detail.to_string())
                    })
                    .unwrap_or(true);
                let should_filter_title = filter
                    .title
                    .as_ref()
                    .map(|t| title.as_ref().is_some_and(|title| t == &title.to_string()))
                    .unwrap_or(true);
                let should_filter_instance = filter
                    .instance
                    .as_ref()
                    .map(|ins| {
                        instance
                            .as_ref()
                            .is_some_and(|instance| ins == &instance.to_string())
                    })
                    .unwrap_or(true);
                tracing::debug!(
                    "Error filter: status: {}, type: {}, details: {}, title: {}, instance: {}",
                    should_filter_status,
                    should_filter_type,
                    should_filter_details,
                    should_filter_title,
                    should_filter_instance
                );

                should_filter_status
                    && should_filter_type
                    && should_filter_details
                    && should_filter_title
                    && should_filter_instance
            } else {
                true
            }
        } else {
            true
        }
    }
}

impl UseExpression for TryStreamTaskExecutor {}
impl UseExpressionTransformer for TryStreamTaskExecutor {}
impl UseJqAndTemplateTransformer for TryStreamTaskExecutor {}

impl StreamTaskExecutorTrait<'_> for TryStreamTaskExecutor {
    fn execute_stream(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_name: Arc<String>,
        task_context: TaskContext,
    ) -> impl futures::Stream<Item = Result<WorkflowStreamEvent, Box<workflow::Error>>> + Send {
        Self::execute_try_stream(
            self.workflow_context.clone(),
            self.default_timeout,
            self.metadata.clone(),
            self.task.clone(),
            self.job_executors.clone(),
            self.checkpoint_repository.clone(),
            self.execution_id.clone(),
            self.emit_streaming_data,
            cx,
            task_name,
            task_context,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::definition::workflow::{
        Document, FlowDirective, FlowDirectiveEnum, Output, RaiseTask, RaiseTaskConfiguration,
        RaiseTaskError, SetTask, Task, TaskList, TryTaskCatch, UriTemplate, WorkflowName,
        WorkflowSchema, WorkflowVersion,
    };
    use crate::workflow::execute::context::{TaskContext, WorkflowContext, WorkflowStatus};
    use app::module::test::create_hybrid_test_app;
    use std::collections::HashMap;
    use std::str::FromStr;
    use tokio::sync::Mutex;

    fn create_test_workflow() -> WorkflowSchema {
        WorkflowSchema {
            checkpointing: None,
            document: Document {
                name: WorkflowName::from_str("try-stream-test-workflow").unwrap(),
                version: WorkflowVersion::from_str("1.0.0").unwrap(),
                metadata: serde_json::Map::new(),
                ..Default::default()
            },
            input: workflow::Input {
                schema: None,
                from: None,
            },
            output: Some(Output {
                as_: None,
                schema: None,
            }),
            do_: TaskList(vec![]),
        }
    }

    fn create_try_task_with_success() -> workflow::TryTask {
        let set_task1 = SetTask {
            set: serde_json::json!({
                "step1": "completed"
            })
            .as_object()
            .unwrap()
            .clone(),
            export: None,
            if_: None,
            input: None,
            metadata: serde_json::Map::new(),
            output: None,
            then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Continue)),
            timeout: None,
            checkpoint: false,
        };
        let set_task2 = SetTask {
            set: serde_json::json!({
                "step2": "completed"
            })
            .as_object()
            .unwrap()
            .clone(),
            export: None,
            if_: None,
            input: None,
            metadata: serde_json::Map::new(),
            output: Some(Output {
                as_: Some(workflow::OutputAs::Variant0("success_output".to_string())),
                schema: None,
            }),
            then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
            timeout: None,
            checkpoint: false,
        };

        let try_task_list = TaskList(vec![
            {
                let mut map = HashMap::new();
                map.insert("set_step1".to_string(), Task::SetTask(set_task1));
                map
            },
            {
                let mut map = HashMap::new();
                map.insert("set_step2".to_string(), Task::SetTask(set_task2));
                map
            },
        ]);

        workflow::TryTask {
            try_: try_task_list,
            catch: TryTaskCatch {
                errors: None,
                as_: Some("error".to_string()),
                when: None,
                except_when: None,
                retry: None,
                do_: None,
            },
            ..Default::default()
        }
    }

    // NOTE: These tests use TEST_RUNTIME.block_on which is now multi-threaded
    // to avoid deadlocks in stream! macro contexts that use RwLock, while also
    // sharing the same DB connection pool across all tests.

    #[test]
    fn test_try_stream_executor_success_emits_multiple_events() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let workflow = create_test_workflow();
            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context,
                None,
            )));
            workflow_context.write().await.status = WorkflowStatus::Running;

            let try_task = create_try_task_with_success();
            let executor = TryStreamTaskExecutor::new(
                workflow_context.clone(),
                Duration::from_secs(60),
                try_task,
                Arc::new(JobExecutorWrapper::new(app_module)),
                None,
                None,
                Arc::new(HashMap::new()),
                false,
            );

            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );

            let mut stream = executor
                .execute_stream(
                    Arc::new(opentelemetry::Context::current()),
                    Arc::new("test_try".to_string()),
                    task_context,
                )
                .boxed();

            let mut event_count = 0;
            let mut final_event = None;

            while let Some(result) = stream.next().await {
                match result {
                    Ok(event) => {
                        event_count += 1;
                        final_event = Some(event);
                    }
                    Err(e) => {
                        panic!("Unexpected error: {:?}", e);
                    }
                }
            }

            // Multiple events should be emitted (one for each internal task + final)
            assert!(
                event_count >= 2,
                "Expected at least 2 events (internal tasks), got {}",
                event_count
            );

            // Verify final event has context
            assert!(final_event.is_some());
            let ctx = final_event.unwrap().into_context();
            assert!(ctx.is_some(), "Final event should have TaskContext");
        });
    }

    #[test]
    fn test_try_stream_executor_empty_try_list() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let workflow = create_test_workflow();
            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context,
                None,
            )));
            workflow_context.write().await.status = WorkflowStatus::Running;

            let try_task = workflow::TryTask {
                try_: TaskList(vec![]), // Empty try list
                catch: TryTaskCatch {
                    errors: None,
                    as_: Some("error".to_string()),
                    when: None,
                    except_when: None,
                    retry: None,
                    do_: None,
                },
                ..Default::default()
            };

            let executor = TryStreamTaskExecutor::new(
                workflow_context.clone(),
                Duration::from_secs(60),
                try_task,
                Arc::new(JobExecutorWrapper::new(app_module)),
                None,
                None,
                Arc::new(HashMap::new()),
                false,
            );

            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );

            let mut stream = executor
                .execute_stream(
                    Arc::new(opentelemetry::Context::current()),
                    Arc::new("test_empty".to_string()),
                    task_context,
                )
                .boxed();

            let mut event_count = 0;
            let mut had_error = false;

            while let Some(result) = stream.next().await {
                match result {
                    Ok(_) => {
                        event_count += 1;
                    }
                    Err(_) => {
                        had_error = true;
                        break;
                    }
                }
            }

            // Should complete without error even with empty try list
            assert!(!had_error, "Empty try list should not cause error");
            // Should emit at least final event
            assert!(event_count >= 1, "Should emit at least one event");
        });
    }

    #[test]
    fn test_try_stream_executor_position_tracking() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let workflow = create_test_workflow();
            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context,
                None,
            )));
            workflow_context.write().await.status = WorkflowStatus::Running;

            let try_task = create_try_task_with_success();
            let executor = TryStreamTaskExecutor::new(
                workflow_context.clone(),
                Duration::from_secs(60),
                try_task,
                Arc::new(JobExecutorWrapper::new(app_module)),
                None,
                None,
                Arc::new(HashMap::new()),
                false,
            );

            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );
            // Add initial position
            task_context.add_position_name("ROOT".to_string()).await;

            let mut stream = executor
                .execute_stream(
                    Arc::new(opentelemetry::Context::current()),
                    Arc::new("test_position".to_string()),
                    task_context,
                )
                .boxed();

            let mut positions: Vec<String> = Vec::new();

            while let Some(result) = stream.next().await {
                match result {
                    Ok(event) => {
                        let pos = event.position();
                        if !pos.is_empty() {
                            positions.push(pos.to_string());
                        }
                    }
                    Err(e) => {
                        panic!("Unexpected error: {:?}", e);
                    }
                }
            }

            // Verify we got position info from internal tasks
            assert!(
                !positions.is_empty(),
                "Should have position info from events"
            );

            // At least one position should contain "try"
            let has_try_position = positions.iter().any(|p| p.contains("try"));
            assert!(
                has_try_position,
                "At least one position should contain 'try': {:?}",
                positions
            );
        });
    }

    #[test]
    fn test_try_stream_executor_error_caught_without_when_condition() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let workflow = create_test_workflow();
            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context,
                None,
            )));
            workflow_context.write().await.status = WorkflowStatus::Running;

            // Create a try task that raises an error, with simple catch (no when/except_when)
            let raise_task = RaiseTask {
                raise: RaiseTaskConfiguration {
                    error: RaiseTaskError::Error(workflow::Error {
                        type_: UriTemplate("test-error".to_string()),
                        status: 500,
                        detail: Some("Test error".to_string()),
                        title: None,
                        instance: None,
                    }),
                },
                output: None,
                checkpoint: false,
                export: None,
                if_: None,
                input: None,
                metadata: serde_json::Map::new(),
                then: None,
                timeout: None,
            };

            let try_task = workflow::TryTask {
                try_: TaskList(vec![{
                    let mut map = HashMap::new();
                    map.insert("raise_error".to_string(), Task::RaiseTask(raise_task));
                    map
                }]),
                catch: TryTaskCatch {
                    errors: None, // catch all
                    as_: Some("error".to_string()),
                    when: None,        // no when condition
                    except_when: None, // no except_when condition
                    retry: None,
                    do_: None, // no catch.do
                },
                ..Default::default()
            };

            let executor = TryStreamTaskExecutor::new(
                workflow_context.clone(),
                Duration::from_secs(60),
                try_task,
                Arc::new(JobExecutorWrapper::new(app_module)),
                None,
                None,
                Arc::new(HashMap::new()),
                false,
            );

            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );

            let mut stream = executor
                .execute_stream(
                    Arc::new(opentelemetry::Context::current()),
                    Arc::new("test_error_catch".to_string()),
                    task_context,
                )
                .boxed();

            let mut event_count = 0;
            let mut had_error = false;
            let mut final_context = None;

            while let Some(result) = stream.next().await {
                match result {
                    Ok(event) => {
                        event_count += 1;
                        if let Some(ctx) = event.context() {
                            final_context = Some(ctx.clone());
                        }
                    }
                    Err(_) => {
                        had_error = true;
                        break;
                    }
                }
            }

            // Error should be caught (no when/except_when to reject it)
            assert!(
                !had_error,
                "Error should be caught when no when/except_when conditions"
            );

            // Should have at least 1 event (the final tryTask completed event)
            assert!(
                event_count >= 1,
                "Expected at least 1 event, got {}",
                event_count
            );

            // Verify error was added to context
            assert!(final_context.is_some(), "Should have final context");
            let ctx = final_context.unwrap();
            let error_value = ctx.get_context_value("error").await;
            assert!(
                error_value.is_some(),
                "Error should be stored in context as 'error'"
            );
        });
    }

    #[test]
    fn test_try_stream_executor_error_with_catch_do() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let workflow = create_test_workflow();
            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context,
                None,
            )));
            workflow_context.write().await.status = WorkflowStatus::Running;

            let raise_task = RaiseTask {
                raise: RaiseTaskConfiguration {
                    error: RaiseTaskError::Error(workflow::Error {
                        type_: UriTemplate("test-error".to_string()),
                        status: 500,
                        detail: Some("Test error for catch.do".to_string()),
                        title: None,
                        instance: None,
                    }),
                },
                output: None,
                checkpoint: false,
                export: None,
                if_: None,
                input: None,
                metadata: serde_json::Map::new(),
                then: None,
                timeout: None,
            };

            let catch_do_set = SetTask {
                set: serde_json::json!({
                    "catch_do_executed": true
                })
                .as_object()
                .unwrap()
                .clone(),
                export: None,
                if_: None,
                input: None,
                metadata: serde_json::Map::new(),
                output: Some(Output {
                    as_: Some(workflow::OutputAs::Variant0("catch_result".to_string())),
                    schema: None,
                }),
                then: None,
                timeout: None,
                checkpoint: false,
            };

            let try_task = workflow::TryTask {
                try_: TaskList(vec![{
                    let mut map = HashMap::new();
                    map.insert("raise_error".to_string(), Task::RaiseTask(raise_task));
                    map
                }]),
                catch: TryTaskCatch {
                    errors: None,
                    as_: Some("error".to_string()),
                    when: None,
                    except_when: None,
                    retry: None,
                    do_: Some(TaskList(vec![{
                        let mut map = HashMap::new();
                        map.insert("catch_set".to_string(), Task::SetTask(catch_do_set));
                        map
                    }])),
                },
                ..Default::default()
            };

            let executor = TryStreamTaskExecutor::new(
                workflow_context.clone(),
                Duration::from_secs(60),
                try_task,
                Arc::new(JobExecutorWrapper::new(app_module)),
                None,
                None,
                Arc::new(HashMap::new()),
                false,
            );

            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );

            let mut stream = executor
                .execute_stream(
                    Arc::new(opentelemetry::Context::current()),
                    Arc::new("test_catch_do".to_string()),
                    task_context,
                )
                .boxed();

            let mut event_count = 0;
            let mut had_error = false;

            while let Some(result) = stream.next().await {
                match result {
                    Ok(_) => {
                        event_count += 1;
                    }
                    Err(_) => {
                        had_error = true;
                        break;
                    }
                }
            }

            assert!(!had_error, "Error should be caught and handled by catch.do");

            // Should have events from catch.do execution
            assert!(
                event_count >= 1,
                "Expected events from catch.do, got {}",
                event_count
            );

            // Verify catch.do was executed
            let wf_ctx = workflow_context.read().await;
            let ctx_vars = wf_ctx.context_variables.lock().await;
            let catch_do_value = ctx_vars.get("catch_do_executed");
            assert!(
                catch_do_value.is_some(),
                "catch.do should have set catch_do_executed"
            );
            assert_eq!(catch_do_value.unwrap(), &serde_json::json!(true));
        });
    }
}
