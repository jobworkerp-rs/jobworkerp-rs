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
use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    ops::Deref,
    str::FromStr,
    sync::Arc,
};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

const MAX_CHILD_EXECUTION_RECEIPTS: usize = 128;
const MAX_CHILD_RECEIPT_POSITION_BYTES: usize = 1024;
const MAX_CHILD_RECEIPT_STRING_BYTES: usize = 1024;

#[derive(Debug, Default)]
struct ChildExecutionReceiptCollection {
    receipts: Vec<jobworkerp_runner::jobworkerp::runner::ChildExecutionReceipt>,
    incomplete: bool,
}

impl ChildExecutionReceiptCollection {
    fn incomplete() -> Self {
        Self {
            receipts: Vec::new(),
            incomplete: true,
        }
    }
}

fn restored_child_execution_receipts() -> Arc<std::sync::Mutex<ChildExecutionReceiptCollection>> {
    Arc::new(std::sync::Mutex::new(
        ChildExecutionReceiptCollection::incomplete(),
    ))
}

fn valid_child_sandbox_observation(
    receipt: &jobworkerp_runner::jobworkerp::runner::ChildExecutionReceipt,
    witness: &jobworkerp_runner::jobworkerp::runner::ChildSandboxObservation,
) -> bool {
    use jobworkerp_runner::jobworkerp::runner::{
        ChildSandboxAnomalyCode, ChildSandboxEndState, ChildSandboxProducerState,
    };

    let Ok(end_state) = ChildSandboxEndState::try_from(witness.end_state) else {
        return false;
    };
    let Ok(producer_state) = ChildSandboxProducerState::try_from(witness.producer_state) else {
        return false;
    };
    let selected_method = if witness.using.is_empty() {
        "run"
    } else {
        witness.using.as_str()
    };
    let anomalies_are_valid = witness.anomaly_codes.len() <= 32
        && witness.anomaly_codes.iter().all(|code| {
            ChildSandboxAnomalyCode::try_from(*code)
                .is_ok_and(|code| code != ChildSandboxAnomalyCode::Unspecified)
        });

    witness.observation_sha256.len() == 32
        && witness.host_settings_sha256.len() == 32
        && witness.using.len() <= 255
        && witness
            .stdout_sha256
            .as_ref()
            .is_none_or(|digest| digest.len() == 32)
        && witness
            .stderr_sha256
            .as_ref()
            .is_none_or(|digest| digest.len() == 32)
        && witness
            .trailer_sha256
            .as_ref()
            .is_none_or(|digest| digest.len() == 32)
        && anomalies_are_valid
        && (producer_state != ChildSandboxProducerState::Clean
            || (end_state == ChildSandboxEndState::Normal
                && witness.cli_exit_code.is_some()
                && witness.anomaly_codes.is_empty()))
        && receipt.method_using.as_deref() == Some(selected_method)
        && receipt
            .cli_exit_code
            .is_none_or(|stream_exit| witness.cli_exit_code == Some(stream_exit))
        && (!receipt.end_received
            || receipt.protocol_failure.is_some()
            || end_state == ChildSandboxEndState::Normal)
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
    #[serde(skip)]
    pub checkpoint_position: Option<WorkflowPosition>,
    #[serde(skip)]
    pub context_variables: Arc<Mutex<serde_json::Map<String, serde_json::Value>>>,
    /// Job IDs of child jobs currently being executed by this workflow's tasks.
    /// Populated when a task enqueues a child job and cleared on completion so
    /// that, on cancellation, the workflow can actively cancel every in-flight
    /// child job. Stored as `JobId.value` (i64) for trivial Hash/Eq. Shared
    /// across all task clones of the context (Arc), so fork/for parallel tasks
    /// register their own jobs into the same set. Transient runtime state, so
    /// `serde(skip)` (never persisted into checkpoints) like `context_variables`.
    ///
    /// A `std::sync::Mutex` (not tokio's) because the critical sections are tiny
    /// set operations with no await inside — register/unregister run on the
    /// per-task hot path, so we avoid the async lock's scheduling overhead.
    #[serde(skip)]
    pub running_job_ids: Arc<std::sync::Mutex<HashSet<i64>>>,
    /// Named authentication policies from `use.authentications`, for resolving
    /// `authentication: { use: <name> }` references. `serde(skip)` so they never
    /// leak into the serialized expression context or persisted checkpoints.
    #[serde(skip)]
    pub authentications: Arc<std::collections::HashMap<String, workflow::AuthenticationPolicy>>,
    /// Secret names declared in `use.secrets`. Only these may be resolved from
    /// the environment. Values are never stored here — only the declared names.
    #[serde(skip)]
    pub declared_secrets: Arc<HashSet<String>>,
    /// Server-observed child execution receipts are transient evidence and are
    /// deliberately excluded from checkpoints and expression contexts.
    #[serde(skip, default = "restored_child_execution_receipts")]
    child_execution_receipts: Arc<std::sync::Mutex<ChildExecutionReceiptCollection>>,
}

#[derive(Debug)]
pub struct RunningJobGuard {
    running_job_ids: Arc<std::sync::Mutex<HashSet<i64>>>,
    job_id: JobId,
    active: bool,
}

impl RunningJobGuard {
    fn new(running_job_ids: Arc<std::sync::Mutex<HashSet<i64>>>, job_id: JobId) -> Self {
        running_job_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job_id.value);
        Self {
            running_job_ids,
            job_id,
            active: true,
        }
    }

    pub fn job_id(&self) -> JobId {
        self.job_id
    }

    /// Mark the job as normally completed, unregistering it from the
    /// in-flight set so a later cancellation does not target it.
    pub fn mark_completed(mut self) {
        self.unregister();
    }

    fn unregister(&mut self) {
        if self.active {
            self.running_job_ids
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.job_id.value);
            self.active = false;
        }
    }
}

impl Drop for RunningJobGuard {
    fn drop(&mut self) {
        self.unregister();
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
            context_variables: context
                .as_object()
                .map(|o| Arc::new(Mutex::new(o.clone())))
                .unwrap_or_else(|| Arc::new(Mutex::new(serde_json::Map::new()))),
            running_job_ids: Arc::new(std::sync::Mutex::new(HashSet::new())),
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
            child_execution_receipts: Arc::new(std::sync::Mutex::new(
                ChildExecutionReceiptCollection::default(),
            )),
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
            running_job_ids: Arc::new(std::sync::Mutex::new(HashSet::new())),
            authentications: Arc::new(std::collections::HashMap::new()),
            declared_secrets: Arc::new(HashSet::new()),
            child_execution_receipts: Arc::new(std::sync::Mutex::new(
                ChildExecutionReceiptCollection::default(),
            )),
        }
    }

    /// Record immutable host evidence for one bounded child execution.
    pub fn record_child_execution_receipt(
        &self,
        receipt: jobworkerp_runner::jobworkerp::runner::ChildExecutionReceipt,
    ) {
        let mut collection = self
            .child_execution_receipts
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let durable_state =
            jobworkerp_runner::jobworkerp::runner::ChildDurableLookupState::try_from(
                receipt.durable_lookup_state,
            )
            .ok();
        let is_verified = durable_state
            == Some(jobworkerp_runner::jobworkerp::runner::ChildDurableLookupState::Verified);
        let is_missing = durable_state
            == Some(jobworkerp_runner::jobworkerp::runner::ChildDurableLookupState::Missing);
        let has_valid_result_facts = receipt
            .child_job_result_id
            .is_none_or(|result_id| result_id > 0)
            && receipt.child_job_result_status.is_none_or(|status| {
                proto::jobworkerp::data::ResultStatus::try_from(status).is_ok()
            });
        let result_facts_are_paired =
            receipt.child_job_result_id.is_some() == receipt.child_job_result_status.is_some();
        let durable_facts_consistent = if is_verified {
            receipt.child_job_result_id.is_some()
                && receipt.child_job_result_status.is_some()
                && has_valid_result_facts
                && receipt
                    .sandbox_observation
                    .as_ref()
                    .is_some_and(|witness| valid_child_sandbox_observation(&receipt, witness))
        } else if is_missing {
            receipt.child_job_result_id.is_none()
                && receipt.child_job_result_status.is_none()
                && receipt.sandbox_observation.is_none()
        } else if durable_state
            == Some(jobworkerp_runner::jobworkerp::runner::ChildDurableLookupState::Unknown)
        {
            // A matching durable row can establish that a result is available,
            // but without an execution-time settings witness its id/status are
            // not proof that the child ran under those settings.
            result_facts_are_paired
                && has_valid_result_facts
                && receipt.sandbox_observation.is_none()
        } else {
            false
        };
        let invalid = receipt.workflow_execution_id != self.id.to_string()
            || receipt.workflow_execution_id.is_empty()
            || receipt.task_position.len() > MAX_CHILD_RECEIPT_POSITION_BYTES
            || receipt.child_job_id <= 0
            || receipt.worker_name.is_empty()
            || receipt.worker_name.len() > MAX_CHILD_RECEIPT_STRING_BYTES
            || receipt.runner_name != "SANDBOX"
            || receipt
                .method_using
                .as_ref()
                .is_some_and(|method| method.is_empty() || method.len() > 128)
            || receipt
                .protocol_failure
                .as_ref()
                .is_some_and(|failure| failure.len() > MAX_CHILD_RECEIPT_STRING_BYTES)
            || receipt.settings_sha256.len() != 32
            || receipt.method_schema_sha256.len() != 32
            || receipt.arguments_sha256.len() != 32
            || receipt.worker_id.is_some_and(|id| id <= 0)
            || receipt.runner_id.is_some_and(|id| id <= 0)
            || receipt.child_job_result_id.is_some_and(|id| id <= 0)
            || receipt.timeout_sec.is_some_and(|timeout| timeout == 0)
            || receipt.producer_eof
                != jobworkerp_runner::jobworkerp::runner::ChildProducerEof::Unknown as i32
            || durable_state.is_none()
            || !durable_facts_consistent
            || (is_missing
                && (receipt.protocol_failure.is_some()
                    || !receipt.end_received
                    || receipt.store_success != Some(true)
                    || receipt.store_failure != Some(true)
                    || receipt.broadcast_results != Some(true)));
        if invalid {
            collection.incomplete = true;
            return;
        }

        if let Some(existing) = collection.receipts.iter().find(|existing| {
            existing.workflow_execution_id == receipt.workflow_execution_id
                && existing.task_position == receipt.task_position
                && existing.child_job_id == receipt.child_job_id
        }) {
            if existing != &receipt {
                collection.incomplete = true;
            }
            return;
        }

        if collection.receipts.len() >= MAX_CHILD_EXECUTION_RECEIPTS {
            collection.incomplete = true;
            return;
        }
        collection.receipts.push(receipt);
    }

    /// Return only server-collected evidence; workflow output is never read.
    pub fn child_execution_receipts(
        &self,
    ) -> Option<jobworkerp_runner::jobworkerp::runner::ChildExecutionReceipts> {
        use jobworkerp_runner::jobworkerp::runner::ChildExecutionReceipts;

        let collection = self
            .child_execution_receipts
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if collection.receipts.is_empty() && !collection.incomplete {
            return None;
        }
        Some(ChildExecutionReceipts {
            schema_version: 1,
            receipts: collection.receipts.clone(),
            collection_incomplete: collection.incomplete,
        })
    }

    /// A checkpoint resume may skip tasks whose receipts were transient and
    /// deliberately not serialized. Keep newly collected receipts, but mark
    /// the collection as permanently incomplete for this workflow execution.
    pub(crate) fn mark_receipts_incomplete(&self) {
        self.child_execution_receipts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .incomplete = true;
    }

    pub fn guard_running_job(&self, job_id: JobId) -> RunningJobGuard {
        RunningJobGuard::new(self.running_job_ids.clone(), job_id)
    }

    pub fn is_cancelled(&self) -> bool {
        self.status == WorkflowStatus::Cancelled
    }

    /// Snapshot the currently in-flight child job IDs. Used on cancellation to
    /// fan out `delete_job` to every running child without holding the lock
    /// across the await points of those calls.
    pub async fn snapshot_running_jobs(&self) -> Vec<JobId> {
        self.running_job_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .map(|value| JobId { value })
            .collect()
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

    fn sample_child_receipt(
        workflow_execution_id: &str,
        position: &str,
        job_id: i64,
    ) -> jobworkerp_runner::jobworkerp::runner::ChildExecutionReceipt {
        jobworkerp_runner::jobworkerp::runner::ChildExecutionReceipt {
            workflow_execution_id: workflow_execution_id.to_string(),
            task_position: position.to_string(),
            child_job_id: job_id,
            worker_id: Some(11),
            worker_name: "sandbox-worker".to_string(),
            runner_id: Some(22),
            runner_name: "SANDBOX".to_string(),
            method_using: Some("run".to_string()),
            settings_sha256: vec![1; 32],
            method_schema_sha256: vec![2; 32],
            arguments_sha256: vec![3; 32],
            timeout_sec: Some(30),
            cli_exit_code: Some(7),
            end_received: true,
            protocol_failure: None,
            producer_eof: jobworkerp_runner::jobworkerp::runner::ChildProducerEof::Unknown as i32,
            child_job_result_id: None,
            child_job_result_status: None,
            durable_lookup_state:
                jobworkerp_runner::jobworkerp::runner::ChildDurableLookupState::Unknown as i32,
            store_success: Some(true),
            store_failure: Some(true),
            broadcast_results: Some(true),
            sandbox_observation: None,
        }
    }

    #[test]
    fn child_execution_receipts_are_private_immutable_and_task_scoped() {
        let context = WorkflowContext::new_empty();
        assert!(context.child_execution_receipts().is_none());

        let first = sample_child_receipt(&context.id.to_string(), "/jobs/0", 101);
        let second = sample_child_receipt(&context.id.to_string(), "/jobs/1", 102);
        context.record_child_execution_receipt(first.clone());
        context
            .clone()
            .record_child_execution_receipt(second.clone());

        let envelope = context.child_execution_receipts().unwrap();
        assert_eq!(envelope.schema_version, 1);
        assert_eq!(envelope.receipts, vec![first.clone(), second.clone()]);
        assert!(!envelope.collection_incomplete);

        // Guest-controlled output can contain a similarly named property, but
        // only the private server-side collector contributes to the envelope.
        let mut guest_context = context.clone();
        guest_context.output = Some(Arc::new(serde_json::json!({
            "_child_execution_receipts": [{"child_job_id": 999}]
        })));
        assert_eq!(
            guest_context.child_execution_receipts().unwrap().receipts,
            vec![first, second]
        );
    }

    #[test]
    fn child_receipt_conflict_keeps_original_and_marks_collection_incomplete() {
        let context = WorkflowContext::new_empty();
        let original = sample_child_receipt(&context.id.to_string(), "/task", 1001);
        let mut conflict = original.clone();
        conflict.arguments_sha256 = vec![9; 32];

        context.record_child_execution_receipt(original.clone());
        context.record_child_execution_receipt(conflict);

        let envelope = context.child_execution_receipts().unwrap();
        assert_eq!(envelope.receipts, vec![original]);
        assert!(envelope.collection_incomplete);
    }

    #[test]
    fn unknown_durable_lookup_can_carry_unverified_result_availability() {
        let context = WorkflowContext::new_empty();
        let mut receipt = sample_child_receipt(&context.id.to_string(), "/task", 1002);
        receipt.child_job_result_id = Some(987);
        receipt.child_job_result_status =
            Some(proto::jobworkerp::data::ResultStatus::Success as i32);

        context.record_child_execution_receipt(receipt.clone());

        let envelope = context.child_execution_receipts().unwrap();
        assert!(!envelope.collection_incomplete);
        assert_eq!(envelope.receipts, vec![receipt]);
        assert_eq!(
            envelope.receipts[0].durable_lookup_state,
            jobworkerp_runner::jobworkerp::runner::ChildDurableLookupState::Unknown as i32
        );
    }

    #[test]
    fn child_receipt_collection_is_bounded_and_marks_overflow() {
        let context = WorkflowContext::new_empty();

        for job_id in 1..=128 {
            context.record_child_execution_receipt(sample_child_receipt(
                &context.id.to_string(),
                &format!("/parallel/{job_id}"),
                job_id,
            ));
        }

        let at_bound = context.child_execution_receipts().unwrap();
        assert_eq!(at_bound.receipts.len(), 128);
        assert!(!at_bound.collection_incomplete);

        context.record_child_execution_receipt(sample_child_receipt(
            &context.id.to_string(),
            "/parallel/129",
            129,
        ));

        let envelope = context.child_execution_receipts().unwrap();
        assert_eq!(envelope.receipts.len(), 128);
        assert!(envelope.collection_incomplete);
    }

    #[test]
    fn invalid_receipt_bounds_mark_collection_incomplete_without_accepting_data() {
        let context = WorkflowContext::new_empty();
        let mut bad_digest = sample_child_receipt(&context.id.to_string(), "/task", 201);
        bad_digest.arguments_sha256.pop();
        context.record_child_execution_receipt(bad_digest);

        let mut bad_position = sample_child_receipt(
            &context.id.to_string(),
            &"x".repeat(MAX_CHILD_RECEIPT_POSITION_BYTES + 1),
            202,
        );
        bad_position.protocol_failure = Some("oversized position".to_string());
        context.record_child_execution_receipt(bad_position);

        let envelope = context.child_execution_receipts().unwrap();
        assert!(envelope.receipts.is_empty());
        assert!(envelope.collection_incomplete);
    }

    #[test]
    fn deserialized_checkpoint_marks_transient_receipt_collection_incomplete() {
        let context = WorkflowContext::new_empty();
        let encoded = serde_json::to_value(&context).unwrap();
        assert!(encoded.get("child_execution_receipts").is_none());

        let restored: WorkflowContext = serde_json::from_value(encoded).unwrap();
        let envelope = restored.child_execution_receipts().unwrap();
        assert!(envelope.collection_incomplete);
        assert!(envelope.receipts.is_empty());
    }

    #[tokio::test]
    async fn test_workflow_context_running_job_guards_share_snapshot() {
        let context = WorkflowContext::new_empty();
        let job1 = JobId { value: 101 };
        let job2 = JobId { value: 202 };

        let guard1 = context.guard_running_job(job1);
        let guard2 = context.guard_running_job(job2);

        let mut snapshot = context.snapshot_running_jobs().await;
        snapshot.sort_by_key(|j| j.value);
        assert_eq!(snapshot, vec![job1, job2]);

        // A clone of the context shares the same underlying set (Arc), so an
        // active guard must be visible from both sides. This mirrors how
        // fork/for spawn clones of the context.
        let cloned = context.clone();
        let mut cloned_snapshot = cloned.snapshot_running_jobs().await;
        cloned_snapshot.sort_by_key(|j| j.value);
        assert_eq!(cloned_snapshot, vec![job1, job2]);

        guard1.mark_completed();
        let snapshot = context.snapshot_running_jobs().await;
        assert_eq!(snapshot, vec![job2]);

        drop(guard2);
        assert!(context.snapshot_running_jobs().await.is_empty());
    }

    #[tokio::test]
    async fn test_running_job_guard_unregisters_on_drop() {
        let context = WorkflowContext::new_empty();
        let job = JobId { value: 333 };

        {
            let guard = context.guard_running_job(job);
            assert_eq!(guard.job_id(), job);
            assert_eq!(context.snapshot_running_jobs().await, vec![job]);
        }

        assert!(
            context.snapshot_running_jobs().await.is_empty(),
            "dropping a running job guard must unregister the child job"
        );
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
        let original_value = original.context_variables.lock().await.get("k").cloned();
        assert_eq!(original_value, Some(serde_json::json!("v")));
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
