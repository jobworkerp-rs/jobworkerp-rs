use crate::workflow::{
    definition::{
        transform::UseJqAndTemplateTransformer,
        workflow::{self, tasks::TaskTrait, FlowDirective, Task},
    },
    execute::checkpoint,
};
use chrono::{DateTime, FixedOffset};
pub use infra::workflow::position::WorkflowPosition;
use std::{collections::BTreeMap, fmt, ops::Deref, str::FromStr, sync::Arc};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

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
    #[serde(skip)]
    pub checkpoint_position: Option<WorkflowPosition>,
    #[serde(skip)]
    pub context_variables: Arc<Mutex<serde_json::Map<String, serde_json::Value>>>,
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
            context_variables: context
                .as_object()
                .map(|o| Arc::new(Mutex::new(o.clone())))
                .unwrap_or_else(|| Arc::new(Mutex::new(serde_json::Map::new()))),
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
            context_variables: Arc::new(Mutex::new(serde_json::Map::new())),
        }
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
        self.context_variables.lock().await.insert(key, value);
    }
    pub async fn remove_context_value(&mut self, key: &str) {
        // overwrite
        self.context_variables.lock().await.remove(key);
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
    pub context_variables: Arc<Mutex<serde_json::Map<String, serde_json::Value>>>,

    pub started_at: DateTime<FixedOffset>,
    pub completed_at: Option<DateTime<FixedOffset>>,
    pub flow_directive: Then,
    pub position: Arc<RwLock<WorkflowPosition>>,
}
impl TaskContext {
    pub fn new(
        task: Option<Arc<workflow::Task>>,
        // input: input data. if not set explicitly, use empty key, previous output
        input: Arc<serde_json::Value>,
        // context_variables: workflow context variables.
        context_variables: Arc<Mutex<serde_json::Map<String, serde_json::Value>>>,
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
            context_variables: Arc::new(Mutex::new((*checkpoint.context_variables).clone())),
            started_at: command_utils::util::datetime::now(),
            completed_at: None,
            flow_directive: Then::from_str(checkpoint.flow_directive.as_str())
                .unwrap_or(Then::Continue),
            position: Arc::new(RwLock::new(WorkflowPosition::new(vec![]))),
        }
    }
    pub fn new_empty() -> Self {
        Self {
            definition: None,
            raw_input: Arc::new(serde_json::Value::Null),
            input: Arc::new(serde_json::Value::Null),
            raw_output: Arc::new(serde_json::Value::Null),
            output: Arc::new(serde_json::Value::Null),
            context_variables: Arc::new(Mutex::new(serde_json::Map::new())),
            started_at: command_utils::util::datetime::now(),
            completed_at: None,
            flow_directive: Then::Continue,
            position: Arc::new(RwLock::new(WorkflowPosition::new(vec![]))),
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
        self.context_variables.lock().await.insert(key, value);
    }
    pub async fn remove_context_value(&self, key: &str) -> Option<serde_json::Value> {
        self.context_variables.lock().await.remove(key)
    }
    //cloned
    pub async fn get_context_value(&self, key: &str) -> Option<serde_json::Value> {
        self.context_variables.lock().await.get(key).cloned()
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
        expression: &BTreeMap<String, Arc<serde_json::Value>>,
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

// Re-export protobuf types for workflow events
pub use proto::jobworkerp::data::{
    workflow_event, JobCompletedEvent, JobStartedEvent, TaskCompletedEvent, TaskStartedEvent,
    WorkflowCancelledEvent, WorkflowEvent,
};

/// Internal representation combining WorkflowEvent with TaskContext
///
/// protobuf's WorkflowEvent is serializable but doesn't contain TaskContext.
/// This enum is used for internal processing, and can be converted to
/// WorkflowEvent for external output (gRPC/serialization).
#[derive(Debug, Clone)]
pub enum WorkflowStreamEvent {
    // Job execution tasks (RunTask)
    StreamingJobStarted {
        event: JobStartedEvent,
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
    // Reserved for future use
    // WorkflowCancelled { event: WorkflowCancelledEvent },
}

impl WorkflowStreamEvent {
    /// Convert to protobuf WorkflowEvent (for gRPC/serialization)
    pub fn to_proto(&self) -> WorkflowEvent {
        match self {
            Self::StreamingJobStarted { event } => WorkflowEvent {
                event: Some(workflow_event::Event::StreamingJobStarted(event.clone())),
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
            workflow::errors::ErrorFactory::create(
                workflow::errors::ErrorCode::InternalError,
                Some("No completed event found in stream".to_string()),
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
    pub fn position(&self) -> &str {
        match self {
            Self::StreamingJobStarted { event } => &event.position,
            Self::StreamingJobCompleted { event, .. } => &event.position,
            Self::JobStarted { event } => &event.position,
            Self::JobCompleted { event, .. } => &event.position,
            Self::TaskStarted { event } => &event.position,
            Self::TaskCompleted { event, .. } => &event.position,
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
        let output = serde_json::to_vec(&context.output).unwrap_or_default();
        Self::TaskCompleted {
            event: TaskCompletedEvent {
                task_type: task_type.to_string(),
                task_name: task_name.to_string(),
                position,
                output,
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
        let output = serde_json::to_vec(&context.output).unwrap_or_default();
        Self::TaskCompleted {
            event: TaskCompletedEvent {
                task_type: task_type.to_string(),
                task_name: task_name.to_string(),
                position: position.to_string(),
                output,
            },
            context,
        }
    }

    /// Create StreamingJobStarted event
    pub fn streaming_job_started(
        job_id: i64,
        runner_name: &str,
        worker_name: &str,
        position: &str,
    ) -> Self {
        Self::StreamingJobStarted {
            event: JobStartedEvent {
                job_id,
                runner_name: runner_name.to_string(),
                worker_name: worker_name.to_string(),
                position: position.to_string(),
            },
        }
    }

    /// Create StreamingJobCompleted event
    pub fn streaming_job_completed(
        job_id: i64,
        job_result_id: Option<i64>,
        position: &str,
        context: TaskContext,
    ) -> Self {
        let output = serde_json::to_vec(&context.output).unwrap_or_default();
        Self::StreamingJobCompleted {
            event: JobCompletedEvent {
                job_id,
                job_result_id,
                position: position.to_string(),
                output,
            },
            context,
        }
    }

    /// Create JobStarted event (non-streaming)
    pub fn job_started(job_id: i64, runner_name: &str, worker_name: &str, position: &str) -> Self {
        Self::JobStarted {
            event: JobStartedEvent {
                job_id,
                runner_name: runner_name.to_string(),
                worker_name: worker_name.to_string(),
                position: position.to_string(),
            },
        }
    }

    /// Create JobCompleted event (non-streaming)
    pub fn job_completed(
        job_id: i64,
        job_result_id: Option<i64>,
        position: &str,
        context: TaskContext,
    ) -> Self {
        let output = serde_json::to_vec(&context.output).unwrap_or_default();
        Self::JobCompleted {
            event: JobCompletedEvent {
                job_id,
                job_result_id,
                position: position.to_string(),
                output,
            },
            context,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
