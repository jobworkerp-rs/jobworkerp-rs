#![recursion_limit = "256"]

use std::collections::HashMap;
use std::time::Duration;

use command_utils::protobuf::ProtobufDescriptor;
use command_utils::util::shutdown::create_lock_and_wait;
use futures::{StreamExt, stream};
use jobworkerp_runner::jobworkerp::runner::{
    SandboxAllowedHostMount, SandboxBindMount, SandboxExecArgs, SandboxNetworkConfig,
    SandboxNetworkDestination, SandboxNetworkRule, SandboxPortRange, SandboxRunnerSettings,
    SandboxVmConfig, sandbox_exec_result::Result as SandboxResult,
    sandbox_network_destination::Destination,
};
use jobworkerp_runner::runner::{
    CollectStreamFuture, FeedData, RunnerSpec, RunnerTrait, cancellation, cancellation_helper,
};
pub use jobworkerp_runner::{jobworkerp, schema_to_json_string};
use microsandbox::{NetworkAction, NetworkPolicy};
use prost::Message;
use proto::jobworkerp::data::{
    JobId, ResultOutputItem, Trailer, WorkerId, result_output_item::Item,
};
use sandbox::{
    SandboxCleanupRegistry, SandboxMode, SandboxRunner, build_network_policy, build_sandbox_name,
    encode_exit_result, encode_output_result, resolve_execution_settings, validate_settings,
};
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

#[path = "../src/runner/sandbox.rs"]
pub mod sandbox;

const DEFAULT_METHOD_NAME: &str = "run";

fn valid_settings() -> SandboxRunnerSettings {
    SandboxRunnerSettings {
        vm: Some(SandboxVmConfig {
            image: Some("python:3.12".to_string()),
            cpus: Some(2),
            memory_mib: Some(1024),
            root_disk_mib: Some(8192),
            working_dir: Some("/tmp".to_string()),
            ..Default::default()
        }),
        allowed_images: vec!["python:3.12".to_string()],
        ..Default::default()
    }
}

#[test]
fn accepts_valid_worker_settings_and_resolves_omitted_network_to_disabled() {
    let settings = validate_settings(&valid_settings()).expect("valid settings");

    assert_eq!(settings.vm.image, "python:3.12");
    assert!(settings.network.is_none());
    let mut maximum_sdk_cpus = valid_settings();
    maximum_sdk_cpus.vm.as_mut().unwrap().cpus = Some(u8::MAX.into());
    assert_eq!(
        validate_settings(&maximum_sdk_cpus).unwrap().vm.cpus,
        u8::MAX
    );
    assert!(
        validate_settings(&SandboxRunnerSettings {
            network: Some(SandboxNetworkConfig {
                enabled: Some(false),
                ..Default::default()
            }),
            ..valid_settings()
        })
        .unwrap()
        .network
        .is_none()
    );
}

#[test]
fn rejects_missing_or_invalid_required_vm_resources_and_unapproved_images() {
    let mut settings = valid_settings();
    settings.vm = None;
    assert!(validate_settings(&settings).is_err());

    let invalid_mutations: [fn(&mut SandboxRunnerSettings); 7] = [
        |settings: &mut SandboxRunnerSettings| {
            settings.vm.as_mut().unwrap().cpus = Some(0);
        },
        |settings: &mut SandboxRunnerSettings| {
            settings.vm.as_mut().unwrap().cpus = Some(256);
        },
        |settings: &mut SandboxRunnerSettings| {
            settings.vm.as_mut().unwrap().memory_mib = Some(0);
        },
        |settings: &mut SandboxRunnerSettings| {
            settings.vm.as_mut().unwrap().root_disk_mib = None;
        },
        |settings: &mut SandboxRunnerSettings| {
            settings.vm.as_mut().unwrap().image = Some("unlisted:latest".to_string());
        },
        |settings: &mut SandboxRunnerSettings| {
            settings.allowed_images.clear();
        },
        |settings: &mut SandboxRunnerSettings| {
            settings.default_exec_timeout_ms = Some(0);
        },
    ];
    for mutate in invalid_mutations {
        let mut settings = valid_settings();
        mutate(&mut settings);
        assert!(validate_settings(&settings).is_err());
    }
}

#[test]
fn rejects_invalid_mount_roots_symlink_escape_and_guest_path_traversal() {
    let host_root = tempfile::tempdir().unwrap();
    let allowed = SandboxAllowedHostMount {
        host_root: host_root.path().to_string_lossy().into_owned(),
        allow_write: false,
        allow_exec: false,
    };
    let host_file = host_root.path().join("input.txt");
    std::fs::write(&host_file, "data").unwrap();

    let mut settings = valid_settings();
    settings.allowed_host_mounts = vec![allowed];
    settings.vm.as_mut().unwrap().mounts = vec![SandboxBindMount {
        host_path: host_file.to_string_lossy().into_owned(),
        guest_path: "/inputs/data.txt".to_string(),
        writable: false,
        executable: false,
    }];
    let validated = validate_settings(&settings).expect("authorized file mount");
    assert_eq!(validated.vm.mounts[0].guest_path, "/inputs/data.txt");
    assert!(validated.vm.mounts[0].host_path.is_absolute());

    let mut duplicate_guest_path = valid_settings();
    duplicate_guest_path.allowed_host_mounts = vec![SandboxAllowedHostMount {
        host_root: host_root.path().to_string_lossy().into_owned(),
        allow_write: false,
        allow_exec: false,
    }];
    duplicate_guest_path.vm.as_mut().unwrap().mounts = vec![
        SandboxBindMount {
            host_path: host_file.to_string_lossy().into_owned(),
            guest_path: "/inputs/data.txt".to_string(),
            writable: false,
            executable: false,
        },
        SandboxBindMount {
            host_path: host_file.to_string_lossy().into_owned(),
            guest_path: "/inputs/data.txt".to_string(),
            writable: false,
            executable: false,
        },
    ];
    assert!(validate_settings(&duplicate_guest_path).is_err());

    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("outside.txt");
    std::fs::write(&outside_file, "outside").unwrap();
    let escape = host_root.path().join("escape.txt");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside_file, &escape).unwrap();

    for invalid_mount in [
        SandboxBindMount {
            host_path: outside_file.to_string_lossy().into_owned(),
            guest_path: "/inputs/outside.txt".to_string(),
            writable: false,
            executable: false,
        },
        SandboxBindMount {
            host_path: escape.to_string_lossy().into_owned(),
            guest_path: "/inputs/escape.txt".to_string(),
            writable: false,
            executable: false,
        },
        SandboxBindMount {
            host_path: host_file.to_string_lossy().into_owned(),
            guest_path: "/inputs/../etc/passwd".to_string(),
            writable: false,
            executable: false,
        },
    ] {
        let mut settings = valid_settings();
        settings.allowed_host_mounts = vec![SandboxAllowedHostMount {
            host_root: host_root.path().to_string_lossy().into_owned(),
            allow_write: false,
            allow_exec: false,
        }];
        settings.vm.as_mut().unwrap().mounts = vec![invalid_mount];
        assert!(validate_settings(&settings).is_err());
    }
}

#[test]
fn mount_permissions_cannot_exceed_allowlist() {
    let host_root = tempfile::tempdir().unwrap();
    let file = host_root.path().join("script.sh");
    std::fs::write(&file, "exit 0").unwrap();

    for (writable, executable, allow_write, allow_exec) in
        [(true, false, false, true), (false, true, true, false)]
    {
        let mut settings = valid_settings();
        settings.allowed_host_mounts = vec![SandboxAllowedHostMount {
            host_root: host_root.path().to_string_lossy().into_owned(),
            allow_write,
            allow_exec,
        }];
        settings.vm.as_mut().unwrap().mounts = vec![SandboxBindMount {
            host_path: file.to_string_lossy().into_owned(),
            guest_path: "/mnt/data".to_string(),
            writable,
            executable,
        }];
        assert!(validate_settings(&settings).is_err());
    }
}

#[test]
fn network_is_deny_by_default_and_metadata_denial_precedes_allow_any() {
    let network = SandboxNetworkConfig {
        enabled: Some(true),
        profiles: vec!["public".to_string()],
        max_tcp_connections: Some(64),
        max_udp_connections: Some(32),
        rules: vec![SandboxNetworkRule {
            action: "allow".to_string(),
            destination: Some(SandboxNetworkDestination {
                destination: Some(Destination::Any(true)),
            }),
            ..Default::default()
        }],
    };

    let policy = build_network_policy(&network).expect("valid network policy");
    assert_eq!(policy.default_egress, NetworkAction::Deny);
    assert_eq!(policy.default_ingress, NetworkAction::Deny);
    assert_eq!(policy.rules[0].action, NetworkAction::Deny);
    let metadata_rule = serde_json::to_value(&policy.rules[0]).unwrap();
    assert_eq!(metadata_rule["destination"]["group"], "metadata");
    assert_eq!(policy.rules[1].action, NetworkAction::Allow);
    assert!(
        policy.rules.len() >= 3,
        "profile rules follow explicit rules"
    );
    assert!(matches!(
        NetworkPolicy::from_profiles(std::iter::empty::<microsandbox::NetworkProfile>())
            .default_ingress,
        NetworkAction::Allow
    ));
}

#[test]
fn rejects_open_ended_or_malformed_network_limits_and_job_permissions() {
    let mut settings = valid_settings();
    settings.network = Some(SandboxNetworkConfig {
        enabled: Some(true),
        max_tcp_connections: Some(0),
        max_udp_connections: Some(1),
        ..Default::default()
    });
    assert!(validate_settings(&settings).is_err());

    let mut settings = valid_settings();
    settings.network = Some(SandboxNetworkConfig {
        enabled: Some(true),
        max_tcp_connections: Some(1),
        max_udp_connections: None,
        ..Default::default()
    });
    assert!(validate_settings(&settings).is_err());

    for network in [
        SandboxNetworkConfig {
            enabled: Some(true),
            max_tcp_connections: Some(1),
            max_udp_connections: Some(1),
            rules: vec![SandboxNetworkRule {
                action: "allow".to_string(),
                destination: None,
                ..Default::default()
            }],
            ..Default::default()
        },
        SandboxNetworkConfig {
            enabled: Some(false),
            profiles: vec!["public".to_string()],
            ..Default::default()
        },
        SandboxNetworkConfig {
            enabled: Some(true),
            max_tcp_connections: Some(1),
            max_udp_connections: Some(1),
            rules: vec![SandboxNetworkRule {
                action: "allow".to_string(),
                destination: Some(SandboxNetworkDestination {
                    destination: Some(Destination::Any(false)),
                }),
                protocols: vec!["icmp".to_string()],
                ..Default::default()
            }],
            ..Default::default()
        },
        SandboxNetworkConfig {
            enabled: Some(true),
            max_tcp_connections: Some(1),
            max_udp_connections: Some(1),
            rules: vec![SandboxNetworkRule {
                action: "allow".to_string(),
                destination: Some(SandboxNetworkDestination {
                    destination: Some(Destination::Group("public".to_string())),
                }),
                ports: vec![SandboxPortRange { start: 0, end: 1 }],
                ..Default::default()
            }],
            ..Default::default()
        },
    ] {
        let mut settings = valid_settings();
        settings.network = Some(network);
        assert!(validate_settings(&settings).is_err());
    }

    let validated = validate_settings(&valid_settings()).unwrap();
    for mode in [SandboxMode::Static, SandboxMode::NonStatic] {
        let args = SandboxExecArgs {
            network: Some(SandboxNetworkConfig {
                enabled: Some(true),
                max_tcp_connections: Some(100),
                max_udp_connections: Some(100),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(resolve_execution_settings(&validated, &args, mode).is_err());
    }

    let mut settings = valid_settings();
    settings.network = Some(SandboxNetworkConfig {
        enabled: Some(true),
        max_tcp_connections: Some(8),
        max_udp_connections: Some(8),
        ..Default::default()
    });
    let validated = validate_settings(&settings).unwrap();
    let job_network_off = SandboxExecArgs {
        network: Some(SandboxNetworkConfig {
            enabled: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(
        resolve_execution_settings(&validated, &job_network_off, SandboxMode::NonStatic)
            .unwrap()
            .network
            .is_none()
    );
}

#[test]
fn job_vm_resources_may_only_shrink_and_environment_keys_are_isolated() {
    let validated = validate_settings(&valid_settings()).unwrap();
    let reduced = SandboxExecArgs {
        vm: Some(SandboxVmConfig {
            cpus: Some(1),
            memory_mib: Some(512),
            ..Default::default()
        }),
        env: HashMap::from([("MODE".to_string(), "exec".to_string())]),
        ..Default::default()
    };
    let resolved = resolve_execution_settings(&validated, &reduced, SandboxMode::NonStatic)
        .expect("job can reduce resources");
    assert_eq!(resolved.vm.cpus, 1);
    assert_eq!(resolved.vm.memory_mib, 512);
    assert_eq!(
        resolved.exec_env.get("MODE").map(String::as_str),
        Some("exec")
    );

    let job_environment_override = SandboxExecArgs {
        vm: Some(SandboxVmConfig {
            root_disk_mib: Some(4096),
            max_duration_sec: Some(120),
            idle_timeout_sec: Some(30),
            env: HashMap::from([("JOB_KEY".to_string(), "vm".to_string())]),
            ..Default::default()
        }),
        env: HashMap::from([("JOB_KEY".to_string(), "exec".to_string())]),
        timeout_ms: Some(90_000),
        ..Default::default()
    };
    let resolved = resolve_execution_settings(
        &validated,
        &job_environment_override,
        SandboxMode::NonStatic,
    )
    .unwrap();
    assert_eq!(
        resolved.vm.env.get("JOB_KEY").map(String::as_str),
        Some("vm")
    );
    assert_eq!(resolved.vm.root_disk_mib, 4096);
    assert_eq!(resolved.vm.max_duration_sec, Some(120));
    assert_eq!(resolved.vm.idle_timeout_sec, Some(30));
    assert_eq!(
        resolved.exec_env.get("JOB_KEY").map(String::as_str),
        Some("exec")
    );
    assert_eq!(resolved.exec_timeout, Some(Duration::from_millis(90_000)));

    let mut default_timeout_settings = valid_settings();
    default_timeout_settings.default_exec_timeout_ms = Some(60_000);
    let defaults = validate_settings(&default_timeout_settings).unwrap();
    let default_execution = resolve_execution_settings(
        &defaults,
        &SandboxExecArgs::default(),
        SandboxMode::NonStatic,
    )
    .unwrap();
    assert_eq!(
        default_execution.exec_timeout,
        Some(Duration::from_millis(60_000))
    );

    let mut lifetime_limited_settings = valid_settings();
    lifetime_limited_settings
        .vm
        .as_mut()
        .unwrap()
        .max_duration_sec = Some(100);
    lifetime_limited_settings
        .vm
        .as_mut()
        .unwrap()
        .idle_timeout_sec = Some(60);
    let lifetime_limits = validate_settings(&lifetime_limited_settings).unwrap();
    let expanded_lifetime = SandboxExecArgs {
        vm: Some(SandboxVmConfig {
            max_duration_sec: Some(101),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(
        resolve_execution_settings(&lifetime_limits, &expanded_lifetime, SandboxMode::NonStatic)
            .is_err()
    );

    for args in [
        SandboxExecArgs {
            vm: Some(SandboxVmConfig {
                cpus: Some(3),
                ..Default::default()
            }),
            ..Default::default()
        },
        SandboxExecArgs {
            vm: Some(SandboxVmConfig {
                env: HashMap::from([("WORKER_SECRET".to_string(), "override".to_string())]),
                ..Default::default()
            }),
            ..Default::default()
        },
        SandboxExecArgs {
            env: HashMap::from([("WORKER_SECRET".to_string(), "override".to_string())]),
            ..Default::default()
        },
    ] {
        let mut settings = valid_settings();
        settings
            .vm
            .as_mut()
            .unwrap()
            .env
            .insert("WORKER_SECRET".to_string(), "worker-value".to_string());
        let validated = validate_settings(&settings).unwrap();
        assert!(resolve_execution_settings(&validated, &args, SandboxMode::NonStatic).is_err());
    }

    let static_override = SandboxExecArgs {
        vm: Some(SandboxVmConfig {
            cpus: Some(1),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(resolve_execution_settings(&validated, &static_override, SandboxMode::Static).is_err());
}

#[test]
fn sandbox_names_are_bounded_and_include_process_and_runner_generations() {
    let worker = WorkerId { value: 42 };
    let job = JobId { value: 99 };
    let first = build_sandbox_name(worker, Some(job), "process-a", "runner-a").unwrap();
    let second = build_sandbox_name(worker, Some(job), "process-b", "runner-a").unwrap();
    let third = build_sandbox_name(worker, Some(job), "process-a", "runner-b").unwrap();
    let static_name =
        build_sandbox_name(WorkerId { value: 42 }, None, "process-a", "runner-a").unwrap();

    assert!(first.len() <= 128);
    assert_ne!(first, second);
    assert_ne!(first, third);
    assert_ne!(first, static_name);
}

#[test]
fn exec_output_and_exit_events_keep_raw_bytes_and_exit_metadata() {
    let output = encode_output_result("stderr", vec![0, 255, b'x']);
    let Some(SandboxResult::Output(output)) = output.result else {
        panic!("expected an output event");
    };
    assert_eq!(output.stream, "stderr");
    assert_eq!(output.data, vec![0, 255, b'x']);

    let exit = encode_exit_result(7, Duration::from_millis(15), "local:41");
    let Some(SandboxResult::Exit(exit)) = exit.result else {
        panic!("expected an exit event");
    };
    assert_eq!(exit.exit_code, 7);
    assert_eq!(exit.execution_time_ms, 15);
    assert_eq!(exit.sandbox_id, "local:41");
}

#[tokio::test]
async fn stream_collection_requires_a_normal_end_and_rejects_error_end() {
    let runner = SandboxRunner::new();
    let data = encode_exit_result(0, Duration::from_millis(1), "local:1").encode_to_vec();
    let normal = stream::iter([
        ResultOutputItem {
            item: Some(Item::Data(data.clone())),
        },
        ResultOutputItem {
            item: Some(Item::End(Trailer {
                metadata: HashMap::from([("trace".to_string(), "abc".to_string())]),
            })),
        },
    ])
    .boxed();
    let (collected, metadata) = runner.collect_stream(normal, None).await.unwrap();
    assert_eq!(collected, data);
    assert_eq!(metadata.get("trace").map(String::as_str), Some("abc"));

    let error_end = stream::iter([ResultOutputItem {
        item: Some(Item::End(
            proto::stream_error::build_stream_error_trailer(
                HashMap::new(),
                "TIMEOUT",
                "execution timed out",
                "SANDBOX",
            )
            .unwrap(),
        )),
    }])
    .boxed();
    assert!(runner.collect_stream(error_end, None).await.is_err());

    let malformed_end = stream::iter([ResultOutputItem {
        item: Some(Item::End(Trailer {
            metadata: HashMap::from([("stream_error".to_string(), "not-json".to_string())]),
        })),
    }])
    .boxed();
    assert!(runner.collect_stream(malformed_end, None).await.is_err());

    let missing_end = stream::iter([ResultOutputItem {
        item: Some(Item::Data(data)),
    }])
    .boxed();
    assert!(runner.collect_stream(missing_end, None).await.is_err());
}

#[tokio::test]
async fn shutdown_cancels_unpolled_exec_and_cleans_registered_idle_vm() {
    let (lock, mut wait) = create_lock_and_wait();
    let (shutdown_sender, shutdown) = watch::channel(false);
    let registry = SandboxCleanupRegistry::new(lock, shutdown).unwrap();
    let cancel = CancellationToken::new();
    let (cleanup_started, cleanup_started_rx) = oneshot::channel();
    let (allow_cleanup, allow_cleanup_rx) = oneshot::channel();

    let mut registration = registry.register_task(Some(cancel.clone())).await.unwrap();
    registration
        .set_cleanup(Box::pin(async move {
            let _ = cleanup_started.send(());
            let _ = allow_cleanup_rx.await;
        }))
        .await
        .unwrap();

    shutdown_sender.send(true).unwrap();

    cleanup_started_rx.await.unwrap();
    assert!(cancel.is_cancelled());
    assert!(
        tokio::time::timeout(Duration::from_millis(25), wait.wait())
            .await
            .is_err()
    );
    let _ = allow_cleanup.send(());
    tokio::time::timeout(Duration::from_secs(1), wait.wait())
        .await
        .expect("shutdown lock is released after cleanup");
}

#[tokio::test]
async fn early_registration_drop_keeps_shutdown_lock_until_cleanup_finishes() {
    let (lock, mut wait) = create_lock_and_wait();
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let registry = SandboxCleanupRegistry::new(lock, shutdown).unwrap();
    let (cleanup_started, cleanup_started_rx) = oneshot::channel();
    let (allow_cleanup, allow_cleanup_rx) = oneshot::channel();

    let mut registration = registry.register_task(None).await.unwrap();
    registration
        .set_cleanup(Box::pin(async move {
            let _ = cleanup_started.send(());
            let _ = allow_cleanup_rx.await;
        }))
        .await
        .unwrap();
    drop(registration);

    cleanup_started_rx.await.unwrap();
    registry.shutdown().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(25), wait.wait())
            .await
            .is_err()
    );
    let _ = allow_cleanup.send(());
    tokio::time::timeout(Duration::from_secs(1), wait.wait())
        .await
        .expect("cleanup task releases its shutdown lock");
}

#[tokio::test]
async fn abandoned_registration_without_vm_releases_shutdown_lock() {
    let (lock, mut wait) = create_lock_and_wait();
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let registry = SandboxCleanupRegistry::new(lock, shutdown).unwrap();
    let registration = registry.register_task(None).await.unwrap();

    // Handoff can be cancelled while the VM identity is still unavailable.
    drop(registration);
    registry.shutdown().await;
    tokio::time::timeout(Duration::from_secs(1), wait.wait())
        .await
        .expect("a registration without a VM cannot block worker-only shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn early_drop_without_a_current_runtime_uses_the_registered_runtime_handle() {
    let (lock, mut wait) = create_lock_and_wait();
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let registry = SandboxCleanupRegistry::new(lock, shutdown).unwrap();
    let (cleanup_started, cleanup_started_rx) = oneshot::channel();
    let (allow_cleanup, allow_cleanup_rx) = oneshot::channel();
    let mut registration = registry.register_task(None).await.unwrap();
    registration
        .set_cleanup(Box::pin(async move {
            let _ = cleanup_started.send(());
            let _ = allow_cleanup_rx.await;
        }))
        .await
        .unwrap();

    tokio::task::spawn_blocking(move || drop(registration))
        .await
        .unwrap();
    cleanup_started_rx.await.unwrap();
    registry.shutdown().await;
    let _ = allow_cleanup.send(());
    tokio::time::timeout(Duration::from_secs(1), wait.wait())
        .await
        .expect("cleanup keeps and releases its lock without a current runtime");
}

#[tokio::test]
async fn shutdown_during_vm_creation_cancels_creation_and_runs_late_cleanup() {
    let (lock, mut wait) = create_lock_and_wait();
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let registry = SandboxCleanupRegistry::new(lock, shutdown).unwrap();
    let cancel = CancellationToken::new();
    let (cleanup_done, cleanup_done_rx) = oneshot::channel();

    // Register before VM creation; cleanup becomes available only after creation returns.
    let mut registration = registry.register_task(Some(cancel.clone())).await.unwrap();
    registry.shutdown().await;
    assert!(cancel.is_cancelled());
    assert!(registry.register_task(None).await.is_err());

    registration
        .set_cleanup(Box::pin(async move {
            let _ = cleanup_done.send(());
        }))
        .await
        .unwrap();
    cleanup_done_rx.await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), wait.wait())
        .await
        .expect("late VM cleanup releases the registered lock");
}

#[test]
fn cleanup_registry_requires_a_tokio_runtime() {
    let (lock, _wait) = create_lock_and_wait();
    let (_shutdown_sender, shutdown) = watch::channel(false);

    assert!(SandboxCleanupRegistry::new(lock, shutdown).is_err());
}

#[tokio::test]
async fn sandbox_runner_accepts_registry_only_before_load() {
    let (lock, _wait) = create_lock_and_wait();
    let (_shutdown_sender, shutdown) = watch::channel(false);
    let registry = SandboxCleanupRegistry::new(lock, shutdown).unwrap();
    let mut runner = SandboxRunner::new();
    runner.set_cleanup_registry(registry.clone()).unwrap();

    runner
        .set_worker_context(WorkerId { value: 123 }, SandboxMode::NonStatic)
        .unwrap();
    runner.load(valid_settings().encode_to_vec()).await.unwrap();

    assert!(runner.set_cleanup_registry(registry).is_err());
}

#[test]
fn runner_schema_uses_two_distinct_streaming_methods_and_self_contained_proto() {
    let runner = SandboxRunner::new();
    assert_eq!(runner.name(), "SANDBOX");
    let settings_proto = runner.runner_settings_proto();
    assert!(settings_proto.contains("message SandboxRunnerSettings"));
    assert!(
        !settings_proto
            .lines()
            .any(|line| line.trim_start().starts_with("import "))
    );
    let settings_descriptor = ProtobufDescriptor::new(&settings_proto).unwrap();
    assert_eq!(
        settings_descriptor.get_messages()[0].name(),
        "SandboxRunnerSettings"
    );

    let methods = runner.method_proto_map();
    assert_eq!(methods.len(), 2);
    let run = methods.get(DEFAULT_METHOD_NAME).unwrap();
    let client = methods.get("run_with_client").unwrap();
    assert_eq!(
        run.output_type,
        proto::jobworkerp::data::StreamingOutputType::Streaming as i32
    );
    assert!(!run.require_client_stream);
    assert_eq!(
        client.output_type,
        proto::jobworkerp::data::StreamingOutputType::Streaming as i32
    );
    assert!(client.require_client_stream);
    assert_eq!(client.client_stream_data_proto.as_deref(), Some(""));
    for schema in [&run.args_proto, &client.args_proto] {
        assert!(schema.contains("message SandboxExecArgs"));
        assert!(
            !schema
                .lines()
                .any(|line| line.trim_start().starts_with("import "))
        );
    }
    assert!(run.result_proto.contains("message SandboxExecResult"));
    let args_descriptor = ProtobufDescriptor::new(&run.args_proto).unwrap();
    assert_eq!(args_descriptor.get_messages()[0].name(), "SandboxExecArgs");
    let result_descriptor = ProtobufDescriptor::new(&run.result_proto).unwrap();
    assert_eq!(
        result_descriptor.get_messages()[0].name(),
        "SandboxExecResult"
    );
}
