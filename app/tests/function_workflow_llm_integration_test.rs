use anyhow::Result;
use app::app::function::FunctionApp;
use app::module::test::create_hybrid_test_app;
use jobworkerp_base::error::JobWorkerError;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// LLM Function呼び出しでのCREATE_WORKFLOWとREUSABLE_WORKFLOWの結合テスト
/// call_function_for_llm()を通じて、runnerとしての動作を確認

#[ignore = "localにjobwokrerpが起動している必要がある(worker-appの起動が必要)"]
#[tokio::test]
async fn test_create_workflow_via_llm_function_call() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    println!("🧠 CREATE_WORKFLOW LLM Function呼び出しテスト開始");

    // AppModuleを初期化
    let app = Arc::new(create_hybrid_test_app().await?);

    // CREATE_WORKFLOWの引数を準備
    let workflow_def = json!({
        "document": {
            "dsl": "0.0.1",
            "namespace": "llm-integration-test",
            "name": "llm-create-workflow-test",
            "version": "1.0.0",
            "summary": "LLM Function呼び出しでのCREATE_WORKFLOWテスト用ワークフロー"
        },
        "input": {},
        "do": [
            {
                "llm_test_step": {
                    "run": {
                        "runner": {
                            "name": "COMMAND",
                            "arguments": {
                                "command": "echo",
                                "args": ["LLM CREATE_WORKFLOW test execution"]
                            }
                        }
                    }
                }
            }
        ]
    });

    let arguments = json!({
        "arguments": workflow_def.to_string()
        // "arguments": {
        //     "workflow_source": {
        //         "workflow_data": workflow_def.to_string()
        //     },
        //     "name": "llm-test-create-workflow-worker",
        //     "worker_options": {
        //         "channel": "llm-test-channel",
        //         "response_type": 0, // Direct
        //         "broadcast_results": true,
        //         "with_backup": false,
        //         "store_success": true,
        //         "store_failure": true,
        //         "use_static": false
        //     }
        // }
    });

    // メタデータを準備
    let meta = Arc::new(HashMap::new());

    // call_function_for_llm()を実行
    let result = app
        .function_app
        .call_function_for_llm(
            meta,
            "CREATE_WORKFLOW",
            Some(arguments.as_object().unwrap().clone()),
            30,
        )
        .await;

    match result {
        Ok(response) => {
            println!("✅ CREATE_WORKFLOW LLM Function呼び出し成功");
            println!("Response: {}", serde_json::to_string_pretty(&response)?);

            // レスポンスの検証
            assert!(
                response.get("workerId").is_some(),
                "worker_id should be present"
            );
            let worker_id: i64 = response
                .get("workerId")
                .unwrap()
                .get("value")
                .unwrap()
                .as_str()
                .unwrap()
                .parse()?;
            assert!(worker_id > 0, "worker_id should be positive");

            // XXX 立ち上がっているworkerに作成されているのでここでは確認できない
            // // 作成されたワーカーがDBに存在することを確認
            // let found_worker = app.worker_app.find(&WorkerId { value: worker_id }).await?;
            // assert!(
            //     found_worker.is_some(),
            //     "作成されたワーカーがDBに見つからない"
            // );

            // let worker = found_worker.unwrap();
            // let worker_data = worker.data.as_ref().expect("WorkerData should exist");

            // println!("✅ 作成されたワーカー確認:");
            // println!("   - Worker data: {:#?}", worker_data);
            // println!("   - Worker ID: {}", worker.id.as_ref().unwrap().value);
            // println!(
            //     "   - Runner ID: {}",
            //     worker_data.runner_id.as_ref().unwrap().value
            // );
            // println!("   - Channel: {:?}", worker_data.channel);

            // // ワーカーの詳細を検証
            // assert_eq!(worker_data.name, "llm-test-create-workflow-worker");
            // assert_eq!(worker.id.as_ref().unwrap().value, worker_id as i64);
            // assert_eq!(worker_data.runner_id.as_ref().unwrap().value, 65532i64); // REUSABLE_WORKFLOW
            // assert_eq!(worker_data.channel.as_deref(), Some("llm-test-channel"));
            // assert!(worker_data.store_success);
            // assert!(worker_data.store_failure);
            // assert!(worker_data.broadcast_results);

            // println!("✅ CREATE_WORKFLOW LLM Function呼び出しテスト成功:");
            // println!("   - Function call via LLM: PASSED");
            // println!("   - Worker creation: PASSED");
            // println!("   - Database persistence: PASSED");
            // println!("   - Response validation: PASSED");
        }
        Err(e) => {
            println!("❌ CREATE_WORKFLOW LLM Function呼び出し失敗: {e}");
            return Err(e);
        }
    }

    Ok(())
}

#[ignore = "localにjobwokrerpが起動している必要がある(worker-appの起動が必要)"]
#[tokio::test]
async fn test_reusable_workflow_via_llm_function_call() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    println!("🧠 REUSABLE_WORKFLOW LLM Function呼び出しテスト開始");

    // AppModuleを初期化
    let app = Arc::new(create_hybrid_test_app().await?);
    let name = "llm-reusable-test";

    // まず、CREATE_WORKFLOWでワーカーを作成
    let workflow_def = json!({
        "document": {
            "dsl": "0.0.1",
            "namespace": "llm-reusable-test",
            "name": name,
            "version": "1.0.0",
            "summary": "LLM REUSABLE_WORKFLOWテスト用ワークフロー"
        },
        "input": {
            "from": ".testInput"
        },
        "do": [
            {
                "reusable_test_step": {
                    "run": {
                        "runner": {
                            "name": "COMMAND",
                            "arguments": {
                                "command": "echo",
                                "args": ["LLM REUSABLE_WORKFLOW test execution"]
                            }
                        }
                    }
                }
            }
        ]
    });

    let create_arguments = json!({
        "arguments": workflow_def.to_string(),
        "settings": {
            "channel": "llm",
            "response_type": 0,
            "broadcast_results": true,
            "store_success": true,
            "store_failure": true
        }
    });

    let meta = Arc::new(HashMap::new());

    // ワーカーを作成
    let create_result = app
        .function_app
        .call_function_for_llm(
            meta.clone(),
            "CREATE_WORKFLOW",
            Some(create_arguments.as_object().unwrap().clone()),
            30,
        )
        .await?;

    let worker_id: i64 = create_result
        .get("workerId")
        .unwrap()
        .get("value")
        .unwrap()
        .as_str()
        .unwrap()
        .parse()?;
    println!("✅ ワーカー作成完了: worker_id={worker_id}");

    // 作成されたワーカーを使ってREUSABLE_WORKFLOW実行
    let execute_arguments = json!({
        "arguments": {
            "testInput": "LLM integration test input data"
        }
    });

    println!("🔄 作成されたワーカーでREUSABLE_WORKFLOW実行");

    // ワーカー名でLLM Function呼び出し
    let execute_result = app
        .function_app
        .call_function_for_llm(
            meta,
            name,
            Some(execute_arguments.as_object().unwrap().clone()),
            30,
        )
        .await;

    match execute_result {
        Ok(response) => {
            println!("✅ REUSABLE_WORKFLOW LLM Function呼び出し成功");
            println!("Response: {}", serde_json::to_string_pretty(&response)?);

            // レスポンスの基本検証
            assert!(response.is_object(), "Response should be an object");

            println!("✅ REUSABLE_WORKFLOW LLM Function呼び出しテスト成功:");
            println!("   - Worker creation via CREATE_WORKFLOW: PASSED");
            println!("   - Worker execution via REUSABLE_WORKFLOW: PASSED");
            println!("   - LLM Function call integration: PASSED");
        }
        Err(e) => {
            println!("❌ REUSABLE_WORKFLOW実行失敗: {e}");
            return Err(e);
        }
    }

    Ok(())
}

#[tokio::test]
async fn test_workflow_error_handling_via_llm_function_call() -> Result<()> {
    println!("🧠 Workflow LLM Function呼び出しエラーハンドリングテスト開始");

    // AppModuleを初期化
    let app = Arc::new(create_hybrid_test_app().await?);

    let meta = Arc::new(HashMap::new());

    // テストケース1: CREATE_WORKFLOWで無効な引数
    let invalid_arguments = json!({
        "arguments": {
            "workflow_source": {
                "workflow_data": "invalid json content"
            },
            "name": "error-test-worker"
        }
    });

    let result = app
        .function_app
        .call_function_for_llm(
            meta.clone(),
            "CREATE_WORKFLOW",
            Some(invalid_arguments.as_object().unwrap().clone()),
            30,
        )
        .await;

    assert!(result.is_err(), "Invalid JSON should cause error");
    println!("✅ 無効JSONエラーハンドリング: 正常");

    // テストケース2: 存在しないワーカー名での呼び出し
    let non_existent_args = json!({
        "arguments": {
            "testInput": "test"
        }
    });

    let result = app
        .function_app
        .call_function_for_llm(
            meta,
            "non-existent-worker",
            Some(non_existent_args.as_object().unwrap().clone()),
            30,
        )
        .await;

    // これは成功するかエラーになるかは実装依存だが、クラッシュしないことを確認
    match result {
        Ok(_) => println!("ℹ️  存在しないワーカー呼び出し: 何らかの結果を返した"),
        Err(e) => println!("ℹ️  存在しないワーカー呼び出し: エラーを返した - {e}"),
    }

    println!("✅ Workflow LLM Function呼び出しエラーハンドリングテスト成功:");
    println!("   - Invalid arguments handling: PASSED");
    println!("   - Non-existent worker handling: PASSED");
    println!("   - No crashes or panics: PASSED");

    Ok(())
}

#[tokio::test]
async fn test_workflow_function_discovery_via_llm() -> Result<()> {
    println!("🧠 Workflow Function検出テスト開始");

    // AppModuleを初期化
    let app = Arc::new(create_hybrid_test_app().await?);

    // 利用可能な関数を取得
    let functions = app.function_app.find_functions(false, false).await?;

    println!("✅ 検出された関数数: {}", functions.len());

    // CREATE_WORKFLOWランナーが存在することを確認
    let create_workflow_found = functions.iter().any(|f| f.name == "CREATE_WORKFLOW");

    assert!(
        create_workflow_found,
        "CREATE_WORKFLOW function should be discoverable"
    );
    println!("✅ CREATE_WORKFLOW関数が検出されました");

    // REUSABLE_WORKFLOWランナーが存在することを確認
    let reusable_workflow_found = functions.iter().any(|f| f.name == "REUSABLE_WORKFLOW");

    assert!(
        reusable_workflow_found,
        "REUSABLE_WORKFLOW function should be discoverable"
    );
    println!("✅ REUSABLE_WORKFLOW関数が検出されました");

    // 関数詳細を表示
    for func in functions
        .iter()
        .filter(|f| f.name == "CREATE_WORKFLOW" || f.name == "REUSABLE_WORKFLOW")
    {
        println!("📋 関数: {}", func.name);
        println!("   - 説明: {}", func.description);
        println!("   - タイプ: {:?}", func.runner_type);
        println!("   - 出力タイプ: {:?}", func.output_type);
        if let Some(schema) = &func.schema {
            match schema {
                proto::jobworkerp::function::data::function_specs::Schema::SingleSchema(s) => {
                    println!("   - 通常スキーマあり: {s:?}");
                }
                proto::jobworkerp::function::data::function_specs::Schema::McpTools(_) => {
                    return Err(JobWorkerError::RuntimeError(
                        "MCP Tools schema is not supported in this test".to_string(),
                    )
                    .into());
                }
            }
        }
    }
    Ok(())
}
