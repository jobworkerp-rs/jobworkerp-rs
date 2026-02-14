//! Integration tests for runner pool release via worker change pubsub in standalone mode.
//!
//! Verifies the full flow: worker delete → ChanWorkerPubSub notify →
//! subscriber receives → RunnerFactoryWithPoolMap releases pool.

use anyhow::Result;
use app::app::WorkerConfig;
use app_wrapper::runner::RunnerFactory;
use infra::infra::worker::pubsub::ChanWorkerPubSubRepositoryImpl;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use proto::jobworkerp::data::{RunnerData, RunnerId, RunnerType, Worker, WorkerData, WorkerId};
use std::sync::Arc;
use tokio::time::{Duration, sleep};
use worker_app::worker::runner::map::RunnerFactoryWithPoolMap;

/// Spawn a subscriber loop that mirrors `subscribe_worker_changed_chan` logic
/// (in worker/dispatcher/chan.rs). Keep in sync if the subscriber logic changes.
/// We duplicate the logic here because `subscribe_worker_changed_chan` requires `&'static self`.
/// Returns a JoinHandle that can be aborted after test.
async fn spawn_pool_release_subscriber(
    pubsub: &ChanWorkerPubSubRepositoryImpl,
    pool_map: Arc<RunnerFactoryWithPoolMap>,
) -> tokio::task::JoinHandle<()> {
    let mut receiver = pubsub.subscribe().await;
    tokio::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(payload) => {
                    match ProstMessageCodec::deserialize_message::<Worker>(&payload) {
                        Ok(worker) => {
                            if let Some(wid) = worker.id.as_ref() {
                                pool_map.delete_runner(wid).await;
                            } else if worker.id.is_none() && worker.data.is_none() {
                                // id=None, data=None means delete all
                                pool_map.clear().await;
                            }
                        }
                        Err(e) => {
                            eprintln!("deserialize error in test subscriber: {:?}", e);
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("lagged by {} messages, clearing all pools", n);
                    pool_map.clear().await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    })
}

/// Helper: create RunnerFactory and RunnerFactoryWithPoolMap for tests
async fn create_runner_pool_map() -> (Arc<RunnerFactory>, Arc<RunnerFactoryWithPoolMap>) {
    let app_module = Arc::new(
        app::module::test::create_rdb_chan_test_app(false, false)
            .await
            .unwrap(),
    );
    let app_wrapper_module = Arc::new(app_wrapper::modules::test::create_test_app_wrapper_module(
        app_module.clone(),
    ));
    let mcp_clients = Arc::new(jobworkerp_runner::runner::mcp::proxy::McpServerFactory::default());

    let runner_factory = Arc::new(RunnerFactory::new(
        app_module.clone(),
        app_wrapper_module,
        mcp_clients,
    ));

    let pool_map = Arc::new(RunnerFactoryWithPoolMap::new(
        runner_factory.clone(),
        Arc::new(WorkerConfig {
            default_concurrency: 1,
            ..WorkerConfig::default()
        }),
    ));

    (runner_factory, pool_map)
}

/// Helper: add a runner pool entry to the map for the given worker_id
async fn add_pool_entry(pool_map: &RunnerFactoryWithPoolMap, worker_id: &WorkerId) {
    let runner_data = Arc::new(RunnerData {
        name: RunnerType::Command.as_str_name().to_string(),
        ..Default::default()
    });
    let worker_data = Arc::new(WorkerData {
        use_static: true,
        ..Default::default()
    });
    let result = pool_map
        .add_and_get_runner(runner_data, worker_id, worker_data)
        .await;
    assert!(result.is_ok(), "add_and_get_runner should succeed");
    assert!(
        result.unwrap().is_some(),
        "should return runner for static worker"
    );
}

/// Test: worker delete → pubsub → subscriber releases specific runner pool
#[test]
fn test_worker_delete_releases_runner_pool_via_pubsub() -> Result<()> {
    infra_utils::infra::test::TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(app::module::test::create_rdb_chan_test_app(false, false).await?);
        let worker_app = app_module.worker_app.clone();
        worker_app.delete_all().await?;

        let rdb_module = app_module
            .repositories
            .rdb_module
            .as_ref()
            .expect("rdb_module should exist in standalone mode");
        let pubsub = &rdb_module.chan_worker_pubsub_repository;

        let (_runner_factory, pool_map) = create_runner_pool_map().await;

        // Create a real worker via WorkerApp
        let worker_data = WorkerData {
            name: "pool_release_test_worker".to_string(),
            runner_id: Some(RunnerId { value: 1 }),
            use_static: true,
            ..Default::default()
        };
        let worker_id = worker_app.create(&worker_data).await?;

        // Manually add a runner pool entry for this worker
        add_pool_entry(&pool_map, &worker_id).await;
        assert!(
            pool_map.pools.read().await.contains_key(&worker_id.value),
            "pool should exist after add_and_get_runner"
        );

        // Start subscriber
        let handle = spawn_pool_release_subscriber(pubsub, pool_map.clone()).await;
        sleep(Duration::from_millis(50)).await;

        // Delete worker → triggers pubsub → subscriber deletes pool
        let deleted = worker_app.delete(&worker_id).await?;
        assert!(deleted, "worker should be deleted");

        // Wait for subscriber to process
        sleep(Duration::from_millis(100)).await;

        // Verify pool was released
        assert!(
            !pool_map.pools.read().await.contains_key(&worker_id.value),
            "pool should be released after worker deletion via pubsub"
        );

        handle.abort();
        worker_app.delete_all().await?;
        Ok(())
    })
}

/// Test: worker update → pubsub → subscriber releases runner pool (for re-creation on next use)
#[test]
fn test_worker_update_releases_runner_pool_via_pubsub() -> Result<()> {
    infra_utils::infra::test::TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(app::module::test::create_rdb_chan_test_app(false, false).await?);
        let worker_app = app_module.worker_app.clone();
        worker_app.delete_all().await?;

        let rdb_module = app_module
            .repositories
            .rdb_module
            .as_ref()
            .expect("rdb_module should exist in standalone mode");
        let pubsub = &rdb_module.chan_worker_pubsub_repository;

        let (_runner_factory, pool_map) = create_runner_pool_map().await;

        // Create a real worker
        let worker_data = WorkerData {
            name: "pool_update_test_worker".to_string(),
            runner_id: Some(RunnerId { value: 1 }),
            use_static: true,
            ..Default::default()
        };
        let worker_id = worker_app.create(&worker_data).await?;

        // Add runner pool
        add_pool_entry(&pool_map, &worker_id).await;
        assert!(pool_map.pools.read().await.contains_key(&worker_id.value));

        // Start subscriber
        let handle = spawn_pool_release_subscriber(pubsub, pool_map.clone()).await;
        sleep(Duration::from_millis(50)).await;

        // Update worker → triggers pubsub → subscriber deletes pool
        let updated_data = WorkerData {
            name: "pool_update_test_worker_v2".to_string(),
            runner_id: Some(RunnerId { value: 1 }),
            use_static: true,
            ..Default::default()
        };
        let updated = worker_app.update(&worker_id, &Some(updated_data)).await?;
        assert!(updated, "worker should be updated");

        sleep(Duration::from_millis(100)).await;

        // Verify pool was released (will be re-created lazily on next job execution)
        assert!(
            !pool_map.pools.read().await.contains_key(&worker_id.value),
            "pool should be released after worker update via pubsub"
        );

        handle.abort();
        worker_app.delete_all().await?;
        Ok(())
    })
}

/// Test: delete_all → pubsub → subscriber clears all pools
#[test]
fn test_delete_all_clears_all_runner_pools_via_pubsub() -> Result<()> {
    infra_utils::infra::test::TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(app::module::test::create_rdb_chan_test_app(false, false).await?);
        let worker_app = app_module.worker_app.clone();
        worker_app.delete_all().await?;

        let rdb_module = app_module
            .repositories
            .rdb_module
            .as_ref()
            .expect("rdb_module should exist in standalone mode");
        let pubsub = &rdb_module.chan_worker_pubsub_repository;

        let (_runner_factory, pool_map) = create_runner_pool_map().await;

        // Create multiple workers and add pools
        let _worker_ids: Vec<WorkerId> = {
            let mut ids = Vec::new();
            for i in 0..3 {
                let wd = WorkerData {
                    name: format!("pool_delete_all_worker_{}", i),
                    runner_id: Some(RunnerId { value: 1 }),
                    use_static: true,
                    ..Default::default()
                };
                let wid = worker_app.create(&wd).await?;
                add_pool_entry(&pool_map, &wid).await;
                ids.push(wid);
            }
            ids
        };
        assert_eq!(pool_map.pools.read().await.len(), 3, "should have 3 pools");

        // Start subscriber
        let handle = spawn_pool_release_subscriber(pubsub, pool_map.clone()).await;
        sleep(Duration::from_millis(50)).await;

        // Delete all workers → pubsub sends id=None, data=None → subscriber clears all pools
        worker_app.delete_all().await?;

        sleep(Duration::from_millis(100)).await;

        // Verify all pools cleared
        assert!(
            pool_map.pools.read().await.is_empty(),
            "all pools should be cleared after delete_all via pubsub"
        );

        handle.abort();
        Ok(())
    })
}

/// Test: pool is lazily re-created after deletion via get_or_create_static_runner
#[test]
fn test_pool_recreated_after_pubsub_deletion() -> Result<()> {
    infra_utils::infra::test::TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(app::module::test::create_rdb_chan_test_app(false, false).await?);
        let worker_app = app_module.worker_app.clone();
        worker_app.delete_all().await?;

        let rdb_module = app_module
            .repositories
            .rdb_module
            .as_ref()
            .expect("rdb_module should exist in standalone mode");
        let pubsub = &rdb_module.chan_worker_pubsub_repository;

        let (_runner_factory, pool_map) = create_runner_pool_map().await;

        // Create worker and add pool
        let worker_data = WorkerData {
            name: "pool_recreate_test_worker".to_string(),
            runner_id: Some(RunnerId { value: 1 }),
            use_static: true,
            ..Default::default()
        };
        let worker_id = worker_app.create(&worker_data).await?;
        add_pool_entry(&pool_map, &worker_id).await;
        assert!(pool_map.pools.read().await.contains_key(&worker_id.value));

        // Start subscriber
        let handle = spawn_pool_release_subscriber(pubsub, pool_map.clone()).await;
        sleep(Duration::from_millis(50)).await;

        // Delete worker → pool released via pubsub
        worker_app.delete(&worker_id).await?;
        sleep(Duration::from_millis(100)).await;
        assert!(
            !pool_map.pools.read().await.contains_key(&worker_id.value),
            "pool should be released"
        );

        // Re-create pool via get_or_create_static_runner (simulates next job execution)
        let runner_data = RunnerData {
            name: RunnerType::Command.as_str_name().to_string(),
            ..Default::default()
        };
        let result = pool_map
            .get_or_create_static_runner(&runner_data, &worker_id, &worker_data, None)
            .await?;
        assert!(
            result.is_some(),
            "pool should be lazily re-created by get_or_create_static_runner"
        );
        assert!(
            pool_map.pools.read().await.contains_key(&worker_id.value),
            "pool should exist again after lazy re-creation"
        );

        handle.abort();
        worker_app.delete_all().await?;
        Ok(())
    })
}
