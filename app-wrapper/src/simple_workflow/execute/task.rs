use super::{
    context::{TaskContext, UseExpression, WorkflowContext},
    job::JobExecutorWrapper,
};
use crate::simple_workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self, supplement::TaskTrait, Task},
    },
    execute::context::Then,
};
use anyhow::Result;
use call::CallTaskExecutor;
use debug_stub_derive::DebugStub;
use do_::DoTaskExecutor;
use for_::ForTaskExecutor;
use infra_utils::infra::net::reqwest;
use run::RunTaskExecutor;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::Level;

pub mod call;
#[path = "task/do.rs"]
pub mod do_;
#[path = "task/for.rs"]
pub mod for_;
pub mod run;

#[derive(DebugStub)]
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
        command_utils::util::tracing::tracing_init_test(Level::DEBUG);
        Self {
            job_executor_wrapper,
            http_client,
            task_name: task_name.to_owned(),
            task,
        }
    }
    // input is the output of the previous task
    // return the output of the task, and next task("continue", "exit", "end", or taskName)
    pub async fn execute(
        &self,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        parent_task: Arc<TaskContext>,
    ) -> Result<Arc<TaskContext>> {
        let input = parent_task.output.clone();
        let mut task_context = TaskContext::new(
            Some(self.task.clone()),
            input.clone(),
            parent_task.context_variables.clone(),
        );
        let expression = self
            .expression(
                &*workflow_context.read().await,
                Arc::new(task_context.clone()),
            )
            .await?;
        // Evaluate if condition
        if let Some(if_cond) = self.task.if_() {
            tracing::debug!("`If' condition: {:#?}", if_cond);
            let eval_result =
                Self::execute_transform(task_context.raw_input.clone(), if_cond, &expression);
            // command_utils::util::jq::execute_jq(serde_json::Value::Null, if_cond, &expression);
            match eval_result {
                Ok(v) => {
                    if v.is_boolean() {
                        if v.as_bool().unwrap_or(true) {
                            // continue
                            tracing::debug!(
                                "`If' condition is true: {:#?}",
                                &task_context.raw_input
                            );
                        } else {
                            // skip to next task
                            tracing::debug!(
                                "`If' condition is false, skip: {:#?}",
                                &task_context.raw_input
                            );
                            return Ok(Arc::new(task_context));
                        }
                    } else if Self::eval_as_bool(&v) {
                        tracing::info!(
                            "`If' condition value treated as true: {:#?}",
                            &task_context.raw_input
                        );
                    } else {
                        // skip to next task
                        tracing::debug!(
                            "`If' condition is evaled as false, skip: {:#?}",
                            &task_context.raw_input
                        );
                        return Ok(Arc::new(task_context));
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to evaluate `if' condition: {:#?}", e);
                    task_context.flow_directive = Then::Exit;
                    return Ok(Arc::new(task_context));
                }
            }
        }

        // XXX invalid by input transformation? (transform argument inner each task)
        // Validate input schema
        // if let Some(schema) = self.task.input().and_then(|i| i.schema.as_ref()) {
        //     if let Some(schema) = schema.json_schema() {
        //         jsonschema::validate(schema, &task_context.input)
        //             .map_err(|e| anyhow::anyhow!("Failed to validate input schema: {:#?}\n{:#?}\n{:#?}", schema, &task_context.raw_input, e))?;
        //     }
        // }

        // Transform input
        let transformed_input = if let Some(from) = self.task.input().and_then(|i| i.from.as_ref())
        {
            Self::transform_input(task_context.raw_input.clone(), from, &expression)?
        } else {
            task_context.raw_input.clone()
        };
        tracing::debug!("Transformed input: {:#?}", transformed_input);
        task_context.set_input(transformed_input);
        // Execute task
        let mut task_context = self
            .execute_task(workflow_context.clone(), task_context)
            .await?;
        tracing::debug!("Task raw output: {:#?}", task_context.raw_output);
        if let Some(as_) = self.task.output().and_then(|o| o.as_.as_ref()) {
            task_context.output =
                Self::transform_output(task_context.raw_output.clone(), as_, &expression)?;
            tracing::debug!("Transformed output: {:#?}", &task_context.output);
        } else {
            tracing::debug!("No output transformation: {:#?}", &task_context.raw_output);
        }
        // Determine next task
        // TODO test (not used yet)
        task_context.flow_directive = match self.task.then() {
            Some(flow) => Then::from(flow.clone()),
            None => Then::Continue,
        };
        Ok(Arc::new(task_context))
    }
    //
    async fn execute_task(
        &self,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> Result<TaskContext> {
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
                let task_executor = DoTaskExecutor::new(task, self.job_executor_wrapper.clone());
                task_executor
                    .execute(self.task_name.as_str(), workflow_context, task_context)
                    .await
            }
            Task::ForkTask(task) => {
                let task_executor = ForkTaskExecutor::new(task);
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
                let task_executor = ForTaskExecutor::new(task, self.job_executor_wrapper.clone());
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
                let task_executor = TryTaskExecutor::new(task);
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

pub trait TaskExecutorTrait: Send + Sync {
    fn execute(
        &self,
        task_name: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> impl std::future::Future<Output = Result<TaskContext>> + Send;
}

pub struct ForkTaskExecutor<'a> {
    task: &'a workflow::ForkTask,
}
impl<'a> ForkTaskExecutor<'a> {
    pub fn new(task: &'a workflow::ForkTask) -> Self {
        Self { task }
    }
}
impl TaskExecutorTrait for ForkTaskExecutor<'_> {
    async fn execute(
        &self,
        _task_name: &str,
        _workflow_context: Arc<RwLock<WorkflowContext>>,
        _task_context: TaskContext,
    ) -> Result<TaskContext> {
        tracing::error!("ForkTaskExecutor not implemented yet!: {:?}", self.task);
        todo!()
    }
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
impl TaskExecutorTrait for RaiseTaskExecutor<'_> {
    async fn execute(
        &self,
        _task_name: &str,
        _workflow_context: Arc<RwLock<WorkflowContext>>,
        _task_context: TaskContext,
    ) -> Result<TaskContext> {
        tracing::error!("RaiseTaskExecutor not implemented yet!: {:?}", self.task);
        todo!()
    }
}

pub struct SetTaskExecutor<'a> {
    task: &'a workflow::SetTask,
}
impl<'a> SetTaskExecutor<'a> {
    pub fn new(task: &'a workflow::SetTask) -> Self {
        Self { task }
    }
}
impl TaskExecutorTrait for SetTaskExecutor<'_> {
    async fn execute(
        &self,
        _task_name: &str,
        _workflow_context: Arc<RwLock<WorkflowContext>>,
        _task_context: TaskContext,
    ) -> Result<TaskContext> {
        tracing::error!("SetTaskExecutor not implemented yet!: {:?}", self.task);
        todo!()
    }
}

pub struct SwitchTaskExecutor<'a> {
    task: &'a workflow::SwitchTask,
}
impl<'a> SwitchTaskExecutor<'a> {
    pub fn new(task: &'a workflow::SwitchTask) -> Self {
        Self { task }
    }
}
impl TaskExecutorTrait for SwitchTaskExecutor<'_> {
    async fn execute(
        &self,
        _task_name: &str,
        _workflow_context: Arc<RwLock<WorkflowContext>>,
        _task_context: TaskContext,
    ) -> Result<TaskContext> {
        tracing::error!("SwitchTaskExecutor not implemented yet!: {:?}", self.task);
        todo!()
    }
}

pub struct TryTaskExecutor<'a> {
    task: &'a workflow::TryTask,
}
impl<'a> TryTaskExecutor<'a> {
    pub fn new(task: &'a workflow::TryTask) -> Self {
        Self { task }
    }
}
impl TaskExecutorTrait for TryTaskExecutor<'_> {
    async fn execute(
        &self,
        _task_name: &str,
        _workflow_context: Arc<RwLock<WorkflowContext>>,
        _task_context: TaskContext,
    ) -> Result<TaskContext> {
        tracing::error!("TryTaskExecutor not implemented yet!: {:?}", self.task);
        todo!()
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
impl TaskExecutorTrait for WaitTaskExecutor<'_> {
    async fn execute(
        &self,
        _task_name: &str,
        _workflow_context: Arc<RwLock<WorkflowContext>>,
        _task_context: TaskContext,
    ) -> Result<TaskContext> {
        tracing::error!("WaitTaskExecutor not implemented yet!: {:?}", self.task);
        todo!()
    }
}
