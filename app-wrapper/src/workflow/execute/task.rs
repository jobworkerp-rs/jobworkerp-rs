use super::{
    context::{TaskContext, WorkflowContext},
    expression::UseExpression,
    job::JobExecutorWrapper,
};
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self, tasks::TaskTrait, Task},
    },
    execute::context::{Then, WorkflowStatus},
};
use anyhow::Result;
use async_stream::stream;
use debug_stub_derive::DebugStub;
use fork::ForkTaskExecutor;
use futures::{pin_mut, StreamExt};
use infra_utils::infra::{net::reqwest, trace::Tracing};
use jobworkerp_base::APP_NAME;
use opentelemetry::trace::TraceContextExt;
use run::RunTaskExecutor;
use set::SetTaskExecutor;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
use stream::{do_::DoTaskStreamExecutor, for_::ForTaskStreamExecutor};
use switch::SwitchTaskExecutor;
use tokio::sync::RwLock;
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

#[derive(DebugStub, Clone)]
pub struct TaskExecutor {
    #[debug_stub = "AppModule"]
    pub job_executor_wrapper: Arc<JobExecutorWrapper>,
    #[debug_stub = "reqwest::HttpClient"]
    pub http_client: reqwest::ReqwestClient,
    pub task_name: String,
    pub task: Arc<Task>,
    pub metadata: Arc<HashMap<String, String>>,
}
impl UseExpression for TaskExecutor {}
impl UseJqAndTemplateTransformer for TaskExecutor {}
impl UseExpressionTransformer for TaskExecutor {}

impl TaskExecutor {
    pub fn new(
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        http_client: reqwest::ReqwestClient,
        task_name: &str,
        task: Arc<Task>,
        metadata: Arc<HashMap<String, String>>,
    ) -> Self {
        // command_utils::util::tracing::tracing_init_test(Level::DEBUG);
        Self {
            job_executor_wrapper,
            http_client,
            task_name: task_name.to_owned(),
            task,
            metadata,
        }
    }
    // input is the output of the previous task
    // return the output of the task, and next task("continue", "exit", "end", or taskName)
    // TODO timeout implementation
    pub async fn execute(
        &self,
        cx: Arc<opentelemetry::Context>,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        parent_task: Arc<TaskContext>,
    ) -> futures::stream::BoxStream<'static, Result<TaskContext, Box<workflow::Error>>> {
        let input = parent_task.output.clone();
        let mut task_context = TaskContext::new(
            Some(self.task.clone()),
            input.clone(),
            parent_task.context_variables.clone(),
        );
        task_context.position = parent_task.position.clone();
        // enter task (add task name to position stack)
        // XXX Do not forget to remove the position after task execution (difficult to debug)
        task_context.add_position_name(self.task_name.clone()).await;

        let expression = Self::expression(
            &*workflow_context.read().await,
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
                    tracing::error!("Failed to evaluate `if' condition: {:#?}", e);
                    task_context.add_position_name("if".to_string()).await;
                    let pos = task_context.position.read().await;
                    e.position(&pos);
                    drop(pos);
                    task_context.set_completed_at();
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

        let res = self
            .execute_task(cx, workflow_context.clone(), task_context)
            .await;
        // remove task position after execution (last task context)
        Box::pin(stream! {
            pin_mut!(res);
            let mut previous_item: Option<Result<TaskContext, Box<workflow::Error>>> = None;

            while let Some(current_item) = res.next().await {
                // If we have a previous item, yield it
                if let Some(prev) = previous_item.take() {
                    yield prev;
                }
                // Store current item as previous for next iteration
                previous_item = Some(current_item);
            }

            // Handle the final item - call remove_position() only if it's Ok
            if let Some(mut final_item) = previous_item {
                if let Ok(ref mut tc) = final_item {
                    tc.remove_position().await; // remove task name from position stack
                }
                yield final_item;
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
        // Validate input schema
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
                        tracing::error!("Failed to transform output: {:#?}", e);
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("output".to_string());
                        e.position(&pos);
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

        task_context.set_completed_at();
        Ok(task_context)
    }

    async fn execute_task(
        &self,
        cx: Arc<opentelemetry::Context>,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> futures::stream::BoxStream<'static, Result<TaskContext, Box<workflow::Error>>> {
        // Prepare owned data for 'static futures
        let job_executor_wrapper = self.job_executor_wrapper.clone();
        let http_client = self.http_client.clone();
        let task_name = Arc::new(self.task_name.clone());
        // Clone Task enum to own inner data
        let task_enum = (*self.task).clone(); // XXX hard clone
        let original_task = self.task.clone(); // XXX hard clone

        // Dispatch based on owned Task
        // need to update output context after task execution (not streaming task depends on the other kind tasks)
        match task_enum {
            Task::DoTask(task) => {
                let task_clone = task.clone();
                let job_executor_wrapper_clone = job_executor_wrapper.clone();
                let http_client_clone = http_client.clone();
                let metadata_clone = self.metadata.clone();

                Box::pin(stream! {
                    // lifetime issue workaround: executor needs to be created inside the stream
                    let executor = DoTaskStreamExecutor::new(
                        metadata_clone,
                        task_clone,
                        job_executor_wrapper_clone,
                        http_client_clone,
                    );

                    let mut base_stream = executor.execute_stream(cx, task_name, workflow_context, task_context);
                    while let Some(item) = base_stream.next().await {
                        yield item
                    }
                })
            }
            Task::ForTask(task) => {
                let task_clone = task.clone();
                let job_executor_wrapper_clone = job_executor_wrapper.clone();
                let http_client_clone = http_client.clone();
                let metadata_clone = self.metadata.clone();

                Box::pin(stream! {
                    // lifetime issue workaround: executor needs to be created inside the stream
                    let executor = ForTaskStreamExecutor::new(
                        task_clone,
                        job_executor_wrapper_clone,
                        http_client_clone,
                        metadata_clone,
                    );

                    let mut base_stream = executor.execute_stream(cx, task_name, workflow_context, task_context);
                    while let Some(item) = base_stream.next().await {
                        yield item
                    }
                })
            }
            Task::ForkTask(task) => {
                // ForkTask: single-shot execution
                let fork_executor = ForkTaskExecutor::new(
                    task,
                    job_executor_wrapper.clone(),
                    http_client.clone(),
                    self.metadata.clone(),
                );
                futures::stream::once(async move {
                    match fork_executor
                        .execute(
                            cx,
                            task_name.as_str(),
                            workflow_context.clone(),
                            task_context.clone(),
                        )
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
                let task_executor = RaiseTaskExecutor::new(task);
                futures::stream::once(async move {
                    match task_executor
                        .execute(
                            cx,
                            task_name.as_str(),
                            workflow_context.clone(),
                            task_context.clone(),
                        )
                        .await
                    {
                        Ok(ctx) => {
                            let mut expr = Self::expression(
                                &*workflow_context.read().await,
                                Arc::new(ctx.clone()),
                            )
                            .await?;
                            Self::update_context_by_output(
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
                let task_executor =
                    RunTaskExecutor::new(job_executor_wrapper.clone(), task, self.metadata.clone());
                futures::stream::once(async move {
                    match task_executor
                        .execute(
                            cx,
                            task_name.as_str(),
                            workflow_context.clone(),
                            task_context.clone(),
                        )
                        .await
                    {
                        Ok(ctx) => {
                            let mut expr = Self::expression(
                                &*workflow_context.read().await,
                                Arc::new(ctx.clone()),
                            )
                            .await?;
                            Self::update_context_by_output(
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
            Task::SetTask(task) => {
                let task_executor = SetTaskExecutor::new(task);
                futures::stream::once(async move {
                    match task_executor
                        .execute(
                            cx,
                            task_name.as_str(),
                            workflow_context.clone(),
                            task_context.clone(),
                        )
                        .await
                    {
                        Ok(ctx) => {
                            let mut expr = Self::expression(
                                &*workflow_context.read().await,
                                Arc::new(ctx.clone()),
                            )
                            .await?;
                            Self::update_context_by_output(
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
                let task_executor = SwitchTaskExecutor::new(&task);
                futures::stream::once(async move {
                    match task_executor
                        .execute(
                            cx,
                            task_name.as_str(),
                            workflow_context.clone(),
                            task_context.clone(),
                        )
                        .await
                    {
                        Ok(ctx) => {
                            let mut expr = Self::expression(
                                &*workflow_context.read().await,
                                Arc::new(ctx.clone()),
                            )
                            .await?;
                            Self::update_context_by_output(
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
                    task,
                    job_executor_wrapper.clone(),
                    http_client.clone(),
                    self.metadata.clone(),
                );
                futures::stream::once(async move {
                    match task_executor
                        .execute(
                            cx,
                            task_name.as_str(),
                            workflow_context.clone(),
                            task_context.clone(),
                        )
                        .await
                    {
                        Ok(ctx) => {
                            let mut expr = Self::expression(
                                &*workflow_context.read().await,
                                Arc::new(ctx.clone()),
                            )
                            .await?;
                            Self::update_context_by_output(
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
                        .execute(
                            cx,
                            task_name.as_str(),
                            workflow_context.clone(),
                            task_context.clone(),
                        )
                        .await
                    {
                        Ok(ctx) => {
                            let mut expr = Self::expression(
                                &*workflow_context.read().await,
                                Arc::new(ctx.clone()),
                            )
                            .await?;
                            Self::update_context_by_output(
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
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> impl std::future::Future<Output = Result<TaskContext, Box<workflow::Error>>> + Send;
}

pub trait StreamTaskExecutorTrait<'a>: Send + Sync {
    fn execute_stream(
        &'a self,
        cx: Arc<opentelemetry::Context>,
        task_name: Arc<String>,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> impl futures::Stream<Item = Result<TaskContext, Box<workflow::Error>>> + Send;
}

pub struct RaiseTaskExecutor {
    task: workflow::RaiseTask,
}
impl RaiseTaskExecutor {
    pub fn new(task: workflow::RaiseTask) -> Self {
        Self { task }
    }
}
impl TaskExecutorTrait<'_> for RaiseTaskExecutor {
    async fn execute(
        &self,
        _cx: Arc<opentelemetry::Context>,
        _task_name: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        tracing::error!("RaiseTaskExecutor raise error: {:?}", self.task.raise.error);
        // TODO add error detail information to workflow_context
        let pos = task_context.position.clone();
        let mut pos = pos.write().await;
        pos.push("raise".to_string());
        workflow_context.write().await.status = WorkflowStatus::Faulted;
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
        _workflow_context: Arc<RwLock<WorkflowContext>>,
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
