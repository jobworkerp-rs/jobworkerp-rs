//! Enqueue failures are classified by whether the job may already be queued.

#[cfg(test)]
mod tests {
    use crate::app::job::JobEnqueueOutcomeUnknown;
    use crate::module::AppModule;
    use crate::module::test::create_hybrid_test_app_with_redis_proxy_and_channel;
    use anyhow::Result;
    use infra::infra::IdGeneratorWrapper;
    use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
    use infra::infra::test_redis_proxy::RedisResponseDropProxy;
    use infra_utils::infra::test::{REDIS_CONFIG, TEST_RUNTIME, setup_test_redis_client};
    use proto::jobworkerp::data::{
        JobId, JobProcessingStatus, Priority, QueueType, ResponseType, StreamingType, Worker,
        WorkerData,
    };
    use redis::AsyncCommands;
    use std::collections::HashMap;
    use std::sync::Arc;

    struct Fixture {
        app: AppModule,
        proxy: RedisResponseDropProxy,
        worker: Worker,
        queue_key: String,
    }

    async fn fixture(name: &str, response_type: ResponseType) -> Result<Fixture> {
        let worker_name = format!("{name}-{}", rand::random::<u32>());
        let channel = format!("test-{:016x}", rand::random::<u64>());
        let proxy = RedisResponseDropProxy::start(&REDIS_CONFIG.url).await?;
        let app =
            create_hybrid_test_app_with_redis_proxy_and_channel(&channel, proxy.url(), &channel)
                .await?;
        let runner = app
            .runner_app
            .find_runner_by_name("COMMAND")
            .await?
            .expect("COMMAND runner");
        let data = WorkerData {
            name: worker_name,
            runner_id: runner.id,
            channel: Some(channel.clone()),
            response_type: response_type as i32,
            queue_type: QueueType::Normal as i32,
            ..Default::default()
        };
        let id = app.worker_app.create(&data).await?;
        let queue_key = JobqueueAndCodec::queue_channel_name(
            channel.as_str(),
            Some(&(Priority::Medium as i32)),
        );
        proxy.observe_queue(&queue_key);
        Ok(Fixture {
            app,
            proxy,
            worker: Worker {
                id: Some(id),
                data: Some(data),
            },
            queue_key,
        })
    }

    /// Connection that bypasses the proxy, for inspecting what Redis really holds.
    async fn direct_redis() -> Result<redis::aio::MultiplexedConnection> {
        Ok(setup_test_redis_client()?
            .get_multiplexed_async_connection()
            .await?)
    }

    async fn queued_len(fixture: &Fixture) -> Result<i64> {
        Ok(direct_redis().await?.llen(&fixture.queue_key).await?)
    }

    async fn enqueue(fixture: &Fixture, reserved_job_id: Option<JobId>) -> Result<()> {
        fixture
            .app
            .job_app
            .enqueue_job_with_channel(
                Arc::new(HashMap::new()),
                fixture.worker.clone(),
                Vec::new(),
                None,
                0,
                Priority::Medium as i32,
                10_000,
                reserved_job_id,
                StreamingType::None,
                None,
                None,
            )
            .await
            .map(|_| ())
    }

    async fn cleanup(fixture: Fixture) -> Result<()> {
        direct_redis()
            .await?
            .del::<_, ()>(&fixture.queue_key)
            .await?;
        fixture
            .app
            .worker_app
            .delete(fixture.worker.id.as_ref().unwrap())
            .await?;
        Ok(())
    }

    #[test]
    fn reply_lost_after_queue_write_is_outcome_unknown_and_keeps_pending() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let fixture = fixture("enqueue-reply-lost", ResponseType::Direct).await?;
            fixture.proxy.drop_next_queue_append_response();

            let error = enqueue(&fixture, None)
                .await
                .expect_err("a lost reply must not look like a successful enqueue");

            let unknown = error
                .downcast_ref::<JobEnqueueOutcomeUnknown>()
                .expect("a write that may have landed is reported as outcome unknown");
            let status = fixture.app.job_app.find_job_status(&unknown.job_id).await?;
            assert_eq!(status, Some(JobProcessingStatus::Pending));
            let queued = queued_len(&fixture).await?;
            assert_eq!(queued, 1, "the job stays queued and may still run");

            fixture.app.job_app.delete_job(&unknown.job_id).await?;
            cleanup(fixture).await
        })
    }

    #[test]
    fn running_job_record_failure_after_queue_write_still_returns_the_job() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let fixture = fixture("enqueue-record-failure", ResponseType::Direct).await?;
            fixture
                .proxy
                .reject_next_command_on_key_prefix("JOB_INDIVIDUAL:");

            let job_id = JobId {
                value: IdGeneratorWrapper::new().generate_id()?,
            };
            enqueue(&fixture, Some(job_id))
                .await
                .expect("a queued job must be returned even if its visibility record fails");

            assert_eq!(
                fixture.app.job_app.find_job_status(&job_id).await?,
                Some(JobProcessingStatus::Pending)
            );
            let queued = queued_len(&fixture).await?;
            assert_eq!(queued, 1);

            fixture.app.job_app.delete_job(&job_id).await?;
            cleanup(fixture).await
        })
    }

    #[test]
    fn record_failure_after_queue_write_does_not_fail_a_regular_enqueue() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let fixture = fixture("enqueue-record-failure-regular", ResponseType::NoResult).await?;
            fixture
                .proxy
                .reject_next_command_on_key_prefix("JOB_INDIVIDUAL:");

            let (job_id, _, _) = fixture
                .app
                .job_app
                .enqueue_job(
                    Arc::new(HashMap::new()),
                    fixture.worker.id.as_ref(),
                    None,
                    Vec::new(),
                    None,
                    0,
                    Priority::Medium as i32,
                    10_000,
                    None,
                    StreamingType::None,
                    None,
                    None,
                )
                .await
                .expect("a queued job must be returned even if its visibility record fails");

            assert_eq!(
                fixture.app.job_app.find_job_status(&job_id).await?,
                Some(JobProcessingStatus::Pending)
            );
            fixture.app.job_app.delete_job(&job_id).await?;
            cleanup(fixture).await
        })
    }

    #[test]
    fn queue_write_rejected_by_redis_releases_pending() -> Result<()> {
        TEST_RUNTIME.block_on(async {
            let fixture = fixture("enqueue-rejected", ResponseType::Direct).await?;
            direct_redis()
                .await?
                .set::<_, _, ()>(&fixture.queue_key, "not a job queue")
                .await?;

            let job_id = JobId {
                value: IdGeneratorWrapper::new().generate_id()?,
            };
            let error = enqueue(&fixture, Some(job_id))
                .await
                .expect_err("Redis rejected the queue write");

            assert!(
                error.downcast_ref::<JobEnqueueOutcomeUnknown>().is_none(),
                "an error reply means the job was not queued: {error:?}"
            );
            assert!(format!("{error:?}").contains("WRONGTYPE"), "{error:?}");
            assert_eq!(
                fixture.app.job_app.find_job_status(&job_id).await?,
                None,
                "a rejected job must not stay pending"
            );

            cleanup(fixture).await
        })
    }
}
