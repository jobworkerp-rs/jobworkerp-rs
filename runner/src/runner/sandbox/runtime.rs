use std::{
    future::Future,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use microsandbox::sandbox::{DestroyOptions, MountBuilder, SandboxHandle, SandboxStatus};
use microsandbox::{
    Backend, BackendKind, ExecControl, ExecHandle, LocalBackend, Sandbox, with_backend,
};
use tokio::sync::Mutex;

use super::{
    SandboxCleanupRegistration,
    config::{ResolvedExecutionSettings, ResolvedMount, ResolvedSandboxVm},
};

const AGENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CLEANUP_DESTROY_TIMEOUT: Duration = Duration::from_secs(2);
const CLEANUP_KILL_TIMEOUT: Duration = Duration::from_secs(3);
const CLEANUP_REMOVE_TIMEOUT: Duration = Duration::from_secs(2);

static LOCAL_BACKEND: OnceLock<Arc<dyn Backend>> = OnceLock::new();

/// A single local microsandbox VM generation owned until the last stream or Runner releases it.
pub(super) struct SandboxRuntime {
    name: String,
    stable_id: String,
    backend: Arc<dyn Backend>,
    process_owners: Mutex<Vec<Sandbox>>,
    cleanup_finished: Arc<tokio::sync::OnceCell<()>>,
    registry_managed: AtomicBool,
    registry_registration: Mutex<Option<SandboxCleanupRegistration>>,
}

impl std::fmt::Debug for SandboxRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxRuntime")
            .field("name", &self.name)
            .field("stable_id", &self.stable_id)
            .finish_non_exhaustive()
    }
}

impl SandboxRuntime {
    pub async fn create(
        name: String,
        settings: &ResolvedSandboxVm,
        network: Option<&super::config::ValidatedNetworkConfig>,
    ) -> Result<Arc<Self>> {
        let backend = local_backend()?;
        ensure!(
            backend.kind() == BackendKind::Local,
            "SANDBOX Runner requires the local microsandbox backend"
        );
        let mut builder = Sandbox::builder(name.clone())
            .image_with(|image| {
                image
                    .oci(settings.image.clone())
                    .root_disk(settings.root_disk_mib)
            })
            .cpus(settings.cpus)
            .memory(settings.memory_mib)
            .ephemeral(false);

        if let Some(working_dir) = &settings.working_dir {
            builder = builder.workdir(working_dir.clone());
        }
        builder = builder.envs(
            settings
                .env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        for mount in &settings.mounts {
            builder = builder.add_volume_mount(sdk_mount(mount)?);
        }
        if let Some(max_duration) = settings.max_duration_sec {
            builder = builder.max_duration(max_duration);
        }
        if let Some(idle_timeout) = settings.idle_timeout_sec {
            builder = builder.idle_timeout(idle_timeout);
        }

        builder = match network {
            Some(network) => builder.network(|config| {
                config
                    .enabled(true)
                    .policy(network.policy.clone())
                    .strict(true)
                    .max_tcp_connections(network.max_tcp_connections)
                    .max_udp_connections(network.max_udp_connections)
                    .tls(|tls| tls.enabled(false))
            }),
            None => builder.disable_network(),
        };

        let sandbox = with_backend(backend.clone(), builder.create())
            .await
            .context("failed to create local microsandbox VM")?;
        ensure!(
            sandbox.backend_kind() == BackendKind::Local,
            "microsandbox did not create the requested local VM"
        );
        let stable_id = sandbox.id().to_string();
        Ok(Arc::new(Self {
            name,
            stable_id,
            backend,
            process_owners: Mutex::new(vec![sandbox]),
            cleanup_finished: Arc::new(tokio::sync::OnceCell::new()),
            registry_managed: AtomicBool::new(false),
            registry_registration: Mutex::new(None),
        }))
    }

    pub fn stable_id(&self) -> &str {
        &self.stable_id
    }

    pub async fn verify_connection(&self) -> Result<()> {
        let sandbox = self.connect_or_start().await?;
        tokio::time::timeout(AGENT_CONNECT_TIMEOUT, sandbox.ping())
            .await
            .context("timed out checking local sandbox agent")?
            .context("local sandbox agent ping failed")?;
        Ok(())
    }

    pub async fn cleanup_now(&self) {
        cleanup_sandbox_once(
            self.backend.clone(),
            self.name.clone(),
            self.stable_id.clone(),
            self.cleanup_finished.clone(),
        )
        .await;
    }

    pub async fn install_cleanup_registration(
        &self,
        mut registration: SandboxCleanupRegistration,
    ) -> Result<()> {
        if let Err(error) = registration
            .set_cleanup(Box::pin(self.cleanup_action()))
            .await
        {
            registration.finish();
            return Err(error);
        }
        self.registry_managed.store(true, Ordering::Release);
        *self.registry_registration.lock().await = Some(registration);
        Ok(())
    }

    pub(super) fn mark_registry_managed(&self) {
        self.registry_managed.store(true, Ordering::Release);
    }

    pub(super) fn cleanup_action(&self) -> impl Future<Output = ()> + Send + 'static {
        let backend = self.backend.clone();
        let name = self.name.clone();
        let stable_id = self.stable_id.clone();
        let cleanup_finished = self.cleanup_finished.clone();
        async move { cleanup_sandbox_once(backend, name, stable_id, cleanup_finished).await }
    }

    pub async fn start_exec(
        &self,
        command: &str,
        args: &[String],
        settings: &ResolvedExecutionSettings,
        client_stream: bool,
    ) -> Result<(
        ExecHandle,
        ExecControl,
        Option<microsandbox::sandbox::exec::ExecSink>,
    )> {
        let sandbox = self.connect_or_start().await?;
        let mut handle = sandbox
            .exec_stream_with(command.to_string(), |options| {
                let options = options.args(args.iter().cloned()).envs(
                    settings
                        .exec_env
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone())),
                );
                let options = if let Some(working_dir) = &settings.exec_working_dir {
                    options.cwd(working_dir.clone())
                } else {
                    options
                };
                let options = if let Some(user) = &settings.exec_user {
                    options.user(user.clone())
                } else {
                    options
                };
                let options = if let Some(timeout) = settings.exec_timeout {
                    options.timeout(timeout)
                } else {
                    options
                };
                if client_stream {
                    options.stdin_pipe().tty(true)
                } else {
                    match settings.stdin.as_ref() {
                        Some(stdin) => options.stdin_bytes(stdin.clone()),
                        None => options.stdin_null(),
                    }
                    .tty(settings.tty)
                }
            })
            .await
            .context("failed to hand off command to the guest agent")?;
        let control = handle.control();
        let stdin = handle.take_stdin();
        if client_stream && stdin.is_none() {
            let _ = bounded(CLEANUP_KILL_TIMEOUT, control.kill()).await;
            bail!("microsandbox did not provide the requested client stdin pipe");
        }
        Ok((handle, control, stdin))
    }

    async fn connect_or_start(&self) -> Result<Sandbox> {
        let lookup = bounded(
            AGENT_CONNECT_TIMEOUT,
            with_backend(self.backend.clone(), Sandbox::get(&self.name)),
        )
        .await
        .context("timed out looking up local microsandbox VM")??;
        ensure!(
            lookup.id().as_str() == self.stable_id,
            "sandbox name {} now refers to a replacement VM",
            self.name
        );

        let current = match lookup.status_snapshot() {
            SandboxStatus::Running => lookup
                .connect_with_timeout(AGENT_CONNECT_TIMEOUT)
                .await
                .context("failed to connect to running sandbox")?,
            SandboxStatus::Created | SandboxStatus::Stopped | SandboxStatus::Crashed => lookup
                .start()
                .await
                .context("failed to start stopped sandbox")?,
            SandboxStatus::Starting => lookup
                .connect_or_start()
                .await
                .context("failed waiting for sandbox startup")?,
            SandboxStatus::Draining => {
                bail!(
                    "sandbox {} is draining and cannot accept new commands",
                    self.name
                )
            }
            SandboxStatus::Paused => {
                bail!(
                    "sandbox {} is paused and cannot accept new commands",
                    self.name
                )
            }
        };
        ensure!(
            current.id().as_str() == self.stable_id,
            "sandbox name {} resolved to a replacement VM",
            self.name
        );
        if current.owns_lifecycle() {
            let mut owners = self.process_owners.lock().await;
            // Keep the initial owner and the current attached process, not one
            // handle per command in a long-lived static pool.
            if owners.len() > 1 {
                owners.truncate(1);
            }
            owners.push(current.clone());
        }
        Ok(current)
    }
}

impl Drop for SandboxRuntime {
    fn drop(&mut self) {
        if self.registry_registration.get_mut().take().is_some() {
            return;
        }
        if self.registry_managed.load(Ordering::Acquire) || self.cleanup_finished.get().is_some() {
            return;
        }
        let backend = self.backend.clone();
        let name = self.name.clone();
        let stable_id = self.stable_id.clone();
        let cleanup_finished = self.cleanup_finished.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    cleanup_sandbox_once(backend, name, stable_id, cleanup_finished).await;
                });
            }
            Err(error) => {
                tracing::error!(
                    sandbox = %self.name,
                    stable_id = %self.stable_id,
                    error = %error,
                    "no Tokio runtime available to clean up local microsandbox"
                );
            }
        }
    }
}

async fn cleanup_sandbox_once(
    backend: Arc<dyn Backend>,
    name: String,
    stable_id: String,
    cleanup_finished: Arc<tokio::sync::OnceCell<()>>,
) {
    run_cleanup_once(&cleanup_finished, || {
        cleanup_sandbox(backend, name, stable_id)
    })
    .await;
}

async fn run_cleanup_once<F, Fut>(finished: &tokio::sync::OnceCell<()>, cleanup: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    finished.get_or_init(cleanup).await;
}

fn local_backend() -> Result<Arc<dyn Backend>> {
    if let Some(backend) = LOCAL_BACKEND.get() {
        return Ok(backend.clone());
    }
    let backend: Arc<dyn Backend> = Arc::new(LocalBackend::lazy()?);
    let _ = LOCAL_BACKEND.set(backend.clone());
    Ok(LOCAL_BACKEND.get().cloned().unwrap_or(backend))
}

fn sdk_mount(mount: &ResolvedMount) -> Result<microsandbox::sandbox::VolumeMount> {
    let mut builder = MountBuilder::new(mount.guest_path.clone())
        .bind(mount.host_path.clone())
        .nosuid()
        .nodev();
    if !mount.writable {
        builder = builder.readonly();
    }
    if !mount.executable {
        builder = builder.noexec();
    }
    builder
        .build()
        .context("failed to build validated sandbox bind mount")
}

async fn cleanup_sandbox(backend: Arc<dyn Backend>, name: String, stable_id: String) {
    match exact_handle(&backend, &name, &stable_id).await {
        Ok(Some(handle)) => {
            match bounded(
                CLEANUP_DESTROY_TIMEOUT + Duration::from_secs(1),
                handle.destroy_with(DestroyOptions {
                    force: false,
                    timeout: CLEANUP_DESTROY_TIMEOUT,
                }),
            )
            .await
            {
                Ok(Ok(())) => {
                    tracing::debug!(sandbox = %name, stable_id = %stable_id, "sandbox destroyed");
                    return;
                }
                Ok(Err(error)) => tracing::warn!(
                    sandbox = %name,
                    stable_id = %stable_id,
                    error = %error,
                    "graceful sandbox cleanup failed; escalating to local kill"
                ),
                Err(error) => tracing::warn!(
                    sandbox = %name,
                    stable_id = %stable_id,
                    error = %error,
                    "graceful sandbox cleanup timed out; escalating to local kill"
                ),
            }
        }
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(
                sandbox = %name,
                stable_id = %stable_id,
                error = %error,
                "cannot verify sandbox identity before cleanup"
            );
            return;
        }
    }

    let handle = match exact_handle(&backend, &name, &stable_id).await {
        Ok(Some(handle)) => handle,
        Ok(None) => return,
        Err(error) => {
            tracing::error!(
                sandbox = %name,
                stable_id = %stable_id,
                error = %error,
                "cannot verify sandbox identity before kill"
            );
            return;
        }
    };
    if let Err(error) = bounded(
        CLEANUP_KILL_TIMEOUT,
        handle.kill_with_timeout(CLEANUP_KILL_TIMEOUT),
    )
    .await
    {
        tracing::error!(
            sandbox = %name,
            stable_id = %stable_id,
            error = %error,
            "bounded sandbox kill failed"
        );
        return;
    }

    let handle = match exact_handle(&backend, &name, &stable_id).await {
        Ok(Some(handle)) => handle,
        Ok(None) => return,
        Err(error) => {
            tracing::error!(
                sandbox = %name,
                stable_id = %stable_id,
                error = %error,
                "cannot verify sandbox identity before remove"
            );
            return;
        }
    };
    match bounded(CLEANUP_REMOVE_TIMEOUT, handle.remove()).await {
        Ok(Ok(())) => {
            tracing::debug!(sandbox = %name, stable_id = %stable_id, "sandbox record removed")
        }
        Ok(Err(error)) => tracing::error!(
            sandbox = %name,
            stable_id = %stable_id,
            error = %error,
            "sandbox stopped but persisted record could not be removed"
        ),
        Err(error) => tracing::error!(
            sandbox = %name,
            stable_id = %stable_id,
            error = %error,
            "sandbox record removal timed out"
        ),
    }
}

async fn exact_handle(
    backend: &Arc<dyn Backend>,
    name: &str,
    stable_id: &str,
) -> Result<Option<SandboxHandle>> {
    let lookup = bounded(
        CLEANUP_REMOVE_TIMEOUT,
        with_backend(backend.clone(), Sandbox::get(name)),
    )
    .await
    .context("timed out looking up sandbox for cleanup")?;
    let handle = match lookup {
        Ok(handle) => handle,
        Err(error) => {
            return Err(error).context("sandbox lookup for cleanup failed");
        }
    };
    ensure!(
        handle.id().as_str() == stable_id,
        "sandbox name {name} was reused; refusing to remove replacement {}",
        handle.id()
    );
    Ok(Some(handle))
}

async fn bounded<F: Future>(duration: Duration, future: F) -> Result<F::Output> {
    tokio::time::timeout(duration, future)
        .await
        .map_err(|_| anyhow!("operation exceeded its {duration:?} deadline"))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use tokio::sync::{Notify, OnceCell};

    use super::run_cleanup_once;

    #[tokio::test]
    async fn simultaneous_cleanup_callers_wait_for_the_single_cleanup_to_finish() {
        let finished = Arc::new(OnceCell::new());
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));

        let first = {
            let (finished, started, release, calls) = (
                finished.clone(),
                started.clone(),
                release.clone(),
                calls.clone(),
            );
            tokio::spawn(async move {
                run_cleanup_once(&finished, || async move {
                    calls.fetch_add(1, Ordering::AcqRel);
                    started.notify_one();
                    release.notified().await;
                })
                .await;
            })
        };
        started.notified().await;
        let second = {
            let (finished, calls) = (finished.clone(), calls.clone());
            tokio::spawn(async move {
                run_cleanup_once(&finished, || async move {
                    calls.fetch_add(1, Ordering::AcqRel);
                })
                .await;
            })
        };
        tokio::task::yield_now().await;
        assert!(
            !second.is_finished(),
            "a second caller must wait for the actual cleanup"
        );
        release.notify_one();
        first.await.unwrap();
        second.await.unwrap();
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }
}
