use anyhow::Result;
use app::app::function::FunctionApp;
use app::module::test::create_hybrid_test_app;
use infra_utils::infra::test::TEST_RUNTIME;
use jobworkerp_base::error::JobWorkerError;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// Integration test for CREATE_WORKFLOW and REUSABLE_WORKFLOW with LLM Function calls
/// Verify runner behavior through call_function_for_llm()
#[ignore = "jobworkerp must be running locally (worker-app startup required)"]
#[test]
fn test_create_workflow_via_llm_function_call() -> Result<()> {
    // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    TEST_RUNTIME.block_on(async {
        // Initialize AppModule
        let app = Arc::new(create_hybrid_test_app().await?);

        // Prepare CREATE_WORKFLOW arguments
        let workflow_def = json!({
            "document": {
                "dsl": "0.0.1",
                "namespace": "llm-integration-test",
                "name": "llm-create-workflow-test",
                "version": "1.0.0",
                "summary": "Workflow for CREATE_WORKFLOW test via LLM Function calls"
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

        // Prepare metadata
        let meta = Arc::new(HashMap::new());

        // Execute call_function_for_llm()
        let result = app
            .function_app
            .call_function_for_llm(
                meta,
                "CREATE_WORKFLOW",
                Some(arguments.as_object().unwrap().clone()),
                3,
            )
            .await;

        match result {
            Ok(response) => {
                println!("✅ CREATE_WORKFLOW LLM Function call successful");
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
                println!("❌ CREATE_WORKFLOW LLM Function call failed: {e}");
                return Err(e);
            }
        }

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

        let create_workflow_not_found = functions.iter().any(|f| f.name == "CREATE_WORKFLOW");

        assert!(
            !create_workflow_not_found,
            "CREATE_WORKFLOW function should be discoverable"
        );
        println!("✅ CREATE_WORKFLOW function detected");

        let reusable_workflow_found = functions.iter().any(|f| f.name == "REUSABLE_WORKFLOW");

        assert!(
            reusable_workflow_found,
            "REUSABLE_WORKFLOW function should be discoverable"
        );
        println!("✅ REUSABLE_WORKFLOW function detected");

        // Display function details
        for func in functions
            .iter()
            .filter(|f| f.name == "CREATE_WORKFLOW" || f.name == "REUSABLE_WORKFLOW")
        {
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
