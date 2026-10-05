//! Opt-in end-to-end check of the pinned microsandbox runtime through the ordinary SANDBOX runner.

#![recursion_limit = "256"]

use std::{
    ffi::CString,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use command_utils::util::shutdown::{ShutdownWait, create_lock_and_wait};
use futures::StreamExt;
use jobworkerp_runner::{
    jobworkerp::runner::{
        SandboxExecArgs, SandboxExecResult, SandboxNetworkConfig, SandboxRunnerSettings,
        SandboxVmConfig, sandbox_exec_result::Result as SandboxResult,
    },
    runner::{
        RunnerTrait,
        sandbox::{SandboxCleanupRegistry, SandboxExecutionContext, SandboxRunner},
    },
};
use prost::Message;
use proto::jobworkerp::data::{JobId, WorkerId, result_output_item::Item};
use tokio::sync::watch;

const CHILD_GATE_ENV: &str = "SANDBOX_PINNED_RUNTIME_KVM_CHILD";
const HOME_ENV: &str = "SANDBOX_PINNED_RUNTIME_KVM_HOME";
const SDK_HOME_ENV: &str = "SANDBOX_PINNED_RUNTIME_KVM_SDK_HOME";
const IMAGE_ENV: &str = "SANDBOX_MANAGED_KVM_IMAGE";
const GATE_ROOT: &str = "/tmp/opencode/coding-agent-msb-076-gate";
const ASSET_DIR_DEFAULT: &str = "/tmp/opencode/coding-agent-msb-076-assets";
const DOCKER_CONFIG: &str = "/tmp/opencode/coding-agent-docker-config";
const ROOT_DISK_MIB: u32 = 20_480;
const IMAGE_IMPORT_MARGIN: u64 = 2 * 1024 * 1024 * 1024;

struct RuntimeAsset {
    source_name: &'static str,
    destination: &'static str,
    sha256: &'static str,
    size: u64,
}

const PINNED_RUNTIME_ASSETS: [RuntimeAsset; 3] = [
    RuntimeAsset {
        source_name: "msb-linux-x86_64",
        destination: "bin/msb",
        sha256: "e5baba0cbc6628a39e12e297729dfa1b137f3e7ce13a3fa9cf5fd22e198cab9c",
        size: 47_788_216,
    },
    RuntimeAsset {
        source_name: "libkrunfw-linux-x86_64.so",
        destination: "lib/libkrunfw.so.5.6.1",
        sha256: "ce9a749e8471e89aa5e2ad88de0c1581c3384c100bcb107a75bb12739a12d590",
        size: 21_628_880,
    },
    RuntimeAsset {
        source_name: "agentd-x86_64",
        destination: "bin/agentd",
        sha256: "78bb21c3bf16f195068c946ce72419d242fa2b3f1bebd2c339cc8e58dd79847d",
        size: 2_515_904,
    },
];

fn approved_image() -> anyhow::Result<String> {
    let image = std::env::var(IMAGE_ENV).map_err(|_| {
        anyhow::anyhow!("set {IMAGE_ENV} to the operator-approved immutable image digest")
    })?;
    let digest = image
        .rsplit_once("@sha256:")
        .map(|(_, digest)| digest)
        .ok_or_else(|| {
            anyhow::anyhow!("the KVM gate image must be configured by immutable digest")
        })?;
    anyhow::ensure!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{IMAGE_ENV} must end in a 64-digit SHA-256 digest"
    );
    Ok(image)
}

fn uid() -> u32 {
    // SAFETY: geteuid has no preconditions and does not access Rust-managed memory.
    unsafe { nix::libc::geteuid() }
}

fn ensure_private_gate_directory(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_private_directory(path, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new().mode(0o700).create(path)?;
            let metadata = fs::symlink_metadata(path)?;
            validate_private_directory(path, &metadata)
        }
        Err(error) => Err(error.into()),
    }
}

fn validate_private_directory(path: &Path, metadata: &fs::Metadata) -> anyhow::Result<()> {
    anyhow::ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == uid()
            && metadata.mode() & 0o777 == 0o700,
        "private KVM test directory must be a real, UID-owned mode-0700 directory: {}",
        path.display()
    );
    Ok(())
}

fn gate_work_root() -> anyhow::Result<PathBuf> {
    let path = Path::new(GATE_ROOT);
    let parent = path.parent().expect("fixed gate root has a parent");
    let metadata = fs::symlink_metadata(parent)?;
    anyhow::ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == uid()
            && metadata.mode() & 0o022 == 0,
        "KVM gate parent must be a real, UID-owned, non-writable directory: {}",
        parent.display()
    );
    ensure_private_gate_directory(path)?;
    Ok(path.to_path_buf())
}

fn create_short_test_home() -> anyhow::Result<(PathBuf, PathBuf)> {
    let base = Path::new("/tmp/opencode");
    let base_metadata = fs::symlink_metadata(base)?;
    anyhow::ensure!(
        base_metadata.is_dir()
            && !base_metadata.file_type().is_symlink()
            && base_metadata.uid() == uid()
            && base_metadata.mode() & 0o022 == 0,
        "short SDK-home parent must be a real, UID-owned, non-writable directory"
    );

    let process_id = std::process::id();
    let mut root = None;
    for suffix in 0_u8..=u8::MAX {
        let candidate = base.join(format!("m{process_id:08x}{suffix:02x}"));
        match fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => {
                root = Some(candidate);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let root =
        root.ok_or_else(|| anyhow::anyhow!("could not allocate a fresh short SDK test home"))?;
    validate_private_directory(&root, &fs::symlink_metadata(&root)?)?;

    let mut current = root.clone();
    for component in [".local", "share", "lookback", "msb"] {
        current.push(component);
        ensure_private_gate_directory(&current)?;
    }
    let sdk_home = current;
    validate_socket_path_lengths(&sdk_home)?;
    Ok((root, sdk_home))
}

fn validate_socket_path_lengths(sdk_home: &Path) -> anyhow::Result<()> {
    let agent = sdk_home
        .join("run/sandboxes")
        .join("0".repeat(24))
        .join("agent.sock");
    let control = agent.with_file_name("control.sock");
    // SAFETY: sockaddr_un is a plain C structure; zero initialization is valid.
    let capacity = unsafe { std::mem::zeroed::<nix::libc::sockaddr_un>() }
        .sun_path
        .len();
    for path in [&agent, &control] {
        anyhow::ensure!(
            path.as_os_str().as_bytes().len() < capacity,
            "runtime_socket_path_too_long: {} is {} bytes, Unix socket limit is {capacity}",
            path.display(),
            path.as_os_str().as_bytes().len()
        );
    }
    Ok(())
}

#[test]
fn isolated_sdk_home_keeps_runtime_socket_paths_below_unix_limit() {
    let sdk_home = Path::new("/tmp/opencode/m0000000000/.local/share/lookback/msb");
    validate_socket_path_lengths(sdk_home).unwrap();
}

fn runtime_asset_directory() -> PathBuf {
    std::env::var_os("SANDBOX_MANAGED_RUNTIME_ASSET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(ASSET_DIR_DEFAULT))
}

fn verify_sha256(path: &Path, expected: &str) -> anyhow::Result<()> {
    let output = Command::new("sha256sum").arg("--").arg(path).output()?;
    anyhow::ensure!(
        output.status.success(),
        "cannot calculate SHA-256 for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let actual = String::from_utf8(output.stdout)?
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("sha256sum omitted the digest for {}", path.display()))?
        .to_owned();
    anyhow::ensure!(
        actual == expected,
        "runtime asset {} does not match its pinned SHA-256",
        path.display()
    );
    Ok(())
}

fn stage_pinned_runtime_assets(sdk_home: &Path) -> anyhow::Result<()> {
    let source_directory = runtime_asset_directory();
    for asset in &PINNED_RUNTIME_ASSETS {
        let source = source_directory.join(asset.source_name);
        let source_metadata = fs::symlink_metadata(&source)?;
        anyhow::ensure!(
            source_metadata.is_file()
                && !source_metadata.file_type().is_symlink()
                && source_metadata.uid() == uid()
                && source_metadata.len() == asset.size,
            "approved runtime asset source has an unexpected type, owner, or length: {}",
            source.display()
        );
        verify_sha256(&source, asset.sha256)?;

        let destination = sdk_home.join(asset.destination);
        fs::create_dir_all(destination.parent().expect("asset has a parent"))?;
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&destination)?;
        anyhow::ensure!(
            io::copy(&mut input, &mut output)? == asset.size,
            "runtime asset size changed while staging: {}",
            source.display()
        );
        output.sync_all()?;
        verify_sha256(&destination, asset.sha256)?;
        anyhow::ensure!(
            fs::metadata(&destination)?.mode() & 0o777 == 0o700,
            "staged runtime asset must have mode 0700: {}",
            destination.display()
        );
    }
    Ok(())
}

fn write_private_sdk_config(sdk_home: &Path) -> anyhow::Result<PathBuf> {
    let config_path = sdk_home.join("config.json");
    let bytes = serde_json::to_vec(&serde_json::json!({
        "paths": { "agentd": sdk_home.join("bin/agentd") },
    }))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(&config_path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(config_path)
}

fn sdk_command(msb: &Path, home: &Path, sdk_home: &Path, config: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(msb);
    command
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("MSB_HOME", sdk_home)
        .env("MSB_CONFIG_PATH", config)
        .env("PATH", "/usr/bin:/bin");
    command
}

struct DockerImage {
    config_digest: String,
    size_bytes: u64,
}

fn inspect_local_image(image: &str) -> anyhow::Result<DockerImage> {
    let output = Command::new("docker")
        .args([
            "image",
            "inspect",
            image,
            "--format",
            "{{json .RepoDigests}}|{{.Id}}|{{.Size}}|{{.Os}}|{{.Architecture}}",
        ])
        .env("DOCKER_CONFIG", DOCKER_CONFIG)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "the approved image must already be in the local Docker daemon; no registry pull was attempted: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let inspected = String::from_utf8(output.stdout)?;
    let mut fields = inspected.trim().split('|');
    let repo_digests: Vec<String> = serde_json::from_str(
        fields
            .next()
            .ok_or_else(|| anyhow::anyhow!("Docker inspect omitted RepoDigests"))?,
    )?;
    let config_digest = fields
        .next()
        .ok_or_else(|| anyhow::anyhow!("Docker inspect omitted the image config digest"))?
        .to_owned();
    let size_bytes = fields
        .next()
        .ok_or_else(|| anyhow::anyhow!("Docker inspect omitted image size"))?
        .parse::<u64>()?;
    let os = fields
        .next()
        .ok_or_else(|| anyhow::anyhow!("Docker inspect omitted image OS"))?;
    let architecture = fields
        .next()
        .ok_or_else(|| anyhow::anyhow!("Docker inspect omitted image architecture"))?;
    anyhow::ensure!(
        repo_digests.iter().any(|digest| digest == image),
        "local Docker RepoDigests do not contain the exact approved reference"
    );
    anyhow::ensure!(
        config_digest.starts_with("sha256:")
            && config_digest.len() == "sha256:".len() + 64
            && config_digest["sha256:".len()..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()),
        "Docker returned a malformed image config digest"
    );
    anyhow::ensure!(
        os == "linux" && architecture == "amd64",
        "the KVM gate image must be linux/amd64, got {os}/{architecture}"
    );
    anyhow::ensure!(
        size_bytes.saturating_add(IMAGE_IMPORT_MARGIN) <= u64::from(ROOT_DISK_MIB) * 1024 * 1024,
        "the approved image plus expansion margin exceeds the guest root disk"
    );
    Ok(DockerImage {
        config_digest,
        size_bytes,
    })
}

fn available_bytes(path: &Path) -> anyhow::Result<u64> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: statvfs writes its output structure on success.
    let mut stats = unsafe { std::mem::zeroed::<nix::libc::statvfs>() };
    let result = unsafe { nix::libc::statvfs(path.as_ptr(), &mut stats) };
    anyhow::ensure!(
        result == 0,
        "cannot inspect free space for the SDK image import: {}",
        io::Error::last_os_error()
    );
    Ok((stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64))
}

fn json_contains_string(value: &serde_json::Value, expected: &str) -> bool {
    match value {
        serde_json::Value::String(value) => value == expected,
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| json_contains_string(value, expected)),
        serde_json::Value::Object(values) => values
            .values()
            .any(|value| json_contains_string(value, expected)),
        _ => false,
    }
}

fn private_cache_has_image(
    msb: &Path,
    home: &Path,
    sdk_home: &Path,
    config: &Path,
    image: &str,
    config_digest: &str,
) -> anyhow::Result<bool> {
    let references = sdk_command(
        msb,
        home,
        sdk_home,
        config,
        &["image", "list", "--format", "json"],
    )
    .output()?;
    anyhow::ensure!(
        references.status.success(),
        "cannot list the isolated microsandbox image cache: {}",
        String::from_utf8_lossy(&references.stderr)
    );
    let references: serde_json::Value = serde_json::from_slice(&references.stdout)?;
    if !json_contains_string(&references, image) {
        return Ok(false);
    }
    let inspected = sdk_command(
        msb,
        home,
        sdk_home,
        config,
        &["image", "inspect", image, "--format", "json"],
    )
    .output()?;
    anyhow::ensure!(
        inspected.status.success(),
        "cannot inspect the approved image in the isolated SDK cache: {}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let inspected: serde_json::Value = serde_json::from_slice(&inspected.stdout)?;
    anyhow::ensure!(
        json_contains_string(&inspected, config_digest),
        "isolated SDK image reference does not match the Docker-verified config digest"
    );
    Ok(true)
}

fn import_local_image_if_needed(
    msb: &Path,
    home: &Path,
    sdk_home: &Path,
    config: &Path,
    image: &str,
    metadata: &DockerImage,
) -> anyhow::Result<()> {
    if private_cache_has_image(msb, home, sdk_home, config, image, &metadata.config_digest)? {
        return Ok(());
    }
    let required = metadata.size_bytes.saturating_add(IMAGE_IMPORT_MARGIN);
    let available = available_bytes(Path::new("/tmp/opencode"))?;
    anyhow::ensure!(
        available >= required,
        "offline import needs at least {required} free bytes; only {available} are available"
    );
    let mut docker = Command::new("docker")
        .args(["image", "save", image])
        .env("DOCKER_CONFIG", DOCKER_CONFIG)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let archive = docker
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Docker image save did not expose its archive stream"))?;
    let load = sdk_command(msb, home, sdk_home, config, &["load", "--tag", image])
        .stdin(Stdio::from(archive))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let load_output = load.wait_with_output()?;
    let docker_status = docker.wait()?;
    anyhow::ensure!(
        docker_status.success(),
        "local Docker image export failed; no registry pull was attempted"
    );
    anyhow::ensure!(
        load_output.status.success(),
        "microsandbox could not import the local approved image: {}",
        String::from_utf8_lossy(&load_output.stderr)
    );
    anyhow::ensure!(
        private_cache_has_image(msb, home, sdk_home, config, image, &metadata.config_digest,)?,
        "microsandbox did not register the exact approved image in its isolated cache"
    );
    Ok(())
}

fn runner_settings(image: &str) -> Vec<u8> {
    SandboxRunnerSettings {
        vm: Some(SandboxVmConfig {
            image: Some(image.to_owned()),
            cpus: Some(2),
            memory_mib: Some(2048),
            root_disk_mib: Some(ROOT_DISK_MIB),
            ..Default::default()
        }),
        allowed_images: vec![image.to_owned()],
        network: Some(SandboxNetworkConfig {
            enabled: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    }
    .encode_to_vec()
}

fn make_runner(
    worker_id: i64,
) -> (
    SandboxRunner,
    SandboxCleanupRegistry,
    watch::Sender<bool>,
    ShutdownWait,
) {
    let (lock, wait) = create_lock_and_wait();
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let registry = SandboxCleanupRegistry::new(lock, shutdown_receiver).unwrap();
    let context = SandboxExecutionContext::non_static_worker(WorkerId { value: worker_id })
        .with_job_id(JobId {
            value: worker_id + 1,
        });
    let mut runner = SandboxRunner::new_with_context(context);
    runner.set_cleanup_registry(registry.clone()).unwrap();
    (runner, registry, shutdown_sender, wait)
}

fn sdk_command_from_environment(msb: &Path, args: &[&str]) -> Command {
    let home = PathBuf::from(std::env::var_os(HOME_ENV).expect("child HOME is required"));
    let sdk_home =
        PathBuf::from(std::env::var_os(SDK_HOME_ENV).expect("child SDK home is required"));
    let config = sdk_home.join("config.json");
    sdk_command(msb, &home, &sdk_home, &config, args)
}

fn sandbox_records(msb: &Path, worker_id: i64) -> anyhow::Result<Vec<String>> {
    let output = sdk_command_from_environment(msb, &["list", "--format", "json"]).output()?;
    anyhow::ensure!(
        output.status.success(),
        "msb list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let prefix = format!("jw-sbx-w{worker_id}-");
    Ok(value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("msb list did not return a JSON array"))?
        .iter()
        .filter_map(|item| item["name"].as_str())
        .filter(|name| name.starts_with(&prefix))
        .map(str::to_owned)
        .collect())
}

async fn run_opencode_version(runner: &mut SandboxRunner) -> anyhow::Result<(Vec<u8>, bool, bool)> {
    let stream = runner
        .run_stream(
            &SandboxExecArgs {
                command: "opencode".to_owned(),
                args: vec!["--version".to_owned()],
                ..Default::default()
            }
            .encode_to_vec(),
            Default::default(),
            Some("run"),
        )
        .await?;
    let mut stream = Box::pin(stream);
    let mut output = Vec::new();
    let mut successful_exit = false;
    let mut normal_end = false;
    loop {
        let next = tokio::time::timeout(Duration::from_secs(120), stream.next()).await?;
        let Some(item) = next else { break };
        match item.item.unwrap() {
            Item::Data(bytes) => {
                let result = SandboxExecResult::decode(bytes.as_slice())?;
                match result.result {
                    Some(SandboxResult::Output(data)) => output.extend(data.data),
                    Some(SandboxResult::Exit(exit)) if exit.exit_code == 0 => {
                        successful_exit = true;
                    }
                    Some(SandboxResult::Exit(exit)) => {
                        anyhow::bail!("OpenCode command exited with status {}", exit.exit_code)
                    }
                    _ => {}
                }
            }
            Item::End(trailer) => {
                anyhow::ensure!(
                    !trailer.metadata.contains_key("stream_error"),
                    "SANDBOX stream reported an execution error: {trailer:?}"
                );
                normal_end = true;
            }
            Item::FinalCollected(_) => anyhow::bail!("unexpected collected SANDBOX output"),
        }
    }
    Ok((output, successful_exit, normal_end))
}

async fn run_gate_child() -> anyhow::Result<()> {
    let sdk_home =
        PathBuf::from(std::env::var_os(SDK_HOME_ENV).expect("SDK home must be provided"));
    let image = approved_image()?;
    let msb = sdk_home.join("bin/msb");
    let worker_id = 7_600_000_000_i64 + i64::from(std::process::id());
    let records_before = sandbox_records(&msb, worker_id)?;
    let (mut runner, registry, shutdown_sender, mut shutdown_wait) = make_runner(worker_id);
    let settings = SandboxRunnerSettings::decode(runner_settings(&image).as_slice())?;
    let vm = settings
        .vm
        .as_ref()
        .expect("KVM gate VM settings are present");
    anyhow::ensure!(
        settings
            .network
            .as_ref()
            .is_some_and(|network| network.enabled == Some(false))
            && vm.mounts.is_empty()
            && vm.env.is_empty()
            && settings.allowed_host_mounts.is_empty(),
        "the KVM test must not enable networking, pass guest environment values, or mount host paths"
    );
    runner.load(settings.encode_to_vec()).await?;
    let (output, successful_exit, normal_end) = run_opencode_version(&mut runner).await?;
    anyhow::ensure!(
        successful_exit && normal_end,
        "OpenCode command did not complete with a normal successful exit"
    );
    verify_opencode_version(&output)?;
    println!("direct OpenCode version=1.18.3; observed exit=0; normal End confirmed");

    shutdown_sender.send(true)?;
    registry.shutdown().await;
    drop(runner);
    tokio::time::timeout(Duration::from_secs(90), shutdown_wait.wait()).await?;
    anyhow::ensure!(
        sandbox_records(&msb, worker_id)? == records_before,
        "ordinary SANDBOX cleanup did not remove the test VM record"
    );
    Ok(())
}

fn verify_opencode_version(output: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(
        std::str::from_utf8(output)?.trim() == "1.18.3",
        "pinned OpenCode image did not report the expected CLI version"
    );
    Ok(())
}

#[test]
fn opencode_version_evidence_requires_the_expected_output() {
    assert!(verify_opencode_version(b"1.18.3\n").is_ok());
    assert!(verify_opencode_version(b"").is_err());
    assert!(verify_opencode_version(b"managed-kvm-ok").is_err());
    assert!(verify_opencode_version(b"1.18.2\n").is_err());
    assert!(verify_opencode_version(b"1.18.3\nmanaged-kvm-ok").is_err());
    assert!(verify_opencode_version(&[0xff]).is_err());
}

#[tokio::test]
#[ignore = "requires Linux/KVM, pinned microsandbox 0.7.6 assets, and a locally cached immutable image digest"]
async fn pinned_076_runtime_executes_and_cleans_through_sandbox_runner() -> anyhow::Result<()> {
    if std::env::var_os(CHILD_GATE_ENV).is_some() {
        return run_gate_child().await;
    }

    let image = approved_image()?;
    let image_metadata = inspect_local_image(&image)?;
    let work_root = gate_work_root()?;
    let (home, sdk_home) = create_short_test_home()?;
    stage_pinned_runtime_assets(&sdk_home)?;
    let config = write_private_sdk_config(&sdk_home)?;
    let msb = sdk_home.join("bin/msb");
    import_local_image_if_needed(&msb, &home, &sdk_home, &config, &image, &image_metadata)?;

    let output = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "pinned_076_runtime_executes_and_cleans_through_sandbox_runner",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .current_dir(work_root)
        .env_clear()
        .env("HOME", &home)
        .env("MSB_HOME", &sdk_home)
        .env("MSB_CONFIG_PATH", &config)
        .env("PATH", "/usr/bin:/bin")
        .env(IMAGE_ENV, &image)
        .env(HOME_ENV, &home)
        .env(SDK_HOME_ENV, &sdk_home)
        .env(CHILD_GATE_ENV, "1")
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "ordinary SANDBOX KVM child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
