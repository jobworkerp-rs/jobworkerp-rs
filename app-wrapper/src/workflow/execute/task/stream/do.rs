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
use futures::{executor::block_on, StreamExt};
use indexmap::IndexMap;
use infra_utils::infra::net::reqwest;
use std::{pin::Pin, sync::Arc};
use tokio::sync::RwLock;

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
        Box::pin(futures::stream::unfold(
            (
                parent_task_context,
                task_map.clone(),
                workflow_context.clone(),
                task_map.iter().next().map(|(k, v)| (k.clone(), v.clone())),
            ),
            move |(prev_context, task_map, workflow_context, next_task_pair)| async move {
                if next_task_pair.is_none() {
                    return None;
                }

                let lock = workflow_context.read().await;
                let status = lock.status.clone();
                drop(lock);

                if status != WorkflowStatus::Running {
                    return None;
                }

                let (name, (pos, task)) = next_task_pair.unwrap();
                tracing::info!("Executing task: {}", name);
                prev_context.add_position_index(pos).await;
                let task_executor = TaskExecutor::new(
                    self.job_executor_wrapper.clone(),
                    self.http_client.clone(),
                    &name,
                    task.clone(),
                );

                let result = match task_executor
                    .execute(workflow_context.clone(), Arc::new(prev_context.clone()))
                    .await
                {
                    Ok(result_task_context) => {
                        let flow_directive = result_task_context.flow_directive.clone();
                        let next_pair = match flow_directive {
                            Then::Continue => {
                                let result_context = result_task_context.clone();
                                result_context.remove_position().await;

                                // Get the next task in sequence
                                let mut found = false;

                                // Find the next task after the current one
                                let next_pair = task_map
                                    .iter()
                                    .skip_while(|(k, _)| {
                                        if *k == &name {
                                            found = true;
                                            return false;
                                        }
                                        !found
                                    })
                                    .next()
                                    .map(|(k, v)| (k.clone(), v.clone()));

                                tracing::info!(
                                    "Task Continue next: {}",
                                    next_pair.as_ref().map(|p| p.0.as_str()).unwrap_or(""),
                                );
                                (result_context, next_pair)
                            }
                            Then::End => {
                                workflow_context.write().await.status = WorkflowStatus::Completed;
                                (result_task_context.clone(), None)
                            }
                            Then::Exit => {
                                tracing::info!("Exit Task: {}", name);
                                (result_task_context.clone(), None)
                            }
                            Then::TaskName(tname) => {
                                let result_context = result_task_context.clone();
                                result_context.remove_position().await;

                                let next_pair = task_map
                                    .iter()
                                    .find(|(k, _)| *k == &tname)
                                    .map(|(k, v)| (k.clone(), v.clone()));

                                if let Some((k, _)) = &next_pair {
                                    tracing::info!("Jump to Task: {}", k);
                                }

                                (result_context, next_pair)
                            }
                        };
                        Ok((result_task_context, next_pair))
                    }
                    Err(e) => Err(e),
                };

                match result {
                    Ok((current_context, (next_context, next_pair))) => Some((
                        Ok(current_context),
                        (
                            next_context,
                            task_map.clone(),
                            workflow_context.clone(),
                            next_pair,
                        ),
                    )),
                    Err(e) => Some((
                        Err(e),
                        (prev_context, task_map.clone(), workflow_context, None),
                    )),
                }
            },
        ))
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
        let task_context = task_context.clone();
        // position name will be added in execute_task_stream
        block_on(task_context.add_position_name("do".to_string()));

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
