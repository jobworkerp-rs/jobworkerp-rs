use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionState {
    Active,
    Isolating,
    Isolated,
}

#[derive(Debug)]
struct SessionEpochState {
    state: SessionState,
    instance_id: i64,
    generation: u64,
    last_heartbeat_success: Instant,
    active_permits: usize,
}

#[derive(Debug)]
struct WorkerInstanceSessionInner {
    state: Mutex<SessionEpochState>,
    permits_changed: Notify,
    instance_timeout: Duration,
    start_timeout: Duration,
}

/// Cloneable worker-local handle. It is deliberately not stored in AppModule:
/// grpc-front must never be able to impersonate a worker execution owner.
#[derive(Debug, Clone)]
pub struct WorkerInstanceSessionHandle(Arc<WorkerInstanceSessionInner>);

/// Provides the worker-local session to dispatcher traits without placing it
/// in the application module shared with grpc-front.
pub trait UseWorkerInstanceSession {
    fn worker_instance_session(&self) -> Option<&WorkerInstanceSessionHandle>;
}

/// A permit prevents an isolated generation from beginning a runner execution.
#[derive(Debug)]
pub struct ExecutionStartPermit {
    session: WorkerInstanceSessionHandle,
    instance_id: i64,
    generation: u64,
    deadline: Instant,
    released: bool,
}

impl WorkerInstanceSessionHandle {
    pub fn new(instance_id: i64, instance_timeout: Duration, start_timeout: Duration) -> Self {
        Self(Arc::new(WorkerInstanceSessionInner {
            state: Mutex::new(SessionEpochState {
                state: SessionState::Active,
                instance_id,
                generation: 0,
                last_heartbeat_success: Instant::now(),
                active_permits: 0,
            }),
            permits_changed: Notify::new(),
            instance_timeout,
            start_timeout,
        }))
    }

    pub fn record_heartbeat_success(&self) {
        let mut state = self
            .0
            .state
            .lock()
            .expect("worker instance session mutex poisoned");
        if state.state == SessionState::Active {
            state.last_heartbeat_success = Instant::now();
        }
    }

    pub fn acquire_start_permit(&self) -> Option<ExecutionStartPermit> {
        let mut state = self
            .0
            .state
            .lock()
            .expect("worker instance session mutex poisoned");
        if state.state != SessionState::Active
            || state.last_heartbeat_success.elapsed() >= self.0.instance_timeout
        {
            state.state = SessionState::Isolating;
            return None;
        }
        state.active_permits += 1;
        Some(ExecutionStartPermit {
            session: self.clone(),
            instance_id: state.instance_id,
            generation: state.generation,
            deadline: Instant::now() + self.0.start_timeout,
            released: false,
        })
    }

    /// Stop new starts. Existing permits are allowed only until their own
    /// deadline, and callers must await `wait_until_isolated` before rejoining.
    pub fn begin_isolation(&self) {
        let mut state = self
            .0
            .state
            .lock()
            .expect("worker instance session mutex poisoned");
        if state.state == SessionState::Active {
            state.state = SessionState::Isolating;
        }
    }

    pub async fn wait_until_isolated(&self) {
        loop {
            let notified = self.0.permits_changed.notified();
            {
                let mut state = self
                    .0
                    .state
                    .lock()
                    .expect("worker instance session mutex poisoned");
                if state.active_permits == 0 {
                    state.state = SessionState::Isolated;
                    return;
                }
            }
            notified.await;
        }
    }
}

impl ExecutionStartPermit {
    pub fn instance_id(&self) -> i64 {
        self.instance_id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Recheck mutable session state immediately before invoking a runner.
    pub fn confirm_start(&self) -> bool {
        let state = self
            .session
            .0
            .state
            .lock()
            .expect("worker instance session mutex poisoned");
        state.state == SessionState::Active
            && state.generation == self.generation
            && state.last_heartbeat_success.elapsed() < self.session.0.instance_timeout
            && Instant::now() < self.deadline
    }
}

impl Drop for ExecutionStartPermit {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let mut state = self
            .session
            .0
            .state
            .lock()
            .expect("worker instance session mutex poisoned");
        state.active_permits = state.active_permits.saturating_sub(1);
        drop(state);
        self.session.0.permits_changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permit_rechecks_isolation_before_runner_start() {
        let session =
            WorkerInstanceSessionHandle::new(7, Duration::from_secs(10), Duration::from_secs(1));
        let permit = session.acquire_start_permit().expect("active permit");
        assert!(permit.confirm_start());
        session.begin_isolation();
        assert!(!permit.confirm_start());
    }

    #[tokio::test]
    async fn isolation_waits_for_permit_without_holding_mutex() {
        let session =
            WorkerInstanceSessionHandle::new(7, Duration::from_secs(10), Duration::from_secs(1));
        let permit = session.acquire_start_permit().expect("active permit");
        session.begin_isolation();
        let waiter = tokio::spawn({
            let session = session.clone();
            async move { session.wait_until_isolated().await }
        });
        drop(permit);
        waiter.await.expect("isolation task");
        assert!(session.acquire_start_permit().is_none());
    }
}
