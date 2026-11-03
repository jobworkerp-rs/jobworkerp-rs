use super::{NamedTimeouts, TaskExecutor};
use crate::workflow::{
    definition::workflow::{Task, tasks::TaskTrait},
    execute::{
        context::{TaskContext, WorkflowContext, WorkflowStreamEvent},
        task::{ExecutionId, Result, TaskExecutorTrait, cancel_child_jobs},
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
use futures::{Future, StreamExt, future, stream::FuturesUnordered};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::RwLock;

// for DebugStub
type CheckPointRepo =
    Arc<dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId>;

#[derive(DebugStub, Clone)]
pub struct ForkTaskExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    default_task_timeout: Option<Duration>,
    named_timeouts: Arc<NamedTimeouts>,
    #[debug_stub = "AppModule"]
    pub job_executor_wrapper: Arc<JobExecutorWrapper>,
    pub checkpoint_repository: Option<CheckPointRepo>,
    task: workflow::ForkTask,
    execution_id: Option<Arc<ExecutionId>>,
    metadata: Arc<HashMap<String, String>>,
    /// Whether to emit StreamingData events
    pub emit_streaming_data: bool,
}

impl Tracing for ForkTaskExecutor {}
impl ForkTaskExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workflow_context: Arc<RwLock<WorkflowContext>>,
        default_task_timeout: Option<Duration>,
        named_timeouts: Arc<NamedTimeouts>,
        task: workflow::ForkTask,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        checkpoint_repository: Option<
            Arc<dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId>,
        >,
        execution_id: Option<Arc<ExecutionId>>,
        metadata: Arc<HashMap<String, String>>,
        emit_streaming_data: bool,
    ) -> Self {
        Self {
            workflow_context,
            default_task_timeout,
            named_timeouts,
            job_executor_wrapper,
            checkpoint_repository,
            task,
            execution_id,
            metadata,
            emit_streaming_data,
        }
    }
    /// Runs one branch to the end and returns its last completed context.
    /// Container tasks such as `do` forward the completions of nested tasks
    /// and may not emit one of their own, so no single event marks the end of
    /// a branch.
    async fn run_branch(
        &self,
        name: &str,
        branch_context: Arc<TaskContext>,
        task: Arc<Task>,
        cx: Arc<opentelemetry::Context>,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        let stream = TaskExecutor::new(
            self.workflow_context.clone(),
            self.default_task_timeout,
            self.named_timeouts.clone(),
            self.job_executor_wrapper.clone(),
            self.checkpoint_repository.clone(),
            name,
            task,
            self.metadata.clone(),
            self.emit_streaming_data,
        )
        .execute(cx, branch_context, self.execution_id.clone())
        .await;
        WorkflowStreamEvent::collect_final_context(stream).await
    }

    /// Waits for all branches concurrently and sets their outputs as an array
    /// in declaration order. A failed branch yields `null`, so index `i`
    /// always holds the result of the `i`-th branch.
    async fn run_all_branches<F>(branches: Vec<F>, mut fork_context: TaskContext) -> TaskContext
    where
        F: Future<Output = Result<TaskContext, Box<workflow::Error>>>,
    {
        let outputs = future::join_all(branches)
            .await
            .into_iter()
            .map(|result| match result {
                Ok(context) => Arc::unwrap_or_clone(context.output),
                Err(e) => {
                    tracing::warn!(error = ?e, "Fork branch failed");
                    serde_json::Value::Null
                }
            })
            .collect();
        fork_context.raw_output = Arc::new(serde_json::Value::Array(outputs));
        fork_context
    }

    /// Returns the context of the first branch that completes successfully.
    /// The winner decides the output, so the losing branches are dropped and
    /// the child jobs they started are cancelled.
    async fn run_compete_branches<F>(
        &self,
        branches: Vec<F>,
        fork_context: &TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>>
    where
        F: Future<Output = Result<TaskContext, Box<workflow::Error>>>,
    {
        let mut pending: FuturesUnordered<F> = branches.into_iter().collect();
        let mut errors = Vec::new();
        while let Some(result) = pending.next().await {
            match result {
                Ok(mut winner) => {
                    drop(pending);
                    // Absent only outside a TaskExecutor (unit tests): no scope to cancel.
                    if let Some(scope) = &fork_context.child_job_scope {
                        cancel_child_jobs(&self.job_executor_wrapper, scope, "fork compete lost")
                            .await;
                    }
                    // The winner ran on an isolated position stack; hand it back
                    // on the fork's stack so the callers pop their own entries.
                    winner.position = fork_context.position.clone();
                    return Ok(winner);
                }
                Err(e) => {
                    tracing::warn!(error = ?e, "Task failed in compete mode");
                    errors.push(e.to_string());
                }
            }
        }

        let pos = fork_context.position.read().await.as_error_instance();
        tracing::error!(position = %pos, "All tasks failed in compete mode");
        Err(workflow::errors::ErrorFactory::new().runtime(
            "All tasks failed in compete mode".to_string(),
            Some(pos),
            Some(errors.join("; ")),
        ))
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

            let mut branches = Vec::new();
            for branch_item in self.task.fork.branches.0.iter() {
                for (branch_name, task) in branch_item.iter() {
                    // Concurrent branches must not share one position stack,
                    // or their pushes and pops interleave.
                    let branch_context =
                        Arc::new(task_context.clone_with_isolated_position().await);
                    let task = Arc::new(task.clone());
                    let cx = cx.clone();
                    branches.push(async move {
                        self.run_branch(branch_name, branch_context, task, cx).await
                    });
                }
            }

            let context = if self.task.fork.compete {
                self.run_compete_branches(branches, &task_context).await?
            } else {
                Self::run_all_branches(branches, task_context).await
            };
            context.remove_position().await;
            Ok(context)
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
    use tokio::sync::Mutex;

    fn task_from_json(value: serde_json::Value) -> WorkflowTask {
        serde_json::from_value(value).expect("valid task json")
    }

    fn fork_task_from_json(branches: serde_json::Value, compete: bool) -> workflow::ForkTask {
        match task_from_json(json!({ "fork": { "branches": branches, "compete": compete } })) {
            WorkflowTask::ForkTask(task) => task,
            other => panic!("not a fork task: {other:?}"),
        }
    }

    fn raise_task_json() -> serde_json::Value {
        json!({ "raise": { "error": {
            "type": "https://open-workflow-specification.org/spec/1.0.0/errors/runtime",
            "status": 500
        } } })
    }

    async fn run_fork(
        branches: serde_json::Value,
        compete: bool,
    ) -> (Result<TaskContext, Box<workflow::Error>>, TaskContext) {
        run_fork_with_inputs(branches, compete, json!({}), json!({})).await
    }

    /// Builds a fork executor with its test app. Setup is kept apart from
    /// execution because it is slow on MySQL and must not skew timings.
    async fn fork_executor(
        branches: serde_json::Value,
        compete: bool,
        workflow_input: serde_json::Value,
    ) -> ForkTaskExecutor {
        let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
        let mut workflow_context = WorkflowContext::new_empty();
        workflow_context.input = Arc::new(workflow_input);
        // Container executors stop early unless the workflow is running.
        workflow_context.status = crate::workflow::execute::context::WorkflowStatus::Running;
        ForkTaskExecutor::new(
            Arc::new(RwLock::new(workflow_context)),
            None,
            Arc::new(NamedTimeouts::new()),
            fork_task_from_json(branches, compete),
            Arc::new(JobExecutorWrapper::new(app_module)),
            None,
            None,
            Arc::new(HashMap::new()),
            false,
        )
    }

    async fn execute_fork(
        executor: &ForkTaskExecutor,
        task_input: serde_json::Value,
    ) -> (Result<TaskContext, Box<workflow::Error>>, TaskContext) {
        let task_context = TaskContext::new(
            None,
            Arc::new(task_input),
            Arc::new(Mutex::new(Default::default())),
        );
        let result = executor
            .execute(
                Arc::new(Context::current()),
                "fork_test",
                task_context.clone(),
            )
            .await;
        (result, task_context)
    }

    /// Runs a fork built from `branches` and returns the result together with
    /// the input context, whose position stack is shared with the fork.
    async fn run_fork_with_inputs(
        branches: serde_json::Value,
        compete: bool,
        workflow_input: serde_json::Value,
        task_input: serde_json::Value,
    ) -> (Result<TaskContext, Box<workflow::Error>>, TaskContext) {
        let executor = fork_executor(branches, compete, workflow_input).await;
        execute_fork(&executor, task_input).await
    }

    fn output_array(context: &TaskContext) -> &Vec<serde_json::Value> {
        context
            .raw_output
            .as_array()
            .expect("fork output must be an array")
    }

    fn wait_then_set_branch(wait_ms: u64, set: serde_json::Value) -> serde_json::Value {
        json!({ "do": [
            { "pause": { "wait": { "milliseconds": wait_ms } } },
            { "mark": { "set": set } }
        ] })
    }

    #[test]
    fn normal_mode_runs_branches_concurrently() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let branches = json!([
                { "b0": wait_then_set_branch(200, json!({ "b": 0 })) },
                { "b1": wait_then_set_branch(200, json!({ "b": 1 })) },
                { "b2": wait_then_set_branch(200, json!({ "b": 2 })) }
            ]);
            let executor = fork_executor(branches, false, json!({})).await;
            let started = std::time::Instant::now();
            let (result, _) = execute_fork(&executor, json!({})).await;
            let elapsed = started.elapsed();

            assert_eq!(output_array(&result.unwrap()).len(), 3);
            assert!(
                elapsed < Duration::from_millis(400),
                "branches must overlap, took {elapsed:?}"
            );
        })
    }

    #[test]
    fn normal_mode_gives_every_branch_its_full_timeout() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let branch = |b: i64| {
                let mut task = wait_then_set_branch(300, json!({ "b": b }));
                task["timeout"] = json!({ "after": { "milliseconds": 400 } });
                task
            };
            let branches = json!([{ "b0": branch(0) }, { "b1": branch(1) }, { "b2": branch(2) }]);
            let (result, _) = run_fork(branches, false).await;

            assert_eq!(
                output_array(&result.unwrap()),
                &vec![json!({ "b": 0 }), json!({ "b": 1 }), json!({ "b": 2 })]
            );
        })
    }

    #[test]
    fn normal_mode_keeps_declaration_order() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let branches = json!([
                { "slow": wait_then_set_branch(150, json!({ "name": "slow" })) },
                { "fast": wait_then_set_branch(10, json!({ "name": "fast" })) }
            ]);
            let (result, _) = run_fork(branches, false).await;

            assert_eq!(
                output_array(&result.unwrap()),
                &vec![json!({ "name": "slow" }), json!({ "name": "fast" })]
            );
        })
    }

    #[test]
    fn normal_mode_outputs_one_element_per_nested_branch() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let branches = json!([
                { "nested": { "do": [
                    { "x": { "set": { "step": "x" } } },
                    { "y": { "set": { "step": "y" } } },
                    { "z": { "set": { "step": "z" } } }
                ] } },
                { "single": { "set": { "step": "single" } } }
            ]);
            let (result, _) = run_fork(branches, false).await;

            assert_eq!(
                output_array(&result.unwrap()),
                &vec![json!({ "step": "z" }), json!({ "step": "single" })]
            );
        })
    }

    #[test]
    fn normal_mode_fills_failed_branch_with_null() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let branches = json!([
                { "fail": raise_task_json() },
                { "ok": { "set": { "ok": true } } }
            ]);
            let (result, input) = run_fork(branches, false).await;

            assert_eq!(
                output_array(&result.unwrap()),
                &vec![serde_json::Value::Null, json!({ "ok": true })]
            );
            assert_eq!(input.position.read().await.as_json_pointer(), "");
        })
    }

    #[test]
    fn compete_mode_waits_for_whole_do_branch() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let branches = json!([
                { "nested": { "do": [
                    { "a1": { "set": { "winner": "a1" } } },
                    { "pause": { "wait": { "milliseconds": 400 } } },
                    { "a2": { "set": { "winner": "a2" } } }
                ] } },
                { "single": wait_then_set_branch(150, json!({ "winner": "b" })) }
            ]);
            let (result, _) = run_fork(branches, true).await;

            assert_eq!(*result.unwrap().output, json!({ "winner": "b" }));
        })
    }

    #[test]
    fn compete_mode_waits_for_whole_try_branch() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let branches = json!([
                { "guarded": {
                    "try": [
                        { "a1": { "set": { "winner": "a1" } } },
                        { "pause": { "wait": { "milliseconds": 400 } } },
                        { "a2": { "set": { "winner": "a2" } } }
                    ],
                    "catch": { "do": [ { "recover": { "set": { "winner": "recovered" } } } ] }
                } },
                { "single": wait_then_set_branch(150, json!({ "winner": "b" })) }
            ]);
            let (result, _) = run_fork(branches, true).await;

            assert_eq!(*result.unwrap().output, json!({ "winner": "b" }));
        })
    }

    #[test]
    fn compete_mode_restores_position_after_failed_branch() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let branches = json!([
                { "fail": raise_task_json() },
                { "ok": wait_then_set_branch(100, json!({ "winner": "ok" })) }
            ]);
            let (result, input) = run_fork(branches, true).await;

            let winner = result.unwrap();
            assert_eq!(*winner.output, json!({ "winner": "ok" }));
            assert!(Arc::ptr_eq(&winner.position, &input.position));
            assert_eq!(input.position.read().await.as_json_pointer(), "");
        })
    }

    #[test]
    fn test_fork_task_executor_normal_mode() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let branches = json!([
                { "branch1": { "set": { "key1": "${$workflow.input.winput}" } } },
                { "branch2": { "set": { "key2": "${$input.initial}" } } }
            ]);
            let (result, _) = run_fork_with_inputs(
                branches,
                false,
                json!({ "winput": "test" }),
                json!({ "initial": "value" }),
            )
            .await;

            assert_eq!(
                output_array(&result.unwrap()),
                &vec![json!({ "key1": "test" }), json!({ "key2": "value" })]
            );
        })
    }

    #[test]
    fn test_fork_task_executor_compete_mode() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let branches = json!([
                { "branch1": { "set": {} } },
                { "branch2": { "set": {} } }
            ]);
            let (result, _) = run_fork(branches, true).await;

            assert!(result.is_ok());
        })
    }

    #[test]
    fn test_fork_task_executor_compete_mode_all_fail() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let branches = json!([
                { "fail_branch1": raise_task_json() },
                { "fail_branch2": raise_task_json() }
            ]);
            let (result, _) = run_fork(branches, true).await;

            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("All tasks failed in compete mode")
            );
        })
    }
}
