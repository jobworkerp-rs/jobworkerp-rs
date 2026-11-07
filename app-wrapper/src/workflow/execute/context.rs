use crate::workflow::execute::expression::ExpressionContext;
use crate::workflow::{
    definition::{
        transform::UseJqAndTemplateTransformer,
        workflow::{self, FlowDirective, Task, tasks::TaskTrait},
    },
    execute::checkpoint,
};
use chrono::{DateTime, FixedOffset};
pub use infra::workflow::position::WorkflowPosition;
pub use proto::jobworkerp::data::{JobId, JobResultId};

// Re-export protobuf types for workflow events
pub use proto::jobworkerp::data::{
    ForItemFailedEvent, JobCompletedEvent, JobStartedEvent, StreamingDataEvent, TaskCompletedEvent,
    TaskStartedEvent, WorkflowEvent, workflow_event,
};
use std::{collections::HashSet, fmt, ops::Deref, str::FromStr, sync::Arc};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

/// Workflow and task variables (`set`, `export`, `for` item/index).
///
/// Values are never mutated in place, only replaced, so they are shared via
/// `Arc`: snapshotting the variables for an expression or a parallel branch
/// clones pointers instead of whole JSON trees. `IndexMap` keeps the insertion
/// order that `serde_json::Map` (with `preserve_order`) had.
pub type ContextVariables = indexmap::IndexMap<String, Arc<serde_json::Value>>;

pub fn context_variables_from_json(
    map: &serde_json::Map<String, serde_json::Value>,
) -> ContextVariables {
    map.iter()
        .map(|(k, v)| (k.clone(), Arc::new(v.clone())))
        .collect()
}

pub fn context_variables_to_json(
    vars: &ContextVariables,
) -> serde_json::Map<String, serde_json::Value> {
    vars.iter()
        .map(|(k, v)| (k.clone(), (**v).clone()))
        .collect()
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct WorkflowContext {
    pub id: Uuid,
    pub name: String,
    #[serde(skip)]
    pub document: Arc<workflow::Document>,
    pub input: Arc<serde_json::Value>,
    pub status: WorkflowStatus,
    pub started_at: DateTime<FixedOffset>,
    pub output: Option<Arc<serde_json::Value>>,
    pub position: WorkflowPosition,
    /// Checkpoint to resume from: tasks before it are skipped during recovery.
    #[serde(skip)]
    pub checkpoint_position: Option<WorkflowPosition>,
    /// Where a waiting workflow resumes, i.e. the key its checkpoint is saved
    /// under. Kept apart from `checkpoint_position` so recovery never mistakes
    /// this run's own pause point for a position to skip to.
    #[serde(skip)]
    pub resume_position: Option<WorkflowPosition>,
    #[serde(skip)]
    pub context_variables: Arc<Mutex<ContextVariables>>,
    /// Root of the child job scope tree. Every in-flight child job enqueued by
    /// this workflow's tasks is registered here (and in the scopes of the tasks
    /// that enclose it), so a cancellation can reach it even after the future
    /// awaiting it was dropped. Transient runtime state, never persisted.
    #[serde(skip)]
    pub child_job_scope: Arc<ChildJobScope>,
    /// Named authentication policies from `use.authentications`, for resolving
    /// `authentication: { use: <name> }` references. `serde(skip)` so they never
    /// leak into the serialized expression context or persisted checkpoints.
    #[serde(skip)]
    pub authentications: Arc<std::collections::HashMap<String, workflow::AuthenticationPolicy>>,
    /// Secret names declared in `use.secrets`. Only these may be resolved from
    /// the environment. Values are never stored here — only the declared names.
    #[serde(skip)]
    pub declared_secrets: Arc<HashSet<String>>,
}

/// Registry of in-flight child jobs for one task, linked to the scopes of the
/// enclosing tasks. A job registered in a scope is also registered in all of
/// its ancestors, so cancelling a scope reaches every child job started below
/// it while leaving sibling scopes (fork branches, parallel for items) alone.
///
/// A `std::sync::Mutex` (not tokio's) because the critical sections are tiny
/// set operations with no await inside.
#[derive(Debug, Default)]
pub struct ChildJobScope {
    parent: Option<Arc<ChildJobScope>>,
    state: std::sync::Mutex<ChildJobScopeState>,
}

#[derive(Debug, Default)]
struct ChildJobScopeState {
    job_ids: HashSet<i64>,
    closed: bool,
}

impl ChildJobScope {
    pub fn new_root() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn child(self: &Arc<Self>) -> Arc<Self> {
        Arc::new(Self {
            parent: Some(self.clone()),
            state: Default::default(),
        })
    }

    fn lineage(&self) -> impl Iterator<Item = &ChildJobScope> {
        std::iter::successors(Some(self), |scope| scope.parent.as_deref())
    }

    fn state(&self) -> std::sync::MutexGuard<'_, ChildJobScopeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// True once this scope or any enclosing scope has been cancelled; new
    /// child jobs must not be started below it.
    pub fn is_closed(&self) -> bool {
        self.lineage().any(|scope| scope.state().closed)
    }

    pub fn snapshot(&self) -> Vec<JobId> {
        self.state().job_ids()
    }

    pub fn guard(self: &Arc<Self>, job_id: JobId) -> RunningJobGuard {
        for scope in self.lineage() {
            scope.state().job_ids.insert(job_id.value);
        }
        RunningJobGuard {
            scope: self.clone(),
            job_id,
        }
    }

    pub(crate) fn unregister(&self, job_id: JobId) {
        for scope in self.lineage() {
            scope.state().job_ids.remove(&job_id.value);
        }
    }

    /// Mark the scope cancelled and return the jobs to cancel. Jobs stay
    /// registered until their cancellation succeeds so that a failed attempt
    /// can be retried by an enclosing scope.
    pub(crate) fn close(&self) -> Vec<JobId> {
        let mut state = self.state();
        state.closed = true;
        state.job_ids()
    }
}

impl ChildJobScopeState {
    fn job_ids(&self) -> Vec<JobId> {
        self.job_ids.iter().map(|&value| JobId { value }).collect()
    }
}

/// Registration of one awaited child job in a [`ChildJobScope`].
///
/// Dropping the guard without [`RunningJobGuard::mark_completed`] keeps the job
/// registered: a dropped future or stream (e.g. a disconnected client) is not a
/// decision to cancel, and the job may still be running, so it must stay
/// reachable by a later explicit cancellation.
#[derive(Debug)]
#[must_use = "call mark_completed() once the child job has finished"]
pub struct RunningJobGuard {
    scope: Arc<ChildJobScope>,
    job_id: JobId,
}

impl RunningJobGuard {
    pub fn job_id(&self) -> JobId {
        self.job_id
    }

    /// Mark the job as finished, unregistering it so a later cancellation
    /// does not target it.
    pub fn mark_completed(self) {
        self.scope.unregister(self.job_id);
    }
}

impl WorkflowContext {
    pub fn new(
        workflow: &workflow::WorkflowSchema,
        input: Arc<serde_json::Value>,
        context: Arc<serde_json::Value>,
        checkpoint_position: Option<WorkflowPosition>,
    ) -> Self {
        let uuid = Uuid::now_v7();
        let started_at = uuid
            .get_timestamp()
            .map(|t| command_utils::util::datetime::from_epoch_sec(t.to_unix().0 as i64))
            .unwrap_or_else(command_utils::util::datetime::now);
        Self {
            id: uuid,
            name: workflow.document.name.deref().to_string(),
            document: Arc::new(workflow.document.clone()),
            input,
            status: WorkflowStatus::Pending,
            started_at,
            output: None,
            position: WorkflowPosition::new(vec![]),
            checkpoint_position,
            resume_position: None,
            context_variables: Arc::new(Mutex::new(
                context
                    .as_object()
                    .map(context_variables_from_json)
                    .unwrap_or_default(),
            )),
            child_job_scope: ChildJobScope::new_root(),
            authentications: Arc::new(
                workflow
                    .use_
                    .as_ref()
                    .map(|c| c.authentications.clone())
                    .unwrap_or_default(),
            ),
            declared_secrets: Arc::new(
                workflow
                    .use_
                    .as_ref()
                    .map(|c| c.secrets.iter().cloned().collect())
                    .unwrap_or_default(),
            ),
        }
    }
    /// Position to report to callers: a waiting workflow reports where it will resume,
    /// which is also the key its checkpoint is saved under.
    pub fn reported_position(&self) -> &WorkflowPosition {
        match (&self.status, &self.resume_position) {
            (WorkflowStatus::Waiting, Some(resume)) => resume,
            _ => &self.position,
        }
    }

    // for test
    pub fn new_empty() -> Self {
        let uuid = Uuid::now_v7();
        let started_at = uuid
            .get_timestamp()
            .map(|t| command_utils::util::datetime::from_epoch_sec(t.to_unix().0 as i64))
            .unwrap_or_else(command_utils::util::datetime::now);
        Self {
            id: uuid,
            name: "default-workflow".to_string(), // TODO
            document: Arc::new(workflow::Document::default()),
            input: Arc::new(serde_json::Value::Null),
            status: WorkflowStatus::Pending,
            started_at,
            output: None,
            position: WorkflowPosition::new(vec![]),
            checkpoint_position: None,
            resume_position: None,
            context_variables: Arc::new(Mutex::new(ContextVariables::new())),
            child_job_scope: ChildJobScope::new_root(),
            authentications: Arc::new(std::collections::HashMap::new()),
            declared_secrets: Arc::new(HashSet::new()),
        }
    }

    /// Resolve a task's scope; `None` stands for the workflow root.
    pub fn scope_or_root(&self, scope: Option<&Arc<ChildJobScope>>) -> Arc<ChildJobScope> {
        scope.unwrap_or(&self.child_job_scope).clone()
    }

    pub fn is_cancelled(&self) -> bool {
        self.status == WorkflowStatus::Cancelled
    }

    pub fn to_descriptor(&self) -> WorkflowDescriptor {
        WorkflowDescriptor {
            id: serde_json::Value::String(self.id.to_string()),
            input: self.input.clone(),
            started_at: serde_json::Value::String(self.started_at.to_rfc3339()),
        }
    }
    pub fn to_runtime_descriptor(&self) -> RuntimeDescriptor {
        RuntimeDescriptor {
            name: self.document.name.deref().to_string(),
            // version: self.definition.document.version.deref().to_string(),
            metadata: self.document.metadata.clone(),
        }
    }
    pub fn output_string(&self) -> String {
        self.output
            .clone()
            .map(Self::output_string_inner)
            .unwrap_or_default()
    }
    fn output_string_inner(output: Arc<serde_json::Value>) -> String {
        match output.deref() {
            serde_json::Value::String(s) => s.clone(),
            // recursive
            serde_json::Value::Array(a) => {
                format!(
                    "[{}]",
                    a.iter()
                        .map(|v| Self::output_string_inner(Arc::new(v.clone())))
                        .collect::<Vec<String>>()
                        .join(",")
                )
            }
            serde_json::Value::Object(o) => {
                format!(
                    "{{{}}}",
                    o.iter()
                        .map(|(k, v)| {
                            format!(
                                "\"{}\":{}",
                                k,
                                Self::output_string_inner(Arc::new(v.clone()))
                            )
                        })
                        .collect::<Vec<String>>()
                        .join(",")
                )
            }
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => "null".to_string(),
        }
    }
    // add context variable
    pub async fn add_context_value(&mut self, key: String, value: serde_json::Value) {
        // overwrite
        self.context_variables
            .lock()
            .await
            .insert(key, Arc::new(value));
    }
    pub async fn remove_context_value(&mut self, key: &str) {
        self.context_variables.lock().await.shift_remove(key);
    }
    // return None if not necessary to checkpoint
    pub async fn match_checkpoint(&self, task: &workflow::Task) -> Option<bool> {
        if let Some(ref pos) = self.checkpoint_position {
            if let Some(last) = pos.last_name() {
                Some(task.task_type() == last.as_str())
            } else {
                // if last is not a string, then it is not a task name
                Some(false)
            }
        } else {
            None
        }
    }
    pub async fn match_checkpoint_by_relative_path(
        &self,
        sub_path: &[serde_json::Value],
    ) -> Option<bool> {
        if let Some(ref pos) = self.checkpoint_position {
            let current = self.position.full();
            let target = pos.full();
            // current + sub_path should match begining of target
            if current.len() + sub_path.len() <= target.len() {
                let mut match_found = true;
                for (i, v) in current.iter().enumerate() {
                    if target.get(i).is_none() || target[i] != *v {
                        match_found = false;
                        break;
                    }
                }
                if match_found {
                    // check if the rest of the target matches
                    for (i, v) in target[current.len()..].iter().enumerate() {
                        if i < sub_path.len() && v != &sub_path[i] {
                            return Some(false);
                        }
                    }
                    Some(true)
                } else {
                    Some(false)
                }
            } else {
                // current + sub_path is longer than target, so no match
                Some(false)
            }
        } else {
            // no checkpoint position, so no match
            None
        }
    }
}
// not implement: validation, secret, auth, event
#[derive(Debug, Clone)]
// #[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct TaskContext {
    pub definition: Option<Arc<workflow::Task>>,
    pub raw_input: Arc<serde_json::Value>,
    pub input: Arc<serde_json::Value>,
    pub raw_output: Arc<serde_json::Value>,
    pub output: Arc<serde_json::Value>,
    // #[serde(skip)]
    pub context_variables: Arc<Mutex<ContextVariables>>,

    pub started_at: DateTime<FixedOffset>,
    pub completed_at: Option<DateTime<FixedOffset>>,
    pub flow_directive: Then,
    pub position: Arc<RwLock<WorkflowPosition>>,
    /// Scope that child jobs started by this task register into. `None` means
    /// the workflow root, so a context built without one still lets an
    /// explicit workflow cancellation reach its child jobs.
    pub child_job_scope: Option<Arc<ChildJobScope>>,
}
impl TaskContext {
    pub fn new(
        task: Option<Arc<workflow::Task>>,
        // input: input data. if not set explicitly, use empty key, previous output
        input: Arc<serde_json::Value>,
        // context_variables: workflow context variables.
        context_variables: Arc<Mutex<ContextVariables>>,
    ) -> Self {
        Self {
            definition: task.clone(),
            raw_input: input.clone(),
            raw_output: input.clone(),
            output: input.clone(),
            input,
            context_variables,
            started_at: command_utils::util::datetime::now(),
            completed_at: None,
            flow_directive: Then::Continue,
            position: Arc::new(RwLock::new(WorkflowPosition::new(vec![]))),
            child_job_scope: None,
        }
    }
    pub fn new_from_cp(
        task: Option<Arc<workflow::Task>>,
        checkpoint: &checkpoint::TaskCheckPointContext,
    ) -> Self {
        Self {
            definition: task.clone(),
            raw_input: checkpoint.input.clone(),
            input: checkpoint.input.clone(),
            raw_output: checkpoint.output.clone(),
            output: checkpoint.output.clone(),
            context_variables: Arc::new(Mutex::new(context_variables_from_json(
                &checkpoint.context_variables,
            ))),
            started_at: command_utils::util::datetime::now(),
            completed_at: None,
            flow_directive: Then::from_str(checkpoint.flow_directive.as_str())
                .unwrap_or(Then::Continue),
            position: Arc::new(RwLock::new(WorkflowPosition::new(vec![]))),
            child_job_scope: None,
        }
    }
    pub fn new_empty() -> Self {
        Self {
            definition: None,
            raw_input: Arc::new(serde_json::Value::Null),
            input: Arc::new(serde_json::Value::Null),
            raw_output: Arc::new(serde_json::Value::Null),
            output: Arc::new(serde_json::Value::Null),
            context_variables: Arc::new(Mutex::new(ContextVariables::new())),
            started_at: command_utils::util::datetime::now(),
            completed_at: None,
            flow_directive: Then::Continue,
            position: Arc::new(RwLock::new(WorkflowPosition::new(vec![]))),
            child_job_scope: None,
        }
    }
    pub async fn add_position_name(&self, name: String) {
        self.position.write().await.push(name);
    }
    pub async fn add_position_index(&self, idx: u32) {
        self.position.write().await.push_idx(idx);
    }
    pub async fn remove_position(&self) -> Option<serde_json::Value> {
        self.position.write().await.pop()
    }
    pub async fn current_position(&self) -> Option<serde_json::Value> {
        self.position.read().await.current().cloned()
    }
    pub async fn prev_position(&self, n: usize) -> Vec<serde_json::Value> {
        self.position.read().await.n_prev(n)
    }
    // add context variable
    pub async fn add_context_value(&self, key: String, value: serde_json::Value) {
        // overwrite
        self.context_variables
            .lock()
            .await
            .insert(key, Arc::new(value));
    }
    pub async fn remove_context_value(&self, key: &str) -> Option<serde_json::Value> {
        self.context_variables
            .lock()
            .await
            .shift_remove(key)
            .map(Arc::unwrap_or_clone)
    }
    //cloned
    pub async fn get_context_value(&self, key: &str) -> Option<serde_json::Value> {
        self.context_variables
            .lock()
            .await
            .get(key)
            .map(|v| (**v).clone())
    }
    pub fn set_completed_at(&mut self) {
        self.completed_at = Some(command_utils::util::datetime::now());
    }
    // XXX clone
    pub fn to_descriptor(&self) -> TaskDescriptor {
        if let Some(def) = self.definition.as_ref() {
            TaskDescriptor {
                definition: Some(def.clone()),
                raw_input: self.raw_input.clone(),
                raw_output: self.raw_output.clone(),
                started_at: self.started_at,
            }
        } else {
            TaskDescriptor {
                definition: None,
                raw_input: self.raw_input.clone(),
                raw_output: self.raw_output.clone(),
                started_at: self.started_at,
            }
        }
    }
    pub fn set_input(&mut self, input: Arc<serde_json::Value>) {
        self.input = input.clone();
        self.raw_output = input.clone();
        self.output = input;
    }
    pub fn set_raw_output(&mut self, raw_output: serde_json::Value) {
        self.raw_output = Arc::new(raw_output).clone();
        self.output = self.raw_output.clone();
    }
    pub fn set_output(&mut self, output: Arc<serde_json::Value>) {
        self.output = output.clone();
    }

    pub async fn deep_copy(&self) -> Self {
        Self {
            definition: self.definition.clone(),
            raw_input: Arc::new(self.raw_input.as_ref().clone()),
            input: Arc::new(self.input.as_ref().clone()),
            raw_output: Arc::new(self.raw_output.as_ref().clone()),
            output: Arc::new(self.output.as_ref().clone()),
            context_variables: Arc::new(Mutex::new(self.context_variables.lock().await.clone())),
            started_at: self.started_at,
            completed_at: self.completed_at,
            flow_directive: self.flow_directive.clone(),
            position: Arc::new(RwLock::new(self.position.read().await.clone())),
            child_job_scope: self.child_job_scope.clone(),
        }
    }

    // Shallow clone that isolates only `position`. Used by sequential for-loop
    // iterations: context_variables must be shared so each iteration sees the
    // accumulated state, but position must be per-iteration to prevent the
    // inner task's pushes from piling up across iterations (e.g. when a try
    // task swallows an error via onError=continue and never pops back).
    pub async fn clone_with_isolated_position(&self) -> Self {
        Self {
            definition: self.definition.clone(),
            raw_input: self.raw_input.clone(),
            input: self.input.clone(),
            raw_output: self.raw_output.clone(),
            output: self.output.clone(),
            context_variables: self.context_variables.clone(),
            started_at: self.started_at,
            completed_at: self.completed_at,
            flow_directive: self.flow_directive.clone(),
            position: Arc::new(RwLock::new(self.position.read().await.clone())),
            child_job_scope: self.child_job_scope.clone(),
        }
    }

    pub fn from_flow_directive(&self, flow_directive: Option<String>) -> Self {
        let mut s = self.clone();
        if let Some(fd) = flow_directive {
            s.flow_directive = match fd.as_str() {
                "exit" => Then::Exit,
                "end" => Then::End,
                "continue" => Then::Continue,
                _ => Then::TaskName(fd), // その他の文字列はタスク名として扱う
            };
        }
        s
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct WorkflowDescriptor {
    id: serde_json::Value,
    input: Arc<serde_json::Value>,
    started_at: serde_json::Value,
}
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct TaskDescriptor {
    // #[serde(skip_serializing, skip_deserializing)]
    definition: Option<Arc<Task>>,
    raw_input: Arc<serde_json::Value>,
    raw_output: Arc<serde_json::Value>,
    started_at: DateTime<chrono::FixedOffset>,
}
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct RuntimeDescriptor {
    name: String,
    // version: String,
    metadata: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub enum Then {
    Continue,
    Exit,
    End,
    Wait,
    TaskName(String),
}

impl UseJqAndTemplateTransformer for Then {}
impl Then {
    pub fn create(
        output: Arc<serde_json::Value>,
        directive: &FlowDirective,
        expression: &ExpressionContext,
    ) -> Result<Self, Box<workflow::Error>> {
        match directive {
            FlowDirective::Variant0(subtype_0) => match subtype_0 {
                workflow::FlowDirectiveEnum::Continue => Ok(Then::Continue),
                workflow::FlowDirectiveEnum::Exit => Ok(Then::Exit),
                workflow::FlowDirectiveEnum::End => Ok(Then::End),
                workflow::FlowDirectiveEnum::Wait => Ok(Then::Wait),
            },
            FlowDirective::Variant1(subtype_1) => {
                match Self::execute_transform(output, subtype_1, expression)? {
                    serde_json::Value::String(s) => Ok(Then::TaskName(s)),
                    r => {
                        tracing::warn!(
                            "Transformed Flow directive is not a string: {:#?}, no translation",
                            r
                        );
                        Ok(Then::TaskName(subtype_1.clone()))
                    }
                }
            }
        }
    }
}
impl FromStr for Then {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "continue" => Ok(Then::Continue),
            "exit" => Ok(Then::Exit),
            "end" => Ok(Then::End),
            "wait" => Ok(Then::Wait),
            _ => Ok(Then::TaskName(s.to_string())),
        }
    }
}
impl fmt::Display for Then {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Then::Continue => write!(f, "continue"),
            Then::Exit => write!(f, "exit"),
            Then::End => write!(f, "end"),
            Then::Wait => write!(f, "wait"),
            Then::TaskName(name) => write!(f, "{name}"),
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub enum WorkflowStatus {
    Pending,
    Running,
    Waiting, // HITL: waiting for user input (then: wait)
    Completed,
    Faulted,
    Cancelled,
}

impl fmt::Display for WorkflowStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkflowStatus::Pending => write!(f, "Pending"),
            WorkflowStatus::Running => write!(f, "Running"),
            WorkflowStatus::Waiting => write!(f, "Waiting"),
            WorkflowStatus::Completed => write!(f, "Completed"),
            WorkflowStatus::Faulted => write!(f, "Faulted"),
            WorkflowStatus::Cancelled => write!(f, "Cancelled"),
        }
    }
}

impl From<jobworkerp_runner::jobworkerp::runner::workflow_result::WorkflowStatus>
    for WorkflowStatus
{
    fn from(
        status: jobworkerp_runner::jobworkerp::runner::workflow_result::WorkflowStatus,
    ) -> Self {
        match status {
            jobworkerp_runner::jobworkerp::runner::workflow_result::WorkflowStatus::Pending => {
                WorkflowStatus::Pending
            }
            jobworkerp_runner::jobworkerp::runner::workflow_result::WorkflowStatus::Running => {
                WorkflowStatus::Running
            }
            jobworkerp_runner::jobworkerp::runner::workflow_result::WorkflowStatus::Waiting => {
                WorkflowStatus::Waiting
            }
            jobworkerp_runner::jobworkerp::runner::workflow_result::WorkflowStatus::Completed => {
                WorkflowStatus::Completed
            }
            jobworkerp_runner::jobworkerp::runner::workflow_result::WorkflowStatus::Faulted => {
                WorkflowStatus::Faulted
            }
            jobworkerp_runner::jobworkerp::runner::workflow_result::WorkflowStatus::Cancelled => {
                WorkflowStatus::Cancelled
            }
        }
    }
}

/// Internal representation combining WorkflowEvent with TaskContext
///
/// protobuf's WorkflowEvent is serializable but doesn't contain TaskContext.
/// This enum is used for internal processing, and can be converted to
/// WorkflowEvent for external output (gRPC/serialization).
///
/// Type conventions:
/// - `event`: Protobuf event type (serializable)
/// - `context`: TaskContext for internal state access (not serialized)
/// - `worker_name` in events: None (empty) for Runner-based jobs, Some for Worker-based jobs
#[derive(Debug, Clone)]
pub enum WorkflowStreamEvent {
    // Job execution tasks (RunTask)
    StreamingJobStarted {
        event: JobStartedEvent,
    },
    /// Streaming data chunk for real-time LLM output
    /// Each chunk is yielded as it arrives from Redis Pub/Sub
    /// Only emitted when emit_streaming_data flag is true (ag-ui-front)
    StreamingData {
        event: StreamingDataEvent,
    },
    StreamingJobCompleted {
        event: JobCompletedEvent,
        context: TaskContext,
    },
    JobStarted {
        event: JobStartedEvent,
    },
    JobCompleted {
        event: JobCompletedEvent,
        context: TaskContext,
    },
    // Generic tasks (ForTask, SwitchTask, DoTask, etc.)
    TaskStarted {
        event: TaskStartedEvent,
    },
    TaskCompleted {
        event: TaskCompletedEvent,
        context: TaskContext,
    },
    /// Per-iteration failure inside a parallel/sequential ForTask running with
    /// `onError: continue`. Carries the captured error payload and a position
    /// for observability, but intentionally does NOT expose a TaskContext: the
    /// for-task itself still emits exactly one final TaskCompleted, and we do
    /// not want this event to overwrite WorkflowContext.output along the way.
    ForItemFailed {
        task_name: String,
        position: String,
        index: u32,
        error_payload: serde_json::Value,
    },
}

impl WorkflowStreamEvent {
    /// Convert to protobuf WorkflowEvent (for gRPC/serialization)
    pub fn to_proto(&self) -> WorkflowEvent {
        match self {
            Self::StreamingJobStarted { event } => WorkflowEvent {
                event: Some(workflow_event::Event::StreamingJobStarted(event.clone())),
            },
            Self::StreamingData { event } => WorkflowEvent {
                event: Some(workflow_event::Event::StreamingData(event.clone())),
            },
            Self::StreamingJobCompleted { event, .. } => WorkflowEvent {
                event: Some(workflow_event::Event::StreamingJobCompleted(event.clone())),
            },
            Self::JobStarted { event } => WorkflowEvent {
                event: Some(workflow_event::Event::JobStarted(event.clone())),
            },
            Self::JobCompleted { event, .. } => WorkflowEvent {
                event: Some(workflow_event::Event::JobCompleted(event.clone())),
            },
            Self::TaskStarted { event } => WorkflowEvent {
                event: Some(workflow_event::Event::TaskStarted(event.clone())),
            },
            Self::TaskCompleted { event, .. } => WorkflowEvent {
                event: Some(workflow_event::Event::TaskCompleted(event.clone())),
            },
            Self::ForItemFailed {
                task_name,
                position,
                index,
                error_payload,
            } => WorkflowEvent {
                event: Some(workflow_event::Event::ForItemFailed(ForItemFailedEvent {
                    task_name: task_name.clone(),
                    position: position.clone(),
                    index: *index,
                    error_payload_json: serde_json::to_string(error_payload).unwrap_or_default(),
                })),
            },
        }
    }

    /// Get TaskContext reference (only for completed events)
    pub fn context(&self) -> Option<&TaskContext> {
        match self {
            Self::StreamingJobCompleted { context, .. } => Some(context),
            Self::JobCompleted { context, .. } => Some(context),
            Self::TaskCompleted { context, .. } => Some(context),
            _ => None,
        }
    }

    /// Mutable TaskContext reference (only for completed events)
    pub fn context_mut(&mut self) -> Option<&mut TaskContext> {
        match self {
            Self::StreamingJobCompleted { context, .. } => Some(context),
            Self::JobCompleted { context, .. } => Some(context),
            Self::TaskCompleted { context, .. } => Some(context),
            _ => None,
        }
    }

    /// Consume and get TaskContext (only for completed events)
    pub fn into_context(self) -> Option<TaskContext> {
        match self {
            Self::StreamingJobCompleted { context, .. } => Some(context),
            Self::JobCompleted { context, .. } => Some(context),
            Self::TaskCompleted { context, .. } => Some(context),
            _ => None,
        }
    }

    /// Helper to collect final TaskContext from a stream
    pub async fn collect_final_context<S>(
        mut stream: S,
    ) -> Result<TaskContext, Box<workflow::Error>>
    where
        S: futures::Stream<Item = Result<WorkflowStreamEvent, Box<workflow::Error>>> + Unpin,
    {
        use futures::StreamExt;
        let mut last_context = None;
        while let Some(result) = stream.next().await {
            if let Some(ctx) = result?.into_context() {
                last_context = Some(ctx);
            }
        }
        last_context.ok_or_else(|| {
            workflow::errors::ErrorFactory::new().runtime(
                "No completed event found in stream".to_string(),
                None,
                None,
            )
        })
    }

    /// Check if this is a start event
    pub fn is_start_event(&self) -> bool {
        matches!(
            self,
            Self::StreamingJobStarted { .. } | Self::JobStarted { .. } | Self::TaskStarted { .. }
        )
    }

    /// Check if this is a completed event
    pub fn is_completed_event(&self) -> bool {
        matches!(
            self,
            Self::StreamingJobCompleted { .. }
                | Self::JobCompleted { .. }
                | Self::TaskCompleted { .. }
        )
    }

    /// Get position from the event
    /// Returns empty string for StreamingData which has no position
    pub fn position(&self) -> &str {
        match self {
            Self::StreamingJobStarted { event } => &event.position,
            Self::StreamingData { .. } => "",
            Self::StreamingJobCompleted { event, .. } => &event.position,
            Self::JobStarted { event } => &event.position,
            Self::JobCompleted { event, .. } => &event.position,
            Self::TaskStarted { event } => &event.position,
            Self::TaskCompleted { event, .. } => &event.position,
            Self::ForItemFailed { position, .. } => position,
        }
    }

    /// Get job_id from the event (only for job events)
    pub fn job_id(&self) -> Option<JobId> {
        match self {
            Self::StreamingJobStarted { event } => event.job_id,
            Self::StreamingData { event } => event.job_id,
            Self::StreamingJobCompleted { event, .. } => event.job_id,
            Self::JobStarted { event } => event.job_id,
            Self::JobCompleted { event, .. } => event.job_id,
            _ => None,
        }
    }

    /// Create TaskStarted event from task information
    pub fn task_started(task_type: &str, task_name: &str, position: &str) -> Self {
        Self::TaskStarted {
            event: TaskStartedEvent {
                task_type: task_type.to_string(),
                task_name: task_name.to_string(),
                position: position.to_string(),
            },
        }
    }

    /// Create TaskCompleted event from TaskContext
    ///
    /// Note: This function uses try_read() which may fail under contention.
    /// For critical position tracking, prefer task_completed_with_position() with pre-acquired position.
    pub fn task_completed(task_type: &str, task_name: &str, context: TaskContext) -> Self {
        let position = match context.position.try_read() {
            Ok(guard) => guard.as_json_pointer(),
            Err(_) => {
                tracing::warn!(
                    "Failed to acquire position lock for task '{}' (type: {}), position info may be incomplete",
                    task_name,
                    task_type
                );
                String::new()
            }
        };
        Self::TaskCompleted {
            event: TaskCompletedEvent {
                task_type: task_type.to_string(),
                task_name: task_name.to_string(),
                position,
            },
            context,
        }
    }

    /// Create TaskCompleted event with explicit position
    pub fn task_completed_with_position(
        task_type: &str,
        task_name: &str,
        position: &str,
        context: TaskContext,
    ) -> Self {
        Self::TaskCompleted {
            event: TaskCompletedEvent {
                task_type: task_type.to_string(),
                task_name: task_name.to_string(),
                position: position.to_string(),
            },
            context,
        }
    }

    /// Build a per-iteration failure event for a ForTask running with
    /// `onError: continue`. The carried `error_payload` contains the error
    /// detail, the iteration index, and the offending item value, so stream
    /// consumers can render or count the failure without needing a
    /// TaskContext (which would otherwise overwrite WorkflowContext.output).
    pub fn for_item_failed(
        task_name: &str,
        position: &str,
        index: u32,
        error_payload: serde_json::Value,
    ) -> Self {
        Self::ForItemFailed {
            task_name: task_name.to_string(),
            position: position.to_string(),
            index,
            error_payload,
        }
    }

    /// Create StreamingJobStarted event
    ///
    /// # Arguments
    /// * `job_id` - Job ID (proto type)
    /// * `runner_name` - Runner name
    /// * `worker_name` - Worker name (None for Runner-based jobs with temporary workers)
    /// * `position` - Position in workflow
    pub fn streaming_job_started(
        job_id: JobId,
        runner_name: &str,
        worker_name: Option<String>,
        position: &str,
    ) -> Self {
        Self::StreamingJobStarted {
            event: JobStartedEvent {
                job_id: Some(job_id),
                runner_name: runner_name.to_string(),
                worker_name,
                position: position.to_string(),
            },
        }
    }

    /// Create StreamingData event for real-time LLM output chunks
    pub fn streaming_data(job_id: JobId, data: Vec<u8>) -> Self {
        Self::StreamingData {
            event: StreamingDataEvent {
                job_id: Some(job_id),
                data,
            },
        }
    }

    /// Create StreamingJobCompleted event
    pub fn streaming_job_completed(
        job_id: JobId,
        job_result_id: Option<JobResultId>,
        position: &str,
        context: TaskContext,
    ) -> Self {
        Self::StreamingJobCompleted {
            event: JobCompletedEvent {
                job_id: Some(job_id),
                job_result_id,
                position: position.to_string(),
            },
            context,
        }
    }

    /// Create JobStarted event (non-streaming)
    ///
    /// # Arguments
    /// * `job_id` - Job ID (proto type)
    /// * `runner_name` - Runner name
    /// * `worker_name` - Worker name (None for Runner-based jobs with temporary workers)
    /// * `position` - Position in workflow
    pub fn job_started(
        job_id: JobId,
        runner_name: &str,
        worker_name: Option<String>,
        position: &str,
    ) -> Self {
        Self::JobStarted {
            event: JobStartedEvent {
                job_id: Some(job_id),
                runner_name: runner_name.to_string(),
                worker_name,
                position: position.to_string(),
            },
        }
    }

    /// Create JobCompleted event (non-streaming)
    pub fn job_completed(
        job_id: JobId,
        job_result_id: Option<JobResultId>,
        position: &str,
        context: TaskContext,
    ) -> Self {
        Self::JobCompleted {
            event: JobCompletedEvent {
                job_id: Some(job_id),
                job_result_id,
                position: position.to_string(),
            },
            context,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_context_clones_share_root_scope() {
        let context = WorkflowContext::new_empty();
        let job = JobId { value: 101 };
        let guard = context.child_job_scope.guard(job);

        assert_eq!(context.clone().child_job_scope.snapshot(), vec![job]);

        guard.mark_completed();
        assert!(context.child_job_scope.snapshot().is_empty());
    }

    #[test]
    fn dropped_guard_keeps_job_registered() {
        let context = WorkflowContext::new_empty();
        let job = JobId { value: 333 };
        drop(context.child_job_scope.guard(job));

        assert_eq!(
            context.child_job_scope.snapshot(),
            vec![job],
            "an abandoned child job must stay reachable by explicit cancellation"
        );
    }

    #[test]
    fn scope_or_root_falls_back_to_workflow_root() {
        let context = WorkflowContext::new_empty();
        let task = context.child_job_scope.child();
        assert!(Arc::ptr_eq(
            &context.scope_or_root(None),
            &context.child_job_scope
        ));
        assert!(Arc::ptr_eq(&context.scope_or_root(Some(&task)), &task));
    }

    #[test]
    fn child_scope_registration_propagates_to_ancestors() {
        let root = ChildJobScope::new_root();
        let task = root.child();
        let sibling = root.child();
        let job = JobId { value: 7 };

        let guard = task.guard(job);
        assert_eq!(task.snapshot(), vec![job]);
        assert_eq!(root.snapshot(), vec![job]);
        assert!(sibling.snapshot().is_empty());

        guard.mark_completed();
        assert!(task.snapshot().is_empty());
        assert!(root.snapshot().is_empty());
    }

    #[test]
    fn closed_scope_closes_descendants_but_not_siblings_or_parent() {
        let root = ChildJobScope::new_root();
        let task = root.child();
        let nested = task.child();
        let sibling = root.child();

        task.close();
        assert!(task.is_closed());
        assert!(nested.is_closed());
        assert!(!sibling.is_closed());
        assert!(!root.is_closed());
    }

    #[test]
    fn close_keeps_jobs_registered_until_unregistered() {
        let root = ChildJobScope::new_root();
        let task = root.child();
        let job = JobId { value: 9 };
        let _guard = task.guard(job);

        assert_eq!(task.close(), vec![job]);
        assert_eq!(root.snapshot(), vec![job]);

        task.unregister(job);
        assert!(root.snapshot().is_empty());
    }

    #[tokio::test]
    async fn task_context_copies_share_child_job_scope() {
        let scope = ChildJobScope::new_root();
        let context = TaskContext {
            child_job_scope: Some(scope.clone()),
            ..TaskContext::new_empty()
        };
        for copy in [
            context.deep_copy().await,
            context.clone_with_isolated_position().await,
            context.from_flow_directive(None),
        ] {
            assert!(Arc::ptr_eq(copy.child_job_scope.as_ref().unwrap(), &scope));
        }
    }

    #[test]
    fn reported_position_is_the_resume_point_only_while_waiting() {
        let current = WorkflowPosition::parse("/ROOT/do/1/wait_task").unwrap();
        let resume = WorkflowPosition::parse("/ROOT/do/2/resume_task").unwrap();
        let mut context = WorkflowContext::new_empty();
        context.position = current.clone();
        context.resume_position = Some(resume.clone());

        context.status = WorkflowStatus::Waiting;
        assert_eq!(context.reported_position(), &resume);

        context.status = WorkflowStatus::Running;
        assert_eq!(context.reported_position(), &current);

        context.status = WorkflowStatus::Waiting;
        context.resume_position = None;
        assert_eq!(context.reported_position(), &current);
    }

    #[tokio::test]
    async fn test_match_checkpoint_by_relative_path_no_checkpoint() {
        let mut context = WorkflowContext::new_empty();
        context.checkpoint_position = None;

        let sub_path = vec![
            serde_json::Value::String("task1".to_string()),
            serde_json::Value::Number(0.into()),
        ];

        let result = context.match_checkpoint_by_relative_path(&sub_path).await;
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn test_match_checkpoint_by_relative_path_exact_match() {
        let mut context = WorkflowContext::new_empty();
        context.position = WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step1".to_string()),
        ]);
        context.checkpoint_position = Some(WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step1".to_string()),
            serde_json::Value::String("task1".to_string()),
            serde_json::Value::Number(0.into()),
        ]));

        let sub_path = vec![
            serde_json::Value::String("task1".to_string()),
            serde_json::Value::Number(0.into()),
        ];

        let result = context.match_checkpoint_by_relative_path(&sub_path).await;
        assert_eq!(result, Some(true));
        // check relative path
        let relative_path = context
            .checkpoint_position
            .as_ref()
            .unwrap()
            .relative_path(context.position.full())
            .unwrap();
        assert_eq!(relative_path, sub_path);
    }

    #[tokio::test]
    async fn test_match_checkpoint_by_relative_path_partial_match() {
        let mut context = WorkflowContext::new_empty();
        context.position = WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step1".to_string()),
        ]);
        context.checkpoint_position = Some(WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step1".to_string()),
            serde_json::Value::String("task1".to_string()),
            serde_json::Value::Number(0.into()),
            serde_json::Value::String("subtask".to_string()),
        ]));

        let sub_path = vec![
            serde_json::Value::String("task1".to_string()),
            serde_json::Value::Number(0.into()),
        ];

        let result = context.match_checkpoint_by_relative_path(&sub_path).await;
        assert_eq!(result, Some(true));

        // check relative path same position
        let relative_path = context
            .position
            .relative_path(context.position.full())
            .unwrap();
        assert_eq!(relative_path, Vec::<serde_json::Value>::new());
    }

    #[tokio::test]
    async fn test_match_checkpoint_by_relative_path_mismatch() {
        let mut context = WorkflowContext::new_empty();
        context.position = WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step1".to_string()),
        ]);
        context.checkpoint_position = Some(WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step1".to_string()),
            serde_json::Value::String("task2".to_string()), // different task
            serde_json::Value::Number(0.into()),
        ]));

        let sub_path = vec![
            serde_json::Value::String("task1".to_string()),
            serde_json::Value::Number(0.into()),
        ];

        let result = context.match_checkpoint_by_relative_path(&sub_path).await;
        assert_eq!(result, Some(false));
    }

    #[tokio::test]
    async fn test_match_checkpoint_by_relative_path_current_position_mismatch() {
        let mut context = WorkflowContext::new_empty();
        context.position = WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step2".to_string()), // different step
        ]);
        context.checkpoint_position = Some(WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step1".to_string()),
            serde_json::Value::String("task1".to_string()),
            serde_json::Value::Number(0.into()),
        ]));

        let sub_path = vec![
            serde_json::Value::String("task1".to_string()),
            serde_json::Value::Number(0.into()),
        ];

        let result = context.match_checkpoint_by_relative_path(&sub_path).await;
        assert_eq!(result, Some(false));

        // check relative path same position
        let relative_path = context
            .checkpoint_position
            .as_ref()
            .unwrap()
            .relative_path(context.position.full());
        assert_eq!(relative_path, None);
    }

    #[tokio::test]
    async fn test_match_checkpoint_by_relative_path_longer_than_target() {
        let mut context = WorkflowContext::new_empty();
        context.position = WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step1".to_string()),
        ]);
        context.checkpoint_position = Some(WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step1".to_string()),
            serde_json::Value::String("task1".to_string()),
        ]));

        let sub_path = vec![
            serde_json::Value::String("task1".to_string()),
            serde_json::Value::Number(0.into()),
            serde_json::Value::String("extra".to_string()), // longer than target
        ];

        let result = context.match_checkpoint_by_relative_path(&sub_path).await;
        assert_eq!(result, Some(false));
    }

    #[tokio::test]
    async fn test_match_checkpoint_by_relative_path_empty_sub_path() {
        let mut context = WorkflowContext::new_empty();
        context.position = WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step1".to_string()),
        ]);
        context.checkpoint_position = Some(WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::String("step1".to_string()),
        ]));

        let sub_path = vec![];

        let result = context.match_checkpoint_by_relative_path(&sub_path).await;
        assert_eq!(result, Some(true));
    }

    #[tokio::test]
    async fn test_match_checkpoint_by_relative_path_empty_current_position() {
        let mut context = WorkflowContext::new_empty();
        context.position = WorkflowPosition::new(vec![]);
        context.checkpoint_position = Some(WorkflowPosition::new(vec![
            serde_json::Value::String("task1".to_string()),
            serde_json::Value::Number(0.into()),
        ]));

        let sub_path = vec![
            serde_json::Value::String("task1".to_string()),
            serde_json::Value::Number(0.into()),
        ];

        let result = context.match_checkpoint_by_relative_path(&sub_path).await;
        assert_eq!(result, Some(true));
    }

    #[tokio::test]
    async fn test_match_checkpoint_by_relative_path_mixed_types() {
        let mut context = WorkflowContext::new_empty();
        context.position = WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::Number(1.into()),
        ]);
        context.checkpoint_position = Some(WorkflowPosition::new(vec![
            serde_json::Value::String("workflow".to_string()),
            serde_json::Value::Number(1.into()),
            serde_json::Value::String("task".to_string()),
            serde_json::Value::Number(42.into()),
            serde_json::Value::Bool(true),
        ]));

        let sub_path = vec![
            serde_json::Value::String("task".to_string()),
            serde_json::Value::Number(42.into()),
            serde_json::Value::Bool(true),
        ];

        let result = context.match_checkpoint_by_relative_path(&sub_path).await;
        assert_eq!(result, Some(true));
    }

    #[tokio::test]
    async fn test_clone_with_isolated_position_isolates_position_only() {
        let original = TaskContext::new_empty();
        original.add_position_name("root".to_string()).await;

        let cloned = original.clone_with_isolated_position().await;

        // position is independent: pushes on the clone must not leak back.
        cloned.add_position_name("child".to_string()).await;
        let original_depth = original.position.read().await.full().len();
        let cloned_depth = cloned.position.read().await.full().len();
        assert_eq!(original_depth, 1, "original position must not be mutated");
        assert_eq!(
            cloned_depth, 2,
            "clone position should track its own pushes"
        );

        // context_variables are shared: writes on the clone are visible from original.
        cloned
            .add_context_value("k".to_string(), serde_json::json!("v"))
            .await;
        let original_value = original.get_context_value("k").await;
        assert_eq!(original_value, Some(serde_json::json!("v")));
    }

    #[test]
    fn context_variables_json_round_trip_preserves_values_and_order() {
        let mut map = serde_json::Map::new();
        map.insert("zeta".to_string(), serde_json::json!({"nested": [1, 2]}));
        map.insert("alpha".to_string(), serde_json::json!("a"));
        map.insert("mid".to_string(), serde_json::Value::Null);
        let vars = context_variables_from_json(&map);
        assert_eq!(
            vars.keys().collect::<Vec<_>>(),
            vec!["zeta", "alpha", "mid"]
        );
        let back = context_variables_to_json(&vars);
        assert_eq!(back, map);
        assert_eq!(
            back.keys().collect::<Vec<_>>(),
            vec!["zeta", "alpha", "mid"]
        );
    }

    #[tokio::test]
    async fn deep_copy_isolates_context_variable_updates() {
        let original = TaskContext::new_empty();
        original
            .add_context_value("k".to_string(), serde_json::json!(1))
            .await;
        let copy = original.deep_copy().await;
        copy.add_context_value("k".to_string(), serde_json::json!(2))
            .await;
        copy.add_context_value("only_copy".to_string(), serde_json::json!(3))
            .await;
        assert_eq!(
            original.get_context_value("k").await,
            Some(serde_json::json!(1))
        );
        assert_eq!(original.get_context_value("only_copy").await, None);
        assert_eq!(
            copy.get_context_value("k").await,
            Some(serde_json::json!(2))
        );
    }

    #[tokio::test]
    async fn remove_context_value_keeps_remaining_order() {
        let ctx = TaskContext::new_empty();
        for k in ["a", "b", "c"] {
            ctx.add_context_value(k.to_string(), serde_json::json!(k))
                .await;
        }
        assert_eq!(
            ctx.remove_context_value("a").await,
            Some(serde_json::json!("a"))
        );
        assert_eq!(
            ctx.context_variables
                .lock()
                .await
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["b".to_string(), "c".to_string()]
        );
    }

    #[tokio::test]
    async fn test_clone_with_isolated_position_no_accumulation_across_iterations() {
        // Reproduces the for-loop sequential-iteration scenario: if the inner
        // task pushes onto position but a swallowed error (try.catch with
        // onError=continue) prevents the matching pop, the next iteration must
        // still start from the parent position — not from the leftover depth.
        let parent = TaskContext::new_empty();
        parent.add_position_name("for".to_string()).await;
        let parent_depth = parent.position.read().await.full().len();

        for _ in 0..5 {
            let iter_ctx = parent.clone_with_isolated_position().await;
            iter_ctx.add_position_name("do".to_string()).await;
            iter_ctx.add_position_name("0".to_string()).await;
            iter_ctx
                .add_position_name("invokeWithRetry".to_string())
                .await;
            // Simulate try-catch swallowing the error without popping back.
        }

        let after = parent.position.read().await.full().len();
        assert_eq!(
            after, parent_depth,
            "parent position must remain stable across iterations"
        );
    }
}
