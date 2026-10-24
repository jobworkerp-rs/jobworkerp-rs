//! The worker image compiles the whole Cargo workspace inside Docker
//! (`worker-main/Dockerfile`), so every workspace member must be part of the
//! build context. `.dockerignore` excludes everything by default and
//! re-includes paths explicitly; a member missing there breaks the image build
//! only at release time.

use std::path::Path;

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
}

fn workspace_members() -> Vec<String> {
    let manifest = std::fs::read_to_string(workspace_root().join("Cargo.toml")).unwrap();
    let manifest: toml::Table = manifest.parse().unwrap();
    manifest["workspace"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap().to_string())
        .collect()
}

fn dockerignore_includes() -> Vec<String> {
    std::fs::read_to_string(workspace_root().join(".dockerignore"))
        .unwrap()
        .lines()
        .filter_map(|line| line.trim().strip_prefix('!'))
        .map(str::to_string)
        .collect()
}

#[test]
fn every_workspace_member_is_in_docker_build_context() {
    let includes = dockerignore_includes();
    let missing: Vec<String> = workspace_members()
        .into_iter()
        .filter(|member| {
            let top = member.split('/').next().unwrap();
            !includes.contains(&format!("{top}/**"))
        })
        .collect();
    assert!(
        missing.is_empty(),
        "workspace members excluded from the Docker build context by .dockerignore: {missing:?}"
    );
}

#[test]
fn workspace_manifests_are_in_docker_build_context() {
    let includes = dockerignore_includes();
    for file in ["Cargo.toml", "Cargo.lock"] {
        assert!(
            includes.iter().any(|i| i == file),
            "{file} must be included in the Docker build context"
        );
    }
}
