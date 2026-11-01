//! Test-only synchronization for verifying queue visibility follows stream subscription.

use proto::jobworkerp::data::JobId;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use tokio::sync::oneshot;

type GateChannels = (oneshot::Sender<()>, oneshot::Receiver<()>);

static STREAM_SUBSCRIPTION_GATES: OnceLock<Mutex<HashMap<i64, GateChannels>>> = OnceLock::new();

// A panicking test must not disable the gates of the tests that follow it.
fn gates() -> MutexGuard<'static, HashMap<i64, GateChannels>> {
    STREAM_SUBSCRIPTION_GATES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Holds one job's result-stream subscription after its receiver is registered,
/// so a test can observe the state before the subscribing call returns.
pub struct ResultStreamSubscriptionGate {
    job_id: i64,
    blocked: Option<oneshot::Receiver<()>>,
    release: Option<oneshot::Sender<()>>,
}

impl ResultStreamSubscriptionGate {
    pub fn install(job_id: JobId) -> anyhow::Result<Self> {
        let (blocked_tx, blocked) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let mut gates = gates();
        if gates.contains_key(&job_id.value) {
            anyhow::bail!(
                "a result stream test gate already exists for job {}",
                job_id.value
            );
        }
        gates.insert(job_id.value, (blocked_tx, release_rx));
        Ok(Self {
            job_id: job_id.value,
            blocked: Some(blocked),
            release: Some(release),
        })
    }

    /// Wait until the subscription is registered and held at the gate.
    pub async fn wait_until_blocked(&mut self) -> anyhow::Result<()> {
        self.blocked
            .take()
            .ok_or_else(|| anyhow::anyhow!("result stream test gate was already reached"))?
            .await
            .map_err(|_| anyhow::anyhow!("result stream subscription ended before gate"))
    }

    pub fn release(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

impl Drop for ResultStreamSubscriptionGate {
    fn drop(&mut self) {
        gates().remove(&self.job_id);
        self.release();
    }
}

/// Called by subscribers after registering their receiver; blocks only while a
/// gate is installed for `job_id`.
pub(crate) async fn hold_stream_subscription(job_id: &JobId) {
    let Some((blocked_tx, release_rx)) = gates().remove(&job_id.value) else {
        return;
    };
    let _ = blocked_tx.send(());
    let _ = release_rx.await;
}
