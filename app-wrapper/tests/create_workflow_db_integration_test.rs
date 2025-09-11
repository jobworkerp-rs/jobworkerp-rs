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

/// CREATE_WORKFLOW DB integration test
/// Use real DB to verify worker creation and access
#[test]
fn test_create_workflow_runner_db_integration() -> Result<()> {
    println!("🗄️ CREATE_WORKFLOW DB integration test start");
    TEST_RUNTIME.block_on(async {
        // Initialize AppModule (hybrid setup for testing)
        let app = Arc::new(create_hybrid_test_app().await?);

        // Initialize CreateWorkflowRunnerImpl
        let mut runner = CreateWorkflowRunnerImpl::new(app.clone())?;

        // Workflow definition for the test
        let test_workflow = json!({
                "document": {
                    "dsl": "0.0.1",
                    "namespace": "db-integration-test",
                    "name": "create-workflow-db-test",
                    "version": "1.0.0",
                    "summary": "CREATE_WORKFLOW workflow for DB integration test"
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
                                        "args": ["Running DB integration test"]
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
            "✅ Proto serialization completed: {} bytes",
            serialized_args.len()
        );

        // Execute CREATE_WORKFLOW
        let metadata = HashMap::new();
        let (result, _returned_metadata) = runner.run(&serialized_args, metadata).await;

        match result {
            Ok(output_bytes) => {
                println!("✅ CREATE_WORKFLOW executed successfully");

                // Deserialize result
                let create_result: CreateWorkflowResult =
                    ProstMessageCodec::deserialize_message(&output_bytes)?;

                // Validate WorkerID
                assert!(
                    create_result.worker_id.is_some(),
                    "WorkerId should be present"
                );
                let worker_id = create_result.worker_id.unwrap();
                assert!(worker_id.value > 0, "WorkerId should be valid and positive");

                println!("✅ WorkerID generated: {}", worker_id.value);

                // Verify that the worker was created in the DB
                let found_worker = app.worker_app.find_by_name(worker_name).await?;
                assert!(found_worker.is_some(), "Created worker not found in DB");

                let found_worker = found_worker.unwrap();
                let worker_data = found_worker.data.as_ref().expect("WorkerData should exist");
                println!("✅ Worker found in DB:");
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

                // Verify worker details
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

                println!("✅ CREATE_WORKFLOW DB integration test succeeded:");
                println!("   - Workflow validation: PASSED");
                println!("   - Worker creation: PASSED");
                println!("   - Database persistence: PASSED");
                println!("   - Worker retrieval: PASSED");
                println!("   - Options configuration: PASSED");

                Ok(())
            }
            Err(e) => {
                println!("❌ CREATE_WORKFLOW execution failed: {e}");
                Err(e)
            }
        }
    })
}

/// CREATE_WORKFLOW Workflow URL DB integration test
/// Fetch workflow from URL and create worker
#[ignore = "local url"]
#[test]
fn test_create_workflow_url_db_integration() -> Result<()> {
    //command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    println!("🌐 CREATE_WORKFLOW URL DB integration test start");
    TEST_RUNTIME.block_on(async {
        // Initialize AppModule (hybrid setup for testing)
        let app = Arc::new(create_hybrid_test_app().await?);

        // Initialize CreateWorkflowRunnerImpl
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

        // Execute CREATE_WORKFLOW
        let metadata = HashMap::new();
        let (result, _returned_metadata) = runner.run(&serialized_args, metadata).await;

        match result {
            Ok(output_bytes) => {
                println!("✅ CREATE_WORKFLOW URL execution successful");

                // Deserialize result
                let create_result: CreateWorkflowResult =
                    ProstMessageCodec::deserialize_message(&output_bytes)?;

                // Verify WorkerID
                assert!(create_result.worker_id.is_some());
                let worker_id = create_result.worker_id.unwrap();

                println!("✅ URL-based WorkerID generation: {}", worker_id.value);

                // Verify that worker was created in DB
                let found_worker = app.worker_app.find_by_name(worker_name).await?;
                assert!(
                    found_worker.is_some(),
                    "Worker created via URL not found in DB"
                );

                let found_worker = found_worker.unwrap();
                let worker_data = found_worker.data.as_ref().expect("WorkerData should exist");
                println!("✅ URL-based worker DB verification successful:");
                println!("   - Worker Name: {}", worker_data.name);
                println!("   - Source URL: {test_url}");
                println!(
                    "   - Worker ID: {}",
                    found_worker.id.as_ref().unwrap().value
                );
                println!("   - Channel: {:?}", worker_data.channel);

                // Verify options
                assert_eq!(worker_data.name, worker_name);
                assert_eq!(worker_data.channel.as_deref(), Some("url-db-test-channel"));
                assert_eq!(worker_data.response_type, ResponseType::NoResult as i32);
                // Verify with_backup via queue_type
                assert_eq!(
                    worker_data.queue_type,
                    proto::jobworkerp::data::QueueType::WithBackup as i32
                );
                Ok(())
            }
            Err(e) => {
                // Allow network errors and validation errors
                let error_msg = e.to_string();
                if error_msg.contains("timeout")
                    || error_msg.contains("network")
                    || error_msg.contains("validation")
                    || error_msg.contains("schema")
                {
                    println!("ℹ️  Network/Validation error: {e}");
                } else {
                    println!("❌ Unexpected error: {e}");
                }
                Err(e)
            }
        }
    })
}

/// CREATE_WORKFLOW error cases DB integration test
/// Verify failures on invalid input
#[test]
fn test_create_workflow_error_cases_db_integration() -> Result<()> {
    println!("❌ CREATE_WORKFLOW error cases DB integration test start");
    TEST_RUNTIME.block_on(async {
        // Initialize AppModule (hybrid setup for testing)
        let app = Arc::new(create_hybrid_test_app().await?);

        // Initialize CreateWorkflowRunnerImpl
        let mut runner = CreateWorkflowRunnerImpl::new(app.clone())?;

        // Test case 1: empty worker name
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
        println!("✅ Empty name rejection: OK");

        // Test case 2: invalid JSON
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
        println!("✅ Invalid JSON rejection: OK");

        // Test case 3: missing workflow_source
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
        println!("✅ Missing workflow_source rejection: OK");

        // After error cases, verify DB integrity
        // Ensure invalid workers were not created
        let found_empty = app.worker_app.find_by_name("").await?;
        assert!(found_empty.is_none(), "Empty name worker should not be created");

        let found_error = app.worker_app.find_by_name("error-test-worker").await?;
        assert!(found_error.is_none(), "Error worker should not be created");

        let found_no_source = app.worker_app.find_by_name("no-source-worker").await?;
        assert!(
            found_no_source.is_none(),
            "Worker with missing source should not be created"
        );

        println!("✅ CREATE_WORKFLOW error cases DB integration test succeeded:");
        println!("   - Empty name rejection: PASSED");
        println!("   - Invalid JSON rejection: PASSED");
        println!("   - Missing source rejection: PASSED");
        println!("   - Database integrity: PASSED");

        Ok(())
    })
}
