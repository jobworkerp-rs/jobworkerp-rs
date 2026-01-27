use anyhow::Result;
use app::app::function::FunctionApp;
use app::app::function::function_set::FunctionSetApp;
use app::module::AppModule;
use infra_utils::infra::test::TEST_RUNTIME;
use proto::jobworkerp::data::{RunnerId, WorkerData};
use proto::jobworkerp::function::data::{FunctionId, FunctionSetData, FunctionUsing, function_id};

#[test]
fn test_find_detail_with_runners_and_workers() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        // Setup test environment
        let app_module = setup_test_app_module().await?;

        let runner_id = RunnerId { value: 1 };

        let worker_data = WorkerData {
            name: "test_worker".to_string(),
            runner_id: Some(runner_id),
            description: "Test worker for detail test".to_string(),
            runner_settings: Vec::new(),
            ..Default::default()
        };
        let worker_id = app_module.worker_app.create(&worker_data).await?;

        let function_set_data = FunctionSetData {
            name: "test_function_set".to_string(),
            description: "Test function set with mixed targets".to_string(),
            category: 1,
            targets: vec![
                FunctionUsing {
                    function_id: Some(FunctionId {
                        id: Some(function_id::Id::RunnerId(runner_id)),
                    }),
                    using: None,
                },
                FunctionUsing {
                    function_id: Some(FunctionId {
                        id: Some(function_id::Id::WorkerId(worker_id)),
                    }),
                    using: None,
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

        // Test: Convert FunctionUsings to FunctionSpecs
        let targets = &found_set.data.as_ref().unwrap().targets;
        let function_specs = app_module
            .function_app
            .convert_function_usings_to_specs(targets, "test_function_set")
            .await?;

        assert_eq!(function_specs.len(), 2);

        let runner_spec = function_specs
            .iter()
            .find(|spec| spec.runner_id == Some(runner_id))
            .expect("Runner spec should exist");
        assert_eq!(runner_spec.name, "COMMAND"); // Builtin runner name
        assert!(runner_spec.worker_id.is_none());

        let worker_spec = function_specs
            .iter()
            .find(|spec| spec.worker_id == Some(worker_id))
            .expect("Worker spec should exist");
        assert_eq!(worker_spec.name, "test_worker");
        assert_eq!(worker_spec.runner_id, Some(runner_id));
        assert_eq!(worker_spec.worker_id, Some(worker_id));

        // Cleanup (only worker and function_set, not builtin runner)
        app_module
            .function_set_app
            .delete_function_set(&function_set_id)
            .await?;
        app_module.worker_app.delete(&worker_id).await?;
        Ok(())
    })
}

#[test]
fn test_convert_function_ids_with_deleted_target() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = setup_test_app_module().await?;

        let temp_worker_data = WorkerData {
            name: "temporary_worker".to_string(),
            runner_id: Some(RunnerId { value: 1 }), // COMMAND runner
            description: "Temporary worker to be deleted".to_string(),
            runner_settings: Vec::new(),
            ..Default::default()
        };
        let deleted_worker_id = app_module.worker_app.create(&temp_worker_data).await?;

        let valid_worker_data = WorkerData {
            name: "valid_worker".to_string(),
            runner_id: Some(RunnerId { value: 2 }), // HTTP_REQUEST runner
            description: "Valid worker that remains".to_string(),
            runner_settings: Vec::new(),
            ..Default::default()
        };
        let valid_worker_id = app_module.worker_app.create(&valid_worker_data).await?;

        app_module.worker_app.delete(&deleted_worker_id).await?;

        // Try to convert both (one deleted, one valid)
        let function_usings = vec![
            FunctionUsing {
                function_id: Some(FunctionId {
                    id: Some(function_id::Id::WorkerId(deleted_worker_id)), // Deleted
                }),
                using: None,
            },
            FunctionUsing {
                function_id: Some(FunctionId {
                    id: Some(function_id::Id::WorkerId(valid_worker_id)), // Valid
                }),
                using: None,
            },
        ];

        let function_specs = app_module
            .function_app
            .convert_function_usings_to_specs(&function_usings, "test_context")
            .await?;

        // Should only have one spec (the valid one)
        assert_eq!(
            function_specs.len(),
            1,
            "Should skip deleted worker and only return valid worker"
        );
        assert_eq!(function_specs[0].worker_id, Some(valid_worker_id));

        // Cleanup
        app_module.worker_app.delete(&valid_worker_id).await?;
        Ok(())
    })
}

#[test]
fn test_convert_function_ids_with_none_id() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = setup_test_app_module().await?;

        let runner_id = RunnerId { value: 2 };

        let function_usings = vec![
            FunctionUsing {
                function_id: None, // Invalid
                using: None,
            },
            FunctionUsing {
                function_id: Some(FunctionId {
                    id: Some(function_id::Id::RunnerId(runner_id)),
                }), // Valid
                using: None,
            },
        ];

        let function_specs = app_module
            .function_app
            .convert_function_usings_to_specs(&function_usings, "test_none_context")
            .await?;

        // Should only have one spec (skip the None)
        assert_eq!(
            function_specs.len(),
            1,
            "Should skip FunctionUsing with None function_id and only return valid runner"
        );
        assert_eq!(function_specs[0].runner_id, Some(runner_id));
        assert_eq!(function_specs[0].name, "HTTP_REQUEST"); // Builtin runner name
        Ok(())
    })
}

// Helper function to setup test AppModule
async fn setup_test_app_module() -> Result<AppModule> {
    app::module::test::create_hybrid_test_app().await
}
