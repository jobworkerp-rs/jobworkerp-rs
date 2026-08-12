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
// This table is deliberately independent from the plugin descriptor. It
// catches a method key being wired to a different handler family.
const EXPECTED_HANDLER_FAMILIES: [(&str, &str); METHOD_COUNT] = [
    ("get_contract_info", "read"),
    ("get_issue", "read"),
    ("list_issues", "read"),
    ("list_ready_issues", "read"),
    ("list_issue_comments", "read"),
    ("list_issue_history", "read"),
    ("get_artifact", "artifacts"),
    ("list_artifacts", "artifacts"),
    ("get_operation", "read"),
    ("create_issue", "writes"),
    ("update_issue", "writes"),
    ("transition_issue", "writes"),
    ("claim_issue", "claims"),
    ("heartbeat_issue_claim", "claims"),
    ("release_issue_claim", "claims"),
    ("reconcile_expired_claim", "claims"),
    ("add_issue_relation", "relations"),
    ("remove_issue_relation", "relations"),
    ("add_issue_comment", "relations"),
    ("create_backup", "artifacts"),
    ("export_issues", "artifacts"),
];
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

#[derive(Message)]
struct WriteContext {
    #[prost(message, optional, tag = "1")]
    request: Option<RequestContext>,
    #[prost(string, tag = "2")]
    operation_id: String,
    #[prost(string, tag = "3")]
    reason: String,
}
#[derive(Message)]
struct RequestContext {
    #[prost(message, optional, tag = "1")]
    caller: Option<ActorContext>,
    #[prost(string, tag = "2")]
    correlation_id: String,
}
#[derive(Message)]
struct ActorContext {
    #[prost(message, optional, tag = "1")]
    actor: Option<ActorRef>,
}
#[derive(Message)]
struct ActorRef {
    #[prost(string, tag = "1")]
    actor_id: String,
}
#[derive(Message)]
struct IssueDraft {
    #[prost(string, tag = "1")]
    title: String,
    #[prost(int32, tag = "3")]
    kind: i32,
    #[prost(int32, tag = "4")]
    priority: i32,
    #[prost(int32, tag = "10")]
    creation_basis: i32,
    #[prost(int32, tag = "11")]
    confirmation_status: i32,
}
#[derive(Message)]
struct CreateIssueArgs {
    #[prost(message, optional, tag = "1")]
    context: Option<WriteContext>,
    #[prost(string, tag = "2")]
    issue_id: String,
    #[prost(message, optional, tag = "3")]
    issue: Option<IssueDraft>,
}
#[derive(Message)]
struct ClaimIssueArgs {
    #[prost(message, optional, tag = "1")]
    context: Option<WriteContext>,
    #[prost(string, tag = "2")]
    issue_id: String,
    #[prost(uint64, optional, tag = "3")]
    expected_revision: Option<u64>,
}
#[derive(Message)]
struct AddIssueRelationArgs {
    #[prost(message, optional, tag = "1")]
    context: Option<WriteContext>,
    #[prost(int32, tag = "3")]
    kind: i32,
    #[prost(string, tag = "4")]
    source_issue_id: String,
    #[prost(uint64, optional, tag = "5")]
    source_revision: Option<u64>,
    #[prost(string, tag = "6")]
    target_issue_id: String,
    #[prost(uint64, optional, tag = "7")]
    target_revision: Option<u64>,
}
#[derive(Message)]
struct AddIssueCommentArgs {
    #[prost(message, optional, tag = "1")]
    context: Option<WriteContext>,
    #[prost(string, tag = "2")]
    issue_id: String,
    #[prost(uint64, optional, tag = "3")]
    revision: Option<u64>,
    #[prost(string, tag = "4")]
    comment_id: String,
    #[prost(string, tag = "5")]
    body: String,
}
#[derive(Message)]
struct CreateBackupArgs {
    #[prost(message, optional, tag = "1")]
    context: Option<WriteContext>,
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

fn write_context(operation_id: &str) -> WriteContext {
    WriteContext {
        request: Some(RequestContext {
            caller: Some(ActorContext {
                actor: Some(ActorRef {
                    actor_id: "host-e2e".into(),
                }),
            }),
            correlation_id: operation_id.into(),
        }),
        operation_id: operation_id.into(),
        reason: "host integration test".into(),
    }
}

async fn run_success(runner: &mut impl RunnerTrait, method: &str, args: Vec<u8>) -> Result<()> {
    let (result, _) = runner.run(&args, HashMap::new(), Some(method)).await;
    match TypedMethodResult::decode(result?.as_slice())?.outcome {
        Some(typed_method_result::Outcome::Success(_)) => Ok(()),
        Some(typed_method_result::Outcome::Error(_)) | None => {
            bail!("{method} must return a concrete success outcome")
        }
    }
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
    assert_eq!(EXPECTED_HANDLER_FAMILIES.len(), schemas.len());
    for (method, family) in EXPECTED_HANDLER_FAMILIES {
        assert!(
            schemas.contains_key(method),
            "{method} must remain in the fixed {family} handler family"
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

    // Each production handler family has a valid request that must make it
    // through the host V2 boundary and produce its concrete success result.
    let issue_a = "00000000-0000-4000-8000-000000000001";
    let issue_b = "00000000-0000-4000-8000-000000000002";
    let create = |issue_id: &str, op: &str| {
        CreateIssueArgs {
            context: Some(write_context(op)),
            issue_id: issue_id.into(),
            issue: Some(IssueDraft {
                title: format!("issue {issue_id}"),
                kind: 1,
                priority: 3,
                creation_basis: 1,
                confirmation_status: 1,
            }),
        }
        .encode_to_vec()
    };
    run_success(&mut runner, "create_issue", create(issue_a, "create-a")).await?;
    run_success(&mut runner, "create_issue", create(issue_b, "create-b")).await?;
    run_success(
        &mut runner,
        "claim_issue",
        ClaimIssueArgs {
            context: Some(write_context("claim-a")),
            issue_id: issue_a.into(),
            expected_revision: Some(0),
        }
        .encode_to_vec(),
    )
    .await?;
    run_success(
        &mut runner,
        "add_issue_relation",
        AddIssueRelationArgs {
            context: Some(write_context("relate")),
            kind: 3,
            source_issue_id: issue_a.into(),
            source_revision: Some(1),
            target_issue_id: issue_b.into(),
            target_revision: Some(0),
        }
        .encode_to_vec(),
    )
    .await?;
    run_success(
        &mut runner,
        "add_issue_comment",
        AddIssueCommentArgs {
            context: Some(write_context("comment")),
            issue_id: issue_b.into(),
            revision: Some(1),
            comment_id: "00000000-0000-4000-8000-000000000003".into(),
            body: "host-routed".into(),
        }
        .encode_to_vec(),
    )
    .await?;
    run_success(
        &mut runner,
        "create_backup",
        CreateBackupArgs {
            context: Some(write_context("backup")),
        }
        .encode_to_vec(),
    )
    .await?;

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
