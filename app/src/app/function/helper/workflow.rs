use crate::app::{job::UseJobApp, worker::UseWorkerApp};
use proto::ProtobufHelper;

pub trait ReusableWorkflowHelper: UseJobApp + UseWorkerApp + ProtobufHelper + Send + Sync {
    const WORKFLOW_CHANNEL: Option<&str> = Some("workflow");
}
