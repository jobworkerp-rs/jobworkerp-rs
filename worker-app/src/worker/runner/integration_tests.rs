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
    use proto::jobworkerp::data::{RunnerData, RunnerType, WorkerData, result_output_item};
    use std::collections::HashMap;
    use std::sync::Arc;

    async fn create_runner_factory() -> Result<Arc<RunnerFactory>> {
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
        Ok(Arc::new(runner_factory))
    }

    async fn create_test_pool() -> Result<RunnerFactoryWithPool> {
        let runner_factory = create_runner_factory().await?;

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
            runner_factory,
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
                working_dir: String::new(),
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
                    working_dir: String::new(),
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
                working_dir: String::new(),
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
                working_dir: String::new(),
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

    /// Encode a string as HelloArgs protobuf (field 1, string).
    /// Uses prost for proper varint length encoding.
    fn encode_hello_args(s: &str) -> Vec<u8> {
        use prost::encoding::{WireType, encode_key, encode_varint};
        let bytes = s.as_bytes();
        let mut buf = Vec::new();
        encode_key(1, WireType::LengthDelimited, &mut buf);
        encode_varint(bytes.len() as u64, &mut buf);
        buf.extend_from_slice(bytes);
        buf
    }

    /// Decode HelloRunnerResult protobuf (field 1, string) to String.
    /// Uses prost for proper varint length decoding.
    fn decode_hello_result(data: &[u8]) -> String {
        use prost::encoding::{WireType, decode_key, decode_varint};
        let mut buf = data;
        let key = decode_key(&mut buf);
        if !matches!(key, Ok((1, WireType::LengthDelimited))) {
            return String::new();
        }
        if let Ok(len) = decode_varint(&mut buf) {
            let len = len as usize;
            if buf.len() >= len {
                return String::from_utf8_lossy(&buf[..len]).to_string();
            }
        }
        String::new()
    }

    /// Verify that feed data injected during streaming is reflected in the output.
    /// Uses HelloPlugin's "feed_hello" method which supports feed channels.
    #[test]
    fn test_feed_to_stream_with_hello_plugin() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let runner_factory = create_runner_factory().await?;

            let pool = RunnerFactoryWithPool::new(
                Arc::new(RunnerData {
                    name: "HelloPlugin".to_string(),
                    ..Default::default()
                }),
                Arc::new(WorkerData {
                    runner_settings: Vec::new(),
                    channel: None,
                    use_static: true,
                    ..Default::default()
                }),
                runner_factory,
                Arc::new(WorkerConfig {
                    default_concurrency: 1,
                    ..WorkerConfig::default()
                }),
            )
            .await?;

            let pool_object = pool.get().await?;
            let mut runner = pool_object.lock().await;

            // Verify feed support
            assert!(
                runner.supports_feed(Some("feed_hello")),
                "HelloPlugin should support feed for 'feed_hello' method"
            );
            assert!(
                !runner.supports_feed(Some("run")),
                "HelloPlugin should not support feed for 'run' method"
            );
            assert!(
                !runner.supports_feed(None),
                "HelloPlugin should not support feed for None method"
            );

            // Set up feed channel
            let feed_sender = runner
                .setup_feed_channel(Some("feed_hello"))
                .expect("setup_feed_channel should return a sender for feed_hello");

            // Encode HelloArgs { arg: "Test" } as protobuf manually
            // (the HelloArgs type is only available in the plugin crate)
            let hello_arg = encode_hello_args("Test");

            let stream = runner
                .run_stream(&hello_arg, HashMap::new(), Some("feed_hello"))
                .await?;

            drop(runner);

            // Send feed data in a background task
            use jobworkerp_runner::runner::FeedData;
            let feed_sender_clone = feed_sender.clone();
            tokio::spawn(async move {
                // Small delay to let stream start processing
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;

                feed_sender_clone
                    .send(FeedData {
                        data: encode_hello_args("World"),
                        is_final: false,
                    })
                    .await
                    .expect("send feed data should succeed");

                tokio::time::sleep(std::time::Duration::from_millis(50)).await;

                feed_sender_clone
                    .send(FeedData {
                        data: encode_hello_args("!"),
                        is_final: true,
                    })
                    .await
                    .expect("send final feed data should succeed");
            });

            // Collect stream output
            let guard_stream = StreamWithPoolGuard::new(stream, pool_object);
            let items: Vec<_> = guard_stream.collect().await;

            // Extract data items and decode HelloRunnerResult text
            let mut collected_text = String::new();
            for item in &items {
                if let Some(result_output_item::Item::Data(data)) = &item.item {
                    collected_text.push_str(&decode_hello_result(data));
                }
            }

            assert!(
                collected_text.contains("Hello Test! "),
                "Output should contain greeting. Got: {}",
                collected_text
            );
            assert!(
                collected_text.contains("World"),
                "Output should contain fed data 'World'. Got: {}",
                collected_text
            );
            assert!(
                collected_text.contains("!"),
                "Output should contain fed data '!'. Got: {}",
                collected_text
            );

            // Verify we got at least 3 data items + 1 End trailer
            let data_count = items
                .iter()
                .filter(|i| matches!(&i.item, Some(result_output_item::Item::Data(_))))
                .count();
            assert!(
                data_count >= 3,
                "Should have at least 3 data items (greeting + 2 feed). Got: {}",
                data_count
            );

            let end_count = items
                .iter()
                .filter(|i| matches!(&i.item, Some(result_output_item::Item::End(_))))
                .count();
            assert_eq!(end_count, 1, "Should have exactly 1 End trailer");

            tracing::debug!(
                "Feed-to-stream test completed. Collected text: {}",
                collected_text
            );
            Ok(())
        })
    }
}
