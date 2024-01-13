pub mod slack;

use app::app::worker::builtin::slack::SLACK_WORKER;
use once_cell::sync::Lazy;

use super::super::Runner;

/// arg: serialized JobResult
static SLACK_RUNNER: Lazy<slack::SlackResultNotificationRunner> =
    Lazy::new(slack::SlackResultNotificationRunner::new);

pub trait BuiltinRunnerTrait {
    fn find_runner_by_operation(
        worker_operation: &String,
    ) -> Option<Box<dyn Runner + Send + Sync>> {
        match worker_operation {
            str if str == &SLACK_WORKER.data.as_ref().unwrap().operation => {
                Some(Box::new(SLACK_RUNNER.clone()) as Box<dyn Runner + Send + Sync>)
            }
            _ => {
                tracing::error!("worker operation not found: {}", worker_operation);
                None
            }
        }
    }
}
pub struct BuiltinRunner {}
impl BuiltinRunnerTrait for BuiltinRunner {}

// create test for BuiltInWorker
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_find_runner_by_operation() {
        let runner =
            BuiltinRunner::find_runner_by_operation(&SLACK_WORKER.data.as_ref().unwrap().operation)
                .unwrap();
        assert_eq!(runner.name().await, SLACK_RUNNER.name().await);
    }
}
