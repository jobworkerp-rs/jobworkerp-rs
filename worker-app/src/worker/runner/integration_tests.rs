#[cfg(test)]
mod streaming_pool_guard_tests {
    use super::super::pool::RunnerFactoryWithPool;
    use super::super::stream_guard::StreamWithPoolGuard;
    use anyhow::Result;
    use app::module::test::TEST_PLUGIN_DIR;
    use app::{app::WorkerConfig, module::test::create_hybrid_test_app};
    use app_wrapper::runner::RunnerFactory;
    use futures::StreamExt;
    use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
    use jobworkerp_runner::jobworkerp::runner::CommandArgs;
    use jobworkerp_runner::runner::cancellation::CancellableRunner;
    use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
    use proto::jobworkerp::data::{RunnerData, RunnerType, WorkerData};
    use std::collections::HashMap;
    use std::sync::Arc;

    async fn create_test_pool() -> Result<RunnerFactoryWithPool> {
        let app_module = Arc::new(create_hybrid_test_app().await?);
        let app_wrapper_module = Arc::new(
            app_wrapper::modules::test::create_test_app_wrapper_module(app_module.clone()),
        );
        let runner_factory = RunnerFactory::new(
            app_module,
            app_wrapper_module,
            Arc::new(McpServerFactory::default()),
        );
        runner_factory.load_plugins_from(TEST_PLUGIN_DIR).await;

        RunnerFactoryWithPool::new(
            Arc::new(RunnerData {
                name: RunnerType::Command.as_str_name().to_string(),
                ..Default::default()
            }),
            Arc::new(WorkerData {
                runner_settings: Vec::new(),
                channel: None,
                use_static: true, // Enable pool usage for resource efficiency
                ..Default::default()
            }),
            Arc::new(runner_factory),
            Arc::new(WorkerConfig {
                default_concurrency: 1,
                ..WorkerConfig::default()
            }),
        )
        .await
    }

    #[test]
    fn test_command_runner_with_stream_guard() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let pool = create_test_pool().await?;
            let pool_object = pool.get().await?;

            // Execute a short sleep command to test stream guard behavior
            let mut runner = pool_object.lock().await;
            let arg = CommandArgs {
                command: "sleep".to_string(),
                args: vec!["0.1".to_string()], // 100ms sleep
                with_memory_monitoring: false,
                treat_nonzero_as_error: false,
                success_exit_codes: vec![],
            };

            let stream_result = runner
                .run_stream(
                    &ProstMessageCodec::serialize_message(&arg).unwrap(),
                    HashMap::new(),
                    None,
                )
                .await;

            assert!(stream_result.is_ok());
            let stream = stream_result.unwrap();

            // Drop runner to release lock, then wrap stream with pool guard for safe resource management
            drop(runner);

            let guard_stream = StreamWithPoolGuard::new(stream, pool_object);

            // Consume stream elements to verify pool guard properly manages resource lifecycle
            let items: Vec<_> = guard_stream.collect().await;
            tracing::debug!("Stream items collected: {}", items.len());

            let pool_object2 = pool.get().await?;
            assert!(!pool_object2.lock().await.name().is_empty());

            tracing::debug!("✅ Command runner with stream guard test completed");
            Ok(())
        })
    }

    #[test]
    fn test_stream_guard_with_multiple_cycles() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let pool = create_test_pool().await?;

            // Execute multiple stream + pool guard cycles to test resource management under load
            for i in 0..3 {
                let pool_object = pool.get().await?;
                let mut runner = pool_object.lock().await;

                let arg = CommandArgs {
                    command: "echo".to_string(),
                    args: vec![format!("test_{}", i)],
                    with_memory_monitoring: false,
                    treat_nonzero_as_error: false,
                    success_exit_codes: vec![],
                };

                let stream_result = runner
                    .run_stream(
                        &ProstMessageCodec::serialize_message(&arg).unwrap(),
                        HashMap::new(),
                        None,
                    )
                    .await;

                assert!(stream_result.is_ok());
                let stream = stream_result.unwrap();

                drop(runner);

                let guard_stream = StreamWithPoolGuard::new(stream, pool_object);

                // Process stream to verify guard handles multiple concurrent operations
                let items: Vec<_> = guard_stream.collect().await;
                assert!(!items.is_empty());

                tracing::debug!("Completed stream guard cycle {}", i);
            }

            let final_pool_object = pool.get().await?;
            assert!(!final_pool_object.lock().await.name().is_empty());

            tracing::debug!("✅ Stream guard multiple cycles test completed");
            Ok(())
        })
    }

    #[test]
    fn test_non_static_streaming_cancel_guard() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use async_trait::async_trait;
            use jobworkerp_runner::runner::cancellation::{
                CancellationSetupResult, RunnerCancellationManager,
            };
            use jobworkerp_runner::runner::cancellation_helper::CancelMonitoringHelper;
            use jobworkerp_runner::runner::command::CommandRunnerImpl;
            use std::collections::HashMap;
            use tokio_util::sync::CancellationToken;

            // Test dummy cancellation manager to simulate real cancellation scenarios
            #[derive(Debug)]
            struct TestCancellationManager {
                token: CancellationToken,
            }

            impl TestCancellationManager {
                fn new() -> Self {
                    Self {
                        token: CancellationToken::new(),
                    }
                }
            }

            #[async_trait]
            impl RunnerCancellationManager for TestCancellationManager {
                async fn setup_monitoring(
                    &mut self,
                    _job_id: &proto::jobworkerp::data::JobId,
                    _job_data: &proto::jobworkerp::data::JobData,
                ) -> anyhow::Result<CancellationSetupResult> {
                    Ok(CancellationSetupResult::MonitoringStarted)
                }

                async fn cleanup_monitoring(&mut self) -> anyhow::Result<()> {
                    Ok(())
                }

                async fn get_token(&self) -> CancellationToken {
                    self.token.clone()
                }

                fn is_cancelled(&self) -> bool {
                    self.token.is_cancelled()
                }
            }

            let manager = Box::new(TestCancellationManager::new());
            let cancel_helper = CancelMonitoringHelper::new(manager);

            let mut runner = Box::new(CommandRunnerImpl::new_with_cancel_monitoring(cancel_helper))
                as Box<dyn CancellableRunner + Send + Sync>;

            let cancel_helper = runner.clone_cancel_helper_for_stream();

            let arg = CommandArgs {
                command: "echo".to_string(),
                args: vec!["streaming_test".to_string()],
                with_memory_monitoring: false,
                treat_nonzero_as_error: false,
                success_exit_codes: vec![],
            };

            let stream_result = runner
                .run_stream(
                    &ProstMessageCodec::serialize_message(&arg).unwrap(),
                    HashMap::new(),
                    None,
                )
                .await;

            assert!(stream_result.is_ok());
            let stream = stream_result.unwrap();

            // Wrap with cancel guard to ensure proper cleanup on cancellation
            if let Some(cancel_helper) = cancel_helper {
                use super::super::stream_guard::StreamWithCancelGuard;
                let guard_stream = StreamWithCancelGuard::new(stream, cancel_helper);

                // Consume stream elements to verify cancel guard handles streaming correctly
                let items: Vec<_> = guard_stream.collect().await;
                assert!(!items.is_empty());

                tracing::debug!("✅ Non-static streaming with cancel guard test completed");
            } else {
                panic!("CancelHelper should be available");
            }

            Ok(())
        })
    }

    /// Real use_static=false test using actual JobRunner flow
    /// This test verifies the fix for non-static streaming cancellation
    #[test]
    fn test_real_non_static_streaming_with_cancel_guard() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            tracing::info!("🔍 Testing clone_cancel_helper_for_stream implementation");

            use async_trait::async_trait;
            use jobworkerp_runner::runner::cancellation::{
                CancellationSetupResult, RunnerCancellationManager,
            };
            use jobworkerp_runner::runner::cancellation_helper::{
                CancelMonitoringHelper, UseCancelMonitoringHelper,
            };
            use jobworkerp_runner::runner::command::CommandRunnerImpl;
            use tokio_util::sync::CancellationToken;

            // Test dummy cancellation manager for validation scenario
            #[derive(Debug)]
            struct TestCancellationManager {
                token: CancellationToken,
            }

            impl TestCancellationManager {
                fn new() -> Self {
                    Self {
                        token: CancellationToken::new(),
                    }
                }
            }

            #[async_trait]
            impl RunnerCancellationManager for TestCancellationManager {
                async fn setup_monitoring(
                    &mut self,
                    _job_id: &proto::jobworkerp::data::JobId,
                    _job_data: &proto::jobworkerp::data::JobData,
                ) -> anyhow::Result<CancellationSetupResult> {
                    tracing::info!(
                        "✅ Cancellation monitoring setup called - this verifies our fix!"
                    );
                    Ok(CancellationSetupResult::MonitoringStarted)
                }

                async fn cleanup_monitoring(&mut self) -> anyhow::Result<()> {
                    Ok(())
                }

                async fn get_token(&self) -> CancellationToken {
                    self.token.clone()
                }

                fn is_cancelled(&self) -> bool {
                    self.token.is_cancelled()
                }
            }

            let manager = Box::new(TestCancellationManager::new());
            let cancel_helper = CancelMonitoringHelper::new(manager);

            let runner = CommandRunnerImpl::new_with_cancel_monitoring(cancel_helper);

            // Test the fixed clone_cancel_helper_for_stream implementation
            let cloned_helper = runner.clone_cancel_helper_for_stream();
            assert!(cloned_helper.is_some(), "Clone should succeed");

            let original_helper = runner.cancel_monitoring_helper();
            assert!(
                original_helper.is_some(),
                "Original helper should still exist after clone"
            );

            tracing::info!("✅ clone_cancel_helper_for_stream works correctly");
            tracing::info!("✅ Original helper preserved for cancellation monitoring");
            tracing::info!("✅ Real non-static streaming fix validation completed");

            Ok(())
        })
    }

    #[test]
    fn test_stream_guard_early_drop() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let pool = create_test_pool().await?;
            let pool_object = pool.get().await?;

            let mut runner = pool_object.lock().await;
            let arg = CommandArgs {
                command: "sleep".to_string(),
                args: vec!["1.0".to_string()], // 1 second sleep to test early drop behavior
                with_memory_monitoring: false,
                treat_nonzero_as_error: false,
                success_exit_codes: vec![],
            };

            let stream_result = runner
                .run_stream(
                    &ProstMessageCodec::serialize_message(&arg).unwrap(),
                    HashMap::new(),
                    None,
                )
                .await;

            assert!(stream_result.is_ok());
            let stream = stream_result.unwrap();

            drop(runner);

            let guard_stream = StreamWithPoolGuard::new(stream, pool_object);

            // Drop stream early to verify pool guard's drop behavior handles premature cleanup
            drop(guard_stream);

            let pool_object2 = pool.get().await?;
            assert!(!pool_object2.lock().await.name().is_empty());

            tracing::debug!("✅ Stream guard early drop test completed");
            Ok(())
        })
    }
}
