use crate::workflow::{
    definition::workflow::{DoTask, TryTaskCatch},
    execute::{
        context::{TaskContext, WorkflowContext},
        job::JobExecutorWrapper,
        task::{Result, TaskExecutor, TaskExecutorTrait},
    },
};
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow,
    },
    execute::expression::UseExpression,
};
use infra_utils::infra::net::reqwest;
use std::sync::Arc;
use tokio::{sync::RwLock, time::Instant};

pub struct TryTaskExecutor<'a> {
    task: &'a workflow::TryTask,
    job_executors: Arc<JobExecutorWrapper>,
    http_client: reqwest::ReqwestClient,
}
impl<'a> TryTaskExecutor<'a> {
    pub fn new(
        task: &'a workflow::TryTask,
        job_executors: Arc<JobExecutorWrapper>,
        http_client: reqwest::ReqwestClient,
    ) -> Self {
        Self {
            task,
            job_executors,
            http_client,
        }
    }
}

impl UseExpression for TryTaskExecutor<'_> {}
impl UseExpressionTransformer for TryTaskExecutor<'_> {}
impl UseJqAndTemplateTransformer for TryTaskExecutor<'_> {}

impl TaskExecutorTrait<'_> for TryTaskExecutor<'_> {
    #[allow(clippy::manual_async_fn)]
    fn execute(
        &self,
        task_name: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
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
                        task_name,
                        &self.task.try_,
                        workflow_context.clone(),
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
                            workflow_context.clone(),
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
                self.execute_task_list("[try_do]", do_tasks, workflow_context, res.clone()) // XXX clone
                    .await?;
            }
            // go out of 'try'
            res.remove_position().await;
            Ok(res)
        }
    }
}

impl TryTaskExecutor<'_> {
    async fn execute_task_list(
        &self,
        task_name: &str,
        task_list: &workflow::TaskList,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        let do_tasks = DoTask {
            do_: task_list.clone(), // XXX clone
            ..Default::default()
        };
        let task_executor = TaskExecutor::new(
            self.job_executors.clone(),
            self.http_client.clone(),
            task_name,
            Arc::new(workflow::Task::DoTask(do_tasks)),
        );
        // for recursive call
        Box::pin(task_executor.execute(workflow_context.clone(), Arc::new(task_context))).await
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
                e.position(&task_context.position.lock().await.clone());
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
                    let mut pos = task_context.position.lock().await.clone();
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
                    let mut pos = task_context.position.lock().await.clone();
                    pos.push("except_when".to_string());
                    e.position(&pos);
                    return Err(e);
                }
            };
            if eval_except_when {
                return Err(error);
            }
        }
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
                self, Error, ErrorDetails, ErrorFilter, ErrorTitle, ErrorType, RetryLimit,
                RetryLimitAttempt, RetryPolicy, TaskList, TryTaskCatch, TryTaskCatchRetry,
                UriTemplate,
            },
            WorkflowLoader,
        },
        execute::context::{TaskContext, WorkflowContext},
    };
    use app::module::test::create_hybrid_test_app;
    use std::time::Duration;
    use tokio::sync::Mutex;

    // Helper function for tests
    async fn setup_test(
        try_task_config: Option<workflow::TryTask>,
    ) -> (
        TryTaskExecutor<'static>,
        Arc<RwLock<WorkflowContext>>,
        TaskContext,
    ) {
        let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
        let job_executors = Arc::new(JobExecutorWrapper::new(app_module));
        let http_client = reqwest::ReqwestClient::new(None, None, None, None).unwrap();

        let loader = WorkflowLoader::new(http_client.clone()).unwrap();
        let flow = loader
            .load_workflow(Some("test-files/ls-test.yaml"), None)
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
        )));
        let task_context = TaskContext::new(
            None,
            Arc::new(serde_json::Value::Null),
            Arc::new(Mutex::new(Default::default())),
        );

        let try_executor = TryTaskExecutor::new(try_task, job_executors.clone(), http_client);

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
            type_: ErrorType::UriTemplate(UriTemplate(format!(
                "http-error://{}",
                error_type.to_lowercase()
            ))),
            status: status.parse().unwrap_or(500),
            detail: detail.map(|d| ErrorDetails {
                subtype_0: Some(workflow::RuntimeExpression(d.to_string())),
                subtype_1: None,
            }),
            title: title.map(|t| ErrorTitle {
                subtype_0: Some(workflow::RuntimeExpression(t.to_string())),
                subtype_1: None,
            }),
            instance: None,
        })
    }

    #[tokio::test]
    async fn test_eval_retry_condition_with_count_limit() {
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
    }

    #[tokio::test]
    async fn test_eval_retry_condition_with_duration_limit() {
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
    }

    #[tokio::test]
    async fn test_eval_retry_condition_with_delay() {
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
            println!("context: {:?}", context);
            println!("error_value: {:?}", error_value);

            // Verify error information content
            let error_json = error_value.unwrap();
            let error_status = error_json["status"].as_i64().unwrap();
            assert_eq!(error_status, 500, "Error status should be correctly stored");
        })
    }
}
