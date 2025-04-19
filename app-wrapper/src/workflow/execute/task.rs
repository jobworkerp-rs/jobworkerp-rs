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
use call::CallTaskExecutor;
use debug_stub_derive::DebugStub;
use do_::DoTaskExecutor;
use for_::ForTaskExecutor;
use fork::ForkTaskExecutor;
use infra_utils::infra::net::reqwest;
use run::RunTaskExecutor;
use set::SetTaskExecutor;
use std::{collections::BTreeMap, sync::Arc};
use switch::SwitchTaskExecutor;
use tokio::sync::RwLock;
// use tracing::Level;
use try_::TryTaskExecutor;

pub mod call;
#[path = "task/do.rs"]
pub mod do_;
#[path = "task/for.rs"]
pub mod for_;
pub mod fork;
pub mod run;
pub mod set;
pub mod switch;
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
    ) -> Self {
        // command_utils::util::tracing::tracing_init_test(Level::DEBUG);
        Self {
            job_executor_wrapper,
            http_client,
            task_name: task_name.to_owned(),
            task,
        }
    }
    // input is the output of the previous task
    // return the output of the task, and next task("continue", "exit", "end", or taskName)
    // TODO timeout implementation
    pub async fn execute(
        &self,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        parent_task: Arc<TaskContext>,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        let input = parent_task.output.clone();
        let mut task_context = TaskContext::new(
            Some(self.task.clone()),
            input.clone(),
            parent_task.context_variables.clone(),
        );
        task_context.position = parent_task.position.clone();
        // enter task (add task name to position stack)
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
                task_context.flow_directive = Then::Exit;
                e.position(&task_context.position.lock().await.clone());
                return Err(e);
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
                        task_context.remove_position().await;
                        return Ok(task_context);
                    }
                }
                Err(mut e) => {
                    tracing::error!("Failed to evaluate `if' condition: {:#?}", e);
                    task_context.add_position_name("if".to_string()).await;
                    e.position(&task_context.position.lock().await.clone());
                    return Err(e);
                }
            }
        }

        // Transform input and update task context
        let task_context = self
            .update_context_by_input(&expression, task_context)
            .await?;

        // Execute task
        let task_context = self
            .execute_task(workflow_context.clone(), task_context)
            .await?;

        // Transform output and export
        let mut task_context = self
            .update_context_by_output(workflow_context, &expression, task_context)
            .await?;

        // Determine next task
        task_context.flow_directive = match self.task.then() {
            Some(flow) => Then::from(flow.clone()),
            None => Then::Continue,
        };
        // go out of the task
        task_context.remove_position().await;
        Ok(task_context)
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
                    let mut pos = task_context.position.lock().await.clone();
                    pos.push("input".to_string());
                    return Err(workflow::errors::ErrorFactory::new().bad_argument(
                        m,
                        Some(&pos),
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
        &self,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        expression: &BTreeMap<String, Arc<serde_json::Value>>,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        // Transform output
        tracing::debug!("Task raw output: {:#?}", task_context.raw_output);
        if let Some(as_) = self.task.output().and_then(|o| o.as_.as_ref()) {
            task_context.output =
                match Self::transform_output(task_context.raw_output.clone(), as_, expression) {
                    Ok(v) => v,
                    Err(mut e) => {
                        let mut pos = task_context.position.lock().await.clone();
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

        // export output to workflow context
        if let Some(export) = self.task.export().and_then(|o| o.as_.as_ref()) {
            let export = Self::transform_export(task_context.output.clone(), export, expression)?;
            tracing::debug!("Transformed export: {:#?}", &export);
            match export.as_ref() {
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
            }
        }

        // Determine next task
        task_context.flow_directive = match self.task.then() {
            Some(flow) => Then::from(flow.clone()),
            None => Then::Continue,
        };
        // go out of the task
        task_context.remove_position().await;
        Ok(task_context)
    }

    //
    async fn execute_task(
        &self,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        match self.task.as_ref() {
            Task::CallTask(task) => {
                let task_executor = CallTaskExecutor::new(
                    task,
                    self.http_client.clone(),
                    self.job_executor_wrapper.clone(),
                );
                task_executor
                    .execute(self.task_name.as_str(), workflow_context, task_context)
                    .await
            }
            Task::DoTask(task) => {
                let task_executor = DoTaskExecutor::new(
                    task,
                    self.job_executor_wrapper.clone(),
                    self.http_client.clone(),
                );
                task_executor
                    .execute(self.task_name.as_str(), workflow_context, task_context)
                    .await
            }
            Task::ForkTask(task) => {
                let task_executor = ForkTaskExecutor::new(self, task);
                task_executor
                    .execute(self.task_name.as_str(), workflow_context, task_context)
                    .await
            }
            // Task::EmitTask(task) => {
            //     let task_executor = EmitTaskExecutor::new(task);
            //     task_executor
            //         .execute(self.task_name.as_str(), workflow_context, task_context)
            //         .await
            // }
            Task::ForTask(task) => {
                let task_executor = ForTaskExecutor::new(
                    task,
                    self.job_executor_wrapper.clone(),
                    self.http_client.clone(),
                );
                task_executor
                    .execute(self.task_name.as_str(), workflow_context, task_context)
                    .await
            }
            Task::RaiseTask(task) => {
                let task_executor = RaiseTaskExecutor::new(task);
                task_executor
                    .execute(self.task_name.as_str(), workflow_context, task_context)
                    .await
            }
            // implemented
            Task::RunTask(task) => {
                let task_executor = RunTaskExecutor::new(self.job_executor_wrapper.clone(), task);
                task_executor
                    .execute(self.task_name.as_str(), workflow_context, task_context)
                    .await
            }
            Task::SetTask(task) => {
                let task_executor = SetTaskExecutor::new(task);
                task_executor
                    .execute(self.task_name.as_str(), workflow_context, task_context)
                    .await
            }
            Task::SwitchTask(task) => {
                let task_executor = SwitchTaskExecutor::new(task);
                task_executor
                    .execute(self.task_name.as_str(), workflow_context, task_context)
                    .await
            }
            Task::TryTask(task) => {
                let task_executor = TryTaskExecutor::new(
                    task,
                    self.job_executor_wrapper.clone(),
                    self.http_client.clone(),
                );
                task_executor
                    .execute(self.task_name.as_str(), workflow_context, task_context)
                    .await
            }
            Task::WaitTask(task) => {
                let task_executor = WaitTaskExecutor::new(task);
                task_executor
                    .execute(self.task_name.as_str(), workflow_context, task_context)
                    .await
            }
        }
    }
}

pub trait TaskExecutorTrait<'a>: Send + Sync {
    fn execute(
        &'a self,
        task_name: &'a str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> impl std::future::Future<Output = Result<TaskContext, Box<workflow::Error>>> + Send;
}

// pub struct EmitTaskExecutor<'a> {
//     task: &'a workflow::EmitTask,
// }
// impl<'a> EmitTaskExecutor<'a> {
//     pub fn new(task: &'a workflow::EmitTask) -> Self {
//         Self { task }
//     }
// }
// impl TaskExecutorTrait for EmitTaskExecutor<'_> {
//     async fn execute(
//         &self,
//         _task_name: &str,
//         _workflow_context: Arc<RwLock<WorkflowContext>>,
//         _task_context: TaskContext,
//     ) -> Result<TaskContext> {
//         tracing::error!("EmitTaskExecutor not implemented yet!: {:?}", self.task);
//         todo!()
//     }
// }

pub struct RaiseTaskExecutor<'a> {
    task: &'a workflow::RaiseTask,
}
impl<'a> RaiseTaskExecutor<'a> {
    pub fn new(task: &'a workflow::RaiseTask) -> Self {
        Self { task }
    }
}
impl TaskExecutorTrait<'_> for RaiseTaskExecutor<'_> {
    async fn execute(
        &self,
        _task_name: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        tracing::error!("RaiseTaskExecutor raise error: {:?}", self.task.raise.error);
        // TODO add error detail information to workflow_context
        let mut pos = task_context.position.lock().await.clone();
        pos.push("raise".to_string());
        workflow_context.write().await.status = WorkflowStatus::Faulted;
        Err(workflow::errors::ErrorFactory::create(
            workflow::errors::ErrorCode::Locked,
            Some(format!("Raise error!: {:?}", self.task.raise.error)),
            Some(&pos),
            None,
        ))
    }
}

pub struct WaitTaskExecutor<'a> {
    task: &'a workflow::WaitTask,
}
impl<'a> WaitTaskExecutor<'a> {
    pub fn new(task: &'a workflow::WaitTask) -> Self {
        Self { task }
    }
}
impl TaskExecutorTrait<'_> for WaitTaskExecutor<'_> {
    async fn execute(
        &self,
        task_name: &str,
        _workflow_context: Arc<RwLock<WorkflowContext>>,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        tracing::info!("WaitTask: {}: {:?}", task_name, self.task);
        tokio::time::sleep(std::time::Duration::from_millis(self.task.wait.to_millis())).await;
        task_context.set_output(task_context.input.clone());
        Ok(task_context)
    }
}
