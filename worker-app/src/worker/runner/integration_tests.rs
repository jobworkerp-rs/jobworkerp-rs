#[cfg(test)]
mod streaming_pool_guard_tests {
    use super::super::pool::RunnerFactoryWithPool;
    use super::super::stream_guard::StreamWithPoolGuard;
    use anyhow::Result;
    use std::sync::Arc;
    use std::collections::HashMap;
    use app::module::test::TEST_PLUGIN_DIR;
    use app_wrapper::runner::RunnerFactory;
    use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
    use jobworkerp_runner::runner::command::CommandRunnerImpl;
    use jobworkerp_runner::runner::RunnerTrait;
    use jobworkerp_runner::jobworkerp::runner::CommandArgs;
    use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
    use proto::jobworkerp::data::{RunnerType, WorkerData, RunnerData};
    use app::{app::WorkerConfig, module::test::create_hybrid_test_app};
    use futures::StreamExt;

    async fn create_test_pool() -> Result<RunnerFactoryWithPool> {
        let app_module = Arc::new(create_hybrid_test_app().await?);
        let app_wrapper_module = Arc::new(
            app_wrapper::modules::test::create_test_app_wrapper_module(app_module.clone())
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
                use_static: true, // Pool使用
                ..Default::default()
            }),
            Arc::new(runner_factory),
            Arc::new(WorkerConfig {
                default_concurrency: 1,
                ..WorkerConfig::default()
            }),
        ).await
    }

    #[test]
    fn test_command_runner_with_stream_guard() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let pool = create_test_pool().await?;
            let pool_object = pool.get().await?;
            
            // CommandRunnerで短いsleepコマンドを実行
            let mut runner = pool_object.lock().await;
            if let Some(command_runner) = runner
                .as_any_mut()
                .downcast_mut::<CommandRunnerImpl>()
            {
                let arg = CommandArgs {
                    command: "sleep".to_string(),
                    args: vec!["0.1".to_string()], // 100ms sleep
                    with_memory_monitoring: false,
                };

                let stream_result = command_runner
                    .run_stream(
                        &ProstMessageCodec::serialize_message(&arg).unwrap(),
                        HashMap::new(),
                    )
                    .await;

                assert!(stream_result.is_ok());
                let stream = stream_result.unwrap();
                
                // RunnerをDropして、StreamをStreamWithPoolGuardでラップ
                drop(runner);
                
                let guard_stream = StreamWithPoolGuard::new(stream, pool_object);
                
                // Stream要素を消費（これによりPool Guard動作確認）
                let items: Vec<_> = guard_stream.collect().await;
                tracing::debug!("Stream items collected: {}", items.len());
                
                // Pool が再利用可能であることを確認
                let pool_object2 = pool.get().await?;
                assert!(!pool_object2.lock().await.name().is_empty());
            }
            
            tracing::debug!("✅ Command runner with stream guard test completed");
            Ok(())
        })
    }

    #[test]
    fn test_stream_guard_with_multiple_cycles() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let pool = create_test_pool().await?;

            // 複数回のStream + Pool Guard サイクルを実行
            for i in 0..3 {
                let pool_object = pool.get().await?;
                let mut runner = pool_object.lock().await;
                
                if let Some(command_runner) = runner
                    .as_any_mut()
                    .downcast_mut::<CommandRunnerImpl>()
                {
                    let arg = CommandArgs {
                        command: "echo".to_string(),
                        args: vec![format!("test_{}", i)],
                        with_memory_monitoring: false,
                    };

                    let stream_result = command_runner
                        .run_stream(
                            &ProstMessageCodec::serialize_message(&arg).unwrap(),
                            HashMap::new(),
                        )
                        .await;

                    assert!(stream_result.is_ok());
                    let stream = stream_result.unwrap();
                    
                    drop(runner);
                    
                    let guard_stream = StreamWithPoolGuard::new(stream, pool_object);
                    
                    // Stream処理
                    let items: Vec<_> = guard_stream.collect().await;
                    assert!(!items.is_empty());
                }
                
                tracing::debug!("Completed stream guard cycle {}", i);
            }

            // 最終的にPoolが正常動作することを確認
            let final_pool_object = pool.get().await?;
            assert!(!final_pool_object.lock().await.name().is_empty());
            
            tracing::debug!("✅ Stream guard multiple cycles test completed");
            Ok(())
        })
    }

    #[test]
    fn test_stream_guard_early_drop() -> Result<()> {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let pool = create_test_pool().await?;
            let pool_object = pool.get().await?;
            
            let mut runner = pool_object.lock().await;
            if let Some(command_runner) = runner
                .as_any_mut()
                .downcast_mut::<CommandRunnerImpl>()
            {
                let arg = CommandArgs {
                    command: "sleep".to_string(),
                    args: vec!["1.0".to_string()], // 1秒sleep
                    with_memory_monitoring: false,
                };

                let stream_result = command_runner
                    .run_stream(
                        &ProstMessageCodec::serialize_message(&arg).unwrap(),
                        HashMap::new(),
                    )
                    .await;

                assert!(stream_result.is_ok());
                let stream = stream_result.unwrap();
                
                drop(runner);
                
                let guard_stream = StreamWithPoolGuard::new(stream, pool_object);
                
                // Stream を途中で破棄（Pool Guard の Drop 動作確認）
                drop(guard_stream);
                
                // Pool が再利用可能であることを確認
                let pool_object2 = pool.get().await?;
                assert!(!pool_object2.lock().await.name().is_empty());
            }
            
            tracing::debug!("✅ Stream guard early drop test completed");
            Ok(())
        })
    }
}