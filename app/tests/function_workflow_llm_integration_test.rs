use anyhow::Result;
use app::app::function::FunctionApp;
use app::module::test::create_hybrid_test_app;
use infra_utils::infra::test::TEST_RUNTIME;
use jobworkerp_base::error::JobWorkerError;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// Integration test for WORKFLOW runner with LLM Function calls
/// Verify runner behavior through call_function_for_llm()
#[ignore = "jobworkerp must be running locally (worker-app startup required)"]
#[test]
fn test_workflow_create_via_llm_function_call() -> Result<()> {
    // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    TEST_RUNTIME.block_on(async {
        // Initialize AppModule
        let app = Arc::new(create_hybrid_test_app().await?);

        // Prepare CREATE_WORKFLOW arguments
        let workflow_def = json!({
            "document": {
                "dsl": "0.0.1",
                "namespace": "llm-integration-test",
                "name": "llm-workflow-create-test",
                "version": "1.0.0",
                "summary": "Workflow for WORKFLOW.create test via LLM Function calls"
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
                                    "args": ["LLM WORKFLOW.create test execution"]
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

        // Prepare metadata
        let meta = Arc::new(HashMap::new());

        // Execute call_function_for_llm()
        let result = app
            .function_app
            .call_function_for_llm(
                meta,
                "WORKFLOW___create",
                Some(arguments.as_object().unwrap().clone()),
                3,
            )
            .await;

        match result {
            Ok(response) => {
                println!("✅ WORKFLOW.create LLM Function call successful");
                println!("Response: {}", serde_json::to_string_pretty(&response)?);

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

                // XXX Cannot verify here because it's created in the running worker
                // // Verify that the created worker exists in the DB
                // let found_worker = app.worker_app.find(&WorkerId { value: worker_id }).await?;
                // assert!(
                //     found_worker.is_some(),
                //     "Created worker not found in DB"
                // );

                // let worker = found_worker.unwrap();
                // let worker_data = worker.data.as_ref().expect("WorkerData should exist");

                // println!("✅ Verified created worker:");
                // println!("   - Worker data: {:#?}", worker_data);
                // println!("   - Worker ID: {}", worker.id.as_ref().unwrap().value);
                // println!(
                //     "   - Runner ID: {}",
                //     worker_data.runner_id.as_ref().unwrap().value
                // );
                // println!("   - Channel: {:?}", worker_data.channel);

                // // Validate worker details
                // assert_eq!(worker_data.name, "llm-test-create-workflow-worker");
                // assert_eq!(worker.id.as_ref().unwrap().value, worker_id as i64);
                // assert_eq!(worker_data.runner_id.as_ref().unwrap().value, 65532i64); // REUSABLE_WORKFLOW
                // assert_eq!(worker_data.channel.as_deref(), Some("llm-test-channel"));
                // assert!(worker_data.store_success);
                // assert!(worker_data.store_failure);
                // assert!(worker_data.broadcast_results);

                // println!("✅ CREATE_WORKFLOW LLM Function call test successful:");
                // println!("   - Function call via LLM: PASSED");
                // println!("   - Worker creation: PASSED");
                // println!("   - Database persistence: PASSED");
                // println!("   - Response validation: PASSED");
            }
            Err(e) => {
                println!("❌ WORKFLOW.create LLM Function call failed: {e}");
                return Err(e);
            }
        }

        Ok(())
    })
}

#[ignore = "jobworkerp must be running locally (worker-app startup required)"]
#[test]
fn test_workflow_create_via_llm_function_call_streaming_fallback() -> Result<()> {
    use futures::StreamExt;

    // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    TEST_RUNTIME.block_on(async {
        // Initialize AppModule
        let app = Arc::new(create_hybrid_test_app().await?);

        // Prepare CREATE_WORKFLOW arguments
        let workflow_def = json!({
            "document": {
                "dsl": "0.0.1",
                "namespace": "llm-integration-test",
                "name": "llm-workflow-create-test-streaming",
                "version": "1.0.0",
                "summary": "Workflow for WORKFLOW.create test via LLM streaming fallback"
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
                                    "args": ["LLM WORKFLOW.create streaming fallback test execution"]
                                }
                            }
                        }
                    }
                }
            ]
        });

        let arguments = json!({
            "arguments": workflow_def.to_string()
        });

        // Prepare metadata
        let meta = Arc::new(HashMap::new());

        // Execute call_function_for_llm_streaming()
        let mut stream = app
            .function_app
            .call_function_for_llm_streaming(
                meta,
                "WORKFLOW___create",
                Some(arguments.as_object().unwrap().clone()),
                3,
            );

        let mut collected_responses = vec![];
        while let Some(result) = stream.next().await {
            match result {
                Ok(response) => {
                    collected_responses.push(response);
                }
                Err(e) => {
                    println!("❌ WORKFLOW.create LLM streaming fallback call failed: {e}");
                    return Err(e);
                }
            }
        }

        assert!(!collected_responses.is_empty(), "Stream should yield at least one result");

        // Grab the last or only response to verify
        let response = collected_responses.last().unwrap();
        println!("✅ WORKFLOW.create LLM streaming fallback call successful");
        println!("Response Output (Raw): {}", &response.output);

        let output_json: serde_json::Value =
            serde_json::from_str(&response.output).expect("Invalid JSON in FunctionResult output");

        assert!(
            output_json.get("workerId").is_some(),
            "worker_id should be present"
        );
        let worker_id: i64 = output_json
            .get("workerId")
            .unwrap()
            .get("value")
            .unwrap()
            .as_str()
            .unwrap()
            .parse()?;
        assert!(worker_id > 0, "worker_id should be positive");

        Ok(())
    })
}
#[ignore = "jobworkerp must be running locally (worker-app startup required)"]
#[test]
fn test_reusable_workflow_via_llm_function_call() -> Result<()> {
    // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    println!("🧠 REUSABLE_WORKFLOW LLM Function call test started");

    TEST_RUNTIME.block_on(async {
        // Initialize AppModule
        let app = Arc::new(create_hybrid_test_app().await?);
        let name = "llm-reusable-test";

        // First, create a worker with CREATE_WORKFLOW
        let workflow_def = json!({
            "document": {
                "dsl": "0.0.1",
                "namespace": "llm-reusable-test",
                "name": name,
                "version": "1.0.0",
                "summary": "Workflow for LLM REUSABLE_WORKFLOW test"
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

        let create_result = app
            .function_app
            .call_function_for_llm(
                meta.clone(),
                "CREATE_WORKFLOW",
                Some(create_arguments.as_object().unwrap().clone()),
                3,
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
        println!("✅ Worker creation completed: worker_id={worker_id}");

        // Cannot verify here because it was created in a separately launched worker (could work with MySQL...)
        //
        // // Execute REUSABLE_WORKFLOW using the created worker
        // let execute_arguments = json!({
        //     "arguments": {
        //         "testInput": "LLM integration test input data"
        //     }
        // });

        // println!("🔄 Executing REUSABLE_WORKFLOW with created worker");

        // // Call LLM Function with worker name
        // let execute_result = app
        //     .function_app
        //     .call_function_for_llm(
        //         meta,
        //         name,
        //         Some(execute_arguments.as_object().unwrap().clone()),
        //         30,
        //     )
        //     .await;

        // match execute_result {
        //     Ok(response) => {
        //         println!("✅ REUSABLE_WORKFLOW LLM Function call successful");
        //         println!("Response: {}", serde_json::to_string_pretty(&response)?);

        //         // Basic response validation
        //         assert!(response.is_object(), "Response should be an object");

        //         println!("✅ REUSABLE_WORKFLOW LLM Function call test successful:");
        //         println!("   - Worker creation via CREATE_WORKFLOW: PASSED");
        //         println!("   - Worker execution via REUSABLE_WORKFLOW: PASSED");
        //         println!("   - LLM Function call integration: PASSED");
        //     }
        //     Err(e) => {
        //         println!("❌ REUSABLE_WORKFLOW execution failed: {e}");
        //         return Err(e);
        //     }
        // }

        Ok(())
    })
}

#[test]
fn test_workflow_error_handling_via_llm_function_call() -> Result<()> {
    println!("🧠 Workflow LLM Function call error handling test started");
    TEST_RUNTIME.block_on(async {
        // Initialize AppModule
        let app = Arc::new(create_hybrid_test_app().await?);

        let meta = Arc::new(HashMap::new());

        // Test case 1: Invalid arguments for CREATE_WORKFLOW
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
                3,
            )
            .await;

        assert!(result.is_err(), "Invalid JSON should cause error");
        println!("✅ Invalid JSON error handling: OK");

        // Test case 2: Call with non-existent worker name
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
                3,
            )
            .await;

        // This may succeed or fail depending on implementation, but verify it doesn't crash
        match result {
            Ok(_) => {
                println!("ℹ️  Non-existent worker call: returned some result");
                return Err(JobWorkerError::RuntimeError(
                    "Non-existent worker call should not return a result".to_string(),
                )
                .into());
            }
            Err(e) => println!("ℹ️  Non-existent worker call: returned error - {e}"),
        }

        Ok(())
    })
}

#[test]
fn test_workflow_function_discovery_via_find_functions() -> Result<()> {
    println!("🧠 Workflow Function discovery test started");
    TEST_RUNTIME.block_on(async {
        // Initialize AppModule
        let app = Arc::new(create_hybrid_test_app().await?);

        let functions = app.function_app.find_functions(false, false).await?;

        println!("✅ Number of functions detected: {:?}", functions.len());

        let workflow_found = functions.iter().any(|f| f.name == "WORKFLOW");

        assert!(workflow_found, "WORKFLOW function should be discoverable");
        println!("✅ WORKFLOW function detected");

        // Verify deprecated runners are no longer discoverable
        for deprecated in &[
            "INLINE_WORKFLOW",
            "REUSABLE_WORKFLOW",
            "CREATE_WORKFLOW",
            "LLM_CHAT",
            "LLM_COMPLETION",
        ] {
            assert!(
                !functions.iter().any(|f| f.name == *deprecated),
                "Deprecated runner '{}' should not be discoverable",
                deprecated
            );
        }
        println!("✅ All deprecated runners confirmed absent");

        // Display function details
        for func in functions.iter().filter(|f| f.name == "WORKFLOW") {
            println!("📋 Function: {}", func.name);
            println!("   - Description: {}", func.description);
            println!("   - Type: {:?}", func.runner_type);
            println!("   - Settings schema: {}", func.settings_schema);
            if let Some(methods) = &func.methods {
                println!(
                    "   - Available methods: {} method(s)",
                    methods.schemas.len()
                );
                for (method_name, method_schema) in &methods.schemas {
                    println!(
                        "     - Method '{}': output_type={:?}",
                        method_name, method_schema.output_type
                    );
                }
            }
        }
        Ok(())
    })
}

// === Tests for enqueue_function_for_llm / await_function_result (2-stage split) ===

#[test]
fn test_enqueue_function_for_llm_streaming_runner() -> Result<()> {
    // COMMAND runner supports streaming (output_type=Both)
    // enqueue_function_for_llm should return is_streaming=true and result=None
    TEST_RUNTIME.block_on(async {
        let app = Arc::new(create_hybrid_test_app().await?);
        let meta = Arc::new(HashMap::new());

        let enqueued = app
            .function_app
            .enqueue_function_for_llm(
                meta,
                "COMMAND",
                Some(
                    json!({"command": "echo", "args": ["hello"]})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                3,
            )
            .await?;

        assert!(
            enqueued.is_streaming,
            "COMMAND runner should be detected as streaming-capable"
        );
        assert!(
            enqueued.result.is_none(),
            "Streaming runner should return None result (deferred to await_function_result)"
        );
        assert!(enqueued.job_id.value > 0, "Job ID should be assigned");
        assert_eq!(
            enqueued.runner_name, "COMMAND",
            "Runner name should be COMMAND"
        );
        assert!(
            enqueued.result_handle.is_some(),
            "Streaming runner should have result_handle"
        );

        Ok(())
    })
}

#[test]
fn test_enqueue_function_for_llm_nonexistent() -> Result<()> {
    // Non-existent runner/worker should return error
    TEST_RUNTIME.block_on(async {
        let app = Arc::new(create_hybrid_test_app().await?);
        let meta = Arc::new(HashMap::new());

        let result = app
            .function_app
            .enqueue_function_for_llm(
                meta,
                "NONEXISTENT_RUNNER_NAME",
                Some(json!({"key": "value"}).as_object().unwrap().clone()),
                3,
            )
            .await;

        assert!(result.is_err(), "Non-existent runner should return error");

        Ok(())
    })
}
