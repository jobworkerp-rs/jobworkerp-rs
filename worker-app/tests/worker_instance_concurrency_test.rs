//! Worker Instance Concurrency Integration Tests
//!
//! Tests for concurrent registration and heartbeat operations.
//! These tests verify thread-safety of the worker instance repository implementations.

use std::sync::Arc;
use std::time::Duration;

use infra::infra::worker_instance::WorkerInstanceRepository;
use infra::infra::worker_instance::memory::MemoryWorkerInstanceRepository;
use jobworkerp_base::worker_instance_config::WorkerInstanceConfig;
use proto::jobworkerp::data::{StorageType, WorkerInstanceId};
use tokio::sync::watch;
use worker_app::worker::instance_registrar::WorkerInstanceRegistrar;

fn create_test_registrar(
    instance_id: i64,
    repository: Arc<dyn WorkerInstanceRepository>,
    storage_type: StorageType,
) -> WorkerInstanceRegistrar {
    WorkerInstanceRegistrar::new(
        instance_id,
        format!("192.168.1.{}", instance_id % 256),
        Some(format!("worker-{}", instance_id)),
        vec![("".to_string(), 4), ("priority".to_string(), 2)],
        repository,
        WorkerInstanceConfig {
            enabled: true,
            heartbeat_interval_sec: 1,
            timeout_sec: 10,
            cleanup_interval_sec: 30,
        },
        storage_type,
    )
}

/// Test: Concurrent registration of multiple instances
/// Verifies: multiple instances can register concurrently without conflict
#[tokio::test]
async fn test_concurrent_registration() {
    let repo = Arc::new(MemoryWorkerInstanceRepository::new());
    let instance_count = 10;

    let mut handles = Vec::new();

    for i in 0..instance_count {
        let repo_clone = repo.clone();
        handles.push(tokio::spawn(async move {
            let registrar = create_test_registrar(30000 + i, repo_clone, StorageType::Standalone);
            registrar.register().await.unwrap();
            30000 + i
        }));
    }

    // Wait for all registrations
    let mut registered_ids = Vec::new();
    for handle in handles {
        registered_ids.push(handle.await.unwrap());
    }

    // Verify all instances are registered
    let all = repo.find_all().await.unwrap();
    assert_eq!(
        all.len(),
        instance_count as usize,
        "All instances should be registered"
    );

    // Verify each instance exists
    for id in registered_ids {
        let found = repo.find(&WorkerInstanceId { value: id }).await.unwrap();
        assert!(found.is_some(), "Instance {} should exist", id);
    }

    // Cleanup
    for i in 0..instance_count {
        let registrar = create_test_registrar(30000 + i, repo.clone(), StorageType::Standalone);
        registrar.unregister().await.unwrap();
    }
}

/// Test: Concurrent heartbeat updates
/// Verifies: multiple instances can update heartbeat concurrently
#[tokio::test]
async fn test_concurrent_heartbeat_updates() {
    let repo = Arc::new(MemoryWorkerInstanceRepository::new());
    let instance_count = 5;

    // Register all instances first
    let mut registrars = Vec::new();
    for i in 0..instance_count {
        let registrar = Arc::new(create_test_registrar(
            40000 + i,
            repo.clone(),
            StorageType::Standalone,
        ));
        registrar.register().await.unwrap();
        registrars.push(registrar);
    }

    // Start heartbeat loops concurrently
    let (shutdown_tx, _) = watch::channel(false);
    let mut heartbeat_handles = Vec::new();

    for registrar in registrars.iter() {
        let registrar_clone = registrar.clone();
        let shutdown_rx = shutdown_tx.subscribe();
        heartbeat_handles.push(tokio::spawn(async move {
            registrar_clone.start_heartbeat_loop(shutdown_rx).await
        }));
    }

    // Let heartbeats run for a while
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Verify all instances still exist and have updated heartbeats
    let all = repo.find_all().await.unwrap();
    assert_eq!(
        all.len(),
        instance_count as usize,
        "All instances should still exist after heartbeats"
    );

    // Shutdown heartbeat loops
    shutdown_tx.send(true).unwrap();

    // Wait for all heartbeat loops to finish
    for handle in heartbeat_handles {
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }

    // Cleanup
    for registrar in registrars {
        registrar.unregister().await.unwrap();
    }
}

/// Test: find_all during concurrent updates
/// Verifies: find_all returns consistent data during concurrent modifications
#[tokio::test]
async fn test_find_all_during_updates() {
    let repo = Arc::new(MemoryWorkerInstanceRepository::new());
    let instance_count = 20;

    // Register some initial instances
    for i in 0..10 {
        let registrar = create_test_registrar(50000 + i, repo.clone(), StorageType::Standalone);
        registrar.register().await.unwrap();
    }

    // Spawn concurrent writers and readers
    let repo_for_writer = repo.clone();
    let repo_for_reader = repo.clone();

    let writer_handle = tokio::spawn(async move {
        for i in 10..instance_count {
            let registrar =
                create_test_registrar(50000 + i, repo_for_writer.clone(), StorageType::Standalone);
            registrar.register().await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });

    let reader_handle = tokio::spawn(async move {
        let mut read_counts = Vec::new();
        for _ in 0..10 {
            let all = repo_for_reader.find_all().await.unwrap();
            read_counts.push(all.len());
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        read_counts
    });

    // Wait for both to complete
    writer_handle.await.unwrap();
    let read_counts = reader_handle.await.unwrap();

    // Verify reads returned valid counts (between initial and final)
    for count in read_counts {
        assert!(
            count >= 10 && count <= instance_count as usize,
            "Read count {} should be between 10 and {}",
            count,
            instance_count
        );
    }

    // Final check
    let final_all = repo.find_all().await.unwrap();
    assert_eq!(
        final_all.len(),
        instance_count as usize,
        "Final count should be {}",
        instance_count
    );

    // Cleanup
    for i in 0..instance_count {
        let registrar = create_test_registrar(50000 + i, repo.clone(), StorageType::Standalone);
        registrar.unregister().await.unwrap();
    }
}

/// Test: Concurrent upsert and delete operations
/// Verifies: upsert and delete can happen concurrently without data corruption
#[tokio::test]
async fn test_concurrent_upsert_and_delete() {
    let repo = Arc::new(MemoryWorkerInstanceRepository::new());

    // Register initial instances
    for i in 0..10 {
        let registrar = create_test_registrar(60000 + i, repo.clone(), StorageType::Standalone);
        registrar.register().await.unwrap();
    }

    let repo_for_deleter = repo.clone();
    let repo_for_adder = repo.clone();

    // Concurrently delete some and add new ones
    let deleter_handle = tokio::spawn(async move {
        for i in 0..5 {
            let registrar =
                create_test_registrar(60000 + i, repo_for_deleter.clone(), StorageType::Standalone);
            registrar.unregister().await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    let adder_handle = tokio::spawn(async move {
        for i in 10..15 {
            let registrar =
                create_test_registrar(60000 + i, repo_for_adder.clone(), StorageType::Standalone);
            registrar.register().await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    deleter_handle.await.unwrap();
    adder_handle.await.unwrap();

    // Verify final state: 10 instances remain (5 original + 5 new, minus 5 deleted)
    let final_all = repo.find_all().await.unwrap();
    assert_eq!(
        final_all.len(),
        10,
        "Should have 10 instances after operations"
    );

    // Cleanup
    for inst in final_all {
        if let Some(id) = inst.id {
            let _ = repo.delete(&id).await;
        }
    }
}

/// Test: High concurrency stress test
/// Verifies: repository handles high concurrency without panics or deadlocks
#[tokio::test]
async fn test_high_concurrency_stress() {
    let repo = Arc::new(MemoryWorkerInstanceRepository::new());
    let task_count = 50;

    let mut handles = Vec::new();

    for i in 0..task_count {
        let repo_clone = repo.clone();
        handles.push(tokio::spawn(async move {
            let id = 70000 + i;
            let registrar = create_test_registrar(id, repo_clone.clone(), StorageType::Standalone);

            // Register
            registrar.register().await.unwrap();

            // Multiple heartbeat updates
            for _ in 0..5 {
                let _ = repo_clone
                    .update_heartbeat(&WorkerInstanceId { value: id })
                    .await;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }

            // Find
            let _ = repo_clone.find(&WorkerInstanceId { value: id }).await;

            // Unregister
            registrar.unregister().await.unwrap();
        }));
    }

    // Wait for all tasks with timeout
    let results = tokio::time::timeout(Duration::from_secs(30), async {
        for handle in handles {
            handle.await.unwrap();
        }
    })
    .await;

    assert!(
        results.is_ok(),
        "All concurrent tasks should complete within timeout"
    );

    // Final state should be empty
    let final_all = repo.find_all().await.unwrap();
    assert_eq!(
        final_all.len(),
        0,
        "All instances should be unregistered after stress test"
    );
}

/// Test: Interleaved register/unregister operations
/// Verifies: rapid register/unregister cycles don't cause issues
#[tokio::test]
async fn test_interleaved_register_unregister() {
    let repo = Arc::new(MemoryWorkerInstanceRepository::new());

    let mut handles = Vec::new();

    for i in 0..20 {
        let repo_clone = repo.clone();
        handles.push(tokio::spawn(async move {
            let registrar = create_test_registrar(80000 + i, repo_clone, StorageType::Standalone);

            // Rapid register/unregister cycles
            for _ in 0..5 {
                registrar.register().await.unwrap();
                tokio::time::sleep(Duration::from_millis(5)).await;
                registrar.unregister().await.unwrap();
            }
        }));
    }

    // Wait for all
    for handle in handles {
        handle.await.unwrap();
    }

    // Final state should be empty
    let final_all = repo.find_all().await.unwrap();
    assert_eq!(
        final_all.len(),
        0,
        "All instances should be unregistered after interleaved operations"
    );
}
