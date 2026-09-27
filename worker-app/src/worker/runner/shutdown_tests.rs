use super::*;
use crate::coordinate_sandbox_shutdown;
use command_utils::util::shutdown::{ShutdownLock, create_lock_and_wait};
use infra_utils::infra::test::TEST_RUNTIME;
use jobworkerp_runner::runner::sandbox::SandboxCleanupRegistry;
use proto::jobworkerp::data::{RunnerType, WorkerData, WorkerId};
use std::time::Duration;
use tokio::sync::watch;

async fn create_runner_factory() -> Arc<RunnerFactory> {
    let app_module = Arc::new(app::module::test::create_hybrid_test_app().await.unwrap());
    let app_wrapper = Arc::new(app_wrapper::modules::test::create_test_app_wrapper_module(
        app_module.clone(),
    ));
    Arc::new(RunnerFactory::new(
        app_module,
        app_wrapper,
        Arc::new(jobworkerp_runner::runner::mcp::proxy::McpServerFactory::default()),
    ))
}

fn sandbox_runner_data() -> RunnerData {
    RunnerData {
        name: RunnerType::Sandbox.as_str_name().to_string(),
        ..Default::default()
    }
}

fn sandbox_worker_data(use_static: bool) -> WorkerData {
    WorkerData {
        use_static,
        ..Default::default()
    }
}

fn create_registry(lock: ShutdownLock) -> (SandboxCleanupRegistry, watch::Sender<bool>) {
    let (registry_shutdown_send, registry_shutdown_recv) = watch::channel(false);
    let registry = SandboxCleanupRegistry::new(lock, registry_shutdown_recv).unwrap();
    (registry, registry_shutdown_send)
}

#[test]
fn sandbox_without_registry_fails_closed_but_legacy_map_allows_other_runners() {
    TEST_RUNTIME.block_on(async {
        let runner_factory = create_runner_factory().await;
        let map = RunnerFactoryWithPoolMap::new(
            runner_factory.clone(),
            Arc::new(app::app::WorkerConfig::default()),
        );
        let command_data = RunnerData {
            name: RunnerType::Command.as_str_name().to_string(),
            ..Default::default()
        };
        assert!(map.checkout_guard(&command_data).await.is_ok());

        let worker_id = WorkerId { value: 7310 };
        let sandbox_data = sandbox_runner_data();
        assert!(
            map.get_or_create_static_runner(
                &sandbox_data,
                &worker_id,
                &sandbox_worker_data(true),
                None,
            )
            .await
            .is_err(),
            "static SANDBOX must not load without a process cleanup registry"
        );
        assert!(
            map.get_non_static_runner(&sandbox_data, &sandbox_worker_data(false), &worker_id)
                .await
                .is_err(),
            "non-static SANDBOX must not load without a process cleanup registry"
        );
    });
}

#[test]
fn shutdown_waits_for_checkout_and_clears_idle_pool_entries() {
    TEST_RUNTIME.block_on(async {
        let runner_factory = create_runner_factory().await;
        let (process_lock, _shutdown_wait) = create_lock_and_wait();
        let (registry, _registry_shutdown_send) = create_registry(process_lock.clone());
        let map = Arc::new(RunnerFactoryWithPoolMap::new_with_cleanup_registry(
            runner_factory.clone(),
            Arc::new(app::app::WorkerConfig::default()),
            registry.clone(),
        ));

        let idle_pool = RunnerFactoryWithPool::new_for_worker_id(
            Arc::new(RunnerData {
                name: RunnerType::Command.as_str_name().to_string(),
                ..Default::default()
            }),
            Arc::new(WorkerData {
                use_static: true,
                ..Default::default()
            }),
            runner_factory,
            Arc::new(app::app::WorkerConfig {
                default_concurrency: 1,
                ..Default::default()
            }),
            WorkerId { value: 7311 },
        )
        .await
        .unwrap();
        let idle_runner = idle_pool.get().await.unwrap();
        drop(idle_runner);
        map.pools.write().await.insert(7311, idle_pool);
        assert_eq!(map.pools.read().await.len(), 1);

        let checkout = map.checkout_guard(&sandbox_runner_data()).await.unwrap();
        let map_for_shutdown = map.clone();
        let mut shutdown = tokio::spawn(async move {
            map_for_shutdown.shutdown_and_clear().await;
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut shutdown)
                .await
                .is_err(),
            "shutdown must wait for a checkout holding the gate"
        );
        assert_eq!(map.pools.read().await.len(), 1);

        drop(checkout);
        tokio::time::timeout(Duration::from_secs(1), shutdown)
            .await
            .unwrap()
            .unwrap();
        assert!(
            map.pools.read().await.is_empty(),
            "idle pools must be cleared"
        );
        assert!(map.checkout_guard(&sandbox_runner_data()).await.is_err());
        registry.shutdown().await;
    });
}

#[test]
fn shutdown_coordinator_holds_lock_until_pool_clear_and_registry_shutdown() {
    TEST_RUNTIME.block_on(async {
        let (process_lock, mut shutdown_wait) = create_lock_and_wait();
        let (registry, registry_shutdown_send) = create_registry(process_lock.clone());
        let (shutdown_send, shutdown_recv) = watch::channel(false);
        let map = Arc::new(
            RunnerFactoryWithPoolMap::new_with_cleanup_registry_and_shutdown(
                create_runner_factory().await,
                Arc::new(app::app::WorkerConfig::default()),
                registry.clone(),
                shutdown_recv.clone(),
            ),
        );
        let checkout = map.checkout_guard(&sandbox_runner_data()).await.unwrap();

        let coordinator = tokio::spawn(coordinate_sandbox_shutdown(
            shutdown_recv,
            map.clone(),
            registry.clone(),
            registry_shutdown_send,
            process_lock.clone(),
        ));
        shutdown_send.send(true).unwrap();
        assert!(
            map.checkout_guard(&sandbox_runner_data()).await.is_err(),
            "the shutdown watch must reject new checkouts before the coordinator can acquire the write gate"
        );
        process_lock.unlock();

        assert!(
            tokio::time::timeout(Duration::from_millis(20), shutdown_wait.wait())
                .await
                .is_err(),
            "the coordinator and registry must retain shutdown locks while checkout is active"
        );
        assert!(!registry.is_shutting_down().await);

        drop(checkout);
        tokio::time::timeout(Duration::from_secs(1), coordinator)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), shutdown_wait.wait())
            .await
            .unwrap();
        assert!(registry.is_shutting_down().await);
        assert!(map.pools.read().await.is_empty());
    });
}

#[test]
fn shutdown_cancels_checkout_waiting_for_a_busy_static_pool() {
    TEST_RUNTIME.block_on(assert_shutdown_cancels_busy_static_checkout(false));
}

#[test]
fn shutdown_cancels_add_and_get_waiting_for_a_busy_static_pool() {
    TEST_RUNTIME.block_on(assert_shutdown_cancels_busy_static_checkout(true));
}

async fn assert_shutdown_cancels_busy_static_checkout(use_add_and_get: bool) {
    let runner_factory = create_runner_factory().await;
    let (process_lock, mut shutdown_wait) = create_lock_and_wait();
    let (registry, registry_shutdown_send) = create_registry(process_lock.clone());
    let (shutdown_send, shutdown_recv) = watch::channel(false);
    let map = Arc::new(
        RunnerFactoryWithPoolMap::new_with_cleanup_registry_and_shutdown(
            runner_factory.clone(),
            Arc::new(app::app::WorkerConfig::default()),
            registry.clone(),
            shutdown_recv.clone(),
        ),
    );
    let worker_id = WorkerId { value: 7312 };
    let worker = WorkerData {
        use_static: true,
        ..Default::default()
    };
    let busy_pool = RunnerFactoryWithPool::new_for_worker_id(
        Arc::new(RunnerData {
            name: RunnerType::Command.as_str_name().to_string(),
            ..Default::default()
        }),
        Arc::new(worker.clone()),
        runner_factory,
        Arc::new(app::app::WorkerConfig {
            default_concurrency: 1,
            ..Default::default()
        }),
        worker_id,
    )
    .await
    .unwrap();
    let occupied = busy_pool.get().await.unwrap();
    map.pools.write().await.insert(worker_id.value, busy_pool);

    let waiting_map = map.clone();
    let mut waiting = tokio::spawn(async move {
        if use_add_and_get {
            waiting_map
                .add_and_get_runner(
                    Arc::new(sandbox_runner_data()),
                    &worker_id,
                    Arc::new(worker),
                )
                .await
        } else {
            waiting_map
                .get_or_create_static_runner(&sandbox_runner_data(), &worker_id, &worker, None)
                .await
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut waiting)
            .await
            .is_err()
    );

    let coordinator = tokio::spawn(coordinate_sandbox_shutdown(
        shutdown_recv,
        map.clone(),
        registry.clone(),
        registry_shutdown_send,
        process_lock.clone(),
    ));
    shutdown_send.send(true).unwrap();
    process_lock.unlock();

    tokio::time::timeout(Duration::from_secs(1), coordinator)
        .await
        .expect("shutdown must not wait for a busy static pool checkout")
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(1), shutdown_wait.wait())
        .await
        .unwrap();
    drop(occupied);
}
