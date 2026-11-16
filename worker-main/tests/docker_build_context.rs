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

/// Files every published image ships under /usr/share/doc/jobworkerp-rs/: the
/// project license and the third-party listing the workflow generates before
/// building the image.
const LICENSE_FILES: [&str; 3] = ["LICENSE-MIT", "LICENSE-APACHE", "THIRD-PARTY-LICENSES.txt"];
const LICENSE_DIR: &str = "/usr/share/doc/jobworkerp-rs/";
const LICENSES_SCRIPT: &str = "scripts/generate-license-listing.sh";

/// Workflows that publish images, with the Dockerfiles they build (`file:`).
fn image_workflows() -> Vec<(String, Vec<String>)> {
    let mut found = Vec::new();
    for dir in [".github/workflows", ".gitea/workflows"] {
        for entry in std::fs::read_dir(workspace_root().join(dir))
            .unwrap()
            .flatten()
        {
            let text = std::fs::read_to_string(entry.path()).unwrap();
            let dockerfiles: Vec<String> = text
                .lines()
                .filter_map(|line| line.trim().strip_prefix("file: "))
                .filter(|file| file.contains("Dockerfile"))
                .map(|file| file.trim().to_string())
                .collect();
            if !dockerfiles.is_empty() {
                found.push((
                    format!("{dir}/{}", entry.file_name().display()),
                    dockerfiles,
                ));
            }
        }
    }
    found
}

#[test]
fn every_published_image_ships_the_licenses() {
    let workflows = image_workflows();
    assert!(!workflows.is_empty(), "no image publishing workflow found");
    for (workflow, dockerfiles) in workflows {
        let text = std::fs::read_to_string(workspace_root().join(&workflow)).unwrap();
        assert!(
            text.contains(LICENSES_SCRIPT),
            "{workflow} builds images but does not generate THIRD-PARTY-LICENSES.txt"
        );
        for dockerfile in dockerfiles {
            let text = std::fs::read_to_string(workspace_root().join(&dockerfile)).unwrap();
            let ships = text.lines().any(|line| {
                line.starts_with("COPY ")
                    && line.trim_end().ends_with(LICENSE_DIR)
                    && LICENSE_FILES.iter().all(|file| line.contains(file))
            });
            assert!(
                ships,
                "{dockerfile} (built by {workflow}) must COPY {LICENSE_FILES:?} to {LICENSE_DIR}"
            );
        }
    }
}

#[test]
fn license_files_are_in_docker_build_context() {
    let includes = dockerignore_includes();
    for file in LICENSE_FILES {
        assert!(
            includes.iter().any(|i| i == file),
            "{file} must be included in the Docker build context"
        );
    }
}
