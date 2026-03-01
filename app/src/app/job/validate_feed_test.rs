//! Unit tests for validate_and_publish_feed function.
//!
//! Tests each validation case: job not running, non-streaming, use_static=false,
//! concurrency != 1, and need_feed=false.

#[cfg(test)]
mod tests {
    use super::super::validate_and_publish_feed;
    use crate::app::WorkerConfig;
    use crate::app::runner::RunnerApp;
    use crate::app::worker::WorkerApp;
    use anyhow::Result;
    use async_trait::async_trait;
    use infra::infra::feed::FeedPublisher;
    use infra::infra::runner::rows::RunnerWithSchema;
    use infra_utils::infra::test::TEST_RUNTIME;
    use jobworkerp_base::error::JobWorkerError;
    use proto::jobworkerp::data::{
        Job, JobData, JobId, JobProcessingStatus, MethodProtoMap, MethodSchema, RunnerData,
        RunnerId, StreamingType, Worker, WorkerData, WorkerId,
    };
    use std::sync::Arc;

    #[derive(Debug)]
    struct MockWorkerApp {
        worker_data: Option<WorkerData>,
        runner_schema: Option<RunnerWithSchema>,
    }

    #[derive(Debug)]
    struct MockRunnerApp {
        runner_schema: Option<RunnerWithSchema>,
    }

    #[async_trait]
    impl RunnerApp for MockRunnerApp {
        async fn load_runner(&self) -> Result<bool> {
            Ok(false)
        }
        async fn create_runner(
            &self,
            _name: &str,
            _description: &str,
            _runner_type: i32,
            _definition: &str,
        ) -> Result<RunnerId> {
            unimplemented!()
        }
        async fn delete_runner(&self, _id: &RunnerId) -> Result<bool> {
            unimplemented!()
        }
        async fn find_runner(&self, _id: &RunnerId) -> Result<Option<RunnerWithSchema>> {
            Ok(self.runner_schema.clone())
        }
        async fn find_runner_by_name(&self, _name: &str) -> Result<Option<RunnerWithSchema>> {
            Ok(None)
        }
        async fn find_runner_list(
            &self,
            _include_full: bool,
            _limit: Option<&i32>,
            _offset: Option<&i64>,
        ) -> Result<Vec<RunnerWithSchema>> {
            Ok(vec![])
        }
        async fn find_runner_all_list(&self, _include_full: bool) -> Result<Vec<RunnerWithSchema>> {
            Ok(vec![])
        }
        async fn count(&self) -> Result<i64> {
            Ok(0)
        }
        async fn find_runner_list_by(
            &self,
            _runner_types: Vec<i32>,
            _name_filter: Option<String>,
            _limit: Option<i32>,
            _offset: Option<i64>,
            _sort_by: Option<proto::jobworkerp::data::RunnerSortField>,
            _ascending: Option<bool>,
        ) -> Result<Vec<RunnerWithSchema>> {
            Ok(vec![])
        }
        async fn count_by(
            &self,
            _runner_types: Vec<i32>,
            _name_filter: Option<String>,
        ) -> Result<i64> {
            Ok(0)
        }
        #[cfg(any(test, feature = "test-utils"))]
        async fn create_test_runner(
            &self,
            _runner_id: &RunnerId,
            _name: &str,
        ) -> Result<crate::app::runner::RunnerDataWithDescriptor> {
            unimplemented!()
        }
    }

    impl crate::app::runner::UseRunnerApp for MockWorkerApp {
        fn runner_app(&self) -> Arc<dyn RunnerApp> {
            Arc::new(MockRunnerApp {
                runner_schema: self.runner_schema.clone(),
            })
        }
    }

    #[async_trait]
    impl WorkerApp for MockWorkerApp {
        async fn create(&self, _worker: &WorkerData) -> Result<WorkerId> {
            unimplemented!()
        }
        async fn create_temp(
            &self,
            _worker: WorkerData,
            _with_random_name: bool,
        ) -> Result<WorkerId> {
            unimplemented!()
        }
        async fn update(&self, _id: &WorkerId, _worker: &Option<WorkerData>) -> Result<bool> {
            unimplemented!()
        }
        async fn delete(&self, _id: &WorkerId) -> Result<bool> {
            unimplemented!()
        }
        async fn delete_temp(&self, _id: &WorkerId) -> Result<bool> {
            unimplemented!()
        }
        async fn delete_all(&self) -> Result<bool> {
            unimplemented!()
        }
        async fn find(&self, _id: &WorkerId) -> Result<Option<Worker>> {
            Ok(self.worker_data.as_ref().map(|d| Worker {
                id: Some(WorkerId { value: 1 }),
                data: Some(d.clone()),
            }))
        }
        async fn find_by_name(&self, _name: &str) -> Result<Option<Worker>> {
            Ok(None)
        }
        async fn find_list(
            &self,
            _runner_types: Vec<i32>,
            _channel: Option<String>,
            _limit: Option<i32>,
            _offset: Option<i64>,
            _name_filter: Option<String>,
            _is_periodic: Option<bool>,
            _runner_ids: Vec<i64>,
            _sort_by: Option<proto::jobworkerp::data::WorkerSortField>,
            _ascending: Option<bool>,
        ) -> Result<Vec<Worker>> {
            Ok(vec![])
        }
        async fn find_all_worker_list(&self) -> Result<Vec<Worker>> {
            Ok(vec![])
        }
        async fn count(&self) -> Result<i64> {
            Ok(0)
        }
        async fn count_by(
            &self,
            _runner_types: Vec<i32>,
            _channel: Option<String>,
            _name_filter: Option<String>,
            _is_periodic: Option<bool>,
            _runner_ids: Vec<i64>,
        ) -> Result<i64> {
            Ok(0)
        }
        async fn count_by_channel(&self) -> Result<Vec<(String, i64)>> {
            Ok(vec![])
        }
        async fn clear_cache_by(
            &self,
            _id: Option<&WorkerId>,
            _name: Option<&String>,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct MockFeedPublisher {
        published: std::sync::Mutex<Vec<(i64, Vec<u8>, bool)>>,
    }

    #[async_trait]
    impl FeedPublisher for MockFeedPublisher {
        async fn publish_feed(&self, job_id: &JobId, data: Vec<u8>, is_final: bool) -> Result<()> {
            self.published
                .lock()
                .unwrap()
                .push((job_id.value, data, is_final));
            Ok(())
        }
    }

    fn make_job(job_id: i64, worker_id: i64, streaming_type: StreamingType) -> Job {
        Job {
            id: Some(JobId { value: job_id }),
            data: Some(JobData {
                worker_id: Some(WorkerId { value: worker_id }),
                streaming_type: streaming_type as i32,
                using: None,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn make_runner_schema(need_feed: bool) -> RunnerWithSchema {
        let mut schemas = std::collections::HashMap::new();
        schemas.insert(
            proto::DEFAULT_METHOD_NAME.to_string(),
            MethodSchema {
                need_feed,
                ..Default::default()
            },
        );
        RunnerWithSchema {
            id: Some(RunnerId { value: 1 }),
            data: Some(RunnerData {
                method_proto_map: Some(MethodProtoMap { schemas }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn make_worker_data(use_static: bool) -> WorkerData {
        WorkerData {
            runner_id: Some(RunnerId { value: 1 }),
            use_static,
            channel: None,
            ..Default::default()
        }
    }

    fn make_config(concurrency: u32) -> WorkerConfig {
        WorkerConfig {
            default_concurrency: concurrency,
            channels: vec![],
            channel_concurrencies: vec![],
        }
    }

    fn make_publisher() -> MockFeedPublisher {
        MockFeedPublisher {
            published: std::sync::Mutex::new(vec![]),
        }
    }

    #[test]
    fn test_validate_feed_not_running() {
        TEST_RUNTIME.block_on(async {
            let job = make_job(1, 1, StreamingType::Response);
            let mock_app = MockWorkerApp {
                worker_data: Some(make_worker_data(true)),
                runner_schema: Some(make_runner_schema(true)),
            };
            let publisher = make_publisher();
            let job_id = JobId { value: 1 };

            let result = validate_and_publish_feed(
                &job,
                Some(JobProcessingStatus::Pending),
                &mock_app,
                &make_config(1),
                &publisher,
                &job_id,
                vec![1, 2, 3],
                false,
            )
            .await;

            let err = result.unwrap_err();
            let jw_err = err.downcast_ref::<JobWorkerError>().unwrap();
            assert!(
                matches!(jw_err, JobWorkerError::FailedPrecondition(_)),
                "expected FailedPrecondition, got: {:?}",
                jw_err
            );
        });
    }

    #[test]
    fn test_validate_feed_not_streaming() {
        TEST_RUNTIME.block_on(async {
            let job = make_job(1, 1, StreamingType::None);
            let mock_app = MockWorkerApp {
                worker_data: Some(make_worker_data(true)),
                runner_schema: Some(make_runner_schema(true)),
            };
            let publisher = make_publisher();
            let job_id = JobId { value: 1 };

            let result = validate_and_publish_feed(
                &job,
                Some(JobProcessingStatus::Running),
                &mock_app,
                &make_config(1),
                &publisher,
                &job_id,
                vec![1, 2, 3],
                false,
            )
            .await;

            let err = result.unwrap_err();
            let jw_err = err.downcast_ref::<JobWorkerError>().unwrap();
            assert!(matches!(jw_err, JobWorkerError::FailedPrecondition(_)));
        });
    }

    #[test]
    fn test_validate_feed_not_static() {
        TEST_RUNTIME.block_on(async {
            let job = make_job(1, 1, StreamingType::Response);
            let mock_app = MockWorkerApp {
                worker_data: Some(make_worker_data(false)),
                runner_schema: Some(make_runner_schema(true)),
            };
            let publisher = make_publisher();
            let job_id = JobId { value: 1 };

            let result = validate_and_publish_feed(
                &job,
                Some(JobProcessingStatus::Running),
                &mock_app,
                &make_config(1),
                &publisher,
                &job_id,
                vec![1, 2, 3],
                false,
            )
            .await;

            let err = result.unwrap_err();
            let jw_err = err.downcast_ref::<JobWorkerError>().unwrap();
            assert!(matches!(jw_err, JobWorkerError::FailedPrecondition(_)));
        });
    }

    #[test]
    fn test_validate_feed_concurrency_not_one() {
        TEST_RUNTIME.block_on(async {
            let job = make_job(1, 1, StreamingType::Response);
            let mock_app = MockWorkerApp {
                worker_data: Some(make_worker_data(true)),
                runner_schema: Some(make_runner_schema(true)),
            };
            let publisher = make_publisher();
            let job_id = JobId { value: 1 };

            let result = validate_and_publish_feed(
                &job,
                Some(JobProcessingStatus::Running),
                &mock_app,
                &make_config(4), // concurrency > 1
                &publisher,
                &job_id,
                vec![1, 2, 3],
                false,
            )
            .await;

            let err = result.unwrap_err();
            let jw_err = err.downcast_ref::<JobWorkerError>().unwrap();
            assert!(
                matches!(jw_err, JobWorkerError::FailedPrecondition(_)),
                "expected FailedPrecondition for concurrency > 1, got: {:?}",
                jw_err
            );
        });
    }

    #[test]
    fn test_validate_feed_no_need_feed() {
        TEST_RUNTIME.block_on(async {
            let job = make_job(1, 1, StreamingType::Response);
            let mock_app = MockWorkerApp {
                worker_data: Some(make_worker_data(true)),
                runner_schema: Some(make_runner_schema(false)),
            };
            let publisher = make_publisher();
            let job_id = JobId { value: 1 };

            let result = validate_and_publish_feed(
                &job,
                Some(JobProcessingStatus::Running),
                &mock_app,
                &make_config(1),
                &publisher,
                &job_id,
                vec![1, 2, 3],
                false,
            )
            .await;

            let err = result.unwrap_err();
            let jw_err = err.downcast_ref::<JobWorkerError>().unwrap();
            assert!(matches!(jw_err, JobWorkerError::FailedPrecondition(_)));
        });
    }

    /// Verifies that the fast path works correctly when a feed sender is registered
    /// in ChanFeedSenderStore but no job record exists.
    /// This simulates the race condition where cleanup_job() deletes the job record
    /// while the feed sender is still active.
    #[test]
    fn test_fast_path_feed_without_job_record() {
        use infra::infra::feed::chan::ChanFeedSenderStore;
        use jobworkerp_runner::runner::FeedData;
        use tokio::sync::mpsc;

        TEST_RUNTIME.block_on(async {
            let store = ChanFeedSenderStore::new();
            let job_id = JobId { value: 42 };

            // Register a feed sender directly (simulating run_job() registration)
            let (tx, mut rx) = mpsc::channel::<FeedData>(16);
            store.register(job_id.value, tx);

            // Verify has_active_feed returns Some(true)
            assert_eq!(store.has_active_feed(&job_id), Some(true));

            // Publish via fast path (no job record needed)
            let result = store
                .publish_feed(&job_id, vec![1, 2, 3], false)
                .await;
            assert!(result.is_ok());

            // Verify data was received correctly
            let feed = rx.recv().await.unwrap();
            assert_eq!(feed.data, vec![1, 2, 3]);
            assert!(!feed.is_final);

            // Publish final message
            let result = store
                .publish_feed(&job_id, vec![4, 5], true)
                .await;
            assert!(result.is_ok());

            let feed = rx.recv().await.unwrap();
            assert_eq!(feed.data, vec![4, 5]);
            assert!(feed.is_final);

            // After final, sender is removed
            assert_eq!(store.has_active_feed(&job_id), Some(false));
        });
    }

    /// Verifies that when has_active_feed returns Some(false) (sender removed between
    /// has_active_feed and publish_feed due to TOCTOU), publish_feed returns an error
    /// rather than panicking.
    #[test]
    fn test_fast_path_toctou_safety() {
        use infra::infra::feed::chan::ChanFeedSenderStore;
        use jobworkerp_runner::runner::FeedData;
        use tokio::sync::mpsc;

        TEST_RUNTIME.block_on(async {
            let store = ChanFeedSenderStore::new();
            let job_id = JobId { value: 99 };

            // Register then immediately remove (simulating concurrent cleanup)
            let (tx, _rx) = mpsc::channel::<FeedData>(16);
            store.register(job_id.value, tx);
            store.remove(job_id.value);

            // has_active_feed now returns false
            assert_eq!(store.has_active_feed(&job_id), Some(false));

            // publish_feed should return error, not panic
            let result = store
                .publish_feed(&job_id, vec![1], false)
                .await;
            assert!(result.is_err());
        });
    }

    #[test]
    fn test_validate_feed_success() {
        TEST_RUNTIME.block_on(async {
            let job = make_job(1, 1, StreamingType::Response);
            let mock_app = MockWorkerApp {
                worker_data: Some(make_worker_data(true)),
                runner_schema: Some(make_runner_schema(true)),
            };
            let publisher = make_publisher();
            let job_id = JobId { value: 1 };

            let result = validate_and_publish_feed(
                &job,
                Some(JobProcessingStatus::Running),
                &mock_app,
                &make_config(1),
                &publisher,
                &job_id,
                vec![1, 2, 3],
                true,
            )
            .await;

            assert!(result.is_ok());
            let published = publisher.published.lock().unwrap();
            assert_eq!(published.len(), 1);
            assert_eq!(published[0], (1, vec![1, 2, 3], true));
        });
    }
}
