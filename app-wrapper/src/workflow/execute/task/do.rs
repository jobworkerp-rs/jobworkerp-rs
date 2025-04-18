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
use indexmap::IndexMap;
use infra_utils::infra::net::reqwest;
use std::{pin::Pin, sync::Arc};
use tokio::sync::RwLock;

use super::TaskExecutorTrait;

#[derive(DebugStub, Clone)]
pub struct DoTaskExecutor<'a> {
    task: &'a workflow::DoTask,
    #[debug_stub = "AppModule"]
    pub job_executor_wrapper: Arc<JobExecutorWrapper>,
    #[debug_stub = "reqwest::HttpClient"]
    pub http_client: reqwest::ReqwestClient,
}
impl UseJqAndTemplateTransformer for DoTaskExecutor<'_> {}
impl UseExpression for DoTaskExecutor<'_> {}

impl<'a> DoTaskExecutor<'a> {
    pub fn new(
        task: &'a workflow::DoTask,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        http_client: reqwest::ReqwestClient,
    ) -> Self {
        Self {
            task,
            job_executor_wrapper,
            http_client,
        }
    }

    fn execute_task_list<'b>(
        &'b self,
        task_map: Arc<IndexMap<String, (u32, Arc<Task>)>>,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        parent_task_context: TaskContext,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<TaskContext, workflow::Error>> + Send + 'b>>
    {
        Box::pin(async move {
            let mut prev_context = parent_task_context;

            let mut task_iterator = task_map.iter();
            let mut next_task_pair = task_iterator.next();
            let lock = workflow_context.read().await;
            let status = lock.status.clone();
            drop(lock);

            while next_task_pair.is_some() && status == WorkflowStatus::Running {
                if let Some((name, (pos, task))) = next_task_pair {
                    // parent_task.position().addIndex(iter.previousIndex());
                    tracing::info!("Executing task: {}", name);
                    prev_context.add_position_index(pos.clone()).await;
                    let task_executor = TaskExecutor::new(
                        self.job_executor_wrapper.clone(),
                        self.http_client.clone(),
                        name,
                        task.clone(),
                    );
                    let result_task_context = task_executor
                        .execute(workflow_context.clone(), Arc::new(prev_context))
                        .await?;
                    let flow_directive = result_task_context.flow_directive.clone();
                    match flow_directive {
                        Then::Continue => {
                            result_task_context.remove_position().await;

                            prev_context = result_task_context;
                            next_task_pair = task_iterator.next();
                            tracing::info!(
                                "Task Continue next: {}",
                                next_task_pair.map(|p| p.0).unwrap_or(&"".to_string()),
                            );
                        }
                        Then::End => {
                            prev_context = result_task_context;
                            workflow_context.write().await.status = WorkflowStatus::Completed;
                            next_task_pair = None;
                        }
                        Then::Exit => {
                            prev_context = result_task_context;
                            next_task_pair = None;
                            tracing::info!("Exit Task: {}", name);
                        }
                        Then::TaskName(tname) => {
                            result_task_context.remove_position().await;

                            prev_context = result_task_context;
                            let mut it = task_map.iter();
                            for (k, v) in it.by_ref() {
                                if k == &tname {
                                    next_task_pair = Some((k, v));
                                    tracing::info!("Jump to Task: {}", k);
                                    break;
                                }
                            }
                            task_iterator = it;
                        }
                    }
                } else {
                    break;
                }
            }
            Ok(prev_context)
        })
    }
}

// XXX runner settings and options in metadata
impl TaskExecutorTrait<'_> for DoTaskExecutor<'_> {
    /// Executes the task list sequentially.
    ///
    /// This function processes each task in the task list, updating the task context
    /// with the output of each task as it progresses.
    ///
    /// # Returns
    /// The updated task context after executing all tasks.
    fn execute(
        &self,
        task_name: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> impl std::future::Future<Output = Result<TaskContext, workflow::Error>> + Send {
        async move {
            tracing::debug!("DoTaskExecutor: {}", task_name);
            task_context.add_position_name("do".to_string()).await;

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
            // Execute tasks
            match self
                .execute_task_list(task_map.clone(), workflow_context, task_context)
                .await
            {
                Ok(tc) => {
                    tc.remove_position().await;
                    Ok(tc)
                }
                Err(e) => {
                    tracing::warn!("Failed to execute task list: {:#?}", e);
                    Err(e)
                }
            }
        }
    }
}
