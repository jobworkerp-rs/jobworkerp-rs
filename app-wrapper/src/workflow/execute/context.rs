use crate::workflow::definition::{
    transform::UseJqAndTemplateTransformer,
    workflow::{self, FlowDirective, Task, WorkflowSchema},
};
use chrono::{DateTime, FixedOffset};
use command_utils::util::stack::StackWithHistory;
use std::{collections::BTreeMap, fmt, ops::Deref, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct WorkflowContext {
    pub id: Uuid,
    #[serde(skip)]
    pub definition: Arc<workflow::WorkflowSchema>,
    pub input: Arc<serde_json::Value>,
    pub status: WorkflowStatus,
    pub started_at: DateTime<FixedOffset>,
    pub output: Option<Arc<serde_json::Value>>,
    pub position: WorkflowPosition,
    #[serde(skip)]
    pub context_variables: Arc<Mutex<serde_json::Map<String, serde_json::Value>>>,
}
impl WorkflowContext {
    pub fn new(
        workflow: &workflow::WorkflowSchema,
        input: Arc<serde_json::Value>,
        context: Arc<serde_json::Value>,
    ) -> Self {
        let uuid = Uuid::now_v7();
        let started_at = uuid
            .get_timestamp()
            .map(|t| command_utils::util::datetime::from_epoch_sec(t.to_unix().0 as i64))
            .unwrap_or_else(command_utils::util::datetime::now);
        Self {
            id: uuid,
            definition: Arc::new(workflow.clone()),
            input,
            status: WorkflowStatus::Pending,
            started_at,
            output: None,
            position: WorkflowPosition::new(vec![]),
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
            definition: Arc::new(WorkflowSchema::default()),
            input: Arc::new(serde_json::Value::Null),
            status: WorkflowStatus::Pending,
            started_at,
            output: None,
            position: WorkflowPosition::new(vec![]),
            context_variables: Arc::new(Mutex::new(serde_json::Map::new())),
        }
    }
    pub fn to_descriptor(&self) -> WorkflowDescriptor {
        WorkflowDescriptor {
            id: serde_json::Value::String(self.id.to_string()),
            definition: self.definition.clone(),
            input: self.input.clone(),
            started_at: serde_json::Value::String(self.started_at.to_rfc3339()),
        }
    }
    pub fn to_runtime_descriptor(&self) -> RuntimeDescriptor {
        RuntimeDescriptor {
            name: self.definition.document.name.deref().to_string(),
            // version: self.definition.document.version.deref().to_string(),
            metadata: self.definition.document.metadata.clone(),
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
    pub position: Arc<Mutex<WorkflowPosition>>,
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
            position: Arc::new(Mutex::new(WorkflowPosition::new(vec![]))),
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
            position: Arc::new(Mutex::new(WorkflowPosition::new(vec![]))),
        }
    }
    pub async fn add_position_name(&self, name: String) {
        self.position.lock().await.push(name);
    }
    pub async fn add_position_index(&self, idx: u32) {
        self.position.lock().await.push_idx(idx);
    }
    pub async fn remove_position(&self) -> Option<serde_json::Value> {
        self.position.lock().await.pop()
    }
    pub async fn current_position(&self) -> Option<serde_json::Value> {
        self.position.lock().await.current().cloned()
    }
    pub async fn prev_position(&self, n: usize) -> Vec<serde_json::Value> {
        self.position.lock().await.n_prev(n)
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
    definition: Arc<WorkflowSchema>,
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

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub enum WorkflowStatus {
    Pending,
    Running,
    Waiting, // not implemented
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

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct WorkflowPosition {
    pub path: StackWithHistory<serde_json::Value>,
}

impl WorkflowPosition {
    pub fn new(path: Vec<serde_json::Value>) -> Self {
        Self {
            path: StackWithHistory::new_with(path),
        }
    }
    pub fn push(&mut self, name: String) {
        self.path.push(serde_json::Value::String(name));
    }
    pub fn push_idx(&mut self, idx: u32) -> bool {
        serde_json::Number::from_u128(idx as u128)
            .map(|n| self.path.push(serde_json::Value::Number(n)))
            .is_some()
    }
    pub fn pop(&mut self) -> Option<serde_json::Value> {
        self.path.pop()
    }
    pub fn current(&self) -> Option<&serde_json::Value> {
        self.path.last()
    }
    pub fn n_prev(&self, n: usize) -> Vec<serde_json::Value> {
        self.path.state_before_operations(n)
    }
    // rfc-6901
    pub fn as_json_pointer(&self) -> String {
        self.path.json_pointer()
    }
    // alias
    pub fn as_error_instance(&self) -> String {
        self.as_json_pointer()
    }
}

impl std::fmt::Display for WorkflowPosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_json_pointer())
    }
}
