//! Only `docker/sandbox/Dockerfile` may install the microsandbox runtime (see
//! its header for why published images must not), and its pins must match the
//! microsandbox SDK linked into the worker.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

const SANDBOX_DOCKERFILE: &str = "docker/sandbox/Dockerfile";

/// Text that only appears where an image installs the microsandbox runtime.
const RUNTIME_MARKERS: [&str; 4] = [
    "superradcompany/microsandbox",
    "libkrunfw",
    "/usr/local/bin/msb",
    "MSB_PATH",
];

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Default value of `ARG <name>=<value>` in a Dockerfile.
fn arg_default(dockerfile: &str, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    dockerfile
        .lines()
        .filter_map(|line| line.trim().strip_prefix("ARG "))
        .find_map(|arg| arg.trim().strip_prefix(prefix.as_str()))
        .map(str::to_string)
}

/// Version of a package in Cargo.lock; `None` when it is absent or locked at
/// more than one version.
fn locked_version(lock: &toml::Table, package: &str) -> Option<String> {
    let versions: Vec<&str> = lock["package"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["name"].as_str() == Some(package))
        .filter_map(|p| p["version"].as_str())
        .collect();
    match versions.as_slice() {
        [version] => Some(version.to_string()),
        _ => None,
    }
}

fn installs_runtime(dockerfile: &str) -> bool {
    dockerfile
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .any(|line| RUNTIME_MARKERS.iter().any(|marker| line.contains(marker)))
}

/// Dockerfiles in the git checkout, tracked or not yet added.
fn dockerfiles() -> Vec<PathBuf> {
    let root = workspace_root();
    let output = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .args(["--", "*Dockerfile*"])
        .current_dir(root)
        .output()
        .expect("run git ls-files");
    assert!(output.status.success(), "git ls-files failed");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| root.join(line))
        .collect()
}

#[test]
fn arg_default_reads_the_named_build_argument() {
    let dockerfile = "ARG BASE_IMAGE=example:latest\nFROM x\n  ARG LIBKRUNFW_VERSION=5.6.1\n";
    assert_eq!(
        arg_default(dockerfile, "LIBKRUNFW_VERSION").as_deref(),
        Some("5.6.1")
    );
    assert_eq!(
        arg_default(dockerfile, "BASE_IMAGE").as_deref(),
        Some("example:latest")
    );
    assert_eq!(arg_default(dockerfile, "MICROSANDBOX_VERSION"), None);
    assert_eq!(
        arg_default("ARG MICROSANDBOX_VERSION\n", "MICROSANDBOX_VERSION"),
        None
    );
}

#[test]
fn locked_version_requires_a_single_locked_version() {
    let lock: toml::Table = r#"
[[package]]
name = "microsandbox"
version = "0.7.6"

[[package]]
name = "twice"
version = "1.0.0"

[[package]]
name = "twice"
version = "2.0.0"
"#
    .parse()
    .unwrap();
    assert_eq!(
        locked_version(&lock, "microsandbox").as_deref(),
        Some("0.7.6")
    );
    assert_eq!(locked_version(&lock, "twice"), None);
    assert_eq!(locked_version(&lock, "absent"), None);
}

#[test]
fn installs_runtime_detects_runtime_files_but_not_msb_home() {
    assert!(installs_runtime(
        "FROM ghcr.io/superradcompany/microsandbox:0.7.6 AS microsandbox-runtime\n"
    ));
    assert!(installs_runtime(
        "COPY --from=m /usr/local/lib/libkrunfw.so.5.6.1 /usr/local/lib/\n"
    ));
    assert!(installs_runtime("ENV MSB_PATH=/usr/local/bin/msb\n"));
    assert!(!installs_runtime(
        "RUN mkdir -p /home/jobworkerp/.microsandbox\nENV MSB_HOME=/home/jobworkerp/.microsandbox\n"
    ));
    assert!(!installs_runtime("# libkrunfw is not shipped here\n"));
}

#[test]
fn sandbox_image_pins_the_runtime_of_the_linked_sdk() {
    let root = workspace_root();
    let dockerfile = read(&root.join(SANDBOX_DOCKERFILE));
    let lock: toml::Table = read(&root.join("Cargo.lock")).parse().unwrap();
    let sdk = locked_version(&lock, "microsandbox").expect("one microsandbox version");
    assert_eq!(
        arg_default(&dockerfile, "MICROSANDBOX_VERSION").as_deref(),
        Some(sdk.as_str()),
        "{SANDBOX_DOCKERFILE}: MICROSANDBOX_VERSION must match microsandbox in Cargo.lock"
    );
    // LIBKRUNFW_VERSION below is read from this crate.
    assert_eq!(
        locked_version(&lock, "microsandbox-utils").as_deref(),
        Some(sdk.as_str()),
        "microsandbox-utils must be locked at the microsandbox version"
    );
    assert_eq!(
        arg_default(&dockerfile, "LIBKRUNFW_VERSION").as_deref(),
        Some(microsandbox_utils::LIBKRUNFW_VERSION),
        "{SANDBOX_DOCKERFILE}: LIBKRUNFW_VERSION must match the microsandbox release"
    );
}

#[test]
fn only_the_sandbox_image_installs_the_runtime() {
    let root = workspace_root();
    let files = dockerfiles();
    assert!(
        files.contains(&root.join(SANDBOX_DOCKERFILE)),
        "{SANDBOX_DOCKERFILE} was not discovered"
    );
    let offending: Vec<String> = files
        .iter()
        .filter(|path| **path != root.join(SANDBOX_DOCKERFILE))
        .filter(|path| installs_runtime(&read(path)))
        .map(|path| path.strip_prefix(root).unwrap().display().to_string())
        .collect();
    assert!(
        offending.is_empty(),
        "published images must not install the microsandbox runtime: {offending:?}"
    );
}
