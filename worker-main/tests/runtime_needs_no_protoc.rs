//! Released binaries run on hosts without protoc, so the distributed crates
//! must compile proto sources in process. protoc is reached only through the
//! code-generating crates below; they may appear as build dependencies but
//! never among the normal dependencies of a distributed binary. As in
//! scripts/generate-third-party-licenses.sh, this crate's dependency tree
//! covers every distributed binary.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::process::Command;

const PROTOC_CRATES: &[&str] = &["prost-build", "tonic-build", "tonic-prost-build"];

fn metadata() -> Value {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Package names reachable from `root` through normal dependency edges only.
fn normal_dependency_closure(metadata: &Value, root: &str) -> HashSet<String> {
    let packages: HashMap<&str, &str> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_str().unwrap(), p["name"].as_str().unwrap()))
        .collect();
    let nodes: HashMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n["id"].as_str().unwrap(), n))
        .collect();
    let root_id = packages.iter().find(|(_, name)| **name == root).unwrap().0;
    let mut stack = vec![*root_id];
    let mut seen: HashSet<&str> = HashSet::from([*root_id]);
    while let Some(id) = stack.pop() {
        for dep in nodes[id]["deps"].as_array().unwrap() {
            let is_normal = dep["dep_kinds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|kind| kind["kind"].is_null());
            let dep_id = dep["pkg"].as_str().unwrap();
            if is_normal && seen.insert(dep_id) {
                stack.push(dep_id);
            }
        }
    }
    seen.into_iter()
        .map(|id| packages[id].to_string())
        .collect()
}

#[test]
fn distributed_binaries_do_not_depend_on_protoc_crates() {
    let metadata = metadata();
    let closure = normal_dependency_closure(&metadata, env!("CARGO_PKG_NAME"));
    // The proto crate builds its generated code with tonic-prost-build, so
    // that crate is in the graph: an empty or kind-blind walk would fail here.
    assert!(closure.contains("proto"));
    assert!(
        metadata["packages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "tonic-prost-build")
    );
    let found: Vec<&&str> = PROTOC_CRATES
        .iter()
        .filter(|name| closure.contains(**name))
        .collect();
    assert!(
        found.is_empty(),
        "protoc crates in normal dependencies: {found:?}"
    );
}
