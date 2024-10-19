pub mod slack;

use once_cell::sync::Lazy;
pub static SLACK_RUNNER: Lazy<slack::SlackResultNotificationRunner> =
    Lazy::new(slack::SlackResultNotificationRunner::new);
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
    use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
    use slack::{SLACK_RUNNER_OPERATION, SLACK_WORKER};

    use super::*;

    #[test]
    fn test_find_worker_by_id() {
        let worker_id = BuiltinWorkerIds::SlackWorkerId.to_worker_id();
        let worker = BuiltinWorker::find_worker_by_id(&worker_id);
        assert!(worker.is_some());
        assert_eq!(worker.clone().unwrap().id.unwrap(), worker_id);
        assert_eq!(
            worker.unwrap().data.unwrap().operation,
            JobqueueAndCodec::serialize_message(&SLACK_RUNNER_OPERATION)
        );
    }

    #[test]
    fn test_workers_list() {
        let workers = BuiltinWorker::workers_list();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0], *SLACK_WORKER);
    }
}
