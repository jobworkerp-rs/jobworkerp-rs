use anyhow::Result;
use app::app::function::function_set::FunctionSetApp;
use app::app::function::FunctionApp;
use app::module::AppModule;
use proto::jobworkerp::data::{RunnerType, WorkerData};
use proto::jobworkerp::function::data::{function_id, FunctionId, FunctionSetData};

#[tokio::test]
#[ignore] // Run with --ignored flag (requires full AppModule setup)
async fn test_find_detail_with_runners_and_workers() -> Result<()> {
    // Setup test environment
    let app_module = setup_test_app_module().await?;

    // Create test runner
    let runner_id = app_module
        .runner_app
        .create_runner(
            "test_runner",
            "Test runner for detail test",
            RunnerType::Command as i32,
            "{}",
        )
        .await?;

    // Create test worker
    let worker_data = WorkerData {
        name: "test_worker".to_string(),
        runner_id: Some(runner_id),
        description: "Test worker for detail test".to_string(),
        runner_settings: Vec::new(),
        ..Default::default()
    };
    let worker_id = app_module.worker_app.create(&worker_data).await?;

    // Create FunctionSet with both runner and worker
    let function_set_data = FunctionSetData {
        name: "test_function_set".to_string(),
        description: "Test function set with mixed targets".to_string(),
        category: 1,
        targets: vec![
            FunctionId {
                id: Some(function_id::Id::RunnerId(runner_id)),
            },
            FunctionId {
                id: Some(function_id::Id::WorkerId(worker_id)),
            },
        ],
    };

    let function_set_id = app_module
        .function_set_app
        .create_function_set(&function_set_data)
        .await?;

    // Test: Find FunctionSet (basic)
    let found_set = app_module
        .function_set_app
        .find_function_set(&function_set_id)
        .await?
        .expect("FunctionSet should be found");

    assert_eq!(found_set.id, Some(function_set_id));
    assert_eq!(found_set.data.as_ref().unwrap().targets.len(), 2);

    // Test: Convert FunctionIds to FunctionSpecs
    let targets = &found_set.data.as_ref().unwrap().targets;
    let function_specs = app_module
        .function_app
        .convert_function_ids_to_specs(targets, "test_function_set")
        .await?;

    assert_eq!(function_specs.len(), 2);

    // Verify first target (Runner)
    let runner_spec = function_specs
        .iter()
        .find(|spec| spec.runner_id == Some(runner_id))
        .expect("Runner spec should exist");
    assert_eq!(runner_spec.name, "test_runner");
    assert_eq!(runner_spec.runner_type, RunnerType::Command as i32);
    assert!(runner_spec.worker_id.is_none());

    // Verify second target (Worker)
    let worker_spec = function_specs
        .iter()
        .find(|spec| spec.worker_id == Some(worker_id))
        .expect("Worker spec should exist");
    assert_eq!(worker_spec.name, "test_worker");
    assert_eq!(worker_spec.runner_id, Some(runner_id));
    assert_eq!(worker_spec.worker_id, Some(worker_id));

    // Cleanup
    app_module
        .function_set_app
        .delete_function_set(&function_set_id)
        .await?;
    app_module.worker_app.delete(&worker_id).await?;
    app_module.runner_app.delete_runner(&runner_id).await?;

    Ok(())
}

#[tokio::test]
#[ignore]
async fn test_convert_function_ids_with_deleted_target() -> Result<()> {
    let app_module = setup_test_app_module().await?;

    // Create and then delete a runner
    let runner_id = app_module
        .runner_app
        .create_runner(
            "temporary_runner",
            "Temporary runner to be deleted",
            RunnerType::Command as i32,
            "{}",
        )
        .await?;

    // Create another runner that will remain
    let valid_runner_id = app_module
        .runner_app
        .create_runner(
            "valid_runner",
            "Valid runner that remains",
            RunnerType::Command as i32,
            "{}",
        )
        .await?;

    // Delete the first runner
    app_module.runner_app.delete_runner(&runner_id).await?;

    // Try to convert both (one deleted, one valid)
    let function_ids = vec![
        FunctionId {
            id: Some(function_id::Id::RunnerId(runner_id)), // Deleted
        },
        FunctionId {
            id: Some(function_id::Id::RunnerId(valid_runner_id)), // Valid
        },
    ];

    let function_specs = app_module
        .function_app
        .convert_function_ids_to_specs(&function_ids, "test_context")
        .await?;

    // Should only have one spec (the valid one)
    assert_eq!(
        function_specs.len(),
        1,
        "Should skip deleted runner and only return valid runner"
    );
    assert_eq!(function_specs[0].runner_id, Some(valid_runner_id));

    // Cleanup
    app_module
        .runner_app
        .delete_runner(&valid_runner_id)
        .await?;

    Ok(())
}

#[tokio::test]
#[ignore]
async fn test_convert_function_ids_with_none_id() -> Result<()> {
    let app_module = setup_test_app_module().await?;

    // Create a valid runner
    let runner_id = app_module
        .runner_app
        .create_runner(
            "valid_runner_none_test",
            "Valid runner",
            RunnerType::Command as i32,
            "{}",
        )
        .await?;

    // Create FunctionIds with one having None id
    let function_ids = vec![
        FunctionId { id: None }, // Invalid
        FunctionId {
            id: Some(function_id::Id::RunnerId(runner_id)), // Valid
        },
    ];

    let function_specs = app_module
        .function_app
        .convert_function_ids_to_specs(&function_ids, "test_none_context")
        .await?;

    // Should only have one spec (skip the None)
    assert_eq!(
        function_specs.len(),
        1,
        "Should skip FunctionId with None and only return valid runner"
    );
    assert_eq!(function_specs[0].runner_id, Some(runner_id));

    // Cleanup
    app_module.runner_app.delete_runner(&runner_id).await?;

    Ok(())
}

// Helper function to setup test AppModule
async fn setup_test_app_module() -> Result<AppModule> {
    // Use the test helper from app module
    app::module::test::create_hybrid_test_app().await
}
