use anyhow::Result;
use app::app::worker::UseWorkerApp;
use async_trait::async_trait;
use command_utils::util::result::TapErr;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra_utils::infra::redis::UseRedisClient;
use tokio_stream::StreamExt;

use super::runner::map::UseRunnerPoolMap;

#[async_trait]
pub trait UseSubscribeWorker:
    UseWorkerApp + UseRunnerPoolMap + UseJobqueueAndCodec + UseRedisClient
{
    // subscribe worker changed event using redis and invalidate cache
    async fn subscribe_worker_changed(&self) -> Result<bool> {
        let mut sub = self
            .subscribe(Self::WORKER_PUBSUB_CHANNEL_NAME)
            .await
            .tap_err(|e| tracing::error!("redis_err:{:?}", e))?;

        // for shutdown notification (spmc broadcast)
        let (send, mut recv) = tokio::sync::watch::channel(false);
        // send msg in ctrl_c signal with send for shutdown notification in parallel
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.map(|_| {
                tracing::debug!("got sigint signal....");
                send.send(true)
                    .tap_err(|e| tracing::error!("mpmc send error: {:?}", e))
                    .unwrap();
            })
        });

        'outer: loop {
            let mut message = sub.on_message();
            tokio::select! {
                _ = recv.changed() => {
                        tracing::debug!("got sigint signal....");
                        break 'outer;
                },
                val = message.next() => {
                    if let Some(msg) = val {
                        // skip if error in processing message
                        let payload: Vec<u8> = msg
                            .get_payload()
                            .tap_err(|e| tracing::error!("get_payload:{:?}", e))?;
                        let worker = Self::deserialize_worker(&payload)
                            .tap_err(|e| tracing::error!("deserialize_worker:{:?}", e))?;
                        tracing::info!("subscribe_worker_changed: worker changed: {:?}", worker);
                        if let Some(wid) = worker.id.as_ref() {
                            // TODO not delete for changing trivial condition?
                            self.runner_pool_map().delete_runner(wid).await;
                        } else if worker.id.as_ref().is_none() && worker.data.is_none() { // delete all
                            self.runner_pool_map().clear().await;
                        }
                        let _ = self.worker_app().clear_cache_by(worker.id.as_ref(), worker.data.as_ref().map(|d| &d.name)).await
                            .tap_err(|e| tracing::error!("update error:{:?}", e));
                    } else {
                        tracing::debug!("message.next() is None");
                    }
                }
            }
            if *recv.borrow() {
                break;
            }
        }
        sub.unsubscribe(Self::WORKER_PUBSUB_CHANNEL_NAME).await?;
        tracing::info!("subscribe_worker_changed end");
        Ok(true)
    }
}

// create test of subscribe_worker_changed() and publish_worker_changed()
#[cfg(test)]
mod test {
    use super::*;
    use crate::{plugins::Plugins, worker::runner::map::RunnerFactoryWithPoolMap};
    use anyhow::Result;
    use app::{
        app::{
            worker::{hybrid::HybridWorkerAppImpl, WorkerApp},
            StorageConfig, StorageType,
        },
        module::load_worker_config,
    };
    use infra::infra::{
        test::new_for_test_config_rdb, worker::event::UseWorkerPublish, IdGeneratorWrapper,
    };
    use infra_utils::infra::test::setup_test_redis_client;
    use proto::jobworkerp::data::{worker_operation::Operation, WorkerData, WorkerOperation};
    use std::sync::Arc;
    use tokio::time::{sleep, Duration};

    // #[derive(Clone)]
    struct UseWorkerMapAndSubscribeImpl {
        pub worker_app: Arc<dyn WorkerApp + 'static>,
        pub redis_client: redis::Client,
        pub runner_map: RunnerFactoryWithPoolMap,
    }
    impl UseWorkerApp for UseWorkerMapAndSubscribeImpl {
        fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
            &self.worker_app
        }
    }
    impl UseJobqueueAndCodec for UseWorkerMapAndSubscribeImpl {}

    impl UseRedisClient for UseWorkerMapAndSubscribeImpl {
        fn redis_client(&self) -> &redis::Client {
            &self.redis_client
        }
    }
    impl UseRunnerPoolMap for UseWorkerMapAndSubscribeImpl {
        fn runner_pool_map(&self) -> &RunnerFactoryWithPoolMap {
            &self.runner_map
        }
    }
    impl UseSubscribeWorker for UseWorkerMapAndSubscribeImpl {}
    impl UseWorkerPublish for UseWorkerMapAndSubscribeImpl {}

    #[tokio::test]
    async fn subscribe_worker_changed_test() -> Result<()> {
        let redis_client = setup_test_redis_client()?;
        let storage_config = Arc::new(StorageConfig {
            r#type: StorageType::Hybrid,
            restore_at_startup: Some(false),
        });
        let id_generator = Arc::new(IdGeneratorWrapper::new());
        let module = new_for_test_config_rdb();
        let repositories =
            Arc::new(infra::infra::module::HybridRepositoryModule::new(&module).await);
        let memory_cache = infra_utils::infra::memory::MemoryCacheImpl::new(
            &infra_utils::infra::memory::MemoryCacheConfig {
                num_counters: 10,
                max_cost: 1000000,
                use_metrics: false,
            },
            Some(Duration::from_secs(60)),
        );
        let mut plugins = Plugins::new();
        plugins.load_plugins_from_env()?;

        let worker_config = Arc::new(load_worker_config());
        // XXX empty runner map (must confirm deletion: use mock?)
        let runner_map = RunnerFactoryWithPoolMap::new(Arc::new(plugins), worker_config);

        let worker_app = Arc::new(HybridWorkerAppImpl::new(
            storage_config,
            id_generator,
            memory_cache,
            repositories,
        ));
        let worker_app2 = worker_app.clone();
        let repo = UseWorkerMapAndSubscribeImpl {
            worker_app,
            redis_client,
            runner_map,
        };
        worker_app2.delete_all().await?;

        let r = repo; //.clone();

        // begin subscribe
        let handle = tokio::spawn(async move {
            r.subscribe_worker_changed().await.unwrap();
        });
        sleep(Duration::from_millis(100)).await;

        let operation = WorkerOperation {
            operation: Some(Operation::Command(
                proto::jobworkerp::data::CommandOperation {
                    name: "ls".to_string(),
                },
            )),
        };
        let worker_data = WorkerData {
            name: "hoge_worker".to_string(),
            operation: Some(operation),
            channel: Some("test_channel".to_string()),
            ..Default::default()
        };
        // create worker_data via redis pubsub

        // publish event and test clear cache (publish in redis repository)
        let worker_id = worker_app2.create(&worker_data).await?;
        sleep(Duration::from_millis(100)).await;

        // ref worker_map2
        assert_eq!(
            worker_app2
                .find_data_by_opt(Some(&worker_id))
                .await
                .unwrap()
                .unwrap()
                .channel,
            Some("test_channel".to_string())
        );
        // publish worker deleted event
        assert!(worker_app2.delete(&worker_id).await?);
        // repo.publish_worker_deleted(&worker_id).await.unwrap();
        sleep(Duration::from_millis(100)).await;

        assert_eq!(worker_app2.find_data(&worker_id).await.unwrap(), None);

        // create worker_data again
        let worker_id2 = worker_app2.create(&worker_data).await?;
        // repo.publish_worker_changed(&worker_id2, &worker_data)
        //     .await
        //     .unwrap();
        sleep(Duration::from_millis(100)).await;
        // ref worker_map2
        assert_eq!(
            worker_app2
                .find_data_by_opt(Some(&worker_id2))
                .await
                .unwrap()
                .unwrap()
                .channel,
            Some("test_channel".to_string())
        );

        // publish worker all deleted event
        // repo.publish_worker_all_deleted().await.unwrap();
        sleep(Duration::from_millis(100)).await;
        assert_eq!(
            worker_app2
                .find_data_by_opt(Some(&worker_id))
                .await
                .unwrap(),
            None
        );
        handle.abort();

        Ok(())
    }
}
