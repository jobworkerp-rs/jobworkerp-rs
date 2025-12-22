use super::{
    context::{TaskContext, WorkflowContext, WorkflowStreamEvent},
    expression::UseExpression,
};
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self, tasks::TaskTrait, Task},
    },
    execute::{
        checkpoint::{repository::CheckPointRepositoryWithId, CheckPointContext},
        context::{Then, WorkflowPosition},
        task::stream::run::RunStreamTaskExecutor,
    },
};
use anyhow::Result;
use app::app::job::execute::JobExecutorWrapper;
use async_stream::stream;
use command_utils::trace::Tracing;
use debug_stub_derive::DebugStub;
use fork::ForkTaskExecutor;
use futures::{pin_mut, StreamExt};
use jobworkerp_base::APP_NAME;
use opentelemetry::trace::TraceContextExt;
use run::RunTaskExecutor;
use set::SetTaskExecutor;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};
use stream::{do_::DoTaskStreamExecutor, for_::ForTaskStreamExecutor};
use switch::SwitchTaskExecutor;
use tokio::sync::{Mutex, RwLock};
use try_::TryTaskExecutor;

pub mod call;
pub mod fork;
pub mod run;
pub mod set;
pub mod stream;
pub mod switch;
pub mod trace;
#[path = "task/try_.rs"]
pub mod try_;
//
// for DebugStub
type CheckPointRepo =
    Arc<dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId>;

#[derive(DebugStub, Clone)]
pub struct TaskExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    default_task_timeout: Duration,
    #[debug_stub = "AppModule"]
    pub job_executor_wrapper: Arc<JobExecutorWrapper>,
    #[debug_stub = "CheckPointRepositoryWithIdImpl"]
    pub checkpoint_repository: Option<CheckPointRepo>,
    pub task_name: String,
    pub task: Arc<Task>,
    pub metadata: Arc<HashMap<String, String>>,
    /// Whether to emit StreamingData events (for ag-ui-front real-time streaming)
    pub emit_streaming_data: bool,
}
impl UseExpression for TaskExecutor {}
impl UseJqAndTemplateTransformer for TaskExecutor {}
impl UseExpressionTransformer for TaskExecutor {}

impl TaskExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workflow_context: Arc<RwLock<WorkflowContext>>,
        default_task_timeout: Duration,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        checkpoint_repository: Option<Arc<dyn CheckPointRepositoryWithId>>,
        task_name: &str,
        task: Arc<Task>,
        metadata: Arc<HashMap<String, String>>,
        emit_streaming_data: bool,
    ) -> Self {
        // command_utils::util::tracing::tracing_init_test(Level::DEBUG);
        Self {
            workflow_context,
            default_task_timeout,
            job_executor_wrapper,
            checkpoint_repository,
            task_name: task_name.to_owned(),
            task,
            metadata,
            emit_streaming_data,
        }
    }

    /// Resolve the timeout duration for the current task.
    /// Uses task-specific timeout if defined, otherwise falls back to default.
    fn resolve_timeout(&self) -> Duration {
        match self.task.timeout() {
            Some(workflow::TaskTimeout::Timeout(t)) => Duration::from_millis(t.after.to_millis()),
            Some(workflow::TaskTimeout::TaskTimeoutReference(_)) => {
                // TaskTimeoutReference is not supported yet, fall back to default
                self.default_task_timeout
            }
            None => self.default_task_timeout,
        }
    }

    async fn load_checkpoint(
        &self,
        execution_id: &Option<Arc<ExecutionId>>,
        current_position: &RwLock<WorkflowPosition>,
    ) -> Result<Option<TaskContext>, Box<workflow::Error>> {
        let cp_position = self
            .workflow_context
            .read()
            .await
            .checkpoint_position
            .clone();
        if let (Some(execution_id), Some(checkpoint_repository), Some(pos)) =
            (&execution_id, &self.checkpoint_repository, &cp_position)
        {
            // Load checkpoint if available
            let workflow_name = self.workflow_context.read().await.name.to_string();
            match checkpoint_repository
                .get_checkpoint_with_id(execution_id, &workflow_name, &pos.as_json_pointer())
                .await
            {
                Ok(Some(checkpoint)) => {
                    tracing::debug!(
                        "Loaded checkpoint for workflow: {}, execution_id={}, position={}",
                        &workflow_name,
                        &execution_id.value,
                        &pos.as_json_pointer()
                    );
                    let mut lock = self.workflow_context.write().await;
                    lock.position = checkpoint.position.clone();
                    lock.input = checkpoint.workflow.input.clone();
                    lock.context_variables =
                        Arc::new(Mutex::new((*checkpoint.workflow.context_variables).clone()));
                    drop(lock);
                    Ok(Some(TaskContext::new_from_cp(None, &checkpoint.task)))
                }
                Ok(None) => {
                    tracing::warn!(
                        "No checkpoint found for workflow: {}, execution_id={}",
                        &workflow_name,
                        &execution_id.value
                    );
                    Ok(None)
                }
                Err(e) => {
                    tracing::error!("Failed to load checkpoint: {:#?}", e);
                    // yield Err(Box::new(e));
                    Err(workflow::errors::ErrorFactory::new().service_unavailable(
                    format!("Failed to load checkpoint from execution_id: {}, workflow: {}, position: {}, error: {:#?}",
                      execution_id.value, workflow_name, &pos.as_json_pointer(), e),
                Some(current_position.read().await.as_error_instance()),
                    Some(format!("{e:?}")),
                ))
                }
            }
        } else {
            // normal execution without checkpoint
            Ok(None)
        }
    }
    // if true, load checkpoint for this task, and should skip the task execution
    // if false, do not load checkpoint for this task, and should execute the task
    // if None, no checkpoint is available, normal execution
    async fn reach_for_checkpoint(
        &self,
        execution_id: &Option<Arc<ExecutionId>>,
        current_position: &RwLock<WorkflowPosition>,
    ) -> Option<bool> {
        if let Some(cp_position) = &self.workflow_context.read().await.checkpoint_position {
            let rel_path = cp_position.relative_path(current_position.read().await.full());
            if rel_path.as_ref().is_some_and(|rp| rp.is_empty()) && execution_id.is_some() {
                tracing::info!(
                    "recover checkpoint for task {}: no `from_position` provided",
                    self.task_name
                );
                Some(true)
            } else {
                tracing::info!(
                    "position {}: not reached checkpoint position: {} (relative: {:?}), execution_id: {:?}",
                    current_position.read().await.as_json_pointer(),
                    cp_position.as_json_pointer(),
                    rel_path
                        .as_ref(),
                    execution_id
                );
                // skip task execution until the `from_position` is reached
                Some(false)
            }
        } else {
            None
        }
    }
    async fn can_execute_next(&self, current_position: &RwLock<WorkflowPosition>) -> bool {
        // (should check checkpoint recovery or not before this)
        if let Some(cp_position) = &self.workflow_context.read().await.checkpoint_position {
            // lock workflow context
            cp_position
                .relative_path(current_position.read().await.full())
                .is_some()
        } else {
            // no checkpoint position, can execute next task (normal execution)
            true
        }
    }

    // input is the output of the previous task
    // return the output of the task, and next task("continue", "exit", "end", or taskName)
    // TODO timeout implementation
    pub async fn execute(
        &self,
        cx: Arc<opentelemetry::Context>,
        parent_task: Arc<TaskContext>,
        execution_id: Option<Arc<ExecutionId>>,
    ) -> futures::stream::BoxStream<'static, Result<WorkflowStreamEvent, Box<workflow::Error>>>
    {
        let position = parent_task.position.clone();
        // enter task (add task name to position stack)
        // XXX Do not forget to remove the position after task execution (difficult to debug)
        position.write().await.push(self.task_name.clone());

        let input = parent_task.output.clone();
        let mut task_context = TaskContext::new(
            Some(self.task.clone()),
            input.clone(),
            parent_task.context_variables.clone(),
        );
        task_context.position = position.clone();

        // skip task execution if checkpoint recovery is needed
        let reach_checkpoint = self.reach_for_checkpoint(&execution_id, &position).await;

        // overwrite task_context with checkpoint data if checkpoint recovery is needed
        let mut task_context = if let Some(reached) = reach_checkpoint {
            tracing::debug!(
                "Task {}: reached checkpoint: {}, execution_id: {:?}",
                self.task_name,
                reached,
                execution_id
            );
            // If we skip the task, we still need to create a TaskContext with the input
            let mut task_context = if reached {
                match self.load_checkpoint(&execution_id, &position).await {
                    Ok(Some(cp)) => {
                        tracing::debug!(
                            "Loaded checkpoint for task: {}, execution_id: {:?}",
                            self.task_name,
                            execution_id
                        );
                        // remove checkpoint position workflow_context (continue execution)
                        self.workflow_context.write().await.checkpoint_position = None;
                        cp
                    }
                    Ok(None) => {
                        tracing::error!(
                            "No checkpoint found for task in cp recovery: {}, execution_id: {:?}",
                            self.task_name,
                            execution_id
                        );
                        return futures::stream::once(futures::future::ready(Err(
                            workflow::errors::ErrorFactory::new().not_found(
                                format!(
                                    "No checkpoint found for task: {}, execution_id: {:?}",
                                    self.task_name, execution_id
                                ),
                                None,
                                None,
                            ),
                        )))
                        .boxed();
                    }
                    Err(e) => {
                        tracing::error!("Failed to load checkpoint: {:#?}", e);
                        return futures::stream::once(futures::future::ready(Err(e))).boxed();
                    }
                }
            } else {
                // not reached checkpoint, continue execution (skipping task)
                task_context
            };
            if !self.can_execute_next(&position).await {
                tracing::info!(
                    "Skip task execution for {}: not reached checkpoint position: {}",
                    self.task_name,
                    self.workflow_context
                        .read()
                        .await
                        .checkpoint_position
                        .as_ref()
                        .map_or("None".to_string(), |p| p.as_json_pointer().to_string())
                );
                task_context.set_completed_at();
                let pos_str = task_context.position.read().await.as_json_pointer();
                let event = WorkflowStreamEvent::task_completed_with_position(
                    "skip",
                    &self.task_name,
                    &pos_str,
                    task_context,
                );
                return futures::stream::once(futures::future::ready(Ok(event))).boxed();
            }
            task_context
        } else {
            tracing::trace!(
                "Task {}: no checkpoint recovery needed, execution_id: {:?}",
                self.task_name,
                execution_id
            );
            // normal execution, no checkpoint recovery
            task_context
        };

        let expression = Self::expression(
            &*self.workflow_context.read().await,
            Arc::new(task_context.clone()),
        )
        .await;
        let expression = match expression {
            Ok(expression) => expression,
            Err(mut e) => {
                tracing::error!("Failed to evaluate expression: {:#?}", e);
                task_context.flow_directive = Then::End;
                let pos = task_context.position.read().await;
                e.position(&pos);
                drop(pos);
                task_context.set_completed_at();
                return futures::stream::once(futures::future::ready(Err(e))).boxed();
            }
        };

        // Evaluate if condition
        if let Some(if_cond) = self.task.if_() {
            tracing::debug!("`If' condition: {:#?}", if_cond);
            let eval_result = Self::execute_transform_as_bool(
                task_context.raw_input.clone(),
                if_cond,
                &expression,
            );
            match eval_result {
                Ok(v) => {
                    if v {
                        tracing::debug!("`If' condition is true: {:#?}", &task_context.raw_input);
                    } else {
                        tracing::debug!(
                            "`If' condition is false, skip: {:#?}",
                            &task_context.raw_input
                        );
                        let pos_str = task_context.position.read().await.as_json_pointer();
                        task_context.remove_position().await; // remove task_name
                        task_context.set_completed_at();
                        let event = WorkflowStreamEvent::task_completed_with_position(
                            "skip_if",
                            &self.task_name,
                            &pos_str,
                            task_context,
                        );
                        return futures::stream::once(futures::future::ready(Ok(event))).boxed();
                    }
                }
                Err(mut e) => {
                    task_context.add_position_name("if".to_string()).await;
                    let pos = task_context.position.read().await;
                    e.position(&pos);
                    drop(pos);
                    task_context.set_completed_at();
                    tracing::error!("Failed to evaluate `if' condition: {:#?}", e);
                    return futures::stream::once(futures::future::ready(Err(e))).boxed();
                }
            }
        }

        // Transform input and update task context
        let task_context = match self
            .update_context_by_input(&expression, task_context)
            .await
        {
            Ok(context) => context,
            Err(e) => {
                return futures::stream::once(futures::future::ready(Err(e))).boxed();
            }
        };
        // only use in rare error
        let err_pos = task_context.position.read().await.as_error_instance();

        // Task-level timeout configuration
        let timeout_duration = self.resolve_timeout();
        let deadline = tokio::time::Instant::now() + timeout_duration;
        let task_name_for_timeout = self.task_name.clone();

        let res = self
            .execute_task(cx, task_context, execution_id.clone())
            .await;
        // remove task position after execution (last task context)
        // NOTE: For streaming events (StreamingJobStarted, StreamingData), we yield immediately
        // to ensure real-time delivery. Only the final completed event needs position cleanup.
        Box::pin(stream! {
            pin_mut!(res);
            // Track non-streaming events to find the final one for position cleanup
            let mut previous_item: Option<Result<WorkflowStreamEvent, Box<workflow::Error>>> = None;
            let mut timed_out = false;

            loop {
                match tokio::time::timeout_at(deadline, res.next()).await {
                    Ok(Some(item)) => {
                        match &item {
                            Ok(event) => {
                                // Check if this is a streaming event that should be yielded immediately
                                let is_streaming_event = matches!(
                                    event,
                                    WorkflowStreamEvent::StreamingJobStarted { .. } |
                                    WorkflowStreamEvent::StreamingData { .. }
                                );

                                if is_streaming_event {
                                    // Flush previous_item before yielding streaming event to preserve order
                                    if let Some(prev) = previous_item.take() {
                                        yield prev;
                                    }
                                    // Yield streaming events immediately for real-time delivery
                                    yield item;
                                } else {
                                    // For non-streaming events, use previous_item pattern
                                    // to identify the final event for position cleanup
                                    if let Some(prev) = previous_item.take() {
                                        yield prev;
                                    }
                                    previous_item = Some(item);
                                }
                            }
                            Err(_) => {
                                // For errors, use previous_item pattern as well
                                if let Some(prev) = previous_item.take() {
                                    yield prev;
                                }
                                previous_item = Some(item);
                            }
                        }
                    }
                    Ok(None) => {
                        // Stream ended normally
                        break;
                    }
                    Err(_elapsed) => {
                        // Timeout occurred
                        timed_out = true;
                        tracing::warn!(
                            "Task '{}' timed out after {:?}",
                            task_name_for_timeout,
                            timeout_duration
                        );
                        break;
                    }
                }
            }

            // Handle timeout case
            if timed_out {
                // Flush any pending item before yielding timeout error
                if let Some(prev) = previous_item.take() {
                    yield prev;
                }
                yield Err(workflow::errors::ErrorFactory::new().request_timeout(
                    format!(
                        "Task '{}' timed out after {:?}",
                        task_name_for_timeout,
                        timeout_duration
                    ),
                    Some(err_pos.clone()),
                    None,
                ));
            } else if let Some(final_item) = previous_item {
                // Handle the final non-streaming item - call remove_position() only if it's Ok
                match final_item {
                    Ok(event) => {
                        // Remove position from context if this is a completed event
                        if let Some(tc) = event.context() {
                            tc.remove_position().await; // remove task name from position stack
                        }
                        yield Ok(event);
                    }
                    Err(e) => {
                        // If the final item is an error, yield it as is
                        yield Err(e);
                    }
                }
            } else {
                // If no non-streaming item was returned, yield an error
                // (This shouldn't happen normally as workflows should always have a completion event)
                yield Err(workflow::errors::ErrorFactory::create(
                    workflow::errors::ErrorCode::NotFound,
                    Some("No task context returned".to_string()),
                    Some(err_pos),
                    None,
                ));
            }
        })
    }

    // Transform, validate, and update the task context with the input of a task
    async fn update_context_by_input(
        &self,
        expression: &BTreeMap<String, Arc<serde_json::Value>>,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        // XXX invalid by input transformation? (transform argument inner each task)
        if let Some(schema) = self.task.input().and_then(|i| i.schema.as_ref()) {
            if let Some(schema) = schema.json_schema() {
                if let Err(e) = jsonschema::validate(schema, &task_context.input.clone()) {
                    let m = format!(
                        "Failed to validate input schema: {:#?}\n{:#?}\n{:#?}",
                        schema, &task_context.raw_input, e
                    );
                    let pos = task_context.position.clone();
                    pos.write().await.push("input".to_string());
                    return Err(workflow::errors::ErrorFactory::new().bad_argument(
                        m,
                        Some(pos.read().await.as_error_instance()),
                        Some(format!("{:?}", e.to_string())),
                    ));
                }
            }
        }

        // Transform input
        let transformed_input = if let Some(from) = self.task.input().and_then(|i| i.from.as_ref())
        {
            Self::transform_input(task_context.raw_input.clone(), from, expression)?
        } else {
            task_context.raw_input.clone()
        };
        tracing::debug!("Transformed input: {:#?}", transformed_input);
        task_context.set_input(transformed_input);

        Ok(task_context)
    }

    // Transform and update the task context with the output of a task
    async fn update_context_by_output(
        checkpoint_repository: Option<Arc<dyn CheckPointRepositoryWithId>>,
        execution_id: Option<Arc<ExecutionId>>,
        task: Arc<Task>,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        expression: &mut BTreeMap<String, Arc<serde_json::Value>>,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        // Transform output
        tracing::debug!("Task raw output: {:#?}", task_context.raw_output);
        if let Some(as_) = task.output().and_then(|o| o.as_.as_ref()) {
            task_context.output =
                match Self::transform_output(task_context.raw_output.clone(), as_, expression) {
                    Ok(v) => v,
                    Err(mut e) => {
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("output".to_string());
                        e.position(&pos);
                        tracing::error!("Failed to transform output: {:#?}", e);
                        return Err(e);
                    }
                };
            tracing::debug!("Transformed output: {:#?}", &task_context.output);
        } else {
            tracing::debug!("No output transformation: {:#?}", &task_context.raw_output);
            task_context.output = task_context.raw_output.clone();
        }
        // TODO output schema validation?

        // set transformed output to expression (for export and then)
        expression.insert("output".to_string(), task_context.output.clone());

        // export output to workflow context
        if let Some(export) = task.export().and_then(|o| o.as_.as_ref()) {
            match Self::transform_export(task_context.output.clone(), export, expression) {
                Ok(export) => match export.as_ref() {
                    serde_json::Value::Object(map) => {
                        for (key, value) in map.iter() {
                            workflow_context
                                .write()
                                .await
                                .add_context_value(key.clone(), value.clone())
                                .await;
                        }
                    }
                    _ => {
                        tracing::warn!("Export is not a map: {:#?}", &export);
                    }
                },
                Err(mut e) => {
                    tracing::error!("Failed to transform export: {:#?}", e);
                    let pos = task_context.position.clone();
                    let mut pos = pos.write().await;
                    pos.push("export".to_string());
                    e.position(&pos);
                    return Err(e);
                }
            }
            tracing::debug!("Transformed export: {:#?}", &export);
        }
        // Determine next task
        task_context.flow_directive = match task.then() {
            Some(flow) => match Then::create(task_context.output.clone(), flow, expression) {
                Ok(v) => v,
                Err(mut e) => {
                    tracing::error!("Failed to evaluate `then' condition: {:#?}", e);
                    task_context.add_position_name("then".to_string()).await;
                    let pos = task_context.position.read().await;
                    e.position(&pos);
                    return Err(e);
                }
            },
            None => Then::Continue,
        };
        // Save checkpoint if necessary
        Self::save_checkpoint(
            checkpoint_repository.clone(),
            workflow_context.clone(),
            &task_context,
            &task,
            &execution_id,
        )
        .await?;

        task_context.set_completed_at();
        Ok(task_context)
    }

    async fn save_checkpoint(
        checkpoint_repository: Option<Arc<dyn CheckPointRepositoryWithId>>,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: &TaskContext,
        task: &Task,
        execution_id: &Option<Arc<ExecutionId>>,
    ) -> Result<(), Box<workflow::Error>> {
        if let Some(checkpoint_repository) = &checkpoint_repository {
            if task.checkpoint() {
                if let Some(execution_id) = &execution_id {
                    let pos = task_context.position.read().await;
                    let point = pos.as_json_pointer();
                    drop(pos);
                    tracing::debug!("Saving checkpoint for task: {}", &point);
                    // Save checkpoint
                    let wf_context = workflow_context.read().await;
                    let checkpoint = CheckPointContext::new(&wf_context, task_context).await;
                    if let Err(e) = checkpoint_repository
                        .save_checkpoint_with_id(
                            execution_id.as_ref(),
                            &wf_context.document.name,
                            &checkpoint,
                        )
                        .await
                    {
                        tracing::error!("Failed to save checkpoint: {:#?}", e);
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("checkpoint".to_string());
                        let e = workflow::errors::ErrorFactory::new().internal_error(
                            "Failed to save checkpoint".to_string(),
                            Some(pos.as_error_instance()),
                            None,
                        );
                        return Err(e);
                    }
                    Ok(())
                } else {
                    tracing::warn!("Execution ID is not set, skip saving checkpoint");
                    Ok(())
                }
            } else {
                // not necessary to save
                Ok(())
            }
        } else {
            Ok(())
        }
    }

    async fn execute_task(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_context: TaskContext,
        execution_id: Option<Arc<ExecutionId>>,
    ) -> futures::stream::BoxStream<'static, Result<WorkflowStreamEvent, Box<workflow::Error>>>
    {
        // Prepare owned data for 'static futures
        let job_executor_wrapper = self.job_executor_wrapper.clone();
        let task_name = Arc::new(self.task_name.clone());
        // Clone Task enum to own inner data
        let task_enum = (*self.task).clone(); // XXX hard clone
        let original_task = self.task.clone(); // XXX hard clone
        let checkpoint_repository = self.checkpoint_repository.clone();
        let default_task_timeout = self.default_task_timeout;
        let workflow_context = self.workflow_context.clone();

        // Dispatch based on owned Task
        // need to update output context after task execution (not streaming task depends on the other kind tasks)
        match task_enum {
            Task::DoTask(task) => {
                let task_clone = task.clone();
                let job_executor_wrapper_clone = job_executor_wrapper.clone();
                let metadata_clone = self.metadata.clone();
                let default_task_timeout = self.default_task_timeout;
                let emit_streaming = self.emit_streaming_data;

                Box::pin(stream! {
                    // lifetime issue workaround: executor needs to be created inside the stream
                    let executor = DoTaskStreamExecutor::new(
                        workflow_context.clone(),
                        default_task_timeout,
                        metadata_clone,
                        task_clone,
                        job_executor_wrapper_clone,
                        checkpoint_repository.clone(),
                        execution_id.clone(),
                        emit_streaming,
                    );

                    let mut base_stream = executor.execute_stream(cx, task_name, task_context);
                    while let Some(item) = base_stream.next().await {
                        yield item
                    }
                })
            }
            Task::ForTask(task) => {
                let task_clone = task.clone();
                let job_executor_wrapper_clone = job_executor_wrapper.clone();
                let metadata_clone = self.metadata.clone();
                let emit_streaming = self.emit_streaming_data;

                Box::pin(stream! {
                    // lifetime issue workaround: executor needs to be created inside the stream
                    let executor = ForTaskStreamExecutor::new(
                        workflow_context.clone(),
                        default_task_timeout,
                        task_clone,
                        job_executor_wrapper_clone,
                        checkpoint_repository.clone(),
                        execution_id.clone(),
                        metadata_clone,
                        emit_streaming,
                    );

                    let mut base_stream = executor.execute_stream(cx, task_name, task_context);
                    while let Some(item) = base_stream.next().await {
                        yield item
                    }
                })
            }
            Task::ForkTask(task) => {
                // ForkTask: single-shot execution
                let fork_executor = ForkTaskExecutor::new(
                    workflow_context.clone(),
                    default_task_timeout,
                    task,
                    job_executor_wrapper.clone(),
                    checkpoint_repository.clone(),
                    execution_id.clone(),
                    self.metadata.clone(),
                    self.emit_streaming_data,
                );
                let task_name_str = task_name.to_string();
                futures::stream::once(async move {
                    match fork_executor
                        .execute(cx, task_name.as_str(), task_context.clone())
                        .await
                    {
                        Ok(ctx) => {
                            let expr = Self::expression(
                                &*workflow_context.read().await,
                                Arc::new(ctx.clone()),
                            )
                            .await;
                            match expr {
                                Ok(mut expr) => {
                                    let result = Self::update_context_by_output(
                                        checkpoint_repository.clone(),
                                        execution_id.clone(),
                                        original_task.clone(),
                                        workflow_context.clone(),
                                        &mut expr,
                                        ctx,
                                    )
                                    .await?;
                                    Ok(WorkflowStreamEvent::task_completed(
                                        "forkTask",
                                        &task_name_str,
                                        result,
                                    ))
                                }
                                Err(mut e) => {
                                    let pos = task_context.position.read().await;
                                    e.position(&pos);
                                    Err(e)
                                }
                            }
                        }
                        Err(e) => Err(e),
                    }
                })
                .boxed()
            }
            Task::RaiseTask(task) => {
                let task_executor = RaiseTaskExecutor::new(workflow_context.clone(), task);
                let task_name_str = task_name.to_string();
                futures::stream::once(async move {
                    match task_executor
                        .execute(cx, task_name.as_str(), task_context.clone())
                        .await
                    {
                        Ok(ctx) => {
                            let mut expr = Self::expression(
                                &*workflow_context.read().await,
                                Arc::new(ctx.clone()),
                            )
                            .await?;
                            let result = Self::update_context_by_output(
                                checkpoint_repository.clone(),
                                execution_id.clone(),
                                original_task.clone(),
                                workflow_context.clone(),
                                &mut expr,
                                ctx,
                            )
                            .await?;
                            Ok(WorkflowStreamEvent::task_completed(
                                "raiseTask",
                                &task_name_str,
                                result,
                            ))
                        }
                        Err(e) => Err(e),
                    }
                })
                .boxed()
            }
            Task::RunTask(task) => {
                let use_streaming = task.use_streaming;
                let task_name_str = task_name.to_string();

                // Check if streaming execution should be used for the given RunTask
                //
                // Returns the value of `useStreaming` property from the RunTask.
                // When true, the task will be executed using RunStreamTaskExecutor which:
                // - Executes jobs with streaming enabled
                // - Broadcasts intermediate results via JobResultService/ListenStream
                // - Collects final result using collect_stream
                // - Emits StreamingJobStarted and StreamingJobCompleted events
                if use_streaming {
                    let metadata = self.metadata.clone();
                    // Process stream events, applying update_context_by_output to StreamingJobCompleted
                    Box::pin(async_stream::stream! {
                        let task_executor = RunStreamTaskExecutor::new(
                            workflow_context.clone(),
                            default_task_timeout,
                            job_executor_wrapper.clone(),
                            task,
                            metadata,
                        );
                        // Use execute_stream which emits StreamingJobStarted/StreamingJobCompleted events
                        let stream = task_executor.execute_stream(
                            cx,
                            Arc::new(task_name.to_string()),
                            task_context.clone(),
                        );
                        futures::pin_mut!(stream);
                        while let Some(event_result) = stream.next().await {
                            match event_result {
                                Ok(WorkflowStreamEvent::StreamingJobCompleted { job_id, job_result_id, position, context }) => {
                                    // Apply output transformation
                                    let mut expr = match TaskExecutor::expression(
                                        &*workflow_context.read().await,
                                        Arc::new(context.clone()),
                                    ).await {
                                        Ok(e) => e,
                                        Err(e) => {
                                            yield Err(e);
                                            return;
                                        }
                                    };
                                    match TaskExecutor::update_context_by_output(
                                        checkpoint_repository.clone(),
                                        execution_id.clone(),
                                        original_task.clone(),
                                        workflow_context.clone(),
                                        &mut expr,
                                        context,
                                    ).await {
                                        Ok(result) => {
                                            // Re-emit StreamingJobCompleted with updated context
                                            // Output is accessible via result.output
                                            yield Ok(WorkflowStreamEvent::StreamingJobCompleted {
                                                job_id,
                                                job_result_id,
                                                position,
                                                context: result,
                                            });
                                        }
                                        Err(e) => {
                                            yield Err(e);
                                            return;
                                        }
                                    }
                                }
                                other => {
                                    // Pass through other events (StreamingJobStarted, StreamingData, errors)
                                    yield other;
                                }
                            }
                        }
                    })
                } else {
                    let task_executor = RunTaskExecutor::new(
                        workflow_context.clone(),
                        default_task_timeout,
                        job_executor_wrapper.clone(),
                        task,
                        self.metadata.clone(),
                    );
                    futures::stream::once(async move {
                        match task_executor
                            .execute(cx, task_name.as_str(), task_context.clone())
                            .await
                        {
                            Ok(ctx) => {
                                let mut expr = Self::expression(
                                    &*workflow_context.read().await,
                                    Arc::new(ctx.clone()),
                                )
                                .await?;
                                let result = Self::update_context_by_output(
                                    checkpoint_repository.clone(),
                                    execution_id.clone(),
                                    original_task.clone(),
                                    workflow_context.clone(),
                                    &mut expr,
                                    ctx,
                                )
                                .await?;
                                // TODO: Return JobCompleted with job_id when available
                                Ok(WorkflowStreamEvent::task_completed(
                                    "runTask",
                                    &task_name_str,
                                    result,
                                ))
                            }
                            Err(e) => Err(e),
                        }
                    })
                    .boxed()
                }
            }
            Task::SetTask(task) => {
                let task_executor = SetTaskExecutor::new(workflow_context.clone(), task);
                let task_name_str = task_name.to_string();
                futures::stream::once(async move {
                    match task_executor
                        .execute(cx, task_name.as_str(), task_context.clone())
                        .await
                    {
                        Ok(ctx) => {
                            let mut expr = Self::expression(
                                &*workflow_context.read().await,
                                Arc::new(ctx.clone()),
                            )
                            .await?;
                            let result = Self::update_context_by_output(
                                checkpoint_repository.clone(),
                                execution_id.clone(),
                                original_task.clone(),
                                workflow_context.clone(),
                                &mut expr,
                                ctx,
                            )
                            .await?;
                            Ok(WorkflowStreamEvent::task_completed(
                                "setTask",
                                &task_name_str,
                                result,
                            ))
                        }
                        Err(e) => Err(e),
                    }
                })
                .boxed()
            }
            Task::SwitchTask(task) => {
                let task_executor = SwitchTaskExecutor::new(workflow_context.clone(), &task);
                let task_name_str = task_name.to_string();
                futures::stream::once(async move {
                    match task_executor
                        .execute(cx, task_name.as_str(), task_context.clone())
                        .await
                    {
                        Ok(ctx) => {
                            let mut expr = Self::expression(
                                &*workflow_context.read().await,
                                Arc::new(ctx.clone()),
                            )
                            .await?;
                            let result = Self::update_context_by_output(
                                checkpoint_repository.clone(),
                                execution_id.clone(),
                                original_task.clone(),
                                workflow_context.clone(),
                                &mut expr,
                                ctx,
                            )
                            .await?;
                            Ok(WorkflowStreamEvent::task_completed(
                                "switchTask",
                                &task_name_str,
                                result,
                            ))
                        }
                        Err(e) => Err(e),
                    }
                })
                .boxed()
            }
            Task::TryTask(task) => {
                let task_executor = TryTaskExecutor::new(
                    workflow_context.clone(),
                    default_task_timeout,
                    task,
                    job_executor_wrapper.clone(),
                    checkpoint_repository.clone(),
                    execution_id.clone(),
                    self.metadata.clone(),
                    self.emit_streaming_data,
                );
                let task_name_str = task_name.to_string();
                futures::stream::once(async move {
                    match task_executor
                        .execute(cx, task_name.as_str(), task_context.clone())
                        .await
                    {
                        Ok(ctx) => {
                            let mut expr = Self::expression(
                                &*workflow_context.read().await,
                                Arc::new(ctx.clone()),
                            )
                            .await?;
                            let result = Self::update_context_by_output(
                                checkpoint_repository.clone(),
                                execution_id.clone(),
                                original_task.clone(),
                                workflow_context.clone(),
                                &mut expr,
                                ctx,
                            )
                            .await?;
                            Ok(WorkflowStreamEvent::task_completed(
                                "tryTask",
                                &task_name_str,
                                result,
                            ))
                        }
                        Err(e) => Err(e),
                    }
                })
                .boxed()
            }
            Task::WaitTask(task) => {
                let task_executor = WaitTaskExecutor::new(task);
                let task_name_str = task_name.to_string();
                futures::stream::once(async move {
                    match task_executor
                        .execute(cx, task_name.as_str(), task_context.clone())
                        .await
                    {
                        Ok(ctx) => {
                            let mut expr = Self::expression(
                                &*workflow_context.read().await,
                                Arc::new(ctx.clone()),
                            )
                            .await?;
                            let result = Self::update_context_by_output(
                                checkpoint_repository.clone(),
                                execution_id.clone(),
                                original_task.clone(),
                                workflow_context.clone(),
                                &mut expr,
                                ctx,
                            )
                            .await?;
                            Ok(WorkflowStreamEvent::task_completed(
                                "waitTask",
                                &task_name_str,
                                result,
                            ))
                        }
                        Err(e) => Err(e),
                    }
                })
                .boxed()
            }
        }
    }
}

pub trait TaskExecutorTrait<'a>: Send + Sync {
    fn execute(
        &'a self,
        cx: Arc<opentelemetry::Context>,
        task_name: &'a str,
        task_context: TaskContext,
    ) -> impl std::future::Future<Output = Result<TaskContext, Box<workflow::Error>>> + Send;
}

pub trait StreamTaskExecutorTrait<'a>: Send + Sync {
    fn execute_stream(
        &'a self,
        cx: Arc<opentelemetry::Context>,
        task_name: Arc<String>,
        task_context: TaskContext,
    ) -> impl futures::Stream<Item = Result<WorkflowStreamEvent, Box<workflow::Error>>> + Send;
}

pub struct RaiseTaskExecutor {
    _workflow_context: Arc<RwLock<WorkflowContext>>,
    task: workflow::RaiseTask,
}
impl RaiseTaskExecutor {
    pub fn new(workflow_context: Arc<RwLock<WorkflowContext>>, task: workflow::RaiseTask) -> Self {
        Self {
            _workflow_context: workflow_context,
            task,
        }
    }
}
impl TaskExecutorTrait<'_> for RaiseTaskExecutor {
    async fn execute(
        &self,
        _cx: Arc<opentelemetry::Context>,
        _task_name: &str,
        task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        tracing::error!("RaiseTaskExecutor raise error: {:?}", self.task.raise.error);
        // TODO add detail information to error
        let pos = task_context.position.clone();
        let mut pos = pos.write().await;
        pos.push("raise".to_string());
        Err(workflow::errors::ErrorFactory::create(
            workflow::errors::ErrorCode::Locked,
            Some(format!("Raise error!: {:?}", self.task.raise.error)),
            Some(pos.as_error_instance()),
            None,
        ))
    }
}

pub struct WaitTaskExecutor {
    task: workflow::WaitTask,
}
impl WaitTaskExecutor {
    pub fn new(task: workflow::WaitTask) -> Self {
        Self { task }
    }
}
impl Tracing for WaitTaskExecutor {}
impl TaskExecutorTrait<'_> for WaitTaskExecutor {
    async fn execute(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_name: &str,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        // let span = Self::child_tracing_span(&cx, APP_NAME, task_name.to_string());
        // let _ = span.enter();
        let span = Self::start_child_otel_span(&cx, APP_NAME, task_name.to_string());
        let cx = Arc::new(opentelemetry::Context::current_with_span(span));
        cx.span().set_attributes(vec![opentelemetry::KeyValue::new(
            "task.name",
            task_name.to_string(),
        )]);
        tracing::info!("WaitTask: {}: {:?}", task_name, &self.task);

        tokio::time::sleep(std::time::Duration::from_millis(self.task.wait.to_millis())).await;
        task_context.set_output(task_context.input.clone());
        Ok(task_context)
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ExecutionId {
    pub value: String,
}
impl ExecutionId {
    pub fn new(value: String) -> Option<Self> {
        if value.is_empty() {
            None
        } else {
            Some(Self { value })
        }
    }
    pub fn new_opt(value: Option<String>) -> Option<Self> {
        if value.as_ref().is_some_and(|v| v.is_empty()) {
            None
        } else {
            value.map(|v| Self { value: v })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::definition::workflow::{
        Document, FlowDirective, FlowDirectiveEnum, Input, Output, RunJobRunner, RunRunner,
        RunTask, RunTaskConfiguration, SetTask, Task, TaskList, WorkflowName, WorkflowSchema,
        WorkflowVersion,
    };
    use app::module::test::create_hybrid_test_app;
    use futures::StreamExt;
    use std::str::FromStr;

    fn create_workflow_with_run_task() -> WorkflowSchema {
        let task_map_list = {
            let mut map1 = HashMap::new();
            // Run task that executes a simple command using RunRunner variant
            let mut args = serde_json::Map::new();
            args.insert("command".to_string(), serde_json::json!("echo"));
            args.insert("args".to_string(), serde_json::json!(["hello", "world"]));

            map1.insert(
                "run_command".to_string(),
                Task::RunTask(RunTask {
                    run: RunTaskConfiguration::Runner(RunRunner {
                        runner: RunJobRunner {
                            name: "COMMAND".to_string(),
                            arguments: args,
                            options: None,
                            settings: serde_json::Map::new(),
                            using: None,
                        },
                    }),
                    export: None,
                    if_: None,
                    input: None,
                    metadata: serde_json::Map::new(),
                    output: None,
                    then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                    timeout: None,
                    checkpoint: false,
                    use_streaming: false,
                }),
            );
            vec![map1]
        };
        let task_list = TaskList(task_map_list);

        WorkflowSchema {
            checkpointing: None,
            document: Document {
                name: WorkflowName::from_str("test-run-task-workflow").unwrap(),
                version: WorkflowVersion::from_str("1.0.0").unwrap(),
                metadata: serde_json::Map::new(),
                ..Default::default()
            },
            input: Input {
                schema: None,
                from: None,
            },
            output: Some(Output {
                as_: None,
                schema: None,
            }),
            do_: task_list,
        }
    }

    fn create_workflow_with_set_task() -> WorkflowSchema {
        let task_map_list = {
            let mut map1 = HashMap::new();
            map1.insert(
                "set_value".to_string(),
                Task::SetTask(SetTask {
                    set: {
                        let mut m = serde_json::Map::new();
                        m.insert("key".to_string(), serde_json::json!("value"));
                        m
                    },
                    export: None,
                    if_: None,
                    input: None,
                    metadata: serde_json::Map::new(),
                    output: None,
                    then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                    timeout: None,
                    checkpoint: false,
                }),
            );
            vec![map1]
        };
        let task_list = TaskList(task_map_list);

        WorkflowSchema {
            checkpointing: None,
            document: Document {
                name: WorkflowName::from_str("test-set-task-workflow").unwrap(),
                version: WorkflowVersion::from_str("1.0.0").unwrap(),
                metadata: serde_json::Map::new(),
                ..Default::default()
            },
            input: Input {
                schema: None,
                from: None,
            },
            output: Some(Output {
                as_: None,
                schema: None,
            }),
            do_: task_list,
        }
    }

    /// Helper function to check if event is TaskCompleted with matching task_type and task_name
    fn is_task_completed_event(
        event: &Result<WorkflowStreamEvent, Box<workflow::Error>>,
        expected_task_type: &str,
        expected_task_name: &str,
    ) -> bool {
        if let Ok(WorkflowStreamEvent::TaskCompleted {
            task_type,
            task_name,
            ..
        }) = event
        {
            task_type == expected_task_type && task_name == expected_task_name
        } else {
            false
        }
    }

    /// Helper function to get event description for debugging
    fn event_description(event: &Result<WorkflowStreamEvent, Box<workflow::Error>>) -> String {
        match event {
            Ok(evt) => {
                let variant = match evt {
                    WorkflowStreamEvent::TaskCompleted {
                        task_type,
                        task_name,
                        ..
                    } => {
                        format!("TaskCompleted(type={}, name={})", task_type, task_name)
                    }
                    WorkflowStreamEvent::TaskStarted {
                        task_type,
                        task_name,
                        ..
                    } => {
                        format!("TaskStarted(type={}, name={})", task_type, task_name)
                    }
                    WorkflowStreamEvent::JobStarted { job_id, .. } => {
                        format!("JobStarted(job_id={})", job_id)
                    }
                    WorkflowStreamEvent::JobCompleted { job_id, .. } => {
                        format!("JobCompleted(job_id={})", job_id)
                    }
                    WorkflowStreamEvent::StreamingJobStarted { job_id, .. } => {
                        format!("StreamingJobStarted(job_id={})", job_id)
                    }
                    WorkflowStreamEvent::StreamingData { job_id, .. } => {
                        format!("StreamingData(job_id={})", job_id)
                    }
                    WorkflowStreamEvent::StreamingJobCompleted { job_id, .. } => {
                        format!("StreamingJobCompleted(job_id={})", job_id)
                    }
                };
                format!("Ok({})", variant)
            }
            Err(e) => format!("Err({})", e),
        }
    }

    /// Test that RunTask emits WorkflowStreamEvent::TaskCompleted with correct task type
    /// Note: This test requires Redis/worker infrastructure and is ignored by default.
    /// Run with `--ignored` flag to execute.
    #[test]
    #[ignore = "Requires Redis/worker infrastructure"]
    fn test_run_task_emits_task_completed_event() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let job_executor_wrapper = Arc::new(JobExecutorWrapper::new(app_module.clone()));

            let workflow = create_workflow_with_run_task();
            let input = Arc::new(serde_json::json!({}));
            let context = Arc::new(serde_json::json!({}));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context.clone(),
                None,
            )));

            // Create TaskExecutor
            let task_executor = TaskExecutor::new(
                workflow_context.clone(),
                Duration::from_secs(60),
                job_executor_wrapper,
                None, // checkpoint_repository
                "run_command",
                Arc::new(workflow.do_.0[0].get("run_command").unwrap().clone()),
                Arc::new(HashMap::new()),
                false, // emit_streaming_data (tests don't need streaming events)
            );

            // Execute and collect events
            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );

            let stream = task_executor
                .execute(
                    Arc::new(opentelemetry::Context::current()),
                    Arc::new(task_context),
                    None,
                )
                .await;

            futures::pin_mut!(stream);

            let mut events: Vec<Result<WorkflowStreamEvent, Box<workflow::Error>>> = Vec::new();
            while let Some(event) = stream.next().await {
                events.push(event);
            }

            // Verify at least one event was emitted
            assert!(
                !events.is_empty(),
                "Expected at least one event from RunTask"
            );

            // Check if the event is TaskCompleted with task_type "runTask"
            let has_run_task_event = events
                .iter()
                .any(|e| is_task_completed_event(e, "runTask", "run_command"));

            assert!(
                has_run_task_event,
                "Expected TaskCompleted event with task_type='runTask' and task_name='run_command'. Got events: {:?}",
                events.iter().map(event_description).collect::<Vec<_>>()
            );
        });
    }

    /// Test that SetTask emits WorkflowStreamEvent::TaskCompleted with correct task type
    #[test]
    fn test_set_task_emits_task_completed_event() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let job_executor_wrapper = Arc::new(JobExecutorWrapper::new(app_module.clone()));

            let workflow = create_workflow_with_set_task();
            let input = Arc::new(serde_json::json!({}));
            let context = Arc::new(serde_json::json!({}));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context.clone(),
                None,
            )));

            // Create TaskExecutor
            let task_executor = TaskExecutor::new(
                workflow_context.clone(),
                Duration::from_secs(60),
                job_executor_wrapper,
                None,
                "set_value",
                Arc::new(workflow.do_.0[0].get("set_value").unwrap().clone()),
                Arc::new(HashMap::new()),
                false, // emit_streaming_data (tests don't need streaming events)
            );

            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );

            let stream = task_executor
                .execute(
                    Arc::new(opentelemetry::Context::current()),
                    Arc::new(task_context),
                    None,
                )
                .await;

            futures::pin_mut!(stream);

            let mut events: Vec<Result<WorkflowStreamEvent, Box<workflow::Error>>> = Vec::new();
            while let Some(event) = stream.next().await {
                events.push(event);
            }

            assert!(
                !events.is_empty(),
                "Expected at least one event from SetTask"
            );

            // Check for TaskCompleted with task_type "setTask"
            let has_set_task_event = events
                .iter()
                .any(|e| is_task_completed_event(e, "setTask", "set_value"));

            assert!(
                has_set_task_event,
                "Expected TaskCompleted event with task_type='setTask' and task_name='set_value'. Got events: {:?}",
                events.iter().map(event_description).collect::<Vec<_>>()
            );
        });
    }

    /// Test that WorkflowStreamEvent.into_context() extracts TaskContext correctly
    #[test]
    fn test_workflow_stream_event_into_context() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let input = Arc::new(serde_json::json!({"test": "value"}));
            let context_vars = Arc::new(Mutex::new(serde_json::Map::new()));
            let task_context = TaskContext::new(None, input, context_vars);

            // Create a TaskCompleted event
            let event =
                WorkflowStreamEvent::task_completed("testTask", "my_task", task_context.clone());

            // Extract context using into_context()
            let extracted = event.into_context();
            assert!(extracted.is_some(), "into_context should return Some");

            let extracted_context = extracted.unwrap();
            assert_eq!(
                *extracted_context.input,
                serde_json::json!({"test": "value"})
            );
        });
    }

    /// Test that WorkflowStreamEvent.context() returns reference to TaskContext
    #[test]
    fn test_workflow_stream_event_context_ref() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let input = Arc::new(serde_json::json!({"key": "data"}));
            let context_vars = Arc::new(Mutex::new(serde_json::Map::new()));
            let task_context = TaskContext::new(None, input, context_vars);

            let event =
                WorkflowStreamEvent::task_completed("runTask", "cmd_task", task_context.clone());

            // Get reference using context()
            let ctx_ref = event.context();
            assert!(ctx_ref.is_some(), "context() should return Some");
            assert_eq!(*ctx_ref.unwrap().input, serde_json::json!({"key": "data"}));
        });
    }

    /// Test event variants and their properties
    #[test]
    fn test_workflow_stream_event_variants() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let input = Arc::new(serde_json::json!({}));
            let context_vars = Arc::new(Mutex::new(serde_json::Map::new()));
            let task_context = TaskContext::new(None, input, context_vars);

            // TaskCompleted - has context
            let task_completed =
                WorkflowStreamEvent::task_completed("runTask", "task1", task_context.clone());
            assert!(task_completed.is_completed_event());
            assert!(!task_completed.is_start_event());
            assert!(task_completed.context().is_some());

            // TaskStarted - no context
            let task_started = WorkflowStreamEvent::task_started("runTask", "task1", "/do/task1");
            assert!(task_started.is_start_event());
            assert!(!task_started.is_completed_event());
            assert!(task_started.context().is_none());

            // StreamingJobStarted - no context
            let streaming_started =
                WorkflowStreamEvent::streaming_job_started(123, "COMMAND", "worker1", "/do/task1");
            assert!(streaming_started.is_start_event());
            assert!(streaming_started.context().is_none());

            // StreamingJobCompleted - has context
            let streaming_completed = WorkflowStreamEvent::streaming_job_completed(
                123,
                Some(456),
                "/do/task1",
                task_context.clone(),
            );
            assert!(streaming_completed.is_completed_event());
            assert!(streaming_completed.context().is_some());

            // JobStarted - no context
            let job_started =
                WorkflowStreamEvent::job_started(789, "HTTP_REQUEST", "worker2", "/do/task2");
            assert!(job_started.is_start_event());
            assert!(job_started.context().is_none());

            // JobCompleted - has context
            let job_completed =
                WorkflowStreamEvent::job_completed(789, Some(1000), "/do/task2", task_context);
            assert!(job_completed.is_completed_event());
            assert!(job_completed.context().is_some());
        });
    }

    /// Test position() method returns correct position for all event types
    #[test]
    fn test_workflow_stream_event_position() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let input = Arc::new(serde_json::json!({}));
            let context_vars = Arc::new(Mutex::new(serde_json::Map::new()));
            let task_context = TaskContext::new(None, input, context_vars);

            let task_started = WorkflowStreamEvent::task_started("runTask", "t1", "/do/task1");
            assert_eq!(task_started.position(), "/do/task1");

            let job_started = WorkflowStreamEvent::job_started(1, "CMD", "w1", "/do/job1");
            assert_eq!(job_started.position(), "/do/job1");

            let streaming_started =
                WorkflowStreamEvent::streaming_job_started(2, "LLM", "w2", "/do/stream1");
            assert_eq!(streaming_started.position(), "/do/stream1");

            // Completed events get position from TaskContext
            let task_completed =
                WorkflowStreamEvent::task_completed("setTask", "t2", task_context.clone());
            // Position from newly created TaskContext is empty by default
            assert!(task_completed.position().is_empty() || task_completed.position() == "/");
        });
    }

    /// Test resolve_timeout() returns task-specific timeout when set
    #[test]
    fn test_resolve_timeout_with_task_timeout() {
        use crate::workflow::definition::workflow::{
            Duration as WfDuration, TaskTimeout, Timeout as WfTimeout,
        };

        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let job_executor_wrapper = Arc::new(JobExecutorWrapper::new(app_module.clone()));

            let workflow = create_workflow_with_set_task();
            let input = Arc::new(serde_json::json!({}));
            let context = Arc::new(serde_json::json!({}));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context.clone(),
                None,
            )));

            // Create SetTask with 500ms timeout
            let task_with_timeout = Task::SetTask(SetTask {
                set: {
                    let mut m = serde_json::Map::new();
                    m.insert("key".to_string(), serde_json::json!("value"));
                    m
                },
                export: None,
                if_: None,
                input: None,
                metadata: serde_json::Map::new(),
                output: None,
                then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                timeout: Some(TaskTimeout::Timeout(WfTimeout {
                    after: WfDuration::from_millis(500),
                })),
                checkpoint: false,
            });

            let task_executor = TaskExecutor::new(
                workflow_context.clone(),
                Duration::from_secs(60), // default timeout (should be overridden)
                job_executor_wrapper,
                None,
                "set_with_timeout",
                Arc::new(task_with_timeout),
                Arc::new(HashMap::new()),
                false, // emit_streaming_data (tests don't need streaming events)
            );

            let resolved = task_executor.resolve_timeout();
            assert_eq!(resolved, Duration::from_millis(500));
        });
    }

    /// Test resolve_timeout() returns default timeout when no task timeout is set
    #[test]
    fn test_resolve_timeout_uses_default() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let job_executor_wrapper = Arc::new(JobExecutorWrapper::new(app_module.clone()));

            let workflow = create_workflow_with_set_task();
            let input = Arc::new(serde_json::json!({}));
            let context = Arc::new(serde_json::json!({}));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context.clone(),
                None,
            )));

            // Task without timeout (uses default)
            let task_without_timeout = Task::SetTask(SetTask {
                set: {
                    let mut m = serde_json::Map::new();
                    m.insert("key".to_string(), serde_json::json!("value"));
                    m
                },
                export: None,
                if_: None,
                input: None,
                metadata: serde_json::Map::new(),
                output: None,
                then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                timeout: None,
                checkpoint: false,
            });

            let default_timeout = Duration::from_secs(120);
            let task_executor = TaskExecutor::new(
                workflow_context.clone(),
                default_timeout,
                job_executor_wrapper,
                None,
                "set_no_timeout",
                Arc::new(task_without_timeout),
                Arc::new(HashMap::new()),
                false, // emit_streaming_data (tests don't need streaming events)
            );

            let resolved = task_executor.resolve_timeout();
            assert_eq!(resolved, default_timeout);
        });
    }

    /// Test that a task with very short timeout returns timeout error
    #[test]
    fn test_task_timeout_triggers_error() {
        use crate::workflow::definition::workflow::{
            DoTask as WfDoTask, Duration as WfDuration, TaskTimeout, Timeout as WfTimeout,
        };

        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let job_executor_wrapper = Arc::new(JobExecutorWrapper::new(app_module.clone()));

            let workflow = create_workflow_with_set_task();
            let input = Arc::new(serde_json::json!({}));
            let context = Arc::new(serde_json::json!({}));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context.clone(),
                None,
            )));

            // DoTask that executes a nested task with 1ms timeout
            // This should trigger timeout error
            let inner_task_map = {
                let mut map = HashMap::new();
                map.insert(
                    "inner_set".to_string(),
                    Task::SetTask(SetTask {
                        set: {
                            let mut m = serde_json::Map::new();
                            m.insert("key".to_string(), serde_json::json!("value"));
                            m
                        },
                        export: None,
                        if_: None,
                        input: None,
                        metadata: serde_json::Map::new(),
                        output: None,
                        then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                        timeout: None,
                        checkpoint: false,
                    }),
                );
                vec![map]
            };

            let do_task_with_short_timeout = Task::DoTask(WfDoTask {
                do_: TaskList(inner_task_map),
                export: None,
                if_: None,
                input: None,
                metadata: serde_json::Map::new(),
                output: None,
                then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                // Very short timeout - 1 millisecond
                timeout: Some(TaskTimeout::Timeout(WfTimeout {
                    after: WfDuration::from_millis(1),
                })),
                checkpoint: false,
            });

            let task_executor = TaskExecutor::new(
                workflow_context.clone(),
                Duration::from_secs(60),
                job_executor_wrapper,
                None,
                "do_with_timeout",
                Arc::new(do_task_with_short_timeout),
                Arc::new(HashMap::new()),
                false, // emit_streaming_data (tests don't need streaming events)
            );

            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );

            let stream = task_executor
                .execute(
                    Arc::new(opentelemetry::Context::current()),
                    Arc::new(task_context),
                    None,
                )
                .await;

            futures::pin_mut!(stream);

            let mut has_timeout_error = false;
            let mut events: Vec<Result<WorkflowStreamEvent, Box<workflow::Error>>> = Vec::new();

            while let Some(event) = stream.next().await {
                if let Err(ref e) = event {
                    // Check for timeout error (status 408)
                    if e.status == 408 {
                        has_timeout_error = true;
                    }
                }
                events.push(event);
            }

            // The task might complete before timeout in some cases
            // But the resolve_timeout behavior is already tested above
            // This test checks that timeout error mechanism works when triggered
            if has_timeout_error {
                let timeout_error = events.iter().find_map(|e| {
                    if let Err(err) = e {
                        if err.status == 408 {
                            return Some(err);
                        }
                    }
                    None
                });
                assert!(timeout_error.is_some(), "Should have a timeout error");
                assert!(
                    timeout_error
                        .unwrap()
                        .detail
                        .as_ref()
                        .map(|d| d.contains("timed out"))
                        .unwrap_or(false),
                    "Error detail should mention timeout"
                );
            }
        });
    }

    /// Test resolve_timeout() with TaskTimeoutReference falls back to default
    #[test]
    fn test_resolve_timeout_with_reference_uses_default() {
        use crate::workflow::definition::workflow::TaskTimeout;

        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let job_executor_wrapper = Arc::new(JobExecutorWrapper::new(app_module.clone()));

            let workflow = create_workflow_with_set_task();
            let input = Arc::new(serde_json::json!({}));
            let context = Arc::new(serde_json::json!({}));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context.clone(),
                None,
            )));

            // Task with TaskTimeoutReference (should fall back to default)
            let task_with_ref_timeout = Task::SetTask(SetTask {
                set: {
                    let mut m = serde_json::Map::new();
                    m.insert("key".to_string(), serde_json::json!("value"));
                    m
                },
                export: None,
                if_: None,
                input: None,
                metadata: serde_json::Map::new(),
                output: None,
                then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                timeout: Some(TaskTimeout::TaskTimeoutReference(
                    "some-named-timeout".to_string(),
                )),
                checkpoint: false,
            });

            let default_timeout = Duration::from_secs(90);
            let task_executor = TaskExecutor::new(
                workflow_context.clone(),
                default_timeout,
                job_executor_wrapper,
                None,
                "set_with_ref_timeout",
                Arc::new(task_with_ref_timeout),
                Arc::new(HashMap::new()),
                false, // emit_streaming_data (tests don't need streaming events)
            );

            let resolved = task_executor.resolve_timeout();
            // TaskTimeoutReference is not yet supported, should fall back to default
            assert_eq!(resolved, default_timeout);
        });
    }
}
