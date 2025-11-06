use anyhow::Result;
use app::app::function::FunctionApp;
use app::module::AppModule;
use infra_utils::infra::test::TEST_RUNTIME;

// Test helper to set up AppModule
async fn setup_test_app_module() -> Result<AppModule> {
    // Use the test helper from app module
    app::module::test::create_hybrid_test_app().await
}

#[test]
fn test_create_workflow_from_data() -> Result<()> {
    // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    TEST_RUNTIME.block_on(async {
        // Setup
        let app = setup_test_app_module().await?;

        // Minimal valid workflow definition (matching existing workflow structure)
        let workflow_data = r#"{
            "document": {
                "dsl": "1.0.0",
                "namespace": "test",
                "name": "test-workflow",
                "summary": "Test workflow",
                "version": "1.0.0"
            },
            "input": {
                "schema": {
                    "document": {
                        "type": "object",
                        "properties": {}
                    }
                }
            },
            "do": [
                {
                    "setMessage": {
                        "set": {
                            "message": "Hello"
                        }
                    }
                }
            ]
        }"#
        .to_string();

        // Execute
        let result = app
            .function_app
            .create_workflow_from_definition(Some(workflow_data), None, None, None)
            .await;

        // Verify
        assert!(result.is_ok(), "Failed to create workflow: {:?}", result);
        let (worker_id, worker_name, workflow_name) = result.unwrap();
        assert_eq!(worker_name, "test-workflow");
        assert_eq!(workflow_name, Some("test-workflow".to_string()));

        // Verify worker exists
        let worker = app
            .worker_app
            .find(&worker_id)
            .await?
            .expect("Worker should exist");
        assert_eq!(worker.data.as_ref().unwrap().name, "test-workflow");

        // Cleanup
        app.worker_app.delete(&worker_id).await?;
        Ok(())
    })
}

#[test]
fn test_create_workflow_with_custom_name() -> Result<()> {
    // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    TEST_RUNTIME.block_on(async {
        let app = setup_test_app_module().await?;

        let workflow_data = r#"{
            "document": {
                "dsl": "1.0.0",
                "namespace": "test",
                "name": "original-name",
                "summary": "Test workflow",
                "version": "1.0.0"
            },
            "input": {
                "schema": {
                    "document": {
                        "type": "object",
                        "properties": {}
                    }
                }
            },
            "do": [
                {
                    "setMessage": {
                        "set": {
                            "message": "Hello"
                        }
                    }
                }
            ]
        }"#
        .to_string();

        // Execute with custom name
        let result = app
            .function_app
            .create_workflow_from_definition(
                Some(workflow_data),
                None,
                Some("custom_workflow_name".to_string()),
                None,
            )
            .await;

        // Verify
        assert!(result.is_ok(), "Failed to create workflow: {:?}", result);
        let (worker_id, worker_name, workflow_name) = result.unwrap();
        assert_eq!(worker_name, "custom_workflow_name");
        assert_eq!(workflow_name, Some("original-name".to_string()));

        // Cleanup
        app.worker_app.delete(&worker_id).await?;
        Ok(())
    })
}

#[test]
fn test_create_workflow_with_duplicate_name() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = setup_test_app_module().await?;

        let workflow_data = r#"{
            "document": {
                "dsl": "1.0.0",
                "namespace": "test",
                "name": "duplicate-workflow",
                "summary": "Test workflow",
                "version": "1.0.0"
            },
            "input": {
                "schema": {
                    "document": {
                        "type": "object",
                        "properties": {}
                    }
                }
            },
            "do": [
                {
                    "setMessage": {
                        "set": {
                            "message": "Hello"
                        }
                    }
                }
            ]
        }"#
        .to_string();

        // Create first workflow
        let (worker_id, _, _) = app
            .function_app
            .create_workflow_from_definition(Some(workflow_data.clone()), None, None, None)
            .await?;

        // Try to create second workflow with same name
        let result = app
            .function_app
            .create_workflow_from_definition(Some(workflow_data), None, None, None)
            .await;

        // Should fail with AlreadyExists error
        assert!(result.is_err(), "Should fail with duplicate name");
        let error = result.unwrap_err();
        let error_msg = error.to_string();
        assert!(
            error_msg.contains("UNIQUE constraint failed") || error_msg.contains("Duplicate"),
            "Unexpected error message: {}",
            error_msg
        );

        // Cleanup
        app.worker_app.delete(&worker_id).await?;
        Ok(())
    })
}

#[test]
fn test_create_workflow_with_invalid_data() -> Result<()> {
    // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
    TEST_RUNTIME.block_on(async {
        let app = setup_test_app_module().await?;

        // Invalid JSON
        let result = app
            .function_app
            .create_workflow_from_definition(Some("{invalid json".to_string()), None, None, None)
            .await;

        assert!(result.is_err(), "Should fail with invalid JSON");
        let error = result.unwrap_err();
        let error_msg = error.to_string();
        // Error should indicate parsing failure (various possible messages from JSON/YAML parsers)
        assert!(
            error_msg.contains("parse")
                || error_msg.contains("JSON")
                || error_msg.contains("YAML")
                || error_msg.contains("expected")
                || error_msg.contains("comma")
                || error_msg.contains("mapping"),
            "Unexpected error message: {}",
            error_msg
        );
        Ok(())
    })
}

#[test]
fn test_create_workflow_with_invalid_workflow_structure() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = setup_test_app_module().await?;

        // Missing required fields
        let workflow_data = r#"{
            "document": {
                "dsl": "1.0.0",
                "namespace": "test",
                "name": "test_workflow"
            }
        }"#
        .to_string();

        let result = app
            .function_app
            .create_workflow_from_definition(Some(workflow_data), None, None, None)
            .await;

        assert!(
            result.is_err(),
            "Should fail with invalid workflow structure"
        );
        let error = result.unwrap_err();
        let error_msg = error.to_string();
        assert!(
            error_msg.contains("do") || error_msg.contains("task"),
            "Unexpected error message: {}",
            error_msg
        );
        Ok(())
    })
}

#[test]
fn test_create_workflow_with_worker_options() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = setup_test_app_module().await?;

        let workflow_data = r#"{
            "document": {
                "dsl": "1.0.0",
                "namespace": "test",
                "name": "workflow_with_options",
                "summary": "Test workflow"
            },
            "do": [
                {
                    "setMessage": {
                        "set": {
                            "message": "Hello"
                        }
                    }
                }
            ]
        }"#
        .to_string();

        // Create worker options
        let worker_options = proto::jobworkerp::function::data::WorkerOptions {
            retry_policy: Some(proto::jobworkerp::data::RetryPolicy {
                r#type: proto::jobworkerp::data::RetryType::Constant as i32,
                interval: 1000,
                max_interval: 5000,
                max_retry: 3,
                basis: 1.0,
            }),
            store_success: true,
            store_failure: true,
            ..Default::default()
        };

        // Execute
        let result = app
            .function_app
            .create_workflow_from_definition(Some(workflow_data), None, None, Some(worker_options))
            .await;

        // Verify
        assert!(result.is_ok(), "Failed to create workflow: {:?}", result);
        let (worker_id, _, _) = result.unwrap();

        // Verify worker options applied
        let worker = app
            .worker_app
            .find(&worker_id)
            .await?
            .expect("Worker should exist");
        let worker_data = worker.data.as_ref().unwrap();
        assert!(worker_data.store_success);
        assert!(worker_data.store_failure);
        assert!(worker_data.retry_policy.is_some());

        // Cleanup
        app.worker_app.delete(&worker_id).await?;
        Ok(())
    })
}

#[test]
fn test_create_workflow_neither_data_nor_url() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = setup_test_app_module().await?;

        // Neither data nor URL provided
        let result = app
            .function_app
            .create_workflow_from_definition(None, None, None, None)
            .await;

        assert!(
            result.is_err(),
            "Should fail when neither data nor URL is provided"
        );
        let error = result.unwrap_err();
        let error_msg = error.to_string();
        assert!(
            error_msg.contains("required") || error_msg.contains("Either"),
            "Unexpected error message: {}",
            error_msg
        );
        Ok(())
    })
}
