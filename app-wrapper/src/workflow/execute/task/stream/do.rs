use super::super::StreamTaskExecutorTrait;
use crate::workflow::{
    definition::{
        transform::UseJqAndTemplateTransformer,
        workflow::{self, Task},
    },
    execute::{
        context::{TaskContext, Then, WorkflowContext, WorkflowStatus},
        expression::UseExpression,
        job::JobExecutorWrapper,
        task::TaskExecutor,
    },
};
use anyhow::Result;
use debug_stub_derive::DebugStub;
use futures::StreamExt;
use indexmap::IndexMap;
use infra_utils::infra::net::reqwest;
use std::{pin::Pin, sync::Arc};
use tokio::sync::RwLock;
use tokio_stream::wrappers::ReceiverStream;

#[derive(DebugStub, Clone)]
pub struct DoTaskStreamExecutor {
    task: workflow::DoTask,
    #[debug_stub = "AppModule"]
    pub job_executor_wrapper: Arc<JobExecutorWrapper>,
    #[debug_stub = "reqwest::HttpClient"]
    pub http_client: reqwest::ReqwestClient,
}
impl UseJqAndTemplateTransformer for DoTaskStreamExecutor {}
impl UseExpression for DoTaskStreamExecutor {}

impl DoTaskStreamExecutor {
    pub fn new(
        task: workflow::DoTask,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        http_client: reqwest::ReqwestClient,
    ) -> Self {
        Self {
            task,
            job_executor_wrapper,
            http_client,
        }
    }

    fn execute_task_stream(
        &self,
        task_map: Arc<IndexMap<String, (u32, Arc<Task>)>>,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        parent_task_context: TaskContext,
    ) -> Pin<Box<dyn futures::Stream<Item = Result<TaskContext, Box<workflow::Error>>> + Send + '_>>
    {
        let job_exec = self.job_executor_wrapper.clone();
        let http_client = self.http_client.clone();
        let task_map = task_map.clone();
        let workflow_context = workflow_context.clone();
        let parent_ctx = parent_task_context.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<TaskContext, Box<workflow::Error>>>(32);
        tokio::spawn(async move {
            let mut prev = parent_ctx;
            let mut iter = task_map.iter();
            let mut next_pair = iter.next().map(|(k, v)| (k.clone(), v.clone()));
            while let Some((name, (pos, task))) = next_pair {
                // stop if not running
                if workflow_context.read().await.status != WorkflowStatus::Running {
                    break;
                }
                tracing::info!("Executing task: {}", name);
                prev.add_position_index(pos);
                let mut stream =
                    TaskExecutor::new(job_exec.clone(), http_client.clone(), &name, task.clone())
                        .execute(workflow_context.clone(), Arc::new(prev.clone()))
                        .await;
                let mut last_ctx = None;
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(ctx) => {
                            last_ctx = Some(ctx.clone());
                            if tx.send(Ok(ctx)).await.is_err() {
                                // tracing::warn!("Receiver dropped, stopping task execution.");
                                // return;
                            }
                        }
                        Err(e) => {
                            // workflow_context.write().await.status = WorkflowStatus::Faulted;
                            let _ = tx.send(Err(e)).await;
                            return;
                        }
                    }
                }
                let mut result = match last_ctx {
                    Some(c) => c,
                    None => break,
                };
                // determine next task
                next_pair = match result.flow_directive.clone() {
                    Then::Continue => {
                        result.remove_position();
                        iter.next().map(|(k, v)| (k.clone(), v.clone()))
                    }
                    Then::End => {
                        // end of workflow
                        workflow_context.write().await.status = WorkflowStatus::Completed;
                        None
                    }
                    Then::Exit => {
                        result.remove_position();
                        // end of task list
                        tracing::info!("Exit Task: {}", name);
                        None
                    }
                    Then::TaskName(ref tname) => {
                        result.remove_position();
                        task_map
                            .iter()
                            .find(|(k, _)| *k == tname)
                            .map(|(k, v)| (k.clone(), v.clone()))
                    }
                };
                prev = result;
            }
        });
        Box::pin(ReceiverStream::new(rx))
    }
}

impl StreamTaskExecutorTrait<'_> for DoTaskStreamExecutor {
    /// Executes the task list sequentially.
    ///
    /// This function processes each task in the task list, updating the task context
    /// with the output of each task as it progresses.
    ///
    /// # Returns
    /// The updated task context after executing all tasks.
    fn execute_stream(
        &self,
        task_name: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> impl futures::Stream<Item = Result<TaskContext, Box<workflow::Error>>> + Send {
        let mut task_context = task_context.clone();
        // position name will be added in execute_task_stream
        task_context.add_position_name("do".to_string());

        tracing::debug!("DoTaskStreamExecutor: {}", task_name);

        let mut idx = 0;
        let task_map = Arc::new(self.task.do_.0.iter().fold(
            IndexMap::<String, (u32, Arc<Task>)>::new(),
            |mut acc, task| {
                task.iter().for_each(|(name, t)| {
                    acc.insert(name.clone(), (idx, Arc::new(t.clone())));
                });
                idx += 1;
                acc
            },
        ));

        // Execute tasks as a stream
        Box::pin(
            self.execute_task_stream(task_map, workflow_context, task_context)
                .map(|result| {
                    match result {
                        Ok(tc) => {
                            // Clone before remove_position to preserve the original
                            let tc_clone = tc.clone();
                            // We'll let the caller handle position removal if needed
                            Ok(tc_clone)
                        }
                        Err(e) => {
                            tracing::warn!("Failed to execute task in stream: {:#?}", e);
                            Err(e)
                        }
                    }
                }),
        )
    }
}
