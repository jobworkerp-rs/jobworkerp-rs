use proto::jobworkerp::data::{Worker, WorkerId};
use strum_macros::EnumIter;

#[derive(Debug, Clone, Copy, EnumIter)]
pub enum BuiltinWorkerIds {
    SlackWorkerId = -1,
}

impl BuiltinWorkerIds {
    pub fn to_worker_id(&self) -> WorkerId {
        WorkerId {
            value: *self as i64,
        }
    }
}
pub mod slack {
    use once_cell::sync::Lazy;
    use proto::jobworkerp::data::{
        worker_operation, QueueType, ResponseType, RetryPolicy, RetryType, RunnerType,
        SlackJobResultOperation, Worker, WorkerData, WorkerOperation,
    };
    pub const SLACK_WORKER_NAME: &str = "__SLACK_NOTIFICATION_WORKER__"; //XXX
    pub const SLACK_RUNNER_OPERATION: WorkerOperation = WorkerOperation {
        operation: Some(worker_operation::Operation::SlackInternal(
            SlackJobResultOperation {},
        )),
    };

    /// treat arg as serialized JobResult
    pub static SLACK_WORKER: Lazy<Worker> = Lazy::new(|| Worker {
        id: Some(super::BuiltinWorkerIds::SlackWorkerId.to_worker_id()),
        data: Some(WorkerData {
            name: SLACK_WORKER_NAME.to_string(),
            r#type: RunnerType::SlackInternal as i32,
            operation: Some(SLACK_RUNNER_OPERATION.clone()),
            channel: None,
            response_type: ResponseType::NoResult as i32,
            periodic_interval: 0,
            retry_policy: Some(RetryPolicy {
                r#type: RetryType::Exponential as i32,
                interval: 1000,
                max_interval: 20000,
                max_retry: 3,
                basis: 2.0,
            }),
            queue_type: QueueType::Redis as i32,
            store_failure: false,
            store_success: false,
            next_workers: vec![],
            use_static: false,
        }),
    });
}

pub trait BuiltinWorkerTrait {
    fn workers_list() -> Vec<Worker> {
        vec![slack::SLACK_WORKER.clone()]
    }

    fn find_worker_by_id(id: &WorkerId) -> Option<Worker> {
        match id.value {
            i if i == BuiltinWorkerIds::SlackWorkerId as i64 => Some(slack::SLACK_WORKER.clone()),
            _ => None,
        }
    }
}

pub struct BuiltinWorker {}
///
/// for static implementation of BuiltInWorkerTrait
impl BuiltinWorkerTrait for BuiltinWorker {}

// create test for BuiltInWorker
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::worker::builtin::slack::{SLACK_RUNNER_OPERATION, SLACK_WORKER};

    #[test]
    fn test_find_worker_by_id() {
        let worker_id = BuiltinWorkerIds::SlackWorkerId.to_worker_id();
        let worker = BuiltinWorker::find_worker_by_id(&worker_id);
        assert!(worker.is_some());
        assert_eq!(worker.clone().unwrap().id.unwrap(), worker_id);
        assert_eq!(
            worker.unwrap().data.unwrap().operation.unwrap(),
            SLACK_RUNNER_OPERATION
        );
    }

    #[test]
    fn test_workers_list() {
        let workers = BuiltinWorker::workers_list();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0], *SLACK_WORKER);
    }
}
