use super::super::StreamTaskExecutorTrait;
use crate::workflow::{
    definition::{
        transform::UseJqAndTemplateTransformer,
        workflow::{self, tasks::TaskTrait, Task},
    },
    execute::{
        context::{TaskContext, Then, WorkflowContext, WorkflowStatus},
        expression::UseExpression,
        task::{trace::TaskTracing, ExecutionId, TaskExecutor},
    },
};
use anyhow::Result;
use app::app::job::execute::JobExecutorWrapper;
use async_stream::stream;
use debug_stub_derive::DebugStub;
use futures::StreamExt;
use indexmap::IndexMap;
use jobworkerp_base::APP_WORKER_NAME;
use net_utils::{net::reqwest, trace::Tracing};
use opentelemetry::trace::TraceContextExt;
use std::{collections::HashMap, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::RwLock;

// for DebugStub
type CheckPointRepo =
    Arc<dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId>;

#[derive(DebugStub, Clone)]
pub struct DoTaskStreamExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    default_timeout: Duration, // default timeout for tasks
    // for secret metadata
    #[debug_stub = "HashMap<String, String>"]
    metadata: Arc<HashMap<String, String>>,
    task: workflow::DoTask,
    #[debug_stub = "AppModule"]
    pub job_executor_wrapper: Arc<JobExecutorWrapper>,
    #[debug_stub = "reqwest::HttpClient"]
    pub http_client: reqwest::ReqwestClient,
    #[debug_stub = "Option<Arc<dyn UseCheckPointRepository>>"]
    pub checkpoint_repository: Option<CheckPointRepo>,
    pub execution_id: Option<Arc<ExecutionId>>,
}
impl UseJqAndTemplateTransformer for DoTaskStreamExecutor {}
impl UseExpression for DoTaskStreamExecutor {}
impl Tracing for DoTaskStreamExecutor {}
impl TaskTracing for DoTaskStreamExecutor {}

impl DoTaskStreamExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workflow_context: Arc<RwLock<WorkflowContext>>,
        default_timeout: Duration,
        metadata: Arc<HashMap<String, String>>,
        task: workflow::DoTask,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        http_client: reqwest::ReqwestClient,
        checkpoint_repository: Option<
            Arc<dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId>,
        >,
        execution_id: Option<Arc<ExecutionId>>,
    ) -> Self {
        Self {
            workflow_context,
            default_timeout,
            metadata,
            task,
            job_executor_wrapper,
            http_client,
            checkpoint_repository,
            execution_id,
        }
    }
    async fn find_checkpoint_task(
        &self,
        parent_task_context: &TaskContext,
        task_map: Arc<IndexMap<String, (u32, Arc<Task>)>>,
    ) -> Option<u32> {
        let checkpoint_pos = self
            .workflow_context
            .read()
            .await
            .checkpoint_position
            .clone();
        if let Some(pos) = checkpoint_pos {
            tracing::debug!(
                "Finding checkpoint task for position: {}",
                pos.as_json_pointer()
            );
            let relative_path = pos.relative_path(parent_task_context.position.read().await.full());
            if let Some(rpath) = relative_path {
                // Check if the relative path is valid
                if rpath.len() < 2 {
                    tracing::warn!(
                        "Invalid checkpoint position: {:?}, ignore checkpoint",
                        &rpath
                    );
                    return None;
                }
                // Find the task in the task map
                for (name, (index, _task)) in task_map.iter() {
                    if rpath[0]
                        == serde_json::value::Value::Number(serde_json::Number::from(*index))
                        && rpath[1] == serde_json::value::Value::String(name.clone())
                    {
                        tracing::debug!(
                            "Found task '{}' at index {} for checkpoint position: {:?}",
                            name,
                            index,
                            &rpath
                        );
                        return Some(*index);
                    }
                }
                // If the task is not found, log a warning
                tracing::warn!(
                    "No task found for checkpoint position: {:?}, ignore checkpoint",
                    &rpath
                );
                None
            } else {
                tracing::warn!(
                    "No relative path '{:?}' from '{}' found for checkpoint, ignoring.",
                    &relative_path,
                    &pos.as_json_pointer()
                );
                None
            }
        } else {
            tracing::debug!("No checkpoint position found, ignoring.");
            None // no checkpoint position
        }
    }

    fn execute_task_stream(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_map: Arc<IndexMap<String, (u32, Arc<Task>)>>,
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
            prev.add_position_name(self.task.task_type().to_string()).await;
            let pos_opt = if self.checkpoint_repository.is_some() {
                // Find the checkpoint position
                self.find_checkpoint_task(&prev, task_map.clone()).await
            } else {
                None // no checkpoint repository
            };

            while let Some((name, (pos, task))) = current_pair {
                if let Some(p) = pos_opt {
                // skip for checkpoint
                    if pos < p {
                        tracing::warn!(
                            "Skipping task {} at position {}, expected position {}",
                            name,
                            pos,
                            pos_opt.unwrap_or_default()
                        );
                        current_pair = iter.next().map(|(k, v)| (k.clone(), v.clone()));
                        continue;
                    }
                }
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
                if self.workflow_context.read().await.status != WorkflowStatus::Running {
                    prev.remove_position().await;
                    break;
                }
                Self::record_task_input(&mut span, name.clone(), &prev,
                    prev.position.read().await.as_json_pointer().to_string());

                tracing::info!("Executing task: {}", &name);
                let mut stream = TaskExecutor::new(
                    self.workflow_context.clone(),
                    self.default_timeout,
                    job_exec.clone(),
                    http_client.clone(),
                    self.checkpoint_repository.clone(),
                    &name,
                    task.clone(),
                    req_meta.clone(),
                )
                .execute(
                    ccx.clone(),
                    Arc::new(prev.clone()),
                    self.execution_id.clone(),
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
                            tracing::debug!(
                                "Error executing task {}: {:?}",
                                name,
                                &e
                            );
                            Self::record_error(&span, &e.to_string());
                            yield Err(e);
                            // stop stream on error
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
                        self.workflow_context.write().await.status = WorkflowStatus::Completed;
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

        self.execute_task_stream(cx, task_map, task_context)
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
                    checkpoint: false,
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
                    checkpoint: false,
                }),
            );
            vec![map1, map2]
        };
        let task_list = TaskList(task_map_list);

        WorkflowSchema {
            checkpointing: None,
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
                None,
            )));

            let do_task = workflow.create_do_task(Arc::new(HashMap::new()));
            let executor = DoTaskStreamExecutor::new(
                workflow_context.clone(),
                Duration::from_secs(1200), // 20 minutes
                Arc::new(HashMap::new()),
                do_task,
                Arc::new(JobExecutorWrapper::new(app_module)),
                http_client,
                None,
                None,
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
                            checkpoint: false,
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
                            checkpoint: false,
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
                            checkpoint: false,
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
                            checkpoint: false,
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
                None,
            )));

            let do_task = workflow.create_do_task(Arc::new(HashMap::new()));
            let executor = DoTaskStreamExecutor::new(
                workflow_context.clone(),
                Duration::from_secs(1200), // 20 minutes
                Arc::new(HashMap::new()),
                do_task,
                Arc::new(JobExecutorWrapper::new(app_module)),
                http_client,
                None,
                None,
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
                                checkpoint: false,
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
                                checkpoint: false,
                            }),
                        );
                        map
                    },
                ]);

                let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                    &exit_workflow,
                    input.clone(),
                    context.clone(),
                    None,
                )));

                let do_task = exit_workflow.create_do_task(Arc::new(HashMap::new()));
                let executor = DoTaskStreamExecutor::new(
                    workflow_context.clone(),
                    Duration::from_secs(1200), // 20 minutes
                    Arc::new(HashMap::new()),
                    do_task,
                    Arc::new(JobExecutorWrapper::new(app_module.clone())),
                    http_client.clone(),
                    None,
                    None,
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
                            checkpoint: false,
                        }),
                    );
                    map
                }]);

                let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                    &end_workflow,
                    input.clone(),
                    context.clone(),
                    None,
                )));

                let do_task = end_workflow.create_do_task(Arc::new(HashMap::new()));
                let executor = DoTaskStreamExecutor::new(
                    workflow_context.clone(),
                    Duration::from_secs(1200), // 20 minutes
                    Arc::new(HashMap::new()),
                    do_task,
                    Arc::new(JobExecutorWrapper::new(app_module.clone())),
                    http_client.clone(),
                    None,
                    None,
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
                checkpointing: None,
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
                                checkpoint: false,
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
                                checkpoint: false,
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
                                checkpoint: false,
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
                None,
            )));

            let do_task = workflow.create_do_task(Arc::new(HashMap::new()));
            let executor = DoTaskStreamExecutor::new(
                workflow_context.clone(),
                Duration::from_secs(1200), // 20 minutes
                Arc::new(HashMap::new()),
                do_task,
                Arc::new(JobExecutorWrapper::new(app_module)),
                http_client,
                None,
                None,
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
