use crate::workflow::{
    definition::workflow::{DoTask, Input, TryTaskCatch},
    execute::{
        context::{TaskContext, WorkflowContext},
        task::{
            stream::do_::DoTaskStreamExecutor, ExecutionId, Result, StreamTaskExecutorTrait,
            TaskExecutorTrait,
        },
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
use net_utils::net::reqwest;
use std::{sync::Arc, time::Duration};
use tokio::{sync::RwLock, time::Instant};

pub struct TryTaskExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    default_timeout: Duration,
    task: workflow::TryTask,
    job_executors: Arc<JobExecutorWrapper>,
    http_client: reqwest::ReqwestClient,
    checkpoint_repository: Option<
        Arc<dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId>,
    >,
    execution_id: Option<Arc<ExecutionId>>, // execution id for checkpoint
    metadata: Arc<std::collections::HashMap<String, String>>,
}
impl TryTaskExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workflow_context: Arc<RwLock<WorkflowContext>>,
        default_timeout: Duration,
        task: workflow::TryTask,
        job_executors: Arc<JobExecutorWrapper>,
        http_client: reqwest::ReqwestClient,
        checkpoint_repository: Option<
            Arc<dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId>,
        >,
        execution_id: Option<Arc<ExecutionId>>,
        metadata: Arc<std::collections::HashMap<String, String>>,
    ) -> Self {
        Self {
            workflow_context,
            default_timeout,
            task,
            job_executors,
            http_client,
            checkpoint_repository,
            execution_id,
            metadata,
        }
    }
}

impl UseExpression for TryTaskExecutor {}
impl UseExpressionTransformer for TryTaskExecutor {}
impl UseJqAndTemplateTransformer for TryTaskExecutor {}

impl TaskExecutorTrait<'_> for TryTaskExecutor {
    #[allow(clippy::manual_async_fn)]
    fn execute(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_name: &str,
        task_context: TaskContext,
    ) -> impl std::future::Future<Output = Result<TaskContext, Box<workflow::Error>>> + Send {
        async move {
            let mut task_context = task_context;
            task_context.add_position_name("try".to_string()).await;
            let retry = self.task.catch.retry.as_ref();
            let start_time = Instant::now();

            let mut retry_count: i64 = 0;
            let res = loop {
                match self
                    .execute_task_list(
                        cx.clone(),
                        task_name,
                        &self.task.try_,
                        task_context.clone(), // XXX now clone (heavy)
                    )
                    .await
                {
                    Ok(try_context) => break Ok(try_context),
                    Err(error) => {
                        tracing::debug!("Try task failed with error: {:?}", error);
                        // TODO return task_context in workflow::Error for catch block (to error recovering)
                        let catch_config = &self.task.catch;
                        match Self::execute_catch_block(
                            catch_config,
                            self.workflow_context.clone(),
                            task_context,
                            error,
                        )
                        .await
                        {
                            Ok(catch_context) => {
                                // successfully executed catch block (can retry)
                                if let Some(retry) = retry {
                                    // check retry condition
                                    if Self::eval_retry_condition(retry, &start_time, retry_count)
                                        .await
                                    {
                                        // continue retry
                                        retry_count += 1;
                                    } else {
                                        // retry limit reached or no retry
                                        break Ok(catch_context);
                                    }
                                } else {
                                    // no retry configured, just return catch context
                                    tracing::debug!(
                                        "No retry configured, continuing with catch context"
                                    );
                                    break Ok(catch_context);
                                }
                                task_context = catch_context;
                            }
                            Err(catch_error) => {
                                // catch block failed
                                tracing::error!("Catch block failed with error: {:?}", catch_error);
                                break Err(catch_error);
                            }
                        }
                    }
                }
            }?;

            // `do` in the end of retries
            if let Some(do_tasks) = &self.task.catch.do_ {
                // TODO return task_context in workflow::Error for catch block (to error recovering)
                self.execute_task_list(cx, "[try_do]", do_tasks, res.clone()) // XXX clone
                    .await
            } else {
                // go out of 'try'
                res.remove_position().await;
                Ok(res)
            }
        }
    }
}

impl TryTaskExecutor {
    async fn execute_task_list(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_name: &str,
        task_list: &workflow::TaskList,
        task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        use futures::StreamExt;
        if task_list.is_empty() {
            tracing::warn!("Task list is empty, nothing to execute");
            return Ok(task_context);
        }
        // for error output
        let try_position = {
            task_context
                .position
                .read()
                .await
                .as_error_instance()
                .clone()
        };

        let do_tasks = DoTask {
            do_: task_list.clone(), // XXX clone
            // use input and output as is
            input: Some(Input {
                from: Some(workflow::InputFrom::Variant0("${.}".to_string())), // raw jq
                ..Default::default()
            }),
            output: Some(workflow::Output {
                as_: Some(workflow::OutputAs::Variant0("${.}".to_string())), // raw jq
                schema: None,
            }),
            metadata: self.task.metadata.clone(),
            ..Default::default()
        };
        let do_stream_executor = Arc::new(DoTaskStreamExecutor::new(
            self.workflow_context.clone(),
            self.default_timeout,
            self.metadata.clone(),
            do_tasks,
            self.job_executors.clone(),
            self.http_client.clone(),
            self.checkpoint_repository.clone(),
            self.execution_id.clone(),
        ));

        let mut stream = do_stream_executor
            .execute_stream(cx, Arc::new(task_name.to_string()), task_context)
            .boxed();

        let mut last_context = None;
        while let Some(task_context) = stream.next().await {
            match task_context {
                Ok(context) => {
                    tracing::debug!(
                        "Task executed successfully: {}: {:#?}",
                        task_name,
                        context.raw_output
                    );
                    last_context = Some(context);
                }
                Err(e) => {
                    tracing::error!("Error executing task: {:?}", e);
                    return Err(e);
                }
            }
        }
        match last_context {
            Some(context) => Ok(context),
            None => Err(Box::new(workflow::Error {
                type_: workflow::UriTemplate("task-execution-error".to_string()),
                status: 400,
                detail: Some("No task context returned from task list execution".to_string()),
                title: None,
                instance: Some(try_position),
            })),
        }
    }

    // return need retry or not
    async fn eval_retry_condition(
        retry: &workflow::TryTaskCatchRetry,
        start_time: &Instant,
        retry_count: i64,
    ) -> bool {
        match retry {
            workflow::TryTaskCatchRetry::Variant0(workflow::RetryPolicy {
                backoff,
                delay,
                limit,
            }) => {
                tracing::debug!("Retry policy: {:?}", retry);
                if let Some(workflow::RetryLimit {
                    attempt: Some(workflow::RetryLimitAttempt { count, duration }),
                }) = limit.as_ref()
                {
                    // check retry count
                    if count.as_ref().is_some_and(|c| *c < retry_count) {
                        tracing::debug!("Retry limit reached");
                        return false;
                    }
                    // check retry duration
                    if let Some(duration) = duration {
                        if start_time.elapsed().as_secs_f64() > duration.to_millis() as f64 / 1000.0
                        {
                            tracing::debug!("Retry duration reached: {:?}", duration);
                            return false;
                        }
                    }
                    tracing::debug!("Retry count: ++{:?}", retry_count);
                } else {
                    tracing::warn!("Set retry without limit, so retry infinite");
                }

                // need to RETRY
                // delay and backoff
                if let Some(backoff) = backoff {
                    // TODO backoff structure is not defined in the spec
                    // only use as constant backoff
                    // https://github.com/serverlessworkflow/specification/blob/main/dsl-reference.md#backoff
                    tracing::warn!(
                        "Backoff: {:?}, but unimplemented now. consider as constant 1 second",
                        backoff
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
                if let Some(delay) = delay {
                    tracing::debug!("Delay: {:?}", delay);
                    tokio::time::sleep(tokio::time::Duration::from_millis(delay.to_millis())).await;
                }
                // loop again
                true
            }
            workflow::TryTaskCatchRetry::Variant1(label) => {
                tracing::error!("Retry policy label not implemented: {:?}, no retry", label);
                false
            }
        }
    }

    async fn execute_catch_block(
        catch_config: &TryTaskCatch,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
        error: Box<workflow::Error>,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        let should_filter = Self::eval_error_filter(catch_config, &error);
        if !should_filter {
            return Err(error); // no recover
        }

        task_context.add_position_name("catch".to_string()).await;
        // add error to task context
        let default_error_name = "error".to_string();
        let error_name = catch_config.as_.as_ref().unwrap_or(&default_error_name);
        task_context
            .add_context_value(
                error_name.clone(),
                serde_json::to_value(&error)
                    .inspect_err(|e| tracing::warn!("Failed to serialize error: {:#?}", e))
                    .unwrap_or(serde_json::Value::Null),
            )
            .await;

        let expression = Self::expression(
            &*workflow_context.read().await,
            Arc::new(task_context.clone()),
        )
        .await;
        let expression = match expression {
            Ok(expression) => expression,
            Err(mut e) => {
                tracing::error!("Failed to evaluate expression: {:#?}", e);
                let pos = task_context.position.read().await;
                e.position(&pos);
                return Err(e);
            }
        };

        // evaluate when and except_when conditions
        if let Some(when) = &catch_config.when {
            let eval_when = match Self::execute_transform_as_bool(
                task_context.raw_input.clone(),
                when,
                &expression,
            ) {
                Ok(value) => value,
                Err(mut e) => {
                    tracing::error!("Failed to evaluate when condition: {:#?}", e);
                    let pos = task_context.position.clone();
                    let mut pos = pos.write().await;
                    pos.push("when".to_string());
                    e.position(&pos);
                    return Err(e);
                }
            };

            if !eval_when {
                return Err(error);
            }
        }

        if let Some(except_when) = &catch_config.except_when {
            let eval_except_when = match Self::execute_transform_as_bool(
                task_context.raw_input.clone(),
                except_when,
                &expression,
            ) {
                Ok(value) => value,
                Err(mut e) => {
                    tracing::error!("Failed to evaluate except_when condition: {:#?}", e);
                    let pos = task_context.position.clone();
                    let mut pos = pos.write().await;
                    pos.push("except_when".to_string());
                    e.position(&pos);
                    return Err(e);
                }
            };
            if eval_except_when {
                return Err(error);
            }
        }
        task_context.remove_position().await;
        // will eval retry condition
        Ok(task_context)
    }

    // true if error should be filtered (catch block should be executed)
    fn eval_error_filter(catch_config: &TryTaskCatch, error: &workflow::Error) -> bool {
        if let Some(errors) = &catch_config.errors {
            if let Some(filter) = &errors.with {
                let workflow::Error {
                    type_,
                    status,
                    detail,
                    title,
                    instance,
                } = &error;

                let should_filter_status = filter
                    .status
                    .as_ref()
                    .map(|st| st == status)
                    .unwrap_or(true);
                let should_filter_type = filter
                    .type_
                    .as_ref()
                    .map(|tp| type_.is_match(tp))
                    .unwrap_or(true);
                let should_filter_details = filter
                    .details
                    .as_ref()
                    .map(|ds| {
                        detail
                            .as_ref()
                            .is_some_and(|detail| ds == &detail.to_string())
                    })
                    .unwrap_or(true);
                let should_filter_title = filter
                    .title
                    .as_ref()
                    .map(|t| title.as_ref().is_some_and(|title| t == &title.to_string()))
                    .unwrap_or(true);
                let should_filter_instance = filter
                    .instance
                    .as_ref()
                    .map(|ins| {
                        instance
                            .as_ref()
                            .is_some_and(|instance| ins == &instance.to_string())
                    })
                    .unwrap_or(true);
                tracing::debug!(
                    "Error filter: status: {}, type: {}, details: {}, title: {}, instance: {}",
                    should_filter_status,
                    should_filter_type,
                    should_filter_details,
                    should_filter_title,
                    should_filter_instance
                );

                should_filter_status
                    && should_filter_type
                    && should_filter_details
                    && should_filter_title
                    && should_filter_instance
            } else {
                // no error filter, so all errors are caught
                true
            }
        } else {
            // no error filter, so all errors are caught
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::{
        definition::{
            workflow::{
                self, Error, ErrorFilter, RetryLimit, RetryLimitAttempt, RetryPolicy, TaskList,
                TryTaskCatch, TryTaskCatchRetry, UriTemplate,
            },
            WorkflowLoader,
        },
        execute::context::{TaskContext, WorkflowContext, WorkflowStatus},
    };
    use app::module::test::create_hybrid_test_app;
    use std::time::Duration;
    use tokio::sync::Mutex;

    // Helper function for tests
    async fn setup_test(
        try_task_config: Option<workflow::TryTask>,
    ) -> (TryTaskExecutor, Arc<RwLock<WorkflowContext>>, TaskContext) {
        let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
        let job_executors = Arc::new(JobExecutorWrapper::new(app_module));
        let http_client = reqwest::ReqwestClient::new(None, None, None, None).unwrap();

        let loader = WorkflowLoader::new(http_client.clone()).unwrap();
        let flow = loader
            .load_workflow(Some("test-files/ls-test.yaml"), None, false)
            .await
            .unwrap();

        // Generate TryTask (stored with static lifetime)
        let try_task = Box::leak(Box::new(try_task_config.unwrap_or_else(|| {
            workflow::TryTask {
                try_: TaskList::default(),
                catch: TryTaskCatch {
                    errors: None,
                    as_: Some("error".to_string()),
                    when: None,
                    except_when: None,
                    retry: Some(TryTaskCatchRetry::Variant0(RetryPolicy {
                        backoff: None,
                        delay: None,
                        limit: Some(RetryLimit {
                            attempt: Some(RetryLimitAttempt {
                                count: Some(3),
                                duration: None,
                            }),
                        }),
                    })),
                    do_: None,
                },
                ..Default::default()
            }
        })));

        let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
            &flow,
            Arc::new(serde_json::Value::Object(Default::default())),
            Arc::new(serde_json::Value::Object(Default::default())),
            None,
        )));
        workflow_context.write().await.status = WorkflowStatus::Running;
        let task_context = TaskContext::new(
            None,
            Arc::new(serde_json::Value::String("test-task-input0".to_string())),
            Arc::new(Mutex::new(Default::default())),
        );

        let try_executor = TryTaskExecutor::new(
            workflow_context.clone(),
            Duration::from_secs(60), // default timeout
            try_task.clone(),
            job_executors.clone(),
            http_client,
            None,
            None,
            Arc::new(Default::default()),
        );

        (try_executor, workflow_context, task_context)
    }

    // Helper function to create test errors
    fn create_test_error(
        status: &str,
        error_type: &str,
        detail: Option<&str>,
        title: Option<&str>,
    ) -> Box<Error> {
        Box::new(Error {
            type_: UriTemplate(format!("http-error://{}", error_type.to_lowercase())),
            status: status.parse().unwrap_or(500),
            detail: detail.map(|d| d.to_string()),
            title: title.map(|t| t.to_string()),
            instance: None,
        })
    }

    #[test]
    fn test_eval_retry_condition_with_count_limit() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let retry = workflow::TryTaskCatchRetry::Variant0(RetryPolicy {
                backoff: None,
                delay: None,
                limit: Some(RetryLimit {
                    attempt: Some(RetryLimitAttempt {
                        count: Some(3),
                        duration: None,
                    }),
                }),
            });

            let start_time = Instant::now();
            let result = TryTaskExecutor::eval_retry_condition(&retry, &start_time, 1).await;
            assert!(
                result,
                "Should return true when retry count is below the limit"
            );

            // When retry count equals the limit
            let result = TryTaskExecutor::eval_retry_condition(&retry, &start_time, 3).await;
            assert!(
                result,
                "Should return true when retry count equals the limit"
            );

            // When retry count exceeds the limit
            let result = TryTaskExecutor::eval_retry_condition(&retry, &start_time, 4).await;
            assert!(
                !result,
                "Should return false when retry count exceeds the limit"
            );
        })
    }

    #[test]
    fn test_eval_retry_condition_with_duration_limit() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            // Time limit retry policy
            let retry = workflow::TryTaskCatchRetry::Variant0(RetryPolicy {
                backoff: None,
                delay: None,
                limit: Some(RetryLimit {
                    attempt: Some(RetryLimitAttempt {
                        count: None,
                        duration: Some(workflow::Duration::from_millis(100)),
                    }),
                }),
            });

            let start_time = Instant::now();
            let result = TryTaskExecutor::eval_retry_condition(&retry, &start_time, 1).await;
            assert!(
                result,
                "Should return true when elapsed time is below the limit"
            );

            // Simulate exceeding time limit (actually wait)
            tokio::time::sleep(Duration::from_millis(110)).await;
            let result = TryTaskExecutor::eval_retry_condition(&retry, &start_time, 1).await;
            assert!(
                !result,
                "Should return false when elapsed time exceeds the limit"
            );
        })
    }

    #[test]
    fn test_eval_retry_condition_with_delay() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let retry = workflow::TryTaskCatchRetry::Variant0(RetryPolicy {
                backoff: None,
                delay: Some(workflow::Duration::from_millis(50)),
                limit: Some(RetryLimit {
                    attempt: Some(RetryLimitAttempt {
                        count: Some(3),
                        duration: None,
                    }),
                }),
            });

            let start_time = Instant::now();
            // Record start time
            let before = Instant::now();
            let result = TryTaskExecutor::eval_retry_condition(&retry, &start_time, 1).await;
            let elapsed = before.elapsed();

            assert!(
                result,
                "Should return true if conditions are met even with delay"
            );
            assert!(
                elapsed.as_millis() >= 50,
                "Should wait at least the specified delay time"
            );
        })
    }

    #[test]
    fn test_eval_error_filter() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let try_task = workflow::TryTask {
                try_: TaskList::default(),
                catch: TryTaskCatch {
                    errors: Some(workflow::CatchErrors {
                        with: Some(ErrorFilter {
                            status: Some(404),
                            type_: Some("NotFound".to_string()),
                            details: None,
                            title: None,
                            instance: None,
                        }),
                    }),
                    as_: Some("error".to_string()),
                    when: None,
                    except_when: None,
                    retry: None,
                    do_: None,
                },
                ..Default::default()
            };

            let (executor, _, _) = setup_test(Some(try_task)).await;

            // Matching error
            let matching_error = create_test_error("404", "notfound", None, None);

            let catch_config = executor.task.catch.clone();
            assert!(
                TryTaskExecutor::eval_error_filter(&catch_config, &matching_error),
                "Should return true when error matches filter conditions"
            );

            // Non-matching error (different status)
            let non_matching_error1 = create_test_error("500", "notfound", None, None);

            assert!(
                !TryTaskExecutor::eval_error_filter(&catch_config, &non_matching_error1),
                "Should return false when error doesn't match filter conditions"
            );

            // Non-matching error (different type)
            let non_matching_error2 = create_test_error("404", "servererror", None, None);

            assert!(
                !TryTaskExecutor::eval_error_filter(&catch_config, &non_matching_error2),
                "Should return false when error doesn't match filter conditions"
            );
        })
    }

    #[test]
    fn test_execute_catch_block() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let try_task = workflow::TryTask {
                try_: TaskList::default(),
                catch: TryTaskCatch {
                    errors: None,
                    as_: Some("custom_error".to_string()),
                    when: None,
                    except_when: None,
                    retry: None,
                    do_: None,
                },
                ..Default::default()
            };

            let (executor, workflow_context, task_context) = setup_test(Some(try_task)).await;

            let error = create_test_error(
                "500",
                "testerror",
                Some("Test error detail"),
                Some("Test Error Title"),
            );

            // catch
            let result = TryTaskExecutor::execute_catch_block(
                &executor.task.catch,
                workflow_context,
                task_context.clone(),
                error.clone(),
            )
            .await;

            assert!(
                result.is_ok(),
                "When error matches filter, catch block should succeed"
            );

            // Verify error information is added to context
            let context = result.unwrap();
            let error_value = context.get_context_value("custom_error").await;
            assert!(
                error_value.is_some(),
                "Error information should be added to context"
            );
            // println!("error_value: {:?}", error_value);

            // Verify error information content
            let error_json = error_value.unwrap();
            let error_status = error_json["status"].as_i64().unwrap();
            assert_eq!(error_status, 500, "Error status should be correctly stored");
        })
    }

    #[test]
    fn test_trytask_success_output_passthrough() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
            // Create a TaskList with a DoTask that sets a known output
            use std::collections::HashMap;
            let set_task = workflow::SetTask {
                set: serde_json::json!({
                    "name": "test_output".to_string(),
                    "value": serde_json::json!("test_output_value"),
                })
                .as_object()
                .unwrap()
                .clone(),
                output: Some(workflow::Output {
                    as_: Some(workflow::OutputAs::Variant0("test_output".to_string())),
                    schema: None,
                }),
                ..Default::default()
            };
            let set_task_list = workflow::TaskList(vec![HashMap::from([(
                "set_task1".to_string(),
                workflow::Task::SetTask(set_task),
            )])]);

            let mut do_task_map = HashMap::new();
            do_task_map.insert(
                "test_do1".to_string(),
                workflow::Task::DoTask(DoTask {
                    do_: set_task_list,
                    output: Some(workflow::Output {
                        as_: Some(workflow::OutputAs::Variant0("${.}".to_string())),
                        schema: None,
                    }),
                    ..Default::default()
                }),
            );
            let task_list = workflow::TaskList(vec![do_task_map]);
            let try_task = workflow::TryTask {
                try_: task_list,
                catch: TryTaskCatch {
                    errors: None,
                    as_: Some("error".to_string()),
                    when: None,
                    except_when: None,
                    retry: None,
                    do_: None,
                },
                ..Default::default()
            };

            let (executor, _workflow_context, task_context) = setup_test(Some(try_task)).await;

            // Execute TryTaskExecutor
            let cx = Arc::new(opentelemetry::Context::current());
            let result = executor.execute(cx, "test-task", task_context).await;

            assert!(result.is_ok(), "TryTask should succeed");
            let context = result.unwrap();
            let output = &*context.output;
            // output should be an empty object (default Output)
            assert_eq!(
                *output,
                serde_json::json!("test_output"),
                "Output should be passed through from the inner task"
            );
        })
    }
}
