use super::super::StreamTaskExecutorTrait;
use crate::workflow::{
    definition::{
        transform::UseJqAndTemplateTransformer,
        workflow::{self, tasks::TaskTrait, Task},
    },
    execute::{
        context::{TaskContext, Then, WorkflowContext, WorkflowStatus, WorkflowStreamEvent},
        expression::UseExpression,
        task::{trace::TaskTracing, ExecutionId, TaskExecutor},
    },
};
use anyhow::Result;
use app::app::job::execute::JobExecutorWrapper;
use async_stream::stream;
use command_utils::trace::Tracing;
use debug_stub_derive::DebugStub;
use futures::StreamExt;
use indexmap::IndexMap;
use jobworkerp_base::APP_WORKER_NAME;
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
    ) -> Pin<
        Box<
            dyn futures::Stream<Item = Result<WorkflowStreamEvent, Box<workflow::Error>>>
                + Send
                + '_,
        >,
    > {
        let job_exec = self.job_executor_wrapper.clone();
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
                        Ok(event) => {
                            // Extract context from completed events
                            if let Some(ctx) = event.context() {
                                last_ctx = Some(ctx.clone());
                            }
                            yield Ok(event);
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
                // The task name added by TaskExecutor (task.rs:210) is already removed by task.rs:390.
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
                    Then::Wait => {
                        // HITL: Workflow suspension requested
                        tracing::info!("Wait Task: {}, suspending workflow for user input", name);

                        // Determine if this is a valid wait scenario
                        // NOTE: At this point, result.position is "/do" (both index and task_name removed)
                        let is_valid_wait = if next_pair.is_some() {
                            // Next task exists - valid wait
                            true
                        } else {
                            // No next task in this do-list
                            let current_pos = result.position.read().await;
                            let pos_len = current_pos.full().len();
                            drop(current_pos);

                            if pos_len <= 2 {
                                // Root-level do (e.g., /ROOT/do) - workflow is essentially complete
                                // Wait at the end of root do-list is meaningless
                                tracing::warn!(
                                    "Wait at the last task of root do-list is ineffective. \
                                     Workflow will complete instead of waiting. \
                                     Consider moving 'then: wait' to an earlier task or restructuring the workflow."
                                );
                                // Set status to Completed instead of Waiting
                                self.workflow_context.write().await.status = WorkflowStatus::Completed;
                                false
                            } else {
                                // Nested do - valid wait (will resume at parent task)
                                true
                            }
                        };

                        // Save checkpoint if repository is available and wait is valid
                        if is_valid_wait {
                            if let (Some(repo), Some(exec_id)) = (&self.checkpoint_repository, &self.execution_id) {
                                // Calculate checkpoint position for resume
                                let checkpoint_position = if let Some((next_name, (next_idx, _))) = next_pair.as_ref() {
                                    // Next task exists: build position for next task
                                    let mut pos = result.position.read().await.clone();
                                    pos.push_idx(*next_idx);       // Add next task's index
                                    pos.push(next_name.to_string());   // Add next task's name
                                    pos
                                } else {
                                    // Nested do: save parent task's position for resume
                                    let current_pos = result.position.read().await;
                                    let mut parent_pos = current_pos.clone();
                                    drop(current_pos);
                                    parent_pos.pop(); // Remove "do" to get parent task position
                                    parent_pos
                                };

                                let wf_ctx = self.workflow_context.read().await;
                                let checkpoint = crate::workflow::execute::checkpoint::CheckPointContext::new_with_position(
                                    &wf_ctx,
                                    &result,
                                    checkpoint_position.clone(),
                                ).await;

                                if let Err(e) = repo
                                    .save_checkpoint_with_id(exec_id, &wf_ctx.document.name, &checkpoint)
                                    .await
                                {
                                    tracing::error!("Failed to save checkpoint for wait: {:#?}", e);
                                } else {
                                    tracing::info!(
                                        "Checkpoint saved for wait, resume position: {}",
                                        checkpoint_position.as_json_pointer()
                                    );
                                }
                                drop(wf_ctx);
                            }

                            // Set status to Waiting
                            self.workflow_context.write().await.status = WorkflowStatus::Waiting;
                        }
                        // If not valid wait, status was already set to Completed above

                        None  // Exit loop
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
    ) -> impl futures::Stream<Item = Result<WorkflowStreamEvent, Box<workflow::Error>>> + Send {
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
            let ctx = final_tc
                .unwrap()
                .into_context()
                .expect("Expected TaskContext");
            let prev_pos = ctx.prev_position(3).await;
            assert_eq!(
                prev_pos.last().unwrap().as_str().unwrap(),
                "task2".to_string()
            );
        })
    }

    #[test]
    fn test_execute_stream_with_flow_directives() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
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
            let ctx = final_tc
                .unwrap()
                .into_context()
                .expect("Expected TaskContext");
            let prev_pos = ctx.prev_position(3).await;
            assert_eq!(
                prev_pos.last().unwrap().as_str().unwrap(),
                "task4".to_string()
            );
        })
    }

    #[test]
    fn test_execute_stream_flow_exit_and_end() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
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
                let ctx = final_tc
                    .unwrap()
                    .into_context()
                    .expect("Expected TaskContext");
                let prev_pos = ctx.prev_position(3).await;
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
                let ctx = final_tc
                    .unwrap()
                    .into_context()
                    .expect("Expected TaskContext");
                let prev_pos = ctx.prev_position(3).await;
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
            let ctx = final_tc
                .unwrap()
                .into_context()
                .expect("Expected TaskContext");
            let prev_pos = ctx.prev_position(3).await;
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

    /// Test wait directive in middle of task list - verifies next task position calculation
    #[test]
    fn test_wait_directive_middle_task_position() {
        use crate::workflow::execute::checkpoint::repository::{
            CheckPointRepositoryWithId, CheckPointRepositoryWithIdImpl,
        };
        use crate::workflow::execute::task::ExecutionId;
        use memory_utils::cache::moka::MokaCacheConfig;

        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

            // Create workflow: task1 -> task2(wait) -> task3
            let task_map_list = {
                let mut map1 = HashMap::new();
                map1.insert(
                    "task1".to_string(),
                    Task::SetTask(SetTask {
                        set: {
                            let mut m = serde_json::Map::new();
                            m.insert("step".to_string(), serde_json::json!("first"));
                            m
                        },
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
                    "task2_wait".to_string(),
                    Task::SetTask(SetTask {
                        set: {
                            let mut m = serde_json::Map::new();
                            m.insert("step".to_string(), serde_json::json!("waiting"));
                            m
                        },
                        export: None,
                        if_: None,
                        input: None,
                        metadata: serde_json::Map::new(),
                        output: None,
                        then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Wait)), // Wait here
                        timeout: None,
                        checkpoint: false,
                    }),
                );
                let mut map3 = HashMap::new();
                map3.insert(
                    "task3_after".to_string(),
                    Task::SetTask(SetTask {
                        set: {
                            let mut m = serde_json::Map::new();
                            m.insert("step".to_string(), serde_json::json!("after_wait"));
                            m
                        },
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
                vec![map1, map2, map3]
            };
            let task_list = TaskList(task_map_list);

            let workflow = WorkflowSchema {
                checkpointing: None,
                document: Document {
                    name: WorkflowName::from_str("wait-position-test").unwrap(),
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
            };

            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));
            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context,
                None,
            )));

            // Setup checkpoint repository
            let checkpoint_repo: Arc<dyn CheckPointRepositoryWithId> = Arc::new(
                CheckPointRepositoryWithIdImpl::new_memory(&MokaCacheConfig::default()),
            );
            let execution_id =
                Arc::new(ExecutionId::new("test-wait-position".to_string()).unwrap());

            let do_task = workflow.create_do_task(Arc::new(HashMap::new()));
            let executor = DoTaskStreamExecutor::new(
                workflow_context.clone(),
                Duration::from_secs(1200),
                Arc::new(HashMap::new()),
                do_task,
                Arc::new(JobExecutorWrapper::new(app_module)),
                Some(checkpoint_repo.clone()),
                Some(execution_id.clone()),
            );

            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );

            // Initialize position with ROOT (as WorkflowExecutor's TaskExecutor does)
            task_context.add_position_name("ROOT".to_string()).await;

            workflow_context.write().await.status = WorkflowStatus::Running;
            let mut task_stream = executor.execute_stream(
                Arc::new(opentelemetry::Context::current()),
                Arc::new("test".to_string()),
                task_context,
            );

            // Collect all results
            while let Some(tc) = task_stream.next().await {
                if tc.is_err() {
                    panic!("Unexpected error: {:?}", tc);
                }
            }

            // Verify workflow status is Waiting
            assert_eq!(
                workflow_context.read().await.status,
                WorkflowStatus::Waiting,
                "Workflow should be in Waiting status after wait directive"
            );

            // Verify checkpoint was saved with correct next task position
            // Key format: workflow_name:execution_id:position
            let checkpoint_key = format!(
                "{}:{}:{}",
                workflow.document.name.as_str(),
                execution_id.value,
                "/ROOT/do/2/task3_after" // Expected: next task position
            );
            let saved_checkpoint = checkpoint_repo
                .checkpoint_repository()
                .get_checkpoint(&checkpoint_key)
                .await
                .expect("Checkpoint query should succeed");

            assert!(
                saved_checkpoint.is_some(),
                "Checkpoint should be saved at next task position: {}",
                checkpoint_key
            );

            let checkpoint = saved_checkpoint.unwrap();
            // Verify the position points to task3_after (index 2)
            let pos_str = checkpoint.position.as_json_pointer();
            assert!(
                pos_str.contains("task3_after"),
                "Checkpoint position should point to next task 'task3_after', got: {}",
                pos_str
            );
            assert!(
                pos_str.contains("/2/"),
                "Checkpoint position should contain index 2 for task3_after, got: {}",
                pos_str
            );
        })
    }

    /// Test wait directive at last task - verifies position when no next task exists
    #[test]
    fn test_wait_directive_last_task_position() {
        use crate::workflow::execute::checkpoint::repository::{
            CheckPointRepositoryWithId, CheckPointRepositoryWithIdImpl,
        };
        use crate::workflow::execute::task::ExecutionId;
        use memory_utils::cache::moka::MokaCacheConfig;

        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

            // Create workflow: task1 -> task2(wait) - no task3
            let task_map_list = {
                let mut map1 = HashMap::new();
                map1.insert(
                    "task1".to_string(),
                    Task::SetTask(SetTask {
                        set: {
                            let mut m = serde_json::Map::new();
                            m.insert("step".to_string(), serde_json::json!("first"));
                            m
                        },
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
                    "task2_wait_last".to_string(),
                    Task::SetTask(SetTask {
                        set: {
                            let mut m = serde_json::Map::new();
                            m.insert("step".to_string(), serde_json::json!("waiting_last"));
                            m
                        },
                        export: None,
                        if_: None,
                        input: None,
                        metadata: serde_json::Map::new(),
                        output: None,
                        then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Wait)), // Wait at last task
                        timeout: None,
                        checkpoint: false,
                    }),
                );
                vec![map1, map2]
            };
            let task_list = TaskList(task_map_list);

            let workflow = WorkflowSchema {
                checkpointing: None,
                document: Document {
                    name: WorkflowName::from_str("wait-last-position-test").unwrap(),
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
            };

            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));
            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context,
                None,
            )));

            // Setup checkpoint repository
            let checkpoint_repo: Arc<dyn CheckPointRepositoryWithId> = Arc::new(
                CheckPointRepositoryWithIdImpl::new_memory(&MokaCacheConfig::default()),
            );
            let execution_id =
                Arc::new(ExecutionId::new("test-wait-last-position".to_string()).unwrap());

            let do_task = workflow.create_do_task(Arc::new(HashMap::new()));
            let executor = DoTaskStreamExecutor::new(
                workflow_context.clone(),
                Duration::from_secs(1200),
                Arc::new(HashMap::new()),
                do_task,
                Arc::new(JobExecutorWrapper::new(app_module)),
                Some(checkpoint_repo.clone()),
                Some(execution_id.clone()),
            );

            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );

            // Initialize position with ROOT (as WorkflowExecutor's TaskExecutor does)
            task_context.add_position_name("ROOT".to_string()).await;

            workflow_context.write().await.status = WorkflowStatus::Running;
            let mut task_stream = executor.execute_stream(
                Arc::new(opentelemetry::Context::current()),
                Arc::new("test".to_string()),
                task_context,
            );

            // Collect all results
            while let Some(tc) = task_stream.next().await {
                if tc.is_err() {
                    panic!("Unexpected error: {:?}", tc);
                }
            }

            // When wait is at last task of root-level do, workflow should be Completed (not Waiting)
            // because wait at the end of root do-list is ineffective
            assert_eq!(
                workflow_context.read().await.status,
                WorkflowStatus::Completed,
                "Workflow should be Completed when wait is at last task of root do-list"
            );

            // No checkpoint should be saved for root-level last task wait
            // Try a few possible key patterns to verify no checkpoint exists
            let checkpoint_key = format!(
                "{}:{}:{}",
                workflow.document.name.as_str(),
                execution_id.value,
                "/ROOT/do"
            );
            let saved_checkpoint = checkpoint_repo
                .checkpoint_repository()
                .get_checkpoint(&checkpoint_key)
                .await
                .expect("Checkpoint query should succeed");

            assert!(
                saved_checkpoint.is_none(),
                "No checkpoint should be saved when wait is at last task of root do-list"
            );
        })
    }
}
