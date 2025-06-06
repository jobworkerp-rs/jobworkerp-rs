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
        task::{trace::TaskTracing, TaskExecutor},
    },
};
use anyhow::Result;
use async_stream::stream;
use debug_stub_derive::DebugStub;
use futures::StreamExt;
use indexmap::IndexMap;
use infra_utils::infra::{net::reqwest, trace::Tracing};
use jobworkerp_base::APP_WORKER_NAME;
use opentelemetry::trace::TraceContextExt;
use std::{collections::HashMap, pin::Pin, sync::Arc};
use tokio::sync::RwLock;

#[derive(DebugStub, Clone)]
pub struct DoTaskStreamExecutor {
    // for secret metadata
    #[debug_stub = "HashMap<String, String>"]
    metadata: Arc<HashMap<String, String>>,
    task: workflow::DoTask,
    #[debug_stub = "AppModule"]
    pub job_executor_wrapper: Arc<JobExecutorWrapper>,
    #[debug_stub = "reqwest::HttpClient"]
    pub http_client: reqwest::ReqwestClient,
}
impl UseJqAndTemplateTransformer for DoTaskStreamExecutor {}
impl UseExpression for DoTaskStreamExecutor {}
impl Tracing for DoTaskStreamExecutor {}
impl TaskTracing for DoTaskStreamExecutor {}

impl DoTaskStreamExecutor {
    pub fn new(
        metadata: Arc<HashMap<String, String>>,
        task: workflow::DoTask,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        http_client: reqwest::ReqwestClient,
    ) -> Self {
        Self {
            metadata,
            task,
            job_executor_wrapper,
            http_client,
        }
    }

    fn execute_task_stream(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_map: Arc<IndexMap<String, (u32, Arc<Task>)>>,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        parent_task_context: TaskContext,
    ) -> Pin<Box<dyn futures::Stream<Item = Result<TaskContext, Box<workflow::Error>>> + Send + '_>>
    {
        let job_exec = self.job_executor_wrapper.clone();
        let http_client = self.http_client.clone();
        let req_meta = self.metadata.clone();

        Box::pin(stream! {
            let mut prev = parent_task_context;
            let mut iter = task_map.iter();
            let mut current_pair = iter.next().map(|(k, v)| (k.clone(), v.clone()));
            // position name will be added in execute_task_stream
            prev.add_position_name("do".to_string()).await;

            while let Some((name, (pos, task))) = current_pair {
                let span = Self::start_child_otel_span(
                    &cx,
                    APP_WORKER_NAME,
                    format!("do_task_{}:{}", &pos, &name),
                );
                let ccx = Arc::new(opentelemetry::Context::current_with_span(span));
                let ccx_clone = ccx.clone();
                let mut span = ccx_clone.span();

                tracing::info!("Starting task execution: {}", name);

                prev.add_position_index(pos).await;
                // Stop if not running
                if workflow_context.read().await.status != WorkflowStatus::Running {
                    prev.remove_position().await;
                    break;
                }
                Self::record_task_input(&mut span, name.clone(), &prev,
                    prev.position.read().await.as_json_pointer().to_string());

                tracing::info!("Executing task: {}", &name);
                let mut stream = TaskExecutor::new(
                    job_exec.clone(),
                    http_client.clone(),
                    &name,
                    task.clone(),
                    req_meta.clone(),
                )
                .execute(
                    ccx.clone(),
                    workflow_context.clone(),
                    Arc::new(prev.clone()),
                )
                .await;

                let mut last_ctx = None;
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(ctx) => {
                            last_ctx = Some(ctx.clone());
                            yield Ok(ctx);
                        }
                        Err(mut e) => {
                            let pos = prev.position.read().await;
                            e.position(&pos);
                            yield Err(e);
                            prev.remove_position().await;
                            return;
                        }
                    }
                }

                let result = match last_ctx {
                    Some(c) => c,
                    None => {
                        tracing::warn!("No task context received, skipping task execution.");
                        prev.remove_position().await;
                        break;
                    }
                };

                // Record task execution result to span attributes
                let execution_duration = result
                    .completed_at
                    .unwrap_or_else(command_utils::util::datetime::now)
                    - result.started_at;

                Self::record_task_output(&span, &result, execution_duration.num_milliseconds());

                tracing::info!(
                    task_name = %name,
                    "Task execution completed"
                );
                result.remove_position().await;

                let next_pair = iter.next();
                // Determine next task
                current_pair = match &result.flow_directive {
                    Then::Continue =>{
                        if next_pair.is_none() {
                            None
                        } else {
                            next_pair.map(|(k, v)| (k.clone(), v.clone()))
                        }
                    }
                    Then::End => {
                        // End of workflow
                        workflow_context.write().await.status = WorkflowStatus::Completed;
                        None
                    }
                    Then::Exit => {
                        // End of task list
                        tracing::info!("Exit Task: {}", name);
                        None
                    }
                    Then::TaskName(ref tname) => {
                        tracing::info!("Jump to task: {}", tname);
                        task_map
                            .iter()
                            .find(|(k, _)| *k == tname)
                            .map(|(k, v)| (k.clone(), v.clone()))
                    }
                };
                prev = result;
            }
            prev.remove_position().await;
        })
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
        cx: Arc<opentelemetry::Context>,
        task_name: Arc<String>,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> impl futures::Stream<Item = Result<TaskContext, Box<workflow::Error>>> + Send {
        let task_context = task_context.clone();
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

        self.execute_task_stream(cx, task_map, workflow_context, task_context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::definition::workflow::{
        Document, FlowDirective, FlowDirectiveEnum, Input, Output, SetTask, Task, TaskList,
        WorkflowName, WorkflowSchema, WorkflowVersion,
    };
    use crate::workflow::execute::context::{TaskContext, WorkflowContext, WorkflowStatus};
    use app::module::test::create_hybrid_test_app;
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::sync::Arc;
    use tokio::sync::{Mutex, RwLock};

    fn create_test_workflow() -> WorkflowSchema {
        let task_map_list = {
            let mut map1 = HashMap::new();
            map1.insert(
                "task1".to_string(),
                Task::SetTask(SetTask {
                    set: serde_json::Map::new(),
                    export: None,
                    if_: None,
                    input: None,
                    metadata: serde_json::Map::new(),
                    output: None,
                    then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Continue)),
                    timeout: None,
                }),
            );
            let mut map2 = HashMap::new();
            map2.insert(
                "task2".to_string(),
                Task::SetTask(SetTask {
                    set: serde_json::Map::new(),
                    export: None,
                    if_: None,
                    input: None,
                    metadata: serde_json::Map::new(),
                    output: None,
                    then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                    timeout: None,
                }),
            );
            vec![map1, map2]
        };
        let task_list = TaskList(task_map_list);

        WorkflowSchema {
            document: Document {
                name: WorkflowName::from_str("test-workflow").unwrap(),
                version: WorkflowVersion::from_str("1.0.0").unwrap(),
                metadata: serde_json::Map::new(),
                ..Default::default()
            },
            input: Input {
                schema: None,
                from: None,
            },
            output: Some(Output {
                as_: None,
                schema: None,
            }),
            do_: task_list,
        }
    }

    #[test]
    fn test_execute_stream() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let http_client = reqwest::ReqwestClient::new(
                Some("test"),
                Some(std::time::Duration::from_secs(1)),
                Some(std::time::Duration::from_secs(1)),
                Some(1),
            )
            .unwrap();

            let workflow = create_test_workflow();
            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context,
            )));

            let do_task = workflow.create_do_task();
            let executor = DoTaskStreamExecutor::new(
                Arc::new(HashMap::new()),
                do_task,
                Arc::new(JobExecutorWrapper::new(app_module)),
                http_client,
            );

            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );

            workflow_context.write().await.status = WorkflowStatus::Running;
            let mut task_stream = executor.execute_stream(
                Arc::new(opentelemetry::Context::current()),
                Arc::new("test".to_string()),
                workflow_context,
                task_context,
            );

            let mut final_tc = None;
            while let Some(tc) = task_stream.next().await {
                match tc {
                    Ok(tc) => {
                        final_tc = Some(tc);
                    }
                    Err(_) => {
                        final_tc = None;
                        break;
                    }
                }
            }

            assert!(final_tc.is_some());
            let prev_pos = final_tc.unwrap().prev_position(3).await;
            assert_eq!(
                prev_pos.last().unwrap().as_str().unwrap(),
                "task2".to_string()
            );
        })
    }

    #[test]
    fn test_execute_stream_with_flow_directives() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let http_client = reqwest::ReqwestClient::new(
                Some("test"),
                Some(std::time::Duration::from_secs(1)),
                Some(std::time::Duration::from_secs(1)),
                Some(1),
            )
            .unwrap();

            let mut workflow = create_test_workflow();

            let task_list = TaskList(vec![
                {
                    let mut map = HashMap::new();
                    map.insert(
                        "task1".to_string(),
                        Task::SetTask(SetTask {
                            set: serde_json::Map::new(),
                            export: None,
                            if_: None,
                            input: None,
                            metadata: serde_json::Map::new(),
                            output: None,
                            then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Continue)),
                            timeout: None,
                        }),
                    );
                    map
                },
                {
                    let mut map = HashMap::new();
                    map.insert(
                        "task2".to_string(),
                        Task::SetTask(SetTask {
                            set: serde_json::Map::new(),
                            export: None,
                            if_: None,
                            input: None,
                            metadata: serde_json::Map::new(),
                            output: None,
                            then: Some(FlowDirective::Variant1("task4".to_string())),
                            timeout: None,
                        }),
                    );
                    map
                },
                {
                    let mut map = HashMap::new();
                    map.insert(
                        "task3".to_string(),
                        Task::SetTask(SetTask {
                            set: serde_json::Map::new(),
                            export: None,
                            if_: None,
                            input: None,
                            metadata: serde_json::Map::new(),
                            output: None,
                            then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Exit)),
                            timeout: None,
                        }),
                    );
                    map
                },
                {
                    let mut map = HashMap::new();
                    map.insert(
                        "task4".to_string(),
                        Task::SetTask(SetTask {
                            set: serde_json::Map::new(),
                            export: None,
                            if_: None,
                            input: None,
                            metadata: serde_json::Map::new(),
                            output: None,
                            then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                            timeout: None,
                        }),
                    );
                    map
                },
            ]);

            workflow.do_ = task_list;

            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));

            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context,
            )));

            let do_task = workflow.create_do_task();
            let executor = DoTaskStreamExecutor::new(
                Arc::new(HashMap::new()),
                do_task,
                Arc::new(JobExecutorWrapper::new(app_module)),
                http_client,
            );

            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );

            workflow_context.write().await.status = WorkflowStatus::Running;
            let mut task_stream = executor.execute_stream(
                Arc::new(opentelemetry::Context::current()),
                Arc::new("test".to_string()),
                workflow_context,
                task_context,
            );

            let mut final_tc = None;
            while let Some(tc) = task_stream.next().await {
                match tc {
                    Ok(tc) => {
                        final_tc = Some(tc);
                    }
                    Err(_) => {
                        final_tc = None;
                        break;
                    }
                }
            }

            assert!(final_tc.is_some());
            let prev_pos = final_tc.unwrap().prev_position(3).await;
            assert_eq!(
                prev_pos.last().unwrap().as_str().unwrap(),
                "task4".to_string()
            );
        })
    }

    #[test]
    fn test_execute_stream_flow_exit_and_end() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let http_client = reqwest::ReqwestClient::new(
                Some("test"),
                Some(std::time::Duration::from_secs(1)),
                Some(std::time::Duration::from_secs(1)),
                Some(1),
            )
            .unwrap();

            let workflow = create_test_workflow();
            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));

            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

            // exit test
            {
                let mut exit_workflow = workflow.clone();
                exit_workflow.do_ = TaskList(vec![
                    {
                        let mut map = HashMap::new();
                        map.insert(
                            "exit_task".to_string(),
                            Task::SetTask(SetTask {
                                set: serde_json::Map::new(),
                                export: None,
                                if_: None,
                                input: None,
                                metadata: serde_json::Map::new(),
                                output: None,
                                then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Exit)),
                                timeout: None,
                            }),
                        );
                        map
                    },
                    {
                        let mut map = HashMap::new();
                        map.insert(
                            "unreachable_task".to_string(),
                            Task::SetTask(SetTask {
                                set: serde_json::Map::new(),
                                export: None,
                                if_: None,
                                input: None,
                                metadata: serde_json::Map::new(),
                                output: None,
                                then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Continue)),
                                timeout: None,
                            }),
                        );
                        map
                    },
                ]);

                let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                    &exit_workflow,
                    input.clone(),
                    context.clone(),
                )));

                let do_task = exit_workflow.create_do_task();
                let executor = DoTaskStreamExecutor::new(
                    Arc::new(HashMap::new()),
                    do_task,
                    Arc::new(JobExecutorWrapper::new(app_module.clone())),
                    http_client.clone(),
                );

                let task_context = TaskContext::new(
                    None,
                    input.clone(),
                    Arc::new(Mutex::new(serde_json::Map::new())),
                );

                workflow_context.write().await.status = WorkflowStatus::Running;
                let mut task_stream = executor.execute_stream(
                    Arc::new(opentelemetry::Context::current()),
                    Arc::new("test".to_string()),
                    workflow_context.clone(),
                    task_context,
                );

                let mut final_tc = None;
                while let Some(tc) = task_stream.next().await {
                    match tc {
                        Ok(tc) => {
                            final_tc = Some(tc);
                        }
                        Err(_) => {
                            final_tc = None;
                            break;
                        }
                    }
                }

                assert!(final_tc.is_some());
                let prev_pos = final_tc.unwrap().prev_position(3).await;
                assert_eq!(
                    prev_pos.last().unwrap().as_str().unwrap(),
                    "exit_task".to_string()
                );
            }

            // End test
            {
                let mut end_workflow = workflow.clone();
                end_workflow.do_ = TaskList(vec![{
                    let mut map = HashMap::new();
                    map.insert(
                        "end_task".to_string(),
                        Task::SetTask(SetTask {
                            set: serde_json::Map::new(),
                            export: None,
                            if_: None,
                            input: None,
                            metadata: serde_json::Map::new(),
                            output: None,
                            then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                            timeout: None,
                        }),
                    );
                    map
                }]);

                let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                    &end_workflow,
                    input.clone(),
                    context.clone(),
                )));

                let do_task = end_workflow.create_do_task();
                let executor = DoTaskStreamExecutor::new(
                    Arc::new(HashMap::new()),
                    do_task,
                    Arc::new(JobExecutorWrapper::new(app_module.clone())),
                    http_client.clone(),
                );

                let task_context = TaskContext::new(
                    None,
                    input.clone(),
                    Arc::new(Mutex::new(serde_json::Map::new())),
                );

                workflow_context.write().await.status = WorkflowStatus::Running;
                let mut task_stream = executor.execute_stream(
                    Arc::new(opentelemetry::Context::current()),
                    Arc::new("test".to_string()),
                    workflow_context.clone(),
                    task_context,
                );

                let mut final_tc = None;
                while let Some(tc) = task_stream.next().await {
                    match tc {
                        Ok(tc) => {
                            final_tc = Some(tc);
                        }
                        Err(_) => {
                            final_tc = None;
                            break;
                        }
                    }
                }

                assert!(final_tc.is_some());
                let prev_pos = final_tc.unwrap().prev_position(3).await;
                assert_eq!(
                    prev_pos.last().unwrap().as_str().unwrap(),
                    "end_task".to_string()
                );
                assert_eq!(
                    workflow_context.read().await.status,
                    WorkflowStatus::Completed
                );
            }
        })
    }

    #[test]
    fn test_execute_stream_task_name_jump() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let http_client = reqwest::ReqwestClient::new(
                Some("test"),
                Some(std::time::Duration::from_secs(1)),
                Some(std::time::Duration::from_secs(1)),
                Some(1),
            )
            .unwrap();

            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));

            let workflow = Arc::new(WorkflowSchema {
                document: Document {
                    name: WorkflowName::from_str("test-workflow").unwrap(),
                    version: WorkflowVersion::from_str("1.0.0").unwrap(),
                    metadata: serde_json::Map::new(),
                    ..Default::default()
                },
                input: Input {
                    schema: None,
                    from: None,
                },
                output: None,
                do_: TaskList(vec![
                    {
                        let mut map = HashMap::new();
                        map.insert(
                            "start_task".to_string(),
                            Task::SetTask(SetTask {
                                set: serde_json::Map::new(),
                                export: None,
                                if_: None,
                                input: None,
                                metadata: serde_json::Map::new(),
                                output: None,
                                then: Some(FlowDirective::Variant1("jump_target".to_string())),
                                timeout: None,
                            }),
                        );
                        map
                    },
                    {
                        let mut map = HashMap::new();
                        map.insert(
                            "skipped_task".to_string(),
                            Task::SetTask(SetTask {
                                set: serde_json::Map::new(),
                                export: None,
                                if_: None,
                                input: None,
                                metadata: serde_json::Map::new(),
                                output: None,
                                then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Exit)),
                                timeout: None,
                            }),
                        );
                        map
                    },
                    {
                        let mut map = HashMap::new();
                        map.insert(
                            "jump_target".to_string(),
                            Task::SetTask(SetTask {
                                set: serde_json::Map::new(),
                                export: None,
                                if_: None,
                                input: None,
                                metadata: serde_json::Map::new(),
                                output: None,
                                then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                                timeout: None,
                            }),
                        );
                        map
                    },
                ]),
            });

            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context,
            )));

            let do_task = workflow.create_do_task();
            let executor = DoTaskStreamExecutor::new(
                Arc::new(HashMap::new()),
                do_task,
                Arc::new(JobExecutorWrapper::new(app_module)),
                http_client,
            );

            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );

            workflow_context.write().await.status = WorkflowStatus::Running;
            let mut task_stream = executor.execute_stream(
                Arc::new(opentelemetry::Context::current()),
                Arc::new("test".to_string()),
                workflow_context.clone(),
                task_context,
            );

            let mut final_tc = None;
            while let Some(tc) = task_stream.next().await {
                match tc {
                    Ok(tc) => {
                        final_tc = Some(tc);
                    }
                    Err(_) => {
                        final_tc = None;
                        break;
                    }
                }
            }

            assert!(final_tc.is_some());
            let prev_pos = final_tc.unwrap().prev_position(3).await;
            assert_eq!(
                prev_pos.last().unwrap().as_str().unwrap(),
                "jump_target".to_string()
            );
            assert_eq!(
                workflow_context.read().await.status,
                WorkflowStatus::Completed
            );
        })
    }
}
