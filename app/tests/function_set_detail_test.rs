use anyhow::Result;
use app::app::function::FunctionApp;
use app::app::function::function_set::FunctionSetApp;
use app::module::AppModule;
use infra::infra::function_set::rdb::{FunctionSetRepository, UseFunctionSetRepository};
use infra_utils::infra::test::TEST_RUNTIME;
use proto::jobworkerp::data::{RunnerId, WorkerData};
use proto::jobworkerp::function::data::{
    FunctionId, FunctionSetData, FunctionSetId, FunctionUsing, function_id,
};

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

#[test]
fn test_update_function_set_invalidates_warmed_name_and_id_caches() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = setup_test_app_module().await?;
        let name = unique_function_set_name("normal_update");
        let function_set_id = app_module
            .function_set_app
            .create_function_set(&function_set_data(&name, "before update"))
            .await?;

        assert!(
            app_module
                .function_set_app
                .find_function_set_by_name(&name)
                .await?
                .is_some()
        );
        assert!(
            app_module
                .function_set_app
                .find_function_set(&function_set_id)
                .await?
                .is_some()
        );

        assert!(
            app_module
                .function_set_app
                .update_function_set(
                    &function_set_id,
                    &Some(function_set_data(&name, "after update")),
                )
                .await?
        );

        for function_set in [
            app_module
                .function_set_app
                .find_function_set_by_name(&name)
                .await?
                .expect("updated FunctionSet should be found by name"),
            app_module
                .function_set_app
                .find_function_set(&function_set_id)
                .await?
                .expect("updated FunctionSet should be found by ID"),
        ] {
            assert_eq!(
                function_set.data.as_ref().unwrap().description,
                "after update"
            );
        }

        app_module
            .function_set_app
            .delete_function_set(&function_set_id)
            .await?;
        Ok(())
    })
}

#[test]
fn test_rename_function_set_invalidates_old_new_name_and_id_caches() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = setup_test_app_module().await?;
        let suffix = unique_function_set_name("rename");
        let old_name = format!("{suffix}_old");
        let new_name = format!("{suffix}_new");
        let renamed_id = app_module
            .function_set_app
            .create_function_set(&function_set_data(&old_name, "rename target"))
            .await?;
        let former_owner_id = app_module
            .function_set_app
            .create_function_set(&function_set_data(&new_name, "former owner"))
            .await?;

        assert!(
            app_module
                .function_set_app
                .find_function_set_by_name(&old_name)
                .await?
                .is_some()
        );
        let former_owner = app_module
            .function_set_app
            .find_function_set_by_name(&new_name)
            .await?
            .expect("destination name should be cached before rename");
        assert_eq!(former_owner.id, Some(former_owner_id));
        assert!(
            app_module
                .function_set_app
                .find_function_set(&renamed_id)
                .await?
                .is_some()
        );

        // Leave the warmed name cache behind to exercise destination-key invalidation.
        assert!(
            app_module
                .function_set_app
                .function_set_repository()
                .delete(&former_owner_id)
                .await?
        );

        assert!(
            app_module
                .function_set_app
                .update_function_set(&renamed_id, &Some(function_set_data(&new_name, "renamed")),)
                .await?
        );

        assert!(
            app_module
                .function_set_app
                .find_function_set_by_name(&old_name)
                .await?
                .is_none()
        );
        let found_by_new_name = app_module
            .function_set_app
            .find_function_set_by_name(&new_name)
            .await?
            .expect("renamed FunctionSet should be found by new name");
        let found_by_id = app_module
            .function_set_app
            .find_function_set(&renamed_id)
            .await?
            .expect("renamed FunctionSet should be found by ID");
        assert_eq!(found_by_new_name.id, Some(renamed_id));
        assert_eq!(found_by_id.data.as_ref().unwrap().name, new_name);
        assert_eq!(
            found_by_new_name.data.as_ref().unwrap().description,
            "renamed"
        );

        app_module
            .function_set_app
            .delete_function_set(&renamed_id)
            .await?;
        Ok(())
    })
}

#[test]
fn test_delete_function_set_invalidates_warmed_name_and_id_caches() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = setup_test_app_module().await?;
        let name = unique_function_set_name("delete");
        let function_set_id = app_module
            .function_set_app
            .create_function_set(&function_set_data(&name, "to delete"))
            .await?;

        assert!(
            app_module
                .function_set_app
                .find_function_set_by_name(&name)
                .await?
                .is_some()
        );
        assert!(
            app_module
                .function_set_app
                .find_function_set(&function_set_id)
                .await?
                .is_some()
        );

        assert!(
            app_module
                .function_set_app
                .delete_function_set(&function_set_id)
                .await?
        );
        assert!(
            app_module
                .function_set_app
                .find_function_set_by_name(&name)
                .await?
                .is_none()
        );
        assert!(
            app_module
                .function_set_app
                .find_function_set(&function_set_id)
                .await?
                .is_none()
        );
        Ok(())
    })
}

#[test]
fn test_missing_function_set_mutations_do_not_report_success() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = setup_test_app_module().await?;
        let missing_id = FunctionSetId { value: -1 };

        assert!(
            !app_module
                .function_set_app
                .update_function_set(
                    &missing_id,
                    &Some(function_set_data("missing_update", "should not exist")),
                )
                .await?
        );
        assert!(
            !app_module
                .function_set_app
                .delete_function_set(&missing_id)
                .await?
        );
        Ok(())
    })
}

// Helper function to setup test AppModule
async fn setup_test_app_module() -> Result<AppModule> {
    app::module::test::create_hybrid_test_app().await
}

fn function_set_data(name: &str, description: &str) -> FunctionSetData {
    FunctionSetData {
        name: name.to_string(),
        description: description.to_string(),
        category: 0,
        targets: Vec::new(),
    }
}

fn unique_function_set_name(label: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after the Unix epoch")
        .as_nanos();
    format!("cache_test_{label}_{timestamp}")
}
