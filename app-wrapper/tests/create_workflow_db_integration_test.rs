use anyhow::Result;
use app::module::test::create_hybrid_test_app;
use app_wrapper::workflow::create_workflow::CreateWorkflowRunnerImpl;
use infra_utils::infra::test::TEST_RUNTIME;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_runner::jobworkerp::runner::create_workflow_args::{
    RetryPolicy, WorkerOptions, WorkflowSource,
};
use jobworkerp_runner::jobworkerp::runner::{CreateWorkflowArgs, CreateWorkflowResult};
use jobworkerp_runner::runner::RunnerTrait;
use proto::jobworkerp::data::{ResponseType, RetryType};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// CREATE_WORKFLOW DB統合テスト
/// 実際のDBを使用してワーカー作成とアクセスを確認
#[test]
fn test_create_workflow_runner_db_integration() -> Result<()> {
    println!("🗄️ CREATE_WORKFLOW DB統合テスト開始");
    TEST_RUNTIME.block_on(async {
        // AppModuleを初期化（test用のhybrid setup）
        let app = Arc::new(create_hybrid_test_app().await?);

        // CreateWorkflowRunnerImplを初期化
        let mut runner = CreateWorkflowRunnerImpl::new(app.clone())?;

        // テスト用のワークフロー定義
        let test_workflow = json!({
            "document": {
                "dsl": "0.0.1",
                "namespace": "db-integration-test",
                "name": "create-workflow-db-test",
                "version": "1.0.0",
                "summary": "CREATE_WORKFLOW DB統合テスト用ワークフロー"
            },
            "input": {
                "from": ".testInput"
            },
            "do": [
                {
                    "db_test_step": {
                        "run": {
                            "runner": {
                                "name": "COMMAND",
                                "arguments": {
                                    "command": "echo",
                                    "args": ["DB統合テスト実行中"]
                                }
                            }
                        }
                    }
                }
            ]
        });

        let worker_name = "db-test-create-workflow-worker";
        let test_args = CreateWorkflowArgs {
            workflow_source: Some(WorkflowSource::WorkflowData(test_workflow.to_string())),
            name: worker_name.to_string(),
            worker_options: Some(WorkerOptions {
                channel: Some("db-test-channel".to_string()),
                response_type: Some(ResponseType::Direct as i32),
                broadcast_results: true,
                queue_type: proto::jobworkerp::data::QueueType::ForcedRdb as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                retry_policy: Some(RetryPolicy {
                    r#type: RetryType::Exponential as i32,
                    interval: 2000,
                    max_interval: 30000,
                    max_retry: 3,
                    basis: 2.0,
                }),
            }),
        };

        // Proto serialization
        let serialized_args = ProstMessageCodec::serialize_message(&test_args)?;
        println!(
            "✅ Proto serialization完了: {} bytes",
            serialized_args.len()
        );

        // CREATE_WORKFLOW実行
        let metadata = HashMap::new();
        let (result, _returned_metadata) = runner.run(&serialized_args, metadata).await;

        match result {
            Ok(output_bytes) => {
                println!("✅ CREATE_WORKFLOW実行成功");

                // 結果のDeserialization
                let create_result: CreateWorkflowResult =
                    ProstMessageCodec::deserialize_message(&output_bytes)?;

                // WorkerIDの検証
                assert!(
                    create_result.worker_id.is_some(),
                    "WorkerId should be present"
                );
                let worker_id = create_result.worker_id.unwrap();
                assert!(worker_id.value > 0, "WorkerId should be valid and positive");

                println!("✅ WorkerID生成成功: {}", worker_id.value);

                // DB内でワーカーが作成されたことを確認
                let found_worker = app.worker_app.find_by_name(worker_name).await?;
                assert!(
                    found_worker.is_some(),
                    "作成されたワーカーがDBに見つからない"
                );

                let found_worker = found_worker.unwrap();
                let worker_data = found_worker.data.as_ref().expect("WorkerData should exist");
                println!("✅ DB内でワーカー確認成功:");
                println!("   - Worker Name: {}", worker_data.name);
                println!(
                    "   - Worker ID: {}",
                    found_worker.id.as_ref().unwrap().value
                );
                println!(
                    "   - Runner ID: {}",
                    worker_data.runner_id.as_ref().unwrap().value
                );
                println!("   - Channel: {:?}", worker_data.channel);

                // ワーカーの詳細を検証
                assert_eq!(worker_data.name, worker_name);
                assert_eq!(found_worker.id.as_ref().unwrap().value, worker_id.value);
                assert!(worker_data.runner_id.as_ref().unwrap().value > 0);
                assert_eq!(worker_data.channel.as_deref(), Some("db-test-channel"));
                assert!(worker_data.store_success);
                assert!(worker_data.store_failure);
                assert_eq!(worker_data.response_type, ResponseType::Direct as i32);
                assert_eq!(
                    worker_data.queue_type,
                    proto::jobworkerp::data::QueueType::ForcedRdb as i32
                );
                assert_eq!(
                    worker_data.retry_policy.as_ref().map(|p| p.r#type),
                    Some(RetryType::Exponential as i32)
                );
                assert_eq!(
                    worker_data.retry_policy.as_ref().map(|p| p.interval),
                    Some(2000)
                );
                assert_eq!(
                    worker_data.retry_policy.as_ref().map(|p| p.max_interval),
                    Some(30000)
                );
                assert_eq!(
                    worker_data.retry_policy.as_ref().map(|p| p.max_retry),
                    Some(3)
                );
                assert_eq!(
                    worker_data.retry_policy.as_ref().map(|p| p.basis),
                    Some(2.0)
                );

                println!("✅ CREATE_WORKFLOW DB統合テスト成功:");
                println!("   - Workflow validation: PASSED");
                println!("   - Worker creation: PASSED");
                println!("   - Database persistence: PASSED");
                println!("   - Worker retrieval: PASSED");
                println!("   - Options configuration: PASSED");
            }
            Err(e) => {
                println!("❌ CREATE_WORKFLOW実行失敗: {e}");
                return Err(e);
            }
        }
        Ok(())
    })
}

/// CREATE_WORKFLOW WorkflowURL DB統合テスト
/// URLからワークフローを取得してワーカー作成
#[ignore = "local url"]
#[test]
fn test_create_workflow_url_db_integration() -> Result<()> {
    //command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    println!("🌐 CREATE_WORKFLOW URL DB統合テスト開始");
    TEST_RUNTIME.block_on(async {

    // AppModuleを初期化（test用のhybrid setup）
    let app = Arc::new(create_hybrid_test_app().await?);

    // CreateWorkflowRunnerImplを初期化
    let mut runner = CreateWorkflowRunnerImpl::new(app.clone())?;

    let worker_name = "url-db-test-workflow-worker";
    let test_url = "https://gitea.sutr.app/jobworkerp-rs/workflow-files/raw/branch/main/deep-research/workflows/deep_research.yml";

    let url_args = CreateWorkflowArgs {
        workflow_source: Some(WorkflowSource::WorkflowUrl(test_url.to_string())),
        name: worker_name.to_string(),
        worker_options: Some(WorkerOptions {
            channel: Some("url-db-test-channel".to_string()),
            response_type: Some(ResponseType::NoResult as i32),
            broadcast_results: false,
            queue_type: proto::jobworkerp::data::QueueType::WithBackup as i32,
            store_success: true,
            store_failure: true,
            use_static: false,
            retry_policy: None,
        }),
    };

    // Proto serialization
    let serialized_args = ProstMessageCodec::serialize_message(&url_args)?;

    // CREATE_WORKFLOW実行
    let metadata = HashMap::new();
    let (result, _returned_metadata) = runner.run(&serialized_args, metadata).await;

    match result {
        Ok(output_bytes) => {
            println!("✅ CREATE_WORKFLOW URL実行成功");

            // 結果のDeserialization
            let create_result: CreateWorkflowResult =
                ProstMessageCodec::deserialize_message(&output_bytes)?;

            // WorkerIDの検証
            assert!(create_result.worker_id.is_some());
            let worker_id = create_result.worker_id.unwrap();

            println!("✅ URL経由WorkerID生成: {}", worker_id.value);

            // DB内でワーカーが作成されたことを確認
            let found_worker = app.worker_app.find_by_name(worker_name).await?;
            assert!(
                found_worker.is_some(),
                "URL経由で作成されたワーカーがDBに見つからない"
            );

            let found_worker = found_worker.unwrap();
            let worker_data = found_worker.data.as_ref().expect("WorkerData should exist");
            println!("✅ URL経由ワーカーDB確認成功:");
            println!("   - Worker Name: {}", worker_data.name);
            println!("   - Source URL: {test_url}");
            println!(
                "   - Worker ID: {}",
                found_worker.id.as_ref().unwrap().value
            );
            println!("   - Channel: {:?}", worker_data.channel);

            // オプション検証
            assert_eq!(worker_data.name, worker_name);
            assert_eq!(worker_data.channel.as_deref(), Some("url-db-test-channel"));
            assert_eq!(worker_data.response_type, ResponseType::NoResult as i32);
            // with_backup は queue_type で確認
            assert_eq!(
                worker_data.queue_type,
                proto::jobworkerp::data::QueueType::WithBackup as i32
            );
        }
        Err(e) => {
            // ネットワークエラーやvalidationエラーは許容
            let error_msg = e.to_string();
            if error_msg.contains("timeout")
                || error_msg.contains("network")
                || error_msg.contains("validation")
                || error_msg.contains("schema")
            {
                println!("ℹ️  ネットワーク/Validationエラー: {e}");
            } else {
                println!("❌ 予期しないエラー: {e}");
            }
            return Err(e);
        }
    }

    Ok(())
})
}

/// CREATE_WORKFLOW エラーケースDB統合テスト
/// 無効なデータでのワーカー作成失敗確認
#[test]
fn test_create_workflow_error_cases_db_integration() -> Result<()> {
    println!("❌ CREATE_WORKFLOW エラーケースDB統合テスト開始");
    TEST_RUNTIME.block_on(async {
        // AppModuleを初期化（test用のhybrid setup）
        let app = Arc::new(create_hybrid_test_app().await?);

        // CreateWorkflowRunnerImplを初期化
        let mut runner = CreateWorkflowRunnerImpl::new(app.clone())?;

        // テストケース1: 空のworker名
        let empty_name_args = CreateWorkflowArgs {
            workflow_source: Some(WorkflowSource::WorkflowData(json!({
                "document": {"name": "test", "version": "1.0.0"},
                "do": [{"step": {"run": {"runner": {"name": "COMMAND", "arguments": {"command": "echo test"}}}}}]
            }).to_string())),
            name: "".to_string(),
            worker_options: None,
        };

        let serialized = ProstMessageCodec::serialize_message(&empty_name_args)?;
        let (result, _) = runner.run(&serialized, HashMap::new()).await;
        assert!(result.is_err(), "Empty name should cause validation error");
        println!("✅ 空名前の拒否: 正常");

        // テストケース2: 無効JSON
        let invalid_json_args = CreateWorkflowArgs {
            workflow_source: Some(WorkflowSource::WorkflowData(
                "invalid json content".to_string(),
            )),
            name: "error-test-worker".to_string(),
            worker_options: None,
        };

        let serialized = ProstMessageCodec::serialize_message(&invalid_json_args)?;
        let (result, _) = runner.run(&serialized, HashMap::new()).await;
        assert!(result.is_err(), "Invalid JSON should cause parsing error");
        println!("✅ 無効JSON拒否: 正常");

        // テストケース3: workflow_source未設定
        let no_source_args = CreateWorkflowArgs {
            workflow_source: None,
            name: "no-source-worker".to_string(),
            worker_options: None,
        };

        let serialized = ProstMessageCodec::serialize_message(&no_source_args)?;
        let (result, _) = runner.run(&serialized, HashMap::new()).await;
        assert!(
            result.is_err(),
            "Missing workflow_source should cause validation error"
        );
        println!("✅ workflow_source未設定の拒否: 正常");

        // エラーケース後にDBが正常であることを確認
        // 不正なワーカーが作成されていないことを検証
        let found_empty = app.worker_app.find_by_name("").await?;
        assert!(found_empty.is_none(), "空名前のワーカーは作成されていない");

        let found_error = app.worker_app.find_by_name("error-test-worker").await?;
        assert!(found_error.is_none(), "エラーワーカーは作成されていない");

        let found_no_source = app.worker_app.find_by_name("no-source-worker").await?;
        assert!(
            found_no_source.is_none(),
            "source未設定ワーカーは作成されていない"
        );

        println!("✅ CREATE_WORKFLOW エラーケースDB統合テスト成功:");
        println!("   - Empty name rejection: PASSED");
        println!("   - Invalid JSON rejection: PASSED");
        println!("   - Missing source rejection: PASSED");
        println!("   - Database integrity: PASSED");

        Ok(())
    })
}
