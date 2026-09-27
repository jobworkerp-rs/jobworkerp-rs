use std::{
    collections::HashMap, future::Future, panic::AssertUnwindSafe, pin::Pin, sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use command_utils::util::shutdown::ShutdownLock;
use futures::FutureExt;
use tokio::{
    runtime::Handle,
    sync::{Mutex, watch},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const CLEANUP_TASK_TIMEOUT: Duration = Duration::from_secs(30);

pub type SandboxCleanupFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

struct CleanupEntry {
    lock: ShutdownLock,
    cancellation: Option<CancellationToken>,
    cleanup: Option<SandboxCleanupFuture>,
    running: bool,
}

#[derive(Default)]
struct RegistryState {
    shutdown_started: bool,
    entries: HashMap<Uuid, CleanupEntry>,
}

struct RegistryInner {
    handle: Handle,
    state: Mutex<RegistryState>,
    base_lock: Mutex<Option<ShutdownLock>>,
}

/// Coordinates SANDBOX executions and VM cleanup with the process shutdown lock.
#[derive(Clone)]
pub struct SandboxCleanupRegistry {
    inner: Arc<RegistryInner>,
}

impl std::fmt::Debug for SandboxCleanupRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxCleanupRegistry")
            .finish_non_exhaustive()
    }
}

impl SandboxCleanupRegistry {
    /// Creates a registry and starts observing the process shutdown signal.
    pub fn new(lock: ShutdownLock, mut shutdown: watch::Receiver<bool>) -> Result<Self> {
        let handle = Handle::try_current()
            .context("SANDBOX cleanup registry requires an active Tokio runtime")?;
        let registry = Self {
            inner: Arc::new(RegistryInner {
                handle: handle.clone(),
                state: Mutex::new(RegistryState::default()),
                base_lock: Mutex::new(Some(lock)),
            }),
        };
        let observer = registry.clone();
        handle.spawn(async move {
            loop {
                if *shutdown.borrow() {
                    observer.shutdown().await;
                    break;
                }
                if shutdown.changed().await.is_err() {
                    observer.shutdown().await;
                    break;
                }
            }
        });
        Ok(registry)
    }

    /// Registers work before VM creation so shutdown cannot pass it unnoticed.
    pub async fn register_task(
        &self,
        cancellation: Option<CancellationToken>,
    ) -> Result<SandboxCleanupRegistration> {
        let mut state = self.inner.state.lock().await;
        if state.shutdown_started {
            return Err(anyhow!("SANDBOX cleanup registry is shutting down"));
        }
        let lock = self
            .inner
            .base_lock
            .lock()
            .await
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("SANDBOX cleanup registry is shutting down"))?;
        let id = Uuid::new_v4();
        state.entries.insert(
            id,
            CleanupEntry {
                lock,
                cancellation,
                cleanup: None,
                running: false,
            },
        );
        Ok(SandboxCleanupRegistration {
            inner: Arc::downgrade(&self.inner),
            id,
            finished: false,
        })
    }

    /// Cancels registered work and starts every cleanup task without waiting for completion.
    /// Cleanup locks remain held until each bounded cleanup task exits.
    pub async fn shutdown(&self) {
        let mut state = self.inner.state.lock().await;
        state.shutdown_started = true;
        for entry in state.entries.values() {
            if let Some(cancellation) = &entry.cancellation {
                cancellation.cancel();
            }
        }
        let ids: Vec<_> = state.entries.keys().copied().collect();
        for id in ids {
            launch_cleanup(&self.inner, &mut state, id);
        }
        self.inner.base_lock.lock().await.take();
    }

    pub async fn is_shutting_down(&self) -> bool {
        self.inner.state.lock().await.shutdown_started
    }
}

/// One registered execution or owned VM whose lock remains held through cleanup.
pub struct SandboxCleanupRegistration {
    inner: std::sync::Weak<RegistryInner>,
    id: Uuid,
    finished: bool,
}

impl SandboxCleanupRegistration {
    /// Installs the cleanup future after a VM identity becomes available.
    pub async fn set_cleanup(&mut self, cleanup: SandboxCleanupFuture) -> Result<()> {
        let inner = self
            .inner
            .upgrade()
            .ok_or_else(|| anyhow!("SANDBOX cleanup registry was dropped"))?;
        let mut state = inner.state.lock().await;
        let entry = state
            .entries
            .get_mut(&self.id)
            .ok_or_else(|| anyhow!("SANDBOX cleanup registration is no longer active"))?;
        if entry.cleanup.is_some() || entry.running {
            return Err(anyhow!("SANDBOX cleanup action was already installed"));
        }
        entry.cleanup = Some(cleanup);
        if state.shutdown_started {
            launch_cleanup(&inner, &mut state, self.id);
        }
        Ok(())
    }

    /// Releases a registration after work failed before it acquired a VM.
    pub fn finish(mut self) {
        self.finished = true;
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let id = self.id;
        let handle = inner.handle.clone();
        handle.spawn(async move {
            inner.state.lock().await.entries.remove(&id);
        });
    }

    /// Starts cleanup after normal completion without marking the exec cancelled.
    pub fn start_cleanup(mut self) {
        self.finished = true;
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let id = self.id;
        let handle = inner.handle.clone();
        handle.spawn(async move {
            let mut state = inner.state.lock().await;
            launch_cleanup(&inner, &mut state, id);
        });
    }
}

impl Drop for SandboxCleanupRegistration {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let id = self.id;
        let handle = inner.handle.clone();
        handle.spawn(async move {
            let mut state = inner.state.lock().await;
            if state
                .entries
                .get(&id)
                .is_some_and(|entry| entry.cleanup.is_none() && !entry.running)
            {
                // A dropped pre-VM registration has no cleanup to await. Late
                // VM creation retains its registration in its own task instead.
                state.entries.remove(&id);
                return;
            }
            if let Some(cancellation) = state
                .entries
                .get(&id)
                .and_then(|entry| entry.cancellation.as_ref())
            {
                cancellation.cancel();
            }
            launch_cleanup(&inner, &mut state, id);
        });
    }
}

fn launch_cleanup(inner: &Arc<RegistryInner>, state: &mut RegistryState, id: Uuid) {
    let Some(entry) = state.entries.get_mut(&id) else {
        return;
    };
    if entry.running {
        return;
    }
    let Some(cleanup) = entry.cleanup.take() else {
        return;
    };
    entry.running = true;
    let lock = entry.lock.clone();
    let inner = inner.clone();
    let handle = inner.handle.clone();
    handle.spawn(async move {
        match AssertUnwindSafe(tokio::time::timeout(CLEANUP_TASK_TIMEOUT, cleanup))
            .catch_unwind()
            .await
        {
            Ok(Ok(())) => {}
            Ok(Err(_)) => tracing::error!(
                registration_id = %id,
                timeout = ?CLEANUP_TASK_TIMEOUT,
                "SANDBOX cleanup task exceeded its deadline"
            ),
            Err(_) => tracing::error!(
                registration_id = %id,
                "SANDBOX cleanup task panicked"
            ),
        }
        drop(lock);
        inner.state.lock().await.entries.remove(&id);
    });
}
