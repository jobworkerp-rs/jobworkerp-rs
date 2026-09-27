use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    time::Duration,
};

use crate::jobworkerp::runner::{
    SandboxAllowedHostMount, SandboxBindMount, SandboxExecArgs, SandboxRunnerSettings,
};
use anyhow::{Context, Result, anyhow, bail, ensure};

#[path = "config/network.rs"]
mod network;

pub use network::{ValidatedNetworkConfig, build_network_policy};
use network::{validate_network_config, validate_network_disabled_override};

const MAX_SDK_CPUS: u32 = u8::MAX as u32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedMount {
    pub host_path: PathBuf,
    pub guest_path: String,
    pub writable: bool,
    pub executable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSandboxVm {
    pub image: String,
    pub cpus: u8,
    pub memory_mib: u32,
    pub root_disk_mib: u32,
    pub env: HashMap<String, String>,
    pub working_dir: Option<String>,
    pub mounts: Vec<ResolvedMount>,
    pub max_duration_sec: Option<u64>,
    pub idle_timeout_sec: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct ValidatedSandboxSettings {
    pub vm: ResolvedSandboxVm,
    pub network: Option<ValidatedNetworkConfig>,
    pub default_exec_timeout_ms: Option<u64>,
    allowed_images: HashSet<String>,
    allowed_host_mounts: Vec<ResolvedAllowedHostMount>,
}

#[derive(Clone, Debug)]
struct ResolvedAllowedHostMount {
    host_root: PathBuf,
    allow_write: bool,
    allow_exec: bool,
}

#[derive(Clone, Debug)]
pub struct ResolvedExecutionSettings {
    pub vm: ResolvedSandboxVm,
    pub network: Option<ValidatedNetworkConfig>,
    pub exec_env: HashMap<String, String>,
    pub exec_working_dir: Option<String>,
    pub exec_user: Option<String>,
    pub exec_timeout: Option<Duration>,
    pub stdin: Option<Vec<u8>>,
    pub tty: bool,
    pub treat_nonzero_as_error: bool,
    pub success_exit_codes: Vec<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxMode {
    Static,
    NonStatic,
}

/// Validate trusted Worker settings before any local VM is created.
pub fn validate_settings(settings: &SandboxRunnerSettings) -> Result<ValidatedSandboxSettings> {
    let vm = settings
        .vm
        .as_ref()
        .ok_or_else(|| anyhow!("SANDBOX settings require vm"))?;
    let image = required_image(vm.image.as_deref(), "worker vm.image")?;
    let mut allowed_images = HashSet::with_capacity(settings.allowed_images.len());
    for image in &settings.allowed_images {
        validate_oci_image(image, "allowed_images")?;
        allowed_images.insert(image.clone());
    }
    ensure!(
        !allowed_images.is_empty(),
        "SANDBOX settings require at least one allowed_images entry"
    );
    ensure!(
        allowed_images.contains(&image),
        "worker vm.image must exactly match an allowed_images entry"
    );

    let cpus = required_resource(vm.cpus, "worker vm.cpus")?;
    ensure!(
        cpus <= MAX_SDK_CPUS,
        "worker vm.cpus must not exceed the microsandbox SDK u8 limit"
    );
    let memory_mib = required_resource(vm.memory_mib, "worker vm.memory_mib")?;
    let root_disk_mib = required_resource(vm.root_disk_mib, "worker vm.root_disk_mib")?;
    validate_env(&vm.env, "worker vm.env")?;
    if let Some(working_dir) = vm.working_dir.as_deref() {
        validate_guest_path(working_dir, "worker vm.working_dir")?;
    }

    let allowed_host_mounts = settings
        .allowed_host_mounts
        .iter()
        .map(validate_allowed_host_mount)
        .collect::<Result<Vec<_>>>()?;
    let mounts = validate_mounts(&vm.mounts, &allowed_host_mounts, "worker vm.mounts")?;
    ensure_unique_guest_mounts(&mounts)?;

    if let Some(timeout) = settings.default_exec_timeout_ms {
        ensure!(timeout > 0, "default_exec_timeout_ms must be positive");
        ensure!(
            std::time::Instant::now()
                .checked_add(Duration::from_millis(timeout))
                .is_some(),
            "default_exec_timeout_ms cannot be represented by the host monotonic clock"
        );
    }

    let network = settings
        .network
        .as_ref()
        .map(validate_network_config)
        .transpose()?
        .flatten();

    Ok(ValidatedSandboxSettings {
        vm: ResolvedSandboxVm {
            image,
            cpus: cpus as u8,
            memory_mib,
            root_disk_mib,
            env: vm.env.clone(),
            working_dir: vm.working_dir.clone(),
            mounts,
            max_duration_sec: validate_optional_positive(
                vm.max_duration_sec,
                "worker vm.max_duration_sec",
            )?,
            idle_timeout_sec: validate_optional_positive(
                vm.idle_timeout_sec,
                "worker vm.idle_timeout_sec",
            )?,
        },
        network,
        default_exec_timeout_ms: settings.default_exec_timeout_ms,
        allowed_images,
        allowed_host_mounts,
    })
}

/// Resolve and authorize job overrides without creating a VM.
pub fn resolve_execution_settings(
    settings: &ValidatedSandboxSettings,
    args: &SandboxExecArgs,
    mode: SandboxMode,
) -> Result<ResolvedExecutionSettings> {
    if mode == SandboxMode::Static {
        ensure!(
            args.vm.is_none() && args.network.is_none(),
            "static SANDBOX jobs cannot override vm or network settings"
        );
    }

    validate_env(&args.env, "job env")?;
    if let Some(user) = args.user.as_deref() {
        ensure!(
            !user.is_empty() && !user.contains('\0'),
            "job user must be non-empty and contain no NUL byte"
        );
    }
    if let Some(working_dir) = args.working_dir.as_deref() {
        validate_guest_path(working_dir, "job working_dir")?;
    }
    if let Some(timeout) = args.timeout_ms {
        ensure!(timeout > 0, "job timeout_ms must be positive");
        ensure!(
            std::time::Instant::now()
                .checked_add(Duration::from_millis(timeout))
                .is_some(),
            "job timeout_ms cannot be represented by the host monotonic clock"
        );
    }

    let mut vm = settings.vm.clone();
    if let Some(override_vm) = args.vm.as_ref() {
        if let Some(image) = override_vm.image.as_deref() {
            validate_oci_image(image, "job vm.image")?;
            ensure!(
                settings.allowed_images.contains(image),
                "job vm.image must exactly match an allowed_images entry"
            );
            vm.image = image.to_string();
        }
        if let Some(cpus) = override_vm.cpus {
            ensure!(cpus > 0, "job vm.cpus must be positive");
            ensure!(
                cpus <= u32::from(settings.vm.cpus),
                "job vm.cpus cannot exceed the worker limit"
            );
            vm.cpus = cpus as u8;
        }
        if let Some(memory_mib) = override_vm.memory_mib {
            ensure!(memory_mib > 0, "job vm.memory_mib must be positive");
            ensure!(
                memory_mib <= settings.vm.memory_mib,
                "job vm.memory_mib cannot exceed the worker limit"
            );
            vm.memory_mib = memory_mib;
        }
        if let Some(root_disk_mib) = override_vm.root_disk_mib {
            ensure!(root_disk_mib > 0, "job vm.root_disk_mib must be positive");
            ensure!(
                root_disk_mib <= settings.vm.root_disk_mib,
                "job vm.root_disk_mib cannot exceed the worker limit"
            );
            vm.root_disk_mib = root_disk_mib;
        }
        if let Some(working_dir) = override_vm.working_dir.as_deref() {
            validate_guest_path(working_dir, "job vm.working_dir")?;
            vm.working_dir = Some(working_dir.to_string());
        }

        validate_env(&override_vm.env, "job vm.env")?;
        reject_worker_env_override(&settings.vm.env, &override_vm.env, "job vm.env")?;
        vm.env.extend(override_vm.env.clone());

        vm.mounts.extend(validate_mounts(
            &override_vm.mounts,
            &settings.allowed_host_mounts,
            "job vm.mounts",
        )?);
        ensure_unique_guest_mounts(&vm.mounts)?;
        vm.max_duration_sec = resolve_lifetime_override(
            settings.vm.max_duration_sec,
            override_vm.max_duration_sec,
            "job vm.max_duration_sec",
        )?;
        vm.idle_timeout_sec = resolve_lifetime_override(
            settings.vm.idle_timeout_sec,
            override_vm.idle_timeout_sec,
            "job vm.idle_timeout_sec",
        )?;
    }

    reject_worker_env_override(&settings.vm.env, &args.env, "job env")?;
    let network = match args.network.as_ref() {
        None => settings.network.clone(),
        Some(job_network) => {
            validate_network_disabled_override(job_network)?;
            None
        }
    };

    let exec_timeout = args
        .timeout_ms
        .or(settings.default_exec_timeout_ms)
        .map(Duration::from_millis);

    Ok(ResolvedExecutionSettings {
        vm,
        network,
        exec_env: args.env.clone(),
        exec_working_dir: args.working_dir.clone(),
        exec_user: args.user.clone(),
        exec_timeout,
        stdin: args.stdin.clone(),
        tty: args.tty.unwrap_or(false),
        treat_nonzero_as_error: args.treat_nonzero_as_error,
        success_exit_codes: args.success_exit_codes.clone(),
    })
}

fn validate_allowed_host_mount(
    mount: &SandboxAllowedHostMount,
) -> Result<ResolvedAllowedHostMount> {
    let root = Path::new(&mount.host_root);
    ensure!(
        root.is_absolute(),
        "allowed host mount root must be absolute"
    );
    let host_root = root
        .canonicalize()
        .with_context(|| format!("allowed host mount root does not exist: {}", root.display()))?;
    Ok(ResolvedAllowedHostMount {
        host_root,
        allow_write: mount.allow_write,
        allow_exec: mount.allow_exec,
    })
}

fn validate_mounts(
    mounts: &[SandboxBindMount],
    allowed: &[ResolvedAllowedHostMount],
    field: &str,
) -> Result<Vec<ResolvedMount>> {
    mounts
        .iter()
        .map(|mount| {
            validate_guest_path(&mount.guest_path, &format!("{field}.guest_path"))?;
            let requested_host_path = Path::new(&mount.host_path);
            ensure!(
                requested_host_path.is_absolute(),
                "{field}.host_path must be absolute"
            );
            let host_path = requested_host_path.canonicalize().with_context(|| {
                format!(
                    "{field}.host_path does not exist: {}",
                    requested_host_path.display()
                )
            })?;
            let matching_roots = allowed
                .iter()
                .filter(|root| host_path.starts_with(&root.host_root))
                .collect::<Vec<_>>();
            ensure!(
                !matching_roots.is_empty(),
                "{field}.host_path is outside allowed_host_mounts"
            );
            ensure!(
                matching_roots.iter().any(|root| {
                    (!mount.writable || root.allow_write) && (!mount.executable || root.allow_exec)
                }),
                "{field} permissions exceed allowed_host_mounts"
            );
            Ok(ResolvedMount {
                host_path,
                guest_path: mount.guest_path.clone(),
                writable: mount.writable,
                executable: mount.executable,
            })
        })
        .collect()
}

fn ensure_unique_guest_mounts(mounts: &[ResolvedMount]) -> Result<()> {
    let mut guests = HashSet::with_capacity(mounts.len());
    for mount in mounts {
        ensure!(
            guests.insert(mount.guest_path.as_str()),
            "duplicate guest mount path: {}",
            mount.guest_path
        );
    }
    Ok(())
}

fn validate_guest_path(path: &str, field: &str) -> Result<()> {
    ensure!(!path.is_empty(), "{field} must not be empty");
    let path_buf = Path::new(path);
    ensure!(path_buf.is_absolute(), "{field} must be absolute");

    let mut normalized = PathBuf::from("/");
    for component in path_buf.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                bail!("{field} must be normalized and cannot contain dot components")
            }
        }
    }
    ensure!(
        normalized.to_string_lossy() == path,
        "{field} must use its normalized absolute path"
    );
    Ok(())
}

fn required_image(image: Option<&str>, field: &str) -> Result<String> {
    let image = image.ok_or_else(|| anyhow!("{field} is required"))?;
    validate_oci_image(image, field)?;
    Ok(image.to_string())
}

fn validate_oci_image(image: &str, field: &str) -> Result<()> {
    ensure!(!image.is_empty(), "{field} must not be empty");
    ensure!(
        image == image.trim()
            && !image.starts_with('/')
            && !image.starts_with("./")
            && !image.starts_with("../")
            && !image.contains('\0')
            && !image.chars().any(char::is_whitespace),
        "{field} must be an OCI image reference, not a local path"
    );
    Ok(())
}

fn required_resource(value: Option<u32>, field: &str) -> Result<u32> {
    value
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow!("{field} is required and must be positive"))
}

fn validate_optional_positive(value: Option<u64>, field: &str) -> Result<Option<u64>> {
    if let Some(value) = value {
        ensure!(value > 0, "{field} must be positive when specified");
    }
    Ok(value)
}

fn validate_env(env: &HashMap<String, String>, field: &str) -> Result<()> {
    for (key, value) in env {
        ensure!(
            !key.is_empty() && !key.contains('=') && !key.contains('\0'),
            "{field} has an invalid environment variable name"
        );
        ensure!(
            !value.contains('\0'),
            "{field} values cannot contain NUL bytes"
        );
    }
    Ok(())
}

fn reject_worker_env_override(
    worker_env: &HashMap<String, String>,
    job_env: &HashMap<String, String>,
    field: &str,
) -> Result<()> {
    for key in job_env.keys() {
        ensure!(
            !worker_env.contains_key(key),
            "{field} cannot override Worker vm.env key {key:?}"
        );
    }
    Ok(())
}

fn resolve_lifetime_override(
    worker_limit: Option<u64>,
    job_limit: Option<u64>,
    field: &str,
) -> Result<Option<u64>> {
    validate_optional_positive(job_limit, field)?;
    if let (Some(limit), Some(job_limit)) = (worker_limit, job_limit) {
        ensure!(job_limit <= limit, "{field} cannot exceed the worker limit");
    }
    Ok(job_limit.or(worker_limit))
}
