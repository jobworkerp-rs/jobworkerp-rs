use super::TaskExecutor;
use crate::workflow::{
    definition::workflow::{tasks::TaskTrait, Task},
    execute::{
        context::{TaskContext, WorkflowContext, WorkflowStreamEvent},
        task::{ExecutionId, Result, TaskExecutorTrait},
    },
};
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow,
    },
    execute::expression::UseExpression,
};
use app::app::job::execute::JobExecutorWrapper;
use command_utils::trace::Tracing;
use debug_stub_derive::DebugStub;
use futures::{future, Future, StreamExt};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::RwLock;
use tokio_stream::StreamMap;

// for DebugStub
type CheckPointRepo =
    Arc<dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId>;

#[derive(DebugStub, Clone)]
pub struct ForkTaskExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    default_task_timeout: Duration,
    #[debug_stub = "AppModule"]
    pub job_executor_wrapper: Arc<JobExecutorWrapper>,
    pub checkpoint_repository: Option<CheckPointRepo>,
    task: workflow::ForkTask,
    execution_id: Option<Arc<ExecutionId>>,
    metadata: Arc<HashMap<String, String>>,
}

impl Tracing for ForkTaskExecutor {}
impl ForkTaskExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workflow_context: Arc<RwLock<WorkflowContext>>,
        default_task_timeout: Duration,
        task: workflow::ForkTask,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        checkpoint_repository: Option<
            Arc<dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId>,
        >,
        execution_id: Option<Arc<ExecutionId>>,
        metadata: Arc<HashMap<String, String>>,
    ) -> Self {
        Self {
            workflow_context,
            default_task_timeout,
            job_executor_wrapper,
            checkpoint_repository,
            task,
            execution_id,
            metadata,
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_task(
        name: &str,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        checkpoint_repository: Option<
            Arc<dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId>,
        >,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        prev_context: Arc<TaskContext>,
        task: Arc<Task>,
        execution_id: Option<Arc<ExecutionId>>,
        default_task_timeout: Duration,
        metadata: Arc<HashMap<String, String>>,
        cx: Arc<opentelemetry::Context>,
    ) -> futures::stream::BoxStream<'static, Result<WorkflowStreamEvent, Box<workflow::Error>>>
    {
        let task_executor = TaskExecutor::new(
            workflow_context,
            default_task_timeout,
            job_executor_wrapper,
            checkpoint_repository,
            name,
            task,
            metadata,
        );
        task_executor
            .execute(cx, prev_context.clone(), execution_id)
            .await
    }
}

impl UseExpression for ForkTaskExecutor {}
impl UseExpressionTransformer for ForkTaskExecutor {}
impl UseJqAndTemplateTransformer for ForkTaskExecutor {}

impl<'a> TaskExecutorTrait<'a> for ForkTaskExecutor {
    #[allow(clippy::manual_async_fn)]
    fn execute(
        &'a self,
        cx: Arc<opentelemetry::Context>,
        task_name: &'a str,
        task_context: TaskContext,
    ) -> impl Future<Output = Result<TaskContext, Box<workflow::Error>>> + Send {
        async move {
            tracing::debug!("ForkTaskExecutor: {}", task_name);

            task_context
                .add_position_name(self.task.task_type().to_string())
                .await;
            // XXX position is incorrect in fork task(idx, branch_name are ignored because of parallel execution(ignore checkpoint))
            //     (not copy large context, just use position from task_context)
            let position = task_context.position.clone();
            let branches = &self.task.fork.branches;
            let compete = self.task.fork.compete;

            let mut tasks = Vec::new();
            let mut original_task_context = task_context.clone();
            let task_context_ref = Arc::new(task_context);

            for branch_item in branches.0.iter() {
                for (branch_name, task) in branch_item.iter() {
                    // XXX need position for each branch?
                    let workflow_context_clone = Arc::clone(&self.workflow_context);
                    let task_context_clone = Arc::clone(&task_context_ref);
                    let name = branch_name.to_string();
                    let task_clone = Arc::new(task.clone());
                    let metadata_clone = Arc::clone(&self.metadata);
                    let execution_id = self.execution_id.clone();
                    let cxc = cx.clone();
                    let default_task_timeout = self.default_task_timeout;
                    let future = Box::pin(async move {
                        Self::execute_task(
                            &name,
                            self.job_executor_wrapper.clone(),
                            self.checkpoint_repository.clone(),
                            workflow_context_clone,
                            task_context_clone,
                            task_clone,
                            execution_id,
                            default_task_timeout,
                            metadata_clone,
                            cxc,
                        )
                        .await
                    });
                    tasks.push(future);
                }
            }

            let res = if compete {
                // compete mode: only one task will succeed (others will be abandoned)
                let mut all_errors = Vec::new();
                let mut streams = Vec::new();

                for stream_fut in tasks {
                    streams.push(stream_fut.await);
                }

                let mut stream_map = StreamMap::new();
                for (i, stream) in streams.into_iter().enumerate() {
                    stream_map.insert(i, stream);
                }

                // Poll streams until we get a success or all fail
                while let Some((_, result)) = stream_map.next().await {
                    match result {
                        Ok(event) => {
                            // Extract context from completed events
                            if let Some(context) = event.into_context() {
                                // Found a successful task, return it immediately
                                // (abandoning all other tasks)
                                return Ok(context);
                            }
                            // Start events don't have context, continue polling
                        }
                        Err(e) => {
                            // Stream returned an error, log and continue with others
                            tracing::warn!("Task failed in compete mode: {:#?}", e);
                            all_errors.push(e);

                            // If this was the last stream, we're done with errors
                            if stream_map.is_empty() {
                                return Err(workflow::errors::ErrorFactory::new()
                                    .service_unavailable(
                                        "All tasks failed in compete mode".to_string(),
                                        Some(position.read().await.as_error_instance()),
                                        Some(format!("{all_errors:#?}")),
                                    ));
                            }
                        }
                    }
                }

                // If we get here, all streams ended without success
                Err(workflow::errors::ErrorFactory::new().service_unavailable(
                    "All tasks failed in compete mode".to_string(),
                    Some(position.read().await.as_error_instance()),
                    Some(format!("{all_errors:#?}")),
                ))
            } else {
                // Normal mode: collect results from all tasks
                let streams_futures = future::join_all(tasks).await;

                let mut output = Vec::new();

                for stream in streams_futures {
                    // Collect all items from this stream
                    let results = stream.collect::<Vec<_>>().await;

                    for result in results {
                        match result {
                            Ok(event) => {
                                // Extract context from completed events
                                if let Some(context) = event.into_context() {
                                    if !context.output.is_null() {
                                        output.push((*context.output).clone());
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Failed to execute task: {:#?}", e);
                            }
                        }
                    }
                }

                // all output as array
                original_task_context.raw_output = Arc::new(serde_json::Value::Array(output));
                Ok(original_task_context)
            };

            match res {
                Ok(context) => {
                    context.remove_position().await;
                    Ok(context)
                }
                Err(e) => Err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::definition::workflow::Task as WorkflowTask;
    use app::module::test::create_hybrid_test_app;
    use opentelemetry::Context;
    use serde_json::json;
    use std::collections::HashMap;
    use tokio::sync::Mutex;

    struct MockTaskContext {}

    impl MockTaskContext {
        fn create(input: serde_json::Value) -> TaskContext {
            TaskContext::new(
                None,
                Arc::new(input),
                Arc::new(Mutex::new(serde_json::Map::new())),
            )
        }
    }

    // 成功するタスクを作成する関数
    fn create_success_task(map: serde_json::Map<String, serde_json::Value>) -> WorkflowTask {
        WorkflowTask::SetTask(crate::workflow::definition::workflow::SetTask {
            set: map,
            export: None,
            if_: None,
            input: None,
            metadata: serde_json::Map::new(),
            output: None,
            then: None,
            timeout: None,
            checkpoint: false,
        })
    }

    #[test]
    fn test_fork_task_executor_normal_mode() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let metadata = Arc::new(HashMap::new());
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let job_executor_wrapper = Arc::new(JobExecutorWrapper::new(app_module));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &crate::workflow::definition::workflow::WorkflowSchema::default(),
                Arc::new(json!({"winput": "test"})),
                Arc::new(json!({})),
                None,
            )));

            // create branches
            let mut branches_map = Vec::new();

            let mut task_map1 = HashMap::new();
            task_map1.insert(
                "branch1".to_string(),
                create_success_task(
                    serde_json::json!({"key1": "${$workflow.input.winput}"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            );
            branches_map.push(task_map1);

            let mut task_map2 = HashMap::new();
            task_map2.insert(
                "branch2".to_string(),
                create_success_task(
                    serde_json::json!({"key2": "${$input.initial}"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            );
            branches_map.push(task_map2);

            // normal mode
            let fork_task = workflow::ForkTask {
                fork: workflow::ForkTaskConfiguration {
                    branches: workflow::TaskList(branches_map),
                    compete: false,
                },
                export: None,
                if_: None,
                input: None,
                metadata: serde_json::Map::new(),
                output: None,
                then: None,
                timeout: None,
                checkpoint: false,
            };

            // TaskExecutor, ForkTaskExecutor
            let fork_task_executor = ForkTaskExecutor::new(
                workflow_context.clone(),
                Duration::from_secs(1200), // default task timeout
                fork_task,
                job_executor_wrapper,
                None,
                None,
                metadata,
            );

            let input = json!({"initial": "value"});
            let task_context = MockTaskContext::create(input.clone());
            let result = fork_task_executor
                .execute(Arc::new(Context::current()), "fork_test", task_context)
                .await;

            assert!(result.is_ok());
            let output = result.unwrap();

            // array output
            if let serde_json::Value::Array(array) = output.raw_output.as_ref() {
                assert_eq!(array.len(), 2);
                assert_eq!(array[0], json!({"key1": "test"}));
                assert_eq!(array[1], json!({"key2": "value"}));
            } else {
                panic!("Output is not an array");
            }
        })
    }

    #[test]
    fn test_fork_task_executor_compete_mode() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let metadata = Arc::new(HashMap::new());
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let job_executor_wrapper = Arc::new(JobExecutorWrapper::new(app_module));
            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &crate::workflow::definition::workflow::WorkflowSchema::default(),
                Arc::new(json!({})),
                Arc::new(json!({})),
                None,
            )));

            let mut branches_map = Vec::new();

            let mut task_map1 = HashMap::new();
            task_map1.insert(
                "branch1".to_string(),
                create_success_task(serde_json::Map::new()),
            );
            branches_map.push(task_map1);

            let mut task_map2 = HashMap::new();
            task_map2.insert(
                "branch2".to_string(),
                create_success_task(serde_json::Map::new()),
            );
            branches_map.push(task_map2);

            // compete
            let fork_task = workflow::ForkTask {
                fork: workflow::ForkTaskConfiguration {
                    branches: workflow::TaskList(branches_map),
                    compete: true,
                },
                export: None,
                if_: None,
                input: None,
                metadata: serde_json::Map::new(),
                output: None,
                then: None,
                timeout: None,
                checkpoint: false,
            };

            let fork_task_executor = ForkTaskExecutor::new(
                workflow_context,
                Duration::from_secs(1200), // default task timeout
                fork_task,
                job_executor_wrapper,
                None,
                None,
                metadata,
            );

            // execute
            let task_context = MockTaskContext::create(json!({"initial": "value"}));
            let result = fork_task_executor
                .execute(
                    Arc::new(opentelemetry::Context::current()),
                    "fork_test",
                    task_context,
                )
                .await;

            assert!(result.is_ok());
        })
    }

    #[test]
    fn test_fork_task_executor_compete_mode_all_fail() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let metadata = Arc::new(HashMap::new());
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let job_executor_wrapper = Arc::new(JobExecutorWrapper::new(app_module));
            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &crate::workflow::definition::workflow::WorkflowSchema::default(),
                Arc::new(json!({})),
                Arc::new(json!({})),
                None,
            )));

            let mut branches_map = Vec::new();

            // raise error task
            let fail_task =
                WorkflowTask::RaiseTask(crate::workflow::definition::workflow::RaiseTask {
                    raise: workflow::RaiseTaskConfiguration {
                        error: workflow::RaiseTaskError::Error(workflow::Error {
                            status: 1,
                            ..Default::default()
                        }),
                    },
                    export: None,
                    if_: None,
                    input: None,
                    metadata: serde_json::Map::new(),
                    output: None,
                    then: None,
                    timeout: None,
                    checkpoint: false,
                });

            let mut task_map1 = HashMap::new();
            task_map1.insert("fail_branch1".to_string(), fail_task.clone());
            branches_map.push(task_map1);

            let mut task_map2 = HashMap::new();
            task_map2.insert("fail_branch2".to_string(), fail_task.clone());
            branches_map.push(task_map2);

            // compete
            let fork_task = workflow::ForkTask {
                fork: workflow::ForkTaskConfiguration {
                    branches: workflow::TaskList(branches_map),
                    compete: true,
                },
                export: None,
                if_: None,
                input: None,
                metadata: serde_json::Map::new(),
                output: None,
                then: None,
                timeout: None,
                checkpoint: false,
            };

            let fork_task_executor = ForkTaskExecutor::new(
                workflow_context,
                Duration::from_secs(1200), // default task timeout
                fork_task,
                job_executor_wrapper,
                None,
                None,
                metadata,
            );

            // execute
            let task_context = MockTaskContext::create(json!({"initial": "value"}));
            let result = fork_task_executor
                .execute(
                    Arc::new(opentelemetry::Context::current()),
                    "fork_test",
                    task_context,
                )
                .await;

            // all tasks failed
            assert!(result.is_err());
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("All tasks failed in compete mode"));
        })
    }
}
