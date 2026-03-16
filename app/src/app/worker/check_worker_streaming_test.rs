//! Unit tests for WorkerApp::check_worker_streaming, focusing on
//! require_client_stream guard logic added for EnqueueWithClientStream.

#[cfg(test)]
mod tests {
    use crate::app::runner::{RunnerApp, UseRunnerApp};
    use crate::app::worker::WorkerApp;
    use anyhow::Result;
    use async_trait::async_trait;
    use infra::infra::runner::rows::RunnerWithSchema;
    use infra_utils::infra::test::TEST_RUNTIME;
    use jobworkerp_base::error::JobWorkerError;
    use proto::jobworkerp::data::{
        MethodProtoMap, MethodSchema, RunnerData, RunnerId, StreamingOutputType, Worker,
        WorkerData, WorkerId, WorkerSortField,
    };
    use std::sync::Arc;

    // --- Mock implementations ---

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

    #[derive(Debug)]
    struct MockWorkerApp {
        worker_data: Option<WorkerData>,
        runner_schema: Option<RunnerWithSchema>,
    }

    impl UseRunnerApp for MockWorkerApp {
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
            _sort_by: Option<WorkerSortField>,
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

    // --- Helpers ---

    fn make_runner_schema(
        output_type: StreamingOutputType,
        require_client_stream: bool,
    ) -> RunnerWithSchema {
        let mut schemas = std::collections::HashMap::new();
        schemas.insert(
            proto::DEFAULT_METHOD_NAME.to_string(),
            MethodSchema {
                output_type: output_type as i32,
                require_client_stream,
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

    fn make_worker_data() -> WorkerData {
        WorkerData {
            runner_id: Some(RunnerId { value: 1 }),
            ..Default::default()
        }
    }

    fn make_app(output_type: StreamingOutputType, require_client_stream: bool) -> MockWorkerApp {
        MockWorkerApp {
            worker_data: Some(make_worker_data()),
            runner_schema: Some(make_runner_schema(output_type, require_client_stream)),
        }
    }

    // --- Tests ---

    #[test]
    fn test_client_stream_runner_with_require_client_stream_true() {
        TEST_RUNTIME.block_on(async {
            let app = make_app(StreamingOutputType::Streaming, true);
            let wid = WorkerId { value: 1 };
            let result = app
                .check_worker_streaming(&wid, true, Some(true), None)
                .await;
            assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        });
    }

    #[test]
    fn test_forward_guard_non_client_stream_runner_with_require_true() {
        TEST_RUNTIME.block_on(async {
            let app = make_app(StreamingOutputType::Streaming, false);
            let wid = WorkerId { value: 1 };
            let result = app
                .check_worker_streaming(&wid, true, Some(true), None)
                .await;
            let err = result.unwrap_err();
            let jw_err = err.downcast_ref::<JobWorkerError>().unwrap();
            assert!(
                matches!(jw_err, JobWorkerError::InvalidParameter(msg) if msg.contains("does not support client streaming")),
                "expected 'does not support client streaming', got: {:?}",
                jw_err
            );
        });
    }

    #[test]
    fn test_reverse_guard_client_stream_runner_with_require_false() {
        TEST_RUNTIME.block_on(async {
            let app = make_app(StreamingOutputType::Streaming, true);
            let wid = WorkerId { value: 1 };
            let result = app
                .check_worker_streaming(&wid, true, Some(false), None)
                .await;
            let err = result.unwrap_err();
            let jw_err = err.downcast_ref::<JobWorkerError>().unwrap();
            assert!(
                matches!(jw_err, JobWorkerError::InvalidParameter(msg) if msg.contains("requires client streaming")),
                "expected 'requires client streaming', got: {:?}",
                jw_err
            );
        });
    }

    #[test]
    fn test_non_client_stream_runner_with_require_false() {
        TEST_RUNTIME.block_on(async {
            let app = make_app(StreamingOutputType::Streaming, false);
            let wid = WorkerId { value: 1 };
            let result = app
                .check_worker_streaming(&wid, true, Some(false), None)
                .await;
            assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        });
    }

    #[test]
    fn test_both_output_type_with_client_stream() {
        TEST_RUNTIME.block_on(async {
            let app = make_app(StreamingOutputType::Both, true);
            let wid = WorkerId { value: 1 };
            // Both output type allows streaming=true
            let result = app
                .check_worker_streaming(&wid, true, Some(true), None)
                .await;
            assert!(
                result.is_ok(),
                "expected Ok for Both+streaming+client_stream, got: {:?}",
                result
            );
        });
    }

    #[test]
    fn test_streaming_output_rejects_non_streaming_request() {
        TEST_RUNTIME.block_on(async {
            let app = make_app(StreamingOutputType::Streaming, false);
            let wid = WorkerId { value: 1 };
            let result = app
                .check_worker_streaming(&wid, false, Some(false), None)
                .await;
            let err = result.unwrap_err();
            let jw_err = err.downcast_ref::<JobWorkerError>().unwrap();
            assert!(
                matches!(jw_err, JobWorkerError::InvalidParameter(msg) if msg.contains("does not support non-streaming")),
                "expected output type error, got: {:?}",
                jw_err
            );
        });
    }

    #[test]
    fn test_non_streaming_output_rejects_streaming_request() {
        TEST_RUNTIME.block_on(async {
            let app = make_app(StreamingOutputType::NonStreaming, false);
            let wid = WorkerId { value: 1 };
            let result = app
                .check_worker_streaming(&wid, true, Some(false), None)
                .await;
            let err = result.unwrap_err();
            let jw_err = err.downcast_ref::<JobWorkerError>().unwrap();
            assert!(
                matches!(jw_err, JobWorkerError::InvalidParameter(msg) if msg.contains("does not support streaming")),
                "expected output type error, got: {:?}",
                jw_err
            );
        });
    }

    #[test]
    fn test_worker_not_found() {
        TEST_RUNTIME.block_on(async {
            let app = MockWorkerApp {
                worker_data: None,
                runner_schema: Some(make_runner_schema(StreamingOutputType::Streaming, true)),
            };
            let wid = WorkerId { value: 999 };
            let result = app
                .check_worker_streaming(&wid, true, Some(true), None)
                .await;
            let err = result.unwrap_err();
            let jw_err = err.downcast_ref::<JobWorkerError>().unwrap();
            assert!(
                matches!(jw_err, JobWorkerError::NotFound(_)),
                "expected NotFound, got: {:?}",
                jw_err
            );
        });
    }

    #[test]
    fn test_runner_not_found() {
        TEST_RUNTIME.block_on(async {
            let app = MockWorkerApp {
                worker_data: Some(make_worker_data()),
                runner_schema: None,
            };
            let wid = WorkerId { value: 1 };
            let result = app
                .check_worker_streaming(&wid, true, Some(true), None)
                .await;
            let err = result.unwrap_err();
            let jw_err = err.downcast_ref::<JobWorkerError>().unwrap();
            assert!(
                matches!(jw_err, JobWorkerError::InvalidParameter(msg) if msg.contains("runner not found")),
                "expected 'runner not found', got: {:?}",
                jw_err
            );
        });
    }

    #[test]
    fn test_none_require_client_stream_skips_client_stream_check() {
        TEST_RUNTIME.block_on(async {
            // Runner requires client streaming, but require_client_stream=None skips the guard
            let app = make_app(StreamingOutputType::Streaming, true);
            let wid = WorkerId { value: 1 };
            let result = app.check_worker_streaming(&wid, true, None, None).await;
            assert!(
                result.is_ok(),
                "None should skip client stream check, got: {:?}",
                result
            );
        });
    }

    #[test]
    fn test_both_output_type_rejects_non_client_stream_when_required() {
        TEST_RUNTIME.block_on(async {
            // Both output + require_client_stream=true runner, but called from
            // Enqueue RPC (request_streaming=false, require_client_stream=Some(false))
            let app = make_app(StreamingOutputType::Both, true);
            let wid = WorkerId { value: 1 };
            let result = app
                .check_worker_streaming(&wid, false, Some(false), None)
                .await;
            let err = result.unwrap_err();
            let jw_err = err.downcast_ref::<JobWorkerError>().unwrap();
            assert!(
                matches!(jw_err, JobWorkerError::InvalidParameter(msg) if msg.contains("requires client streaming")),
                "expected reverse guard error, got: {:?}",
                jw_err
            );
        });
    }

    #[test]
    fn test_none_require_client_stream_skips_reverse_guard() {
        TEST_RUNTIME.block_on(async {
            // Runner does NOT require client streaming, require_client_stream=None should still pass
            let app = make_app(StreamingOutputType::Streaming, false);
            let wid = WorkerId { value: 1 };
            let result = app.check_worker_streaming(&wid, true, None, None).await;
            assert!(
                result.is_ok(),
                "None should skip client stream check, got: {:?}",
                result
            );
        });
    }
}
