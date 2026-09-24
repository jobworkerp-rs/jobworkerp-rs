//! End-to-end checks for hosts provisioned with the microsandbox local runtime.

use std::{
    collections::{HashMap, HashSet},
    process::Command,
    time::Duration,
};

use command_utils::util::shutdown::{ShutdownWait, create_lock_and_wait};
use futures::StreamExt;
use jobworkerp_runner::{
    jobworkerp::runner::{
        SandboxExecArgs, SandboxExecResult, SandboxRunnerSettings, SandboxVmConfig,
        sandbox_exec_result::Result as SandboxResult,
    },
    runner::{
        FeedData, RunnerTrait,
        cancellation::CancelMonitoring,
        sandbox::{SandboxCleanupRegistry, SandboxExecutionContext, SandboxRunner},
    },
};
use prost::Message;
use proto::jobworkerp::data::{JobId, WorkerId, result_output_item::Item};
use tokio::sync::watch;

fn settings() -> Vec<u8> {
    let image = std::env::var("SANDBOX_TEST_IMAGE").unwrap_or_else(|_| "python:3.12".into());
    SandboxRunnerSettings {
        vm: Some(SandboxVmConfig {
            image: Some(image.clone()),
            cpus: Some(1),
            memory_mib: Some(1024),
            root_disk_mib: Some(8192),
            ..Default::default()
        }),
        allowed_images: vec![image],
        ..Default::default()
    }
    .encode_to_vec()
}

fn make_runner(
    context: SandboxExecutionContext,
) -> (
    SandboxRunner,
    SandboxCleanupRegistry,
    watch::Sender<bool>,
    ShutdownWait,
) {
    let (lock, wait) = create_lock_and_wait();
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let registry = SandboxCleanupRegistry::new(lock, shutdown_receiver).unwrap();
    let mut runner = SandboxRunner::new_with_context(context);
    runner.set_cleanup_registry(registry.clone()).unwrap();
    (runner, registry, shutdown_sender, wait)
}

fn sandbox_records(worker_id: i64) -> HashSet<String> {
    let result = Command::new("msb")
        .args(["list", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "msb list failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let values: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let prefix = format!("jw-sbx-w{worker_id}-");
    values
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["name"].as_str())
        .filter(|name| name.starts_with(&prefix))
        .map(str::to_string)
        .collect()
}

async fn read_results(
    runner: &mut SandboxRunner,
    args: SandboxExecArgs,
    using: &str,
) -> (Vec<SandboxExecResult>, bool) {
    let stream = runner
        .run_stream(&args.encode_to_vec(), HashMap::new(), Some(using))
        .await
        .unwrap();
    let mut stream = Box::pin(stream);
    let mut results = Vec::new();
    let mut normal_end = false;
    while let Some(item) = tokio::time::timeout(Duration::from_secs(90), stream.next())
        .await
        .unwrap()
    {
        match item.item.unwrap() {
            Item::Data(bytes) => results.push(SandboxExecResult::decode(bytes.as_slice()).unwrap()),
            Item::End(trailer) => {
                assert!(
                    !trailer.metadata.contains_key("stream_error"),
                    "{trailer:?}"
                );
                normal_end = true;
            }
            Item::FinalCollected(_) => panic!("unexpected collected output"),
        }
    }
    (results, normal_end)
}

#[tokio::test]
#[ignore = "requires provisioned Linux/KVM microsandbox local runtime and a cached SANDBOX_TEST_IMAGE"]
async fn non_static_command_executes_and_reports_exit() {
    let before = sandbox_records(73101);
    let (mut runner, registry, shutdown_sender, mut wait) = make_runner(
        SandboxExecutionContext::non_static_worker(WorkerId { value: 73101 })
            .with_job_id(JobId { value: 73102 }),
    );
    runner.load(settings()).await.unwrap();
    let (results, end) = read_results(
        &mut runner,
        SandboxExecArgs {
            command: "sh".into(),
            args: vec!["-c".into(), "printf 'hello'".into()],
            ..Default::default()
        },
        "run",
    )
    .await;
    assert!(end);
    assert!(results.iter().any(|result| matches!(&result.result, Some(SandboxResult::Output(output)) if output.data == b"hello")));
    assert!(results.iter().any(|result| matches!(&result.result, Some(SandboxResult::Exit(exit)) if exit.exit_code == 0 && !exit.sandbox_id.is_empty())));
    shutdown_sender.send(true).unwrap();
    registry.shutdown().await;
    drop(runner);
    tokio::time::timeout(Duration::from_secs(30), wait.wait())
        .await
        .unwrap();
    assert_eq!(
        sandbox_records(73101),
        before,
        "non-static VM record must be removed"
    );
}

#[tokio::test]
#[ignore = "requires provisioned Linux/KVM microsandbox local runtime and a cached SANDBOX_TEST_IMAGE"]
async fn static_vm_reuses_files_and_client_feed_closes_stdin() {
    let before = sandbox_records(73103);
    let (mut runner, registry, shutdown_sender, mut wait) =
        make_runner(SandboxExecutionContext::static_worker(WorkerId {
            value: 73103,
        }));
    runner.load(settings()).await.unwrap();
    runner.set_job_context(JobId { value: 73104 });
    let (written, end) = read_results(
        &mut runner,
        SandboxExecArgs {
            command: "sh".into(),
            args: vec![
                "-c".into(),
                "printf persistent > /tmp/jobworkerp-sandbox-state".into(),
            ],
            ..Default::default()
        },
        "run",
    )
    .await;
    assert!(end && written.iter().any(|result| matches!(&result.result, Some(SandboxResult::Exit(exit)) if exit.exit_code == 0)));
    runner.verify_before_reuse().await.unwrap();
    runner.set_job_context(JobId { value: 73105 });
    let sender = runner
        .setup_client_stream_channel(Some("run_with_client"))
        .unwrap();
    let stream = runner
        .run_stream(
            &SandboxExecArgs {
                command: "sh".into(),
                args: vec![
                    "-c".into(),
                    "stty -echo; cat /tmp/jobworkerp-sandbox-state; cat".into(),
                ],
                ..Default::default()
            }
            .encode_to_vec(),
            HashMap::new(),
            Some("run_with_client"),
        )
        .await
        .unwrap();
    sender
        .send(FeedData {
            data: b" from client".to_vec(),
            is_final: true,
        })
        .await
        .unwrap();
    let mut stream = Box::pin(stream);
    let mut results = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(8), stream.next()).await {
            Ok(Some(item)) => results.push(item),
            Ok(None) => break,
            Err(_) => panic!("PTY did not finish after final feed; received {results:?}"),
        }
    }
    assert!(results.iter().any(|item| matches!(&item.item, Some(Item::Data(bytes)) if matches!(SandboxExecResult::decode(bytes.as_slice()).unwrap().result, Some(SandboxResult::Output(output)) if String::from_utf8_lossy(&output.data).contains("persistent")))));
    let output = results
        .iter()
        .filter_map(|item| match &item.item {
            Some(Item::Data(bytes)) => {
                match SandboxExecResult::decode(bytes.as_slice()).unwrap().result {
                    Some(SandboxResult::Output(output)) => Some(output.data),
                    _ => None,
                }
            }
            _ => None,
        })
        .flatten()
        .collect::<Vec<u8>>();
    assert!(
        output
            .windows(b"persistent from client".len())
            .any(|slice| slice == b"persistent from client"),
        "guest command must read the client feed, not just echo terminal input: {output:?}"
    );
    assert!(results.iter().any(|item| matches!(&item.item, Some(Item::End(trailer)) if !trailer.metadata.contains_key("stream_error"))));
    shutdown_sender.send(true).unwrap();
    registry.shutdown().await;
    drop(runner);
    tokio::time::timeout(Duration::from_secs(30), wait.wait())
        .await
        .unwrap();
    assert_eq!(
        sandbox_records(73103),
        before,
        "static VM record must be removed"
    );
}

#[tokio::test]
#[ignore = "requires provisioned Linux/KVM microsandbox local runtime and a cached SANDBOX_TEST_IMAGE"]
async fn failed_and_timed_out_non_static_commands_report_errors_and_remove_vms() {
    let before = sandbox_records(73106);
    let (mut runner, registry, shutdown_sender, mut wait) = make_runner(
        SandboxExecutionContext::non_static_worker(WorkerId { value: 73106 })
            .with_job_id(JobId { value: 73107 }),
    );
    runner.load(settings()).await.unwrap();
    let failed_stream = runner
        .run_stream(
            &SandboxExecArgs {
                command: "sh".into(),
                args: vec!["-c".into(), "exit 7".into()],
                treat_nonzero_as_error: true,
                ..Default::default()
            }
            .encode_to_vec(),
            HashMap::new(),
            Some("run"),
        )
        .await
        .unwrap();
    let failed = failed_stream.collect::<Vec<_>>().await;
    assert!(failed.iter().any(|item| matches!(&item.item, Some(Item::Data(bytes)) if matches!(SandboxExecResult::decode(bytes.as_slice()).unwrap().result, Some(SandboxResult::Exit(exit)) if exit.exit_code == 7))));
    assert!(failed.iter().any(|item| matches!(&item.item, Some(Item::End(trailer)) if matches!(proto::stream_error::parse_stream_error(trailer), proto::stream_error::StreamErrorOutcome::Error(error) if error.code == "EXECUTION_FAILED"))));

    runner.set_job_context(JobId { value: 73108 });
    let timed_out_stream = runner
        .run_stream(
            &SandboxExecArgs {
                command: "sh".into(),
                args: vec!["-c".into(), "sleep 10".into()],
                timeout_ms: Some(300),
                ..Default::default()
            }
            .encode_to_vec(),
            HashMap::new(),
            Some("run"),
        )
        .await
        .unwrap();
    let timed_out =
        tokio::time::timeout(Duration::from_secs(8), timed_out_stream.collect::<Vec<_>>())
            .await
            .unwrap();
    assert!(timed_out.iter().any(|item| matches!(&item.item, Some(Item::End(trailer)) if matches!(proto::stream_error::parse_stream_error(trailer), proto::stream_error::StreamErrorOutcome::Error(error) if error.code == "TIMEOUT"))), "{timed_out:?}");
    assert!(!timed_out.iter().any(|item| matches!(&item.item, Some(Item::Data(bytes)) if matches!(SandboxExecResult::decode(bytes.as_slice()).unwrap().result, Some(SandboxResult::Exit(_))))));

    shutdown_sender.send(true).unwrap();
    registry.shutdown().await;
    drop(runner);
    tokio::time::timeout(Duration::from_secs(30), wait.wait())
        .await
        .unwrap();
    assert_eq!(
        sandbox_records(73106),
        before,
        "failed and timed-out VMs must be removed"
    );
}

#[tokio::test]
#[ignore = "requires provisioned Linux/KVM microsandbox local runtime and a cached SANDBOX_TEST_IMAGE"]
async fn cancelled_non_static_command_reports_error_and_removes_vm() {
    let before = sandbox_records(73109);
    let (mut runner, registry, shutdown_sender, mut wait) = make_runner(
        SandboxExecutionContext::non_static_worker(WorkerId { value: 73109 })
            .with_job_id(JobId { value: 73110 }),
    );
    runner.load(settings()).await.unwrap();
    let stream = runner
        .run_stream(
            &SandboxExecArgs {
                command: "sh".into(),
                args: vec!["-c".into(), "sleep 10".into()],
                ..Default::default()
            }
            .encode_to_vec(),
            HashMap::new(),
            Some("run"),
        )
        .await
        .unwrap();
    runner.request_cancellation().await.unwrap();
    let results = tokio::time::timeout(Duration::from_secs(8), stream.collect::<Vec<_>>())
        .await
        .unwrap();
    assert!(results.iter().any(|item| matches!(&item.item, Some(Item::End(trailer)) if matches!(proto::stream_error::parse_stream_error(trailer), proto::stream_error::StreamErrorOutcome::Error(error) if error.code == "CANCELLED"))), "{results:?}");
    shutdown_sender.send(true).unwrap();
    registry.shutdown().await;
    drop(runner);
    tokio::time::timeout(Duration::from_secs(30), wait.wait())
        .await
        .unwrap();
    assert_eq!(
        sandbox_records(73109),
        before,
        "cancelled VM record must be removed"
    );
}

#[tokio::test]
#[ignore = "requires provisioned Linux/KVM microsandbox local runtime and a cached SANDBOX_TEST_IMAGE"]
async fn shutdown_stops_unpolled_exec_and_removes_its_vm() {
    let before = sandbox_records(73111);
    let (mut runner, registry, shutdown_sender, mut wait) = make_runner(
        SandboxExecutionContext::non_static_worker(WorkerId { value: 73111 })
            .with_job_id(JobId { value: 73112 }),
    );
    runner.load(settings()).await.unwrap();
    let stream = runner
        .run_stream(
            &SandboxExecArgs {
                command: "sh".into(),
                args: vec!["-c".into(), "sleep 10".into()],
                ..Default::default()
            }
            .encode_to_vec(),
            HashMap::new(),
            Some("run"),
        )
        .await
        .unwrap();

    shutdown_sender.send(true).unwrap();
    registry.shutdown().await;
    tokio::time::timeout(Duration::from_secs(30), wait.wait())
        .await
        .unwrap();
    assert_eq!(
        sandbox_records(73111),
        before,
        "shutdown must remove the VM even if the client never polls its output"
    );
    drop(stream);
    drop(runner);
}
