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

        // Verify worker exists
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

        // Create first worker (COMMAND runner has no settings)
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
            error_msg.contains("already exists") || error_msg.contains("duplicate"),
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

// Helper function to setup test AppModule
async fn setup_test_app_module() -> Result<AppModule> {
    // Use the test helper from app module
    app::module::test::create_hybrid_test_app().await
}
