use anyhow::Result;
use app::app::worker::UseWorkerApp;
use async_trait::async_trait;
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
            .inspect_err(|e| tracing::error!("redis_err:{:?}", e))?;

        // for shutdown notification (spmc broadcast)
        let (send, mut recv) = tokio::sync::watch::channel(false);
        // send msg in ctrl_c signal with send for shutdown notification in parallel
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.map(|_| {
                tracing::debug!("got sigint signal....");
                send.send(true)
                    .inspect_err(|e| tracing::error!("mpmc send error: {:?}", e))
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
                            .inspect_err(|e| tracing::error!("get_payload:{:?}", e))?;
                        let worker = Self::deserialize_worker(&payload)
                            .inspect_err(|e| tracing::error!("deserialize_worker:{:?}", e))?;
                        tracing::info!("subscribe_worker_changed: worker changed: {:?}", worker);
                        if let Some(wid) = worker.id.as_ref() {
                            // TODO not delete for changing trivial condition?
                            self.runner_pool_map().delete_runner(wid).await;
                        } else if worker.id.as_ref().is_none() && worker.data.is_none() { // delete all
                            self.runner_pool_map().clear().await;
                        }
                        let _ = self.worker_app().clear_cache_by(worker.id.as_ref(), worker.data.as_ref().map(|d| &d.name)).await
                            .inspect_err(|e| tracing::error!("update error:{:?}", e));
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
    use crate::worker::runner::map::RunnerFactoryWithPoolMap;
    use anyhow::Result;
    use app::{app::worker::WorkerApp, module::load_worker_config};
    use app_wrapper::runner::RunnerFactory;
    use infra::infra::worker::event::UseWorkerPublish;
    use infra_utils::infra::test::setup_test_redis_client;
    use proto::jobworkerp::data::{RunnerId, WorkerData};
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

    #[cfg(test)]
    #[test]
    fn subscribe_worker_changed_test() -> Result<()> {
        use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;

        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let redis_client = setup_test_redis_client()?;
            let worker_config = Arc::new(load_worker_config());
            let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
            let worker_app2 = app_module.worker_app.clone();
            worker_app2.delete_all().await?;

            let runner_factory = Arc::new(RunnerFactory::new(
                app_module.clone(),
                Arc::new(McpServerFactory::default()),
            ));
            // XXX empty runner map (must confirm deletion: use mock?)
            let runner_map = RunnerFactoryWithPoolMap::new(runner_factory.clone(), worker_config);

            let repo = UseWorkerMapAndSubscribeImpl {
                worker_app: app_module.worker_app.clone(),
                redis_client,
                runner_map,
            };

            let r = repo; //.clone();

            // begin subscribe
            let handle = tokio::spawn(async move {
                r.subscribe_worker_changed().await.unwrap();
            });
            sleep(Duration::from_millis(100)).await;

            let runner_settings = vec![];
            let worker_data = WorkerData {
                name: "hoge_worker".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings,
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
        })
    }
}
