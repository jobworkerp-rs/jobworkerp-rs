//! Host-side V2 ABI integration coverage for the IssueTrackerRunner cdylib.
//!
//! The plugin lives in its own Cargo workspace, so this test explicitly builds
//! its cdylib and loads that artifact through the production runner loader.

use anyhow::{Context, Result, bail};
use jobworkerp_runner::runner::plugins::Plugins;
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use prost::Message;
use serde::Deserialize;
use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;

const ISSUE_TRACKER_PLUGIN: &str = "IssueTrackerRunner";
const METHOD_COUNT: usize = 21;
const EXPECTED_METHODS: [(&str, &str, &str); METHOD_COUNT] = [
    (
        "get_contract_info",
        "GetContractInfoArgs",
        "GetContractInfoResult",
    ),
    ("get_issue", "GetIssueArgs", "GetIssueResult"),
    ("list_issues", "ListIssuesArgs", "ListIssuesResult"),
    (
        "list_ready_issues",
        "ListReadyIssuesArgs",
        "ListReadyIssuesResult",
    ),
    (
        "list_issue_comments",
        "ListIssueCommentsArgs",
        "ListIssueCommentsResult",
    ),
    (
        "list_issue_history",
        "ListIssueHistoryArgs",
        "ListIssueHistoryResult",
    ),
    ("get_artifact", "GetArtifactArgs", "GetArtifactResult"),
    ("list_artifacts", "ListArtifactsArgs", "ListArtifactsResult"),
    ("get_operation", "GetOperationArgs", "GetOperationResult"),
    ("create_issue", "CreateIssueArgs", "CreateIssueResult"),
    ("update_issue", "UpdateIssueArgs", "UpdateIssueResult"),
    (
        "transition_issue",
        "TransitionIssueArgs",
        "TransitionIssueResult",
    ),
    ("claim_issue", "ClaimIssueArgs", "ClaimIssueResult"),
    (
        "heartbeat_issue_claim",
        "HeartbeatIssueClaimArgs",
        "HeartbeatIssueClaimResult",
    ),
    (
        "release_issue_claim",
        "ReleaseIssueClaimArgs",
        "ReleaseIssueClaimResult",
    ),
    (
        "reconcile_expired_claim",
        "ReconcileExpiredClaimArgs",
        "ReconcileExpiredClaimResult",
    ),
    (
        "add_issue_relation",
        "AddIssueRelationArgs",
        "AddIssueRelationResult",
    ),
    (
        "remove_issue_relation",
        "RemoveIssueRelationArgs",
        "RemoveIssueRelationResult",
    ),
    (
        "add_issue_comment",
        "AddIssueCommentArgs",
        "AddIssueCommentResult",
    ),
    ("create_backup", "CreateBackupArgs", "CreateBackupResult"),
    ("export_issues", "ExportIssuesArgs", "ExportIssuesResult"),
];

#[derive(Deserialize)]
struct SettingsFixture {
    expected_contract_fingerprint: String,
    expected_storage_schema_fingerprint: String,
    lease_policy: LeasePolicyFixture,
    pagination_policy: PaginationPolicyFixture,
    resource_limits: ResourceLimitsFixture,
}

#[derive(Deserialize)]
struct LeasePolicyFixture {
    default_lease_millis: u64,
    minimum_lease_millis: u64,
    maximum_lease_millis: u64,
    maximum_heartbeat_interval_millis: u64,
}

#[derive(Deserialize)]
struct PaginationPolicyFixture {
    default_page_size: u32,
    maximum_page_size: u32,
}

#[derive(Deserialize)]
struct ResourceLimitsFixture {
    max_string_bytes: u64,
    max_body_bytes: u64,
    max_uri_bytes: u64,
    max_token_bytes: u64,
    max_handle_bytes: u64,
    max_repeated_item_count: u64,
    max_export_bytes: u64,
    max_export_item_count: u64,
    max_artifact_bytes: u64,
    max_artifact_count: u64,
    max_total_count_scan_items: u64,
}

#[derive(Message)]
struct IssueTrackerRunnerSettings {
    #[prost(string, tag = "1")]
    data_root: String,
    #[prost(string, tag = "4")]
    expected_contract_fingerprint: String,
    #[prost(string, tag = "5")]
    expected_storage_schema_fingerprint: String,
    #[prost(message, optional, tag = "6")]
    lease_policy: Option<LeasePolicy>,
    #[prost(message, optional, tag = "7")]
    pagination_policy: Option<PaginationPolicy>,
    #[prost(int32, tag = "8")]
    secret_filter_mode: i32,
    #[prost(message, optional, tag = "9")]
    resource_limits: Option<ResourceLimits>,
}

#[derive(Message)]
struct TimeSpan {
    #[prost(uint64, tag = "1")]
    millis: u64,
}

#[derive(Message)]
struct LeasePolicy {
    #[prost(message, optional, tag = "1")]
    default_lease: Option<TimeSpan>,
    #[prost(message, optional, tag = "2")]
    minimum_lease: Option<TimeSpan>,
    #[prost(message, optional, tag = "3")]
    maximum_lease: Option<TimeSpan>,
    #[prost(message, optional, tag = "4")]
    maximum_heartbeat_interval: Option<TimeSpan>,
}

#[derive(Message)]
struct PaginationPolicy {
    #[prost(uint32, tag = "1")]
    default_page_size: u32,
    #[prost(uint32, tag = "2")]
    maximum_page_size: u32,
}

#[derive(Message)]
struct ResourceLimits {
    #[prost(uint64, tag = "1")]
    max_string_bytes: u64,
    #[prost(uint64, tag = "2")]
    max_body_bytes: u64,
    #[prost(uint64, tag = "3")]
    max_uri_bytes: u64,
    #[prost(uint64, tag = "4")]
    max_token_bytes: u64,
    #[prost(uint64, tag = "5")]
    max_handle_bytes: u64,
    #[prost(uint64, tag = "6")]
    max_repeated_item_count: u64,
    #[prost(uint64, tag = "7")]
    max_export_bytes: u64,
    #[prost(uint64, tag = "8")]
    max_export_item_count: u64,
    #[prost(uint64, tag = "9")]
    max_artifact_bytes: u64,
    #[prost(uint64, tag = "10")]
    max_artifact_count: u64,
    #[prost(uint64, tag = "11")]
    max_total_count_scan_items: u64,
}

#[derive(Message)]
struct GetContractInfoResult {
    #[prost(oneof = "get_contract_info_result::Outcome", tags = "1, 2")]
    outcome: Option<get_contract_info_result::Outcome>,
}

#[derive(Message)]
struct TypedMethodResult {
    #[prost(oneof = "typed_method_result::Outcome", tags = "1, 2")]
    outcome: Option<typed_method_result::Outcome>,
}

mod typed_method_result {
    #[derive(prost::Oneof)]
    pub enum Outcome {
        // All IssueTrackerRunner result messages use this common oneof wire
        // contract. Bytes deliberately preserve the method-specific payload.
        #[prost(bytes, tag = "1")]
        Success(Vec<u8>),
        #[prost(bytes, tag = "2")]
        Error(Vec<u8>),
    }
}

mod get_contract_info_result {
    #[derive(prost::Oneof)]
    pub enum Outcome {
        #[prost(message, tag = "1")]
        Success(super::ContractInfo),
        #[prost(bytes, tag = "2")]
        Error(Vec<u8>),
    }
}

#[derive(Message)]
struct ContractInfo {
    #[prost(string, tag = "1")]
    contract_version: String,
    #[prost(string, tag = "2")]
    contract_fingerprint: String,
    #[prost(string, tag = "4")]
    storage_schema_fingerprint: String,
    #[prost(string, repeated, tag = "6")]
    method_keys: Vec<String>,
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("runner must be under the workspace root")
        .to_path_buf()
}

fn plugin_path() -> PathBuf {
    let extension = if cfg!(target_os = "windows") {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    let prefix = if cfg!(target_os = "windows") {
        ""
    } else {
        "lib"
    };
    let root = workspace_root();
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        })
        .unwrap_or_else(|| root.join("issue-tracker-runner").join("target"));
    target_dir
        .join(if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        })
        .join(format!("{prefix}issue_tracker_runner.{extension}"))
}

fn build_issue_tracker_cdylib() -> Result<PathBuf> {
    let root = workspace_root();
    let status = Command::new(env!("CARGO"))
        .args(["build", "--manifest-path"])
        .arg(root.join("issue-tracker-runner/Cargo.toml"))
        .current_dir(&root)
        .status()
        .context("issue-tracker-runner cdylib build must start")?;
    if !status.success() {
        bail!("issue-tracker-runner cdylib build failed: {status}");
    }
    let path = plugin_path();
    if !path.is_file() {
        bail!(
            "issue-tracker-runner cdylib was not produced at {}",
            path.display()
        );
    }
    Ok(path)
}

fn plugin_settings(data_root: &Path) -> Result<Vec<u8>> {
    let fixture: SettingsFixture =
        serde_json::from_str(include_str!("fixtures/issue_tracker_runner_settings.json"))?;
    let timespan = |millis| Some(TimeSpan { millis });
    Ok(IssueTrackerRunnerSettings {
        data_root: data_root.to_string_lossy().into_owned(),
        expected_contract_fingerprint: fixture.expected_contract_fingerprint,
        expected_storage_schema_fingerprint: fixture.expected_storage_schema_fingerprint,
        lease_policy: Some(LeasePolicy {
            default_lease: timespan(fixture.lease_policy.default_lease_millis),
            minimum_lease: timespan(fixture.lease_policy.minimum_lease_millis),
            maximum_lease: timespan(fixture.lease_policy.maximum_lease_millis),
            maximum_heartbeat_interval: timespan(
                fixture.lease_policy.maximum_heartbeat_interval_millis,
            ),
        }),
        pagination_policy: Some(PaginationPolicy {
            default_page_size: fixture.pagination_policy.default_page_size,
            maximum_page_size: fixture.pagination_policy.maximum_page_size,
        }),
        // `SECRET_FILTER_MODE_FAIL_CLOSED` is the stable protobuf wire value.
        secret_filter_mode: 1,
        resource_limits: Some(ResourceLimits {
            max_string_bytes: fixture.resource_limits.max_string_bytes,
            max_body_bytes: fixture.resource_limits.max_body_bytes,
            max_uri_bytes: fixture.resource_limits.max_uri_bytes,
            max_token_bytes: fixture.resource_limits.max_token_bytes,
            max_handle_bytes: fixture.resource_limits.max_handle_bytes,
            max_repeated_item_count: fixture.resource_limits.max_repeated_item_count,
            max_export_bytes: fixture.resource_limits.max_export_bytes,
            max_export_item_count: fixture.resource_limits.max_export_item_count,
            max_artifact_bytes: fixture.resource_limits.max_artifact_bytes,
            max_artifact_count: fixture.resource_limits.max_artifact_count,
            max_total_count_scan_items: fixture.resource_limits.max_total_count_scan_items,
        }),
    }
    .encode_to_vec())
}

fn declared_primary_message(proto: &str) -> Option<&str> {
    proto.split("message ").nth(1)?.split_whitespace().next()
}

#[tokio::test]
async fn issue_tracker_v2_cdylib_exposes_contract_and_routes_typed_calls() -> Result<()> {
    let plugin_path = build_issue_tracker_cdylib()?;
    let plugins = Plugins::new();
    let loaded = plugins
        .load_plugin_file(
            None,
            plugin_path.to_str().expect("plugin path must be UTF-8"),
            false,
        )
        .await?;
    assert_eq!(loaded.name, ISSUE_TRACKER_PLUGIN);

    let loader = plugins.runner_plugins();
    let guard = loader.read().await;
    let mut runner = guard
        .find_plugin_runner_by_name(ISSUE_TRACKER_PLUGIN)
        .await
        .expect("host must instantiate the V2 plugin through load_multi_method_plugin_v2");

    assert_eq!(runner.name(), ISSUE_TRACKER_PLUGIN);
    assert!(
        runner
            .runner_settings_proto()
            .contains("IssueTrackerRunnerSettings")
    );
    assert!(serde_json::from_str::<serde_json::Value>(&runner.settings_schema())?.is_object());
    let schemas = runner.method_proto_map();
    assert_eq!(schemas.len(), METHOD_COUNT);
    assert!(schemas.contains_key("get_contract_info"));
    assert!(schemas.contains_key("export_issues"));
    let json_schemas = runner.method_json_schema_map();
    assert_eq!(json_schemas.len(), METHOD_COUNT);
    for (method, expected_args, expected_result) in EXPECTED_METHODS {
        let schema = schemas
            .get(method)
            .expect("every fixed IssueTrackerRunner method must be registered");
        assert_eq!(schema.output_type, 0, "{method} must be non-streaming");
        assert!(
            declared_primary_message(&schema.args_proto) == Some(expected_args),
            "{method} request descriptor must match the fixed contract"
        );
        assert!(
            declared_primary_message(&schema.result_proto) == Some(expected_result),
            "{method} result descriptor must match the fixed contract"
        );
        let json_schema = json_schemas
            .get(method)
            .expect("JSON schema key must match proto schema key");
        assert!(serde_json::from_str::<serde_json::Value>(&json_schema.args_schema)?.is_object());
        assert!(
            serde_json::from_str::<serde_json::Value>(
                json_schema
                    .result_schema
                    .as_deref()
                    .expect("non-streaming method requires a result schema"),
            )?
            .is_object()
        );
    }

    let data_root = TempDir::new()?;
    runner.load(plugin_settings(data_root.path())?).await?;

    let (result, metadata) = runner
        .run(&[], HashMap::new(), Some("get_contract_info"))
        .await;
    let contract = match GetContractInfoResult::decode(result?.as_slice())?.outcome {
        Some(get_contract_info_result::Outcome::Success(contract)) => contract,
        Some(get_contract_info_result::Outcome::Error(_)) | None => {
            bail!("get_contract_info must return its typed success result")
        }
    };
    assert_eq!(contract.contract_version, "lookback-issue/v1");
    assert_eq!(contract.method_keys.len(), METHOD_COUNT);
    assert!(
        contract
            .method_keys
            .iter()
            .any(|key| key == "get_contract_info")
    );
    assert_eq!(
        metadata.get("contract"),
        Some(&"lookback-issue/v1".to_owned())
    );

    // Route every advertised `using` value through the host. Empty protobuf
    // requests are valid wire values; methods needing fields respond with
    // their declared typed error outcome, while no-argument reads succeed.
    for (method, _, _) in EXPECTED_METHODS {
        let (result, _) = runner.run(&[], HashMap::new(), Some(method)).await;
        let typed = TypedMethodResult::decode(result?.as_slice())?;
        assert!(
            typed.outcome.is_some(),
            "{method} must encode either its typed success or typed error outcome"
        );
    }

    for using in [None, Some("run"), Some("unknown_method")] {
        let (result, metadata) = runner.run(&[0xff, 0x00], HashMap::new(), using).await;
        assert!(
            result.is_err(),
            "legacy invocation must fail closed for {using:?}"
        );
        assert!(
            metadata.is_empty(),
            "legacy invocation must not return metadata"
        );
    }
    Ok(())
}
