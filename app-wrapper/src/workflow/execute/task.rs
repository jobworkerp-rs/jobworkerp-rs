use super::{
    context::{TaskContext, WorkflowContext},
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
use run::{RunStreamTaskExecutor, RunTaskExecutor};
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
    ) -> futures::stream::BoxStream<'static, Result<TaskContext, Box<workflow::Error>>> {
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
                return futures::stream::once(futures::future::ready(Ok(task_context))).boxed();
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
                        task_context.remove_position().await; // remove task_name
                        task_context.set_completed_at();
                        return futures::stream::once(futures::future::ready(Ok(task_context)))
                            .boxed();
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

        let res = self
            .execute_task(cx, task_context, execution_id.clone())
            .await;
        // remove task position after execution (last task context)
        Box::pin(stream! {
            pin_mut!(res);
            let mut previous_item: Option<Result<TaskContext, Box<workflow::Error>>> = res.next().await;
            if previous_item.is_none() {
                // If no item is returned, we yield an empty result
                yield Err(workflow::errors::ErrorFactory::create(
                    workflow::errors::ErrorCode::NotFound,
                    Some("No task context returned".to_string()),
                    Some(err_pos),
                    None,
                ));
            } else {
                while let Some(current_item) = res.next().await {
                    // If we have a previous item, yield it
                    if let Some(prev) = previous_item.take() {
                        yield prev;
                    }
                    // Store current item as previous for next iteration
                    previous_item = Some(current_item);
                }

                // Handle the final item - call remove_position() only if it's Ok
                if let Some(final_item) = previous_item {
                    if let Ok(tc) = final_item {
                        tc.remove_position().await; // remove task name from position stack
                        yield Ok(tc);
                    } else {
                        tracing::info!("Final item is an error: {:#?}", final_item);
                        // If the final item is an error, yield it as is
                        yield final_item;
                    }
                }
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
    ) -> futures::stream::BoxStream<'static, Result<TaskContext, Box<workflow::Error>>> {
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
                );
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
                                    Self::update_context_by_output(
                                        checkpoint_repository.clone(),
                                        execution_id.clone(),
                                        original_task.clone(),
                                        workflow_context.clone(),
                                        &mut expr,
                                        ctx,
                                    )
                                    .await
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
                            Self::update_context_by_output(
                                checkpoint_repository.clone(),
                                execution_id.clone(),
                                original_task.clone(),
                                workflow_context.clone(),
                                &mut expr,
                                ctx,
                            )
                            .await
                        }
                        Err(e) => Err(e),
                    }
                })
                .boxed()
            }
            Task::RunTask(task) => {
                let use_streaming = task.use_streaming;

                // Check if streaming execution should be used for the given RunTask
                //
                // Returns the value of `useStreaming` property from the RunTask.
                // When true, the task will be executed using RunStreamTaskExecutor which:
                // - Executes jobs with streaming enabled
                // - Broadcasts intermediate results via JobResultService/ListenStream
                // - Collects final result using collect_stream
                if use_streaming {
                    let task_executor = RunStreamTaskExecutor::new(
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
                                Self::update_context_by_output(
                                    checkpoint_repository.clone(),
                                    execution_id.clone(),
                                    original_task.clone(),
                                    workflow_context.clone(),
                                    &mut expr,
                                    ctx,
                                )
                                .await
                            }
                            Err(e) => Err(e),
                        }
                    })
                    .boxed()
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
                                Self::update_context_by_output(
                                    checkpoint_repository.clone(),
                                    execution_id.clone(),
                                    original_task.clone(),
                                    workflow_context.clone(),
                                    &mut expr,
                                    ctx,
                                )
                                .await
                            }
                            Err(e) => Err(e),
                        }
                    })
                    .boxed()
                }
            }
            Task::SetTask(task) => {
                let task_executor = SetTaskExecutor::new(workflow_context.clone(), task);
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
                            Self::update_context_by_output(
                                checkpoint_repository.clone(),
                                execution_id.clone(),
                                original_task.clone(),
                                workflow_context.clone(),
                                &mut expr,
                                ctx,
                            )
                            .await
                        }
                        Err(e) => Err(e),
                    }
                })
                .boxed()
            }
            Task::SwitchTask(task) => {
                let task_executor = SwitchTaskExecutor::new(workflow_context.clone(), &task);
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
                            Self::update_context_by_output(
                                checkpoint_repository.clone(),
                                execution_id.clone(),
                                original_task.clone(),
                                workflow_context.clone(),
                                &mut expr,
                                ctx,
                            )
                            .await
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
                            Self::update_context_by_output(
                                checkpoint_repository.clone(),
                                execution_id.clone(),
                                original_task.clone(),
                                workflow_context.clone(),
                                &mut expr,
                                ctx,
                            )
                            .await
                        }
                        Err(e) => Err(e),
                    }
                })
                .boxed()
            }
            Task::WaitTask(task) => {
                let task_executor = WaitTaskExecutor::new(task);
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
                            Self::update_context_by_output(
                                checkpoint_repository.clone(),
                                execution_id.clone(),
                                original_task.clone(),
                                workflow_context.clone(),
                                &mut expr,
                                ctx,
                            )
                            .await
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
    ) -> impl futures::Stream<Item = Result<TaskContext, Box<workflow::Error>>> + Send;
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
