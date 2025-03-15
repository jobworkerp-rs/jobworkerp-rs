use anyhow::Result;
use chrono::{DateTime, FixedOffset};
use std::{collections::BTreeMap, fmt, ops::Deref, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::simple_workflow::definition::workflow::{self, FlowDirective, ServerlessWorkflow, Task};

pub trait UseExpression {
    fn expression(
        &self,
        workflow_context: &WorkflowContext,
        task_context: Arc<TaskContext>,
    ) -> impl std::future::Future<Output = Result<BTreeMap<String, Arc<serde_json::Value>>>> + Send
    {
        async move {
            let mut expression = BTreeMap::<String, Arc<serde_json::Value>>::new();
            expression.insert("input".to_string(), task_context.input.clone());
            expression.insert("output".to_string(), task_context.output.clone());
            expression.insert(
                "context".to_string(),
                Arc::new(serde_json::to_value(workflow_context).map_err(|e| {
                    anyhow::anyhow!("Failed to serialize workflow context: {:#?}", e)
                })?),
            );
            expression.insert(
                "runtime".to_string(),
                Arc::new(
                    serde_json::to_value(workflow_context.to_runtime_descriptor()).map_err(
                        |e| anyhow::anyhow!("Failed to serialize runtime descriptor: {:#?}", e),
                    )?,
                ),
            );
            expression.insert(
                "workflow".to_string(),
                Arc::new(serde_json::to_value(workflow_context.to_descriptor())?),
            );
            expression.insert(
                "task".to_string(),
                Arc::new(serde_json::to_value(task_context.to_descriptor())?),
            );
            {
                task_context
                    .context_variables
                    .lock()
                    .await
                    .iter()
                    .for_each(|(k, v)| {
                        expression.insert(k.clone(), Arc::new(v.clone()));
                    });
            }
            workflow_context
                .context_variables
                .iter()
                .for_each(|(k, v)| {
                    expression.insert(k.clone(), Arc::new(v.clone()));
                });
            Ok(expression)
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct WorkflowContext {
    pub id: Uuid,
    pub definition: Arc<workflow::ServerlessWorkflow>,
    pub input: Arc<serde_json::Value>,
    pub status: WorkflowStatus,
    pub started_at: DateTime<FixedOffset>,
    pub output: Option<Arc<serde_json::Value>>,
    pub context_variables: Arc<serde_json::Map<String, serde_json::Value>>,
}
impl WorkflowContext {
    pub fn new(
        workflow: &workflow::ServerlessWorkflow,
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
            context_variables: context
                .as_object()
                .map(|o| Arc::new(o.clone()))
                .unwrap_or_else(|| Arc::new(serde_json::Map::new())),
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
            _ => output.to_string(),
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
                definition: self.definition.clone(),
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
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct WorkflowDescriptor {
    id: serde_json::Value,
    definition: Arc<ServerlessWorkflow>,
    input: Arc<serde_json::Value>,
    started_at: serde_json::Value,
}
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct TaskDescriptor {
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

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub enum Then {
    Continue,
    Exit,
    End,
    TaskName(String),
}
impl From<FlowDirective> for Then {
    fn from(directive: FlowDirective) -> Self {
        match directive {
            FlowDirective {
                subtype_0: Some(subtype_0),
                subtype_1: Some(subtype_1),
            } => match subtype_0 {
                workflow::FlowDirectiveSubtype0::Continue => Then::TaskName(subtype_1.clone()),
                workflow::FlowDirectiveSubtype0::Exit => Then::Exit,
                workflow::FlowDirectiveSubtype0::End => Then::End,
            },
            FlowDirective {
                subtype_0: Some(subtype_0),
                subtype_1: None,
            } => match subtype_0 {
                workflow::FlowDirectiveSubtype0::Continue => Then::Continue,
                workflow::FlowDirectiveSubtype0::Exit => Then::Exit,
                workflow::FlowDirectiveSubtype0::End => Then::End,
            },
            FlowDirective {
                subtype_0: None,
                subtype_1: Some(label),
            } => Then::TaskName(label.clone()),
            _ => Then::Continue,
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
