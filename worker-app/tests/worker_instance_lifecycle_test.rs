//! Worker Instance Lifecycle Integration Tests
//!
//! Tests for worker instance registration, heartbeat, and unregistration lifecycle.
//! These tests verify the WorkerInstanceRegistrar behavior in both Standalone and Scalable modes.

use std::sync::Arc;
use std::time::Duration;

use infra::infra::worker_instance::memory::MemoryWorkerInstanceRepository;
use infra::infra::worker_instance::WorkerInstanceRepository;
use jobworkerp_base::worker_instance_config::WorkerInstanceConfig;
use proto::jobworkerp::data::StorageType;
use tokio::sync::watch;
use tokio::time::timeout;
use worker_app::worker::instance_registrar::WorkerInstanceRegistrar;

fn create_test_registrar(
    instance_id: i64,
    repository: Arc<dyn WorkerInstanceRepository>,
    config: WorkerInstanceConfig,
    storage_type: StorageType,
) -> WorkerInstanceRegistrar {
    WorkerInstanceRegistrar::new(
        instance_id,
        "192.168.1.100".to_string(),
        Some("test-worker".to_string()),
        vec![("".to_string(), 4), ("priority".to_string(), 2)],
        repository,
        config,
        storage_type,
    )
}

fn create_enabled_config() -> WorkerInstanceConfig {
    WorkerInstanceConfig {
        enabled: true,
        heartbeat_interval_sec: 1, // Short interval for testing
        timeout_sec: 3,
        cleanup_interval_sec: 5,
    }
}

/// Test: Full lifecycle in Standalone mode
/// Verifies: register -> heartbeat -> unregister sequence
#[tokio::test]
async fn test_full_lifecycle_standalone() {
    let repo = Arc::new(MemoryWorkerInstanceRepository::new());
    let config = create_enabled_config();
    let instance_id = 10001;

    let registrar = Arc::new(create_test_registrar(
        instance_id,
        repo.clone(),
        config,
        StorageType::Standalone,
    ));

    // Register
    registrar.register().await.unwrap();

    // Verify registered
    let found = repo
        .find(&proto::jobworkerp::data::WorkerInstanceId { value: instance_id })
        .await
        .unwrap();
    assert!(found.is_some(), "Instance should be registered");

    let instance = found.unwrap();
    assert_eq!(instance.data.as_ref().unwrap().ip_address, "192.168.1.100");
    assert_eq!(
        instance.data.as_ref().unwrap().hostname,
        Some("test-worker".to_string())
    );
    assert_eq!(instance.data.as_ref().unwrap().channels.len(), 2);

    // Unregister
    registrar.unregister().await.unwrap();

    // Verify unregistered
    let found = repo
        .find(&proto::jobworkerp::data::WorkerInstanceId { value: instance_id })
        .await
        .unwrap();
    assert!(found.is_none(), "Instance should be unregistered");
}

/// Test: Full lifecycle in Scalable mode
/// Verifies: register -> heartbeat -> unregister sequence with Scalable storage type
#[tokio::test]
async fn test_full_lifecycle_scalable() {
    let repo = Arc::new(MemoryWorkerInstanceRepository::new());
    let config = create_enabled_config();
    let instance_id = 10002;

    let registrar = Arc::new(create_test_registrar(
        instance_id,
        repo.clone(),
        config,
        StorageType::Scalable,
    ));

    // Register
    registrar.register().await.unwrap();

    // Verify registered
    let found = repo
        .find(&proto::jobworkerp::data::WorkerInstanceId { value: instance_id })
        .await
        .unwrap();
    assert!(found.is_some(), "Instance should be registered");

    // Verify storage type
    assert_eq!(registrar.storage_type(), StorageType::Scalable);

    // Unregister
    registrar.unregister().await.unwrap();

    // Verify unregistered
    let found = repo
        .find(&proto::jobworkerp::data::WorkerInstanceId { value: instance_id })
        .await
        .unwrap();
    assert!(found.is_none(), "Instance should be unregistered");
}

/// Test: Heartbeat updates last_heartbeat timestamp
/// Verifies: heartbeat loop updates the timestamp correctly
#[tokio::test]
async fn test_heartbeat_updates_timestamp() {
    let repo = Arc::new(MemoryWorkerInstanceRepository::new());
    let config = WorkerInstanceConfig {
        enabled: true,
        heartbeat_interval_sec: 1, // 1 second for quick testing
        timeout_sec: 10,
        cleanup_interval_sec: 30,
    };
    let instance_id = 10003;

    let registrar = Arc::new(create_test_registrar(
        instance_id,
        repo.clone(),
        config,
        StorageType::Standalone,
    ));

    // Register
    registrar.register().await.unwrap();

    // Get initial heartbeat
    let initial = repo
        .find(&proto::jobworkerp::data::WorkerInstanceId { value: instance_id })
        .await
        .unwrap()
        .unwrap();
    let initial_heartbeat = initial.data.as_ref().unwrap().last_heartbeat;

    // Create shutdown channel
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Start heartbeat loop in background
    let registrar_clone = registrar.clone();
    let heartbeat_handle =
        tokio::spawn(async move { registrar_clone.start_heartbeat_loop(shutdown_rx).await });

    // Wait for at least one heartbeat cycle
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Verify heartbeat was updated
    let updated = repo
        .find(&proto::jobworkerp::data::WorkerInstanceId { value: instance_id })
        .await
        .unwrap()
        .unwrap();
    let updated_heartbeat = updated.data.as_ref().unwrap().last_heartbeat;

    assert!(
        updated_heartbeat > initial_heartbeat,
        "Heartbeat should be updated: initial={}, updated={}",
        initial_heartbeat,
        updated_heartbeat
    );

    // Shutdown heartbeat loop
    shutdown_tx.send(true).unwrap();

    // Wait for heartbeat loop to finish with timeout
    let result = timeout(Duration::from_secs(5), heartbeat_handle).await;
    assert!(
        result.is_ok(),
        "Heartbeat loop should finish within timeout"
    );

    // Cleanup
    registrar.unregister().await.unwrap();
}

/// Test: Unregister removes instance from repository
/// Verifies: after unregister, find returns None
#[tokio::test]
async fn test_unregister_removes_instance() {
    let repo = Arc::new(MemoryWorkerInstanceRepository::new());
    let config = create_enabled_config();
    let instance_id = 10004;

    let registrar =
        create_test_registrar(instance_id, repo.clone(), config, StorageType::Standalone);

    // Register
    registrar.register().await.unwrap();

    // Verify exists
    let found = repo
        .find(&proto::jobworkerp::data::WorkerInstanceId { value: instance_id })
        .await
        .unwrap();
    assert!(found.is_some());

    // Unregister
    registrar.unregister().await.unwrap();

    // Verify removed
    let found = repo
        .find(&proto::jobworkerp::data::WorkerInstanceId { value: instance_id })
        .await
        .unwrap();
    assert!(
        found.is_none(),
        "Instance should be removed after unregister"
    );

    // Unregister again (should not error)
    registrar.unregister().await.unwrap();
}

/// Test: Re-registration when instance is missing
/// Verifies: heartbeat loop re-registers when instance is not found
#[tokio::test]
async fn test_re_register_on_missing() {
    let repo = Arc::new(MemoryWorkerInstanceRepository::new());
    let config = WorkerInstanceConfig {
        enabled: true,
        heartbeat_interval_sec: 1,
        timeout_sec: 10,
        cleanup_interval_sec: 30,
    };
    let instance_id = 10005;

    let registrar = Arc::new(create_test_registrar(
        instance_id,
        repo.clone(),
        config,
        StorageType::Standalone,
    ));

    // Register
    registrar.register().await.unwrap();

    // Manually delete the instance (simulating external deletion)
    repo.delete(&proto::jobworkerp::data::WorkerInstanceId { value: instance_id })
        .await
        .unwrap();

    // Verify deleted
    let found = repo
        .find(&proto::jobworkerp::data::WorkerInstanceId { value: instance_id })
        .await
        .unwrap();
    assert!(found.is_none(), "Instance should be deleted");

    // Create shutdown channel
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Start heartbeat loop
    let registrar_clone = registrar.clone();
    let heartbeat_handle =
        tokio::spawn(async move { registrar_clone.start_heartbeat_loop(shutdown_rx).await });

    // Wait for heartbeat to detect missing instance and re-register
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Verify re-registered
    let found = repo
        .find(&proto::jobworkerp::data::WorkerInstanceId { value: instance_id })
        .await
        .unwrap();
    assert!(
        found.is_some(),
        "Instance should be re-registered after heartbeat detects missing"
    );

    // Shutdown
    shutdown_tx.send(true).unwrap();
    let _ = timeout(Duration::from_secs(5), heartbeat_handle).await;

    // Cleanup
    registrar.unregister().await.unwrap();
}

/// Test: Disabled registration does nothing
/// Verifies: when config.enabled=false, register/unregister are no-ops
#[tokio::test]
async fn test_disabled_registration() {
    let repo = Arc::new(MemoryWorkerInstanceRepository::new());
    let config = WorkerInstanceConfig {
        enabled: false,
        heartbeat_interval_sec: 1,
        timeout_sec: 10,
        cleanup_interval_sec: 30,
    };
    let instance_id = 10006;

    let registrar =
        create_test_registrar(instance_id, repo.clone(), config, StorageType::Standalone);

    // Register (should be no-op)
    registrar.register().await.unwrap();

    // Verify NOT registered
    let found = repo
        .find(&proto::jobworkerp::data::WorkerInstanceId { value: instance_id })
        .await
        .unwrap();
    assert!(
        found.is_none(),
        "Instance should NOT be registered when disabled"
    );

    // Unregister (should also be no-op)
    registrar.unregister().await.unwrap();
}

/// Test: Heartbeat loop shutdown on signal
/// Verifies: heartbeat loop terminates gracefully on shutdown signal
#[tokio::test]
async fn test_heartbeat_loop_shutdown() {
    let repo = Arc::new(MemoryWorkerInstanceRepository::new());
    let config = WorkerInstanceConfig {
        enabled: true,
        heartbeat_interval_sec: 10, // Long interval
        timeout_sec: 30,
        cleanup_interval_sec: 60,
    };
    let instance_id = 10007;

    let registrar = Arc::new(create_test_registrar(
        instance_id,
        repo.clone(),
        config,
        StorageType::Standalone,
    ));

    registrar.register().await.unwrap();

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let registrar_clone = registrar.clone();
    let heartbeat_handle =
        tokio::spawn(async move { registrar_clone.start_heartbeat_loop(shutdown_rx).await });

    // Send shutdown signal immediately
    shutdown_tx.send(true).unwrap();

    // Heartbeat loop should exit quickly
    let result = timeout(Duration::from_secs(2), heartbeat_handle).await;
    assert!(
        result.is_ok(),
        "Heartbeat loop should terminate quickly on shutdown signal"
    );

    registrar.unregister().await.unwrap();
}

/// Test: Multiple instances can coexist
/// Verifies: multiple registrars can register without conflict
#[tokio::test]
async fn test_multiple_instances_coexist() {
    let repo = Arc::new(MemoryWorkerInstanceRepository::new());
    let config = create_enabled_config();

    let registrar1 =
        create_test_registrar(20001, repo.clone(), config.clone(), StorageType::Standalone);
    let registrar2 =
        create_test_registrar(20002, repo.clone(), config.clone(), StorageType::Standalone);
    let registrar3 = create_test_registrar(20003, repo.clone(), config, StorageType::Standalone);

    // Register all
    registrar1.register().await.unwrap();
    registrar2.register().await.unwrap();
    registrar3.register().await.unwrap();

    // Verify all registered
    let all = repo.find_all().await.unwrap();
    assert_eq!(all.len(), 3, "All three instances should be registered");

    // Unregister one
    registrar2.unregister().await.unwrap();

    // Verify only two remain
    let all = repo.find_all().await.unwrap();
    assert_eq!(all.len(), 2, "Two instances should remain");

    // Cleanup
    registrar1.unregister().await.unwrap();
    registrar3.unregister().await.unwrap();

    let all = repo.find_all().await.unwrap();
    assert_eq!(all.len(), 0, "All instances should be unregistered");
}
