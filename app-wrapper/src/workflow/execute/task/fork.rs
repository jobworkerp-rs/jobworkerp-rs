use super::TaskExecutor;
use crate::workflow::{
    definition::workflow::Task,
    execute::{
        context::{TaskContext, WorkflowContext},
        job::JobExecutorWrapper,
        task::{Result, TaskExecutorTrait},
    },
};
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow,
    },
    execute::expression::UseExpression,
};
use debug_stub_derive::DebugStub;
use futures::{future, Future, StreamExt};
use infra_utils::infra::net::reqwest;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_stream::StreamMap;

#[derive(DebugStub, Clone)]
pub struct ForkTaskExecutor {
    #[debug_stub = "AppModule"]
    pub job_executor_wrapper: Arc<JobExecutorWrapper>,
    #[debug_stub = "reqwest::HttpClient"]
    pub http_client: reqwest::ReqwestClient,
    task: workflow::ForkTask,
}

impl ForkTaskExecutor {
    pub fn new(
        task: workflow::ForkTask,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        http_client: reqwest::ReqwestClient,
    ) -> Self {
        Self {
            job_executor_wrapper,
            http_client,
            task,
        }
    }
    pub async fn execute_task(
        name: &str,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        http_client: reqwest::ReqwestClient,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        prev_context: Arc<TaskContext>,
        task: Arc<Task>,
    ) -> futures::stream::BoxStream<'static, Result<TaskContext, Box<workflow::Error>>> {
        let task_executor = TaskExecutor::new(job_executor_wrapper, http_client, name, task);
        task_executor
            .execute(workflow_context, prev_context.clone())
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
        task_name: &'a str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> impl Future<Output = Result<TaskContext, Box<workflow::Error>>> + Send {
        async move {
            tracing::debug!("ForkTaskExecutor: {}", task_name);

            task_context.add_position_name("fork".to_string()).await;
            let position = task_context.position.lock().await.clone();
            let branches = &self.task.fork.branches;
            let compete = self.task.fork.compete;

            let mut tasks = Vec::new();
            let mut original_task_context = task_context.clone();
            let task_context_ref = Arc::new(task_context);

            for branch_item in branches.0.iter() {
                for (branch_name, task) in branch_item.iter() {
                    let workflow_context_clone = Arc::clone(&workflow_context);
                    let task_context_clone = Arc::clone(&task_context_ref);
                    let name = branch_name.to_string();
                    let task_clone = Arc::new(task.clone());

                    let future = Box::pin(async move {
                        Self::execute_task(
                            &name,
                            self.job_executor_wrapper.clone(),
                            self.http_client.clone(),
                            workflow_context_clone,
                            task_context_clone,
                            task_clone,
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

                // Convert all stream futures into actual streams
                for stream_fut in tasks {
                    streams.push(stream_fut.await);
                }

                // Create a StreamMap to manage all streams
                let mut stream_map = StreamMap::new();
                for (i, stream) in streams.into_iter().enumerate() {
                    stream_map.insert(i, stream);
                }

                // Poll streams until we get a success or all fail
                while let Some((_, result)) = stream_map.next().await {
                    match result {
                        Ok(context) => {
                            // Found a successful task, return it immediately
                            // (abandoning all other tasks)
                            return Ok(context);
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
                                        Some(&position),
                                        Some(format!("{:#?}", all_errors)),
                                    ));
                            }
                        }
                    }
                }

                // If we get here, all streams ended without success
                Err(workflow::errors::ErrorFactory::new().service_unavailable(
                    "All tasks failed in compete mode".to_string(),
                    Some(&position),
                    Some(format!("{:#?}", all_errors)),
                ))
            } else {
                // Normal mode: collect results from all tasks
                let streams_futures = future::join_all(tasks).await;

                // Get all results from the streams
                let mut output = Vec::new();

                for stream in streams_futures {
                    // Collect all items from this stream
                    let results = stream.collect::<Vec<_>>().await;

                    for result in results {
                        match result {
                            Ok(context) => {
                                if !context.output.is_null() {
                                    output.push((*context.output).clone());
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
    use crate::workflow::execute::job::JobExecutorWrapper;
    use app::module::test::create_hybrid_test_app;
    use infra_utils::infra::net::reqwest;
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
        })
    }

    #[test]
    fn test_fork_task_executor_normal_mode() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let job_executor_wrapper = Arc::new(JobExecutorWrapper::new(app_module));
            let http_client = reqwest::ReqwestClient::new(
                Some("test"),
                Some(std::time::Duration::from_secs(1)),
                Some(std::time::Duration::from_secs(1)),
                Some(1),
            )
            .unwrap();

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &crate::workflow::definition::workflow::WorkflowSchema::default(),
                Arc::new(json!({"winput": "test"})),
                Arc::new(json!({})),
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
            };

            // TaskExecutor, ForkTaskExecutor
            let task = Arc::new(WorkflowTask::SetTask(
                crate::workflow::definition::workflow::SetTask::default(),
            ));
            let fork_task_executor =
                ForkTaskExecutor::new(fork_task, job_executor_wrapper, http_client);

            let input = json!({"initial": "value"});
            let task_context = MockTaskContext::create(input.clone());
            let result = fork_task_executor
                .execute("fork_test", workflow_context, task_context)
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
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let job_executor_wrapper = Arc::new(JobExecutorWrapper::new(app_module));
            let http_client = reqwest::ReqwestClient::new(
                Some("test"),
                Some(std::time::Duration::from_secs(1)),
                Some(std::time::Duration::from_secs(1)),
                Some(1),
            )
            .unwrap();

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &crate::workflow::definition::workflow::WorkflowSchema::default(),
                Arc::new(json!({})),
                Arc::new(json!({})),
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
            };

            let task = Arc::new(WorkflowTask::SetTask(
                crate::workflow::definition::workflow::SetTask::default(),
            ));
            let fork_task_executor =
                ForkTaskExecutor::new(fork_task, job_executor_wrapper, http_client);

            // execute
            let task_context = MockTaskContext::create(json!({"initial": "value"}));
            let result = fork_task_executor
                .execute("fork_test", workflow_context, task_context)
                .await;

            assert!(result.is_ok());
        })
    }

    #[test]
    fn test_fork_task_executor_compete_mode_all_fail() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let job_executor_wrapper = Arc::new(JobExecutorWrapper::new(app_module));
            let http_client = reqwest::ReqwestClient::new(
                Some("test"),
                Some(std::time::Duration::from_secs(1)),
                Some(std::time::Duration::from_secs(1)),
                Some(1),
            )
            .unwrap();

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &crate::workflow::definition::workflow::WorkflowSchema::default(),
                Arc::new(json!({})),
                Arc::new(json!({})),
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
            };

            let task = Arc::new(WorkflowTask::SetTask(
                crate::workflow::definition::workflow::SetTask::default(),
            ));

            let fork_task_executor =
                ForkTaskExecutor::new(fork_task, job_executor_wrapper, http_client);

            // execute
            let task_context = MockTaskContext::create(json!({"initial": "value"}));
            let result = fork_task_executor
                .execute("fork_test", workflow_context, task_context)
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
