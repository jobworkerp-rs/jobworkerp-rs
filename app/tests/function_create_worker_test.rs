use anyhow::Result;
use app::app::function::FunctionApp;
use app::module::AppModule;
use infra_utils::infra::test::TEST_RUNTIME;

#[test]
fn test_create_worker_from_command_runner() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        // Setup
        let app = setup_test_app_module().await?;

        // Execute - COMMAND runner doesn't accept settings, so use None
        let result = app
            .function_app
            .create_worker_from_runner(
                Some("COMMAND".to_string()),
                None,
                "test_worker".to_string(),
                Some("Test worker description".to_string()),
                None, // COMMAND runner has no settings schema
                None,
            )
            .await;

        // Verify
        assert!(result.is_ok(), "Failed to create worker: {:?}", result);
        let (worker_id, worker_name) = result.unwrap();
        assert_eq!(worker_name, "test_worker");

        let worker = app
            .worker_app
            .find(&worker_id)
            .await?
            .expect("Worker should exist");
        assert_eq!(worker.data.as_ref().unwrap().name, "test_worker");

        // Cleanup
        app.worker_app.delete(&worker_id).await?;
        Ok(())
    })
}

#[test]
fn test_create_worker_with_invalid_settings() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = setup_test_app_module().await?;

        // Test with invalid JSON (not parseable)
        let result = app
            .function_app
            .create_worker_from_runner(
                Some("HTTP_REQUEST".to_string()),
                None,
                "invalid_worker".to_string(),
                None,
                Some(r#"{"invalid json syntax"#.to_string()),
                None,
            )
            .await;

        // Should fail with validation error
        assert!(result.is_err(), "Should fail with invalid settings");
        let error = result.unwrap_err();
        let error_msg = error.to_string();
        assert!(
            error_msg.contains("Invalid JSON") || error_msg.contains("parse"),
            "Unexpected error message: {}",
            error_msg
        );
        Ok(())
    })
}

#[test]
fn test_create_worker_with_duplicate_name() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = setup_test_app_module().await?;

        let (worker_id, _) = app
            .function_app
            .create_worker_from_runner(
                Some("COMMAND".to_string()),
                None,
                "duplicate_test".to_string(),
                None,
                None,
                None,
            )
            .await?;

        // Try to create second worker with same name
        let result = app
            .function_app
            .create_worker_from_runner(
                Some("COMMAND".to_string()),
                None,
                "duplicate_test".to_string(),
                None,
                None,
                None,
            )
            .await;

        // Should fail with AlreadyExists error
        assert!(result.is_err(), "Should fail with duplicate name");
        let error = result.unwrap_err();
        let error_msg = error.to_string();
        assert!(
            error_msg.contains("UNIQUE constraint failed") || error_msg.contains("Duplicate"),
            "Expected AlreadyExists error, got: {}",
            error_msg
        );

        // Cleanup
        app.worker_app.delete(&worker_id).await?;
        Ok(())
    })
}

// Note: Name validation tests (invalid/valid patterns) have been moved to grpc-front/src/service/function.rs
// as they are request-level validations that should be tested at the gRPC layer.
// Only business logic tests (like duplicate name checking) remain here.

#[test]
fn test_create_worker_with_empty_settings() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = setup_test_app_module().await?;

        // Test with various empty values (should be allowed per spec)
        let empty_settings = [
            None,
            Some("null".to_string()),
            Some("{}".to_string()),
            Some("[]".to_string()),
            Some(r#""""#.to_string()),
        ];

        let mut worker_ids = Vec::new();

        for (idx, settings) in empty_settings.iter().enumerate() {
            let result = app
                .function_app
                .create_worker_from_runner(
                    Some("COMMAND".to_string()),
                    None,
                    format!("empty_settings_test_{}", idx),
                    None,
                    settings.clone(),
                    None,
                )
                .await;

            assert!(
                result.is_ok(),
                "Should allow empty settings {:?}, error: {:?}",
                settings,
                result
            );
            worker_ids.push(result.unwrap().0);
        }

        // Cleanup
        for worker_id in worker_ids {
            app.worker_app.delete(&worker_id).await?;
        }

        Ok(())
    })
}

#[test]
fn test_create_worker_with_http_request_runner() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = setup_test_app_module().await?;

        // HTTP_REQUEST runner settings
        let settings = r#"{
            "url": "https://example.com/api",
            "method": "GET"
        }"#;

        let result = app
            .function_app
            .create_worker_from_runner(
                Some("HTTP_REQUEST".to_string()),
                None,
                "http_test_worker".to_string(),
                Some("HTTP request test worker".to_string()),
                Some(settings.to_string()),
                None,
            )
            .await;

        assert!(
            result.is_ok(),
            "Failed to create HTTP_REQUEST worker: {:?}",
            result
        );
        let (worker_id, worker_name) = result.unwrap();
        assert_eq!(worker_name, "http_test_worker");

        // Cleanup
        app.worker_app.delete(&worker_id).await?;
        Ok(())
    })
}

#[test]
fn test_create_worker_with_nonexistent_runner() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = setup_test_app_module().await?;

        let result = app
            .function_app
            .create_worker_from_runner(
                Some("NONEXISTENT_RUNNER".to_string()),
                None,
                "test_worker".to_string(),
                None,
                None,
                None,
            )
            .await;

        // Should fail with NotFound error
        assert!(result.is_err(), "Should fail with nonexistent runner");
        let error = result.unwrap_err();
        let error_msg = error.to_string();
        assert!(
            error_msg.contains("not found") || error_msg.contains("NotFound"),
            "Expected NotFound error, got: {}",
            error_msg
        );
        Ok(())
    })
}

// --- WORKFLOW runner tests ---

#[test]
fn test_create_workflow_worker_sets_summary_as_description() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = setup_test_app_module().await?;

        let workflow_yaml = r#"{
            "document": {
                "dsl": "0.0.1",
                "namespace": "test-ns",
                "name": "test-workflow",
                "version": "1.0.0",
                "summary": "My workflow summary for description"
            },
            "input": { "from": ".input" },
            "do": [
                {
                    "step1": {
                        "run": {
                            "runner": {
                                "name": "COMMAND",
                                "arguments": { "command": "echo", "args": ["hello"] }
                            }
                        }
                    }
                }
            ]
        }"#;

        let settings_json = serde_json::json!({
            "workflow_data": workflow_yaml
        });

        let result = app
            .function_app
            .create_worker_from_runner(
                Some("WORKFLOW".to_string()),
                None,
                "wf-summary-test".to_string(),
                None, // no explicit description
                Some(settings_json.to_string()),
                None,
            )
            .await;

        assert!(
            result.is_ok(),
            "Failed to create workflow worker: {:?}",
            result
        );
        let (worker_id, worker_name) = result.unwrap();
        assert_eq!(worker_name, "wf-summary-test");

        let worker = app
            .worker_app
            .find(&worker_id)
            .await?
            .expect("Worker should exist");
        let data = worker.data.as_ref().unwrap();
        assert_eq!(
            data.description, "My workflow summary for description",
            "Description should be set from workflow document.summary"
        );

        // Cleanup
        app.worker_app.delete(&worker_id).await?;
        Ok(())
    })
}

#[test]
fn test_create_workflow_worker_explicit_description_takes_precedence() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = setup_test_app_module().await?;

        let workflow_yaml = r#"{
            "document": {
                "dsl": "0.0.1",
                "namespace": "test-ns",
                "name": "test-workflow-desc",
                "version": "1.0.0",
                "summary": "Workflow summary"
            },
            "input": { "from": ".input" },
            "do": [
                {
                    "step1": {
                        "run": {
                            "runner": {
                                "name": "COMMAND",
                                "arguments": { "command": "echo", "args": ["hello"] }
                            }
                        }
                    }
                }
            ]
        }"#;

        let settings_json = serde_json::json!({
            "workflow_data": workflow_yaml
        });

        let result = app
            .function_app
            .create_worker_from_runner(
                Some("WORKFLOW".to_string()),
                None,
                "wf-desc-override-test".to_string(),
                Some("Explicit description".to_string()),
                Some(settings_json.to_string()),
                None,
            )
            .await;

        assert!(
            result.is_ok(),
            "Failed to create workflow worker: {:?}",
            result
        );
        let (worker_id, _) = result.unwrap();

        let worker = app
            .worker_app
            .find(&worker_id)
            .await?
            .expect("Worker should exist");
        let data = worker.data.as_ref().unwrap();
        assert_eq!(
            data.description, "Explicit description",
            "Explicit description should take precedence over summary"
        );

        // Cleanup
        app.worker_app.delete(&worker_id).await?;
        Ok(())
    })
}

#[test]
fn test_create_workflow_worker_validates_invalid_yaml() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = setup_test_app_module().await?;

        let settings_json = serde_json::json!({
            "workflow_data": "invalid yaml content { not valid"
        });

        let result = app
            .function_app
            .create_worker_from_runner(
                Some("WORKFLOW".to_string()),
                None,
                "wf-invalid-test".to_string(),
                None,
                Some(settings_json.to_string()),
                None,
            )
            .await;

        assert!(result.is_err(), "Should fail with invalid workflow YAML");
        Ok(())
    })
}

#[test]
fn test_create_workflow_worker_requires_settings() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = setup_test_app_module().await?;

        let result = app
            .function_app
            .create_worker_from_runner(
                Some("WORKFLOW".to_string()),
                None,
                "wf-no-settings-test".to_string(),
                None,
                None, // no settings_json
                None,
            )
            .await;

        assert!(
            result.is_err(),
            "Should fail when settings_json is missing for WORKFLOW"
        );
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("settings_json is required"),
            "Expected settings_json required error, got: {}",
            error_msg
        );
        Ok(())
    })
}

#[test]
fn test_create_workflow_worker_requires_workflow_source() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = setup_test_app_module().await?;

        let settings_json = serde_json::json!({
            "workflow_context": "{\"key\": \"value\"}"
        });

        let result = app
            .function_app
            .create_worker_from_runner(
                Some("WORKFLOW".to_string()),
                None,
                "wf-no-source-test".to_string(),
                None,
                Some(settings_json.to_string()),
                None,
            )
            .await;

        assert!(
            result.is_err(),
            "Should fail when workflow_source is missing"
        );
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("workflow_url") || error_msg.contains("workflow_data"),
            "Expected workflow source required error, got: {}",
            error_msg
        );
        Ok(())
    })
}

// Helper function to setup test AppModule
async fn setup_test_app_module() -> Result<AppModule> {
    app::module::test::create_hybrid_test_app().await
}
