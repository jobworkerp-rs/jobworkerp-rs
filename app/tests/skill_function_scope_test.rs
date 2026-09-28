use anyhow::Result;
use app::app::function::{
    FunctionApp,
    function_set::{FunctionScopeTarget, FunctionSetApp},
};
use app::module::AppModule;
use infra::infra::function_set::rdb::UseFunctionSetRepository;
use infra_utils::infra::rdb::{Rdb, UseRdbPool};
use infra_utils::infra::test::TEST_RUNTIME;
use proto::jobworkerp::data::{RunnerId, WorkerData};
use proto::jobworkerp::function::data::{FunctionId, FunctionSetData, FunctionUsing, function_id};
use std::{collections::HashMap, sync::Arc};

fn target_worker(worker_id: i64) -> FunctionUsing {
    FunctionUsing {
        function_id: Some(FunctionId {
            id: Some(function_id::Id::WorkerId(
                proto::jobworkerp::data::WorkerId { value: worker_id },
            )),
        }),
        using: None,
    }
}

fn target_runner(runner_id: i64) -> FunctionUsing {
    FunctionUsing {
        function_id: Some(FunctionId {
            id: Some(function_id::Id::RunnerId(RunnerId { value: runner_id })),
        }),
        using: None,
    }
}

fn target_runner_method(runner_id: i64, method: &str) -> FunctionUsing {
    FunctionUsing {
        function_id: Some(FunctionId {
            id: Some(function_id::Id::RunnerId(RunnerId { value: runner_id })),
        }),
        using: Some(method.to_string()),
    }
}

async fn create_worker(app: &AppModule, name: &str) -> Result<i64> {
    let id = app
        .worker_app
        .create(&WorkerData {
            name: name.to_string(),
            runner_id: Some(RunnerId { value: 1 }),
            description: "scope test worker".to_string(),
            ..Default::default()
        })
        .await?;
    Ok(id.value)
}

async fn create_set(app: &AppModule, name: &str, targets: Vec<FunctionUsing>) -> Result<i64> {
    Ok(app
        .function_set_app
        .create_function_set(&FunctionSetData {
            name: name.to_string(),
            description: "scope test set".to_string(),
            category: 0,
            targets,
        })
        .await?
        .value)
}

#[test]
fn current_scope_exposes_specs_and_exact_target() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = app::module::test::create_hybrid_test_app().await?;
        let worker_id = create_worker(&app, "scope_worker").await?;
        let set_id = create_set(&app, "scope_happy", vec![target_worker(worker_id)]).await?;

        let scope = app
            .function_set_app
            .resolve_current_function_scope("scope_happy")
            .await?;

        assert_eq!(scope.set_id, set_id);
        assert!(
            scope
                .functions
                .iter()
                .any(|spec| spec.name == "scope_worker")
        );
        assert_eq!(
            scope.tools.get("scope_worker"),
            Some(&FunctionScopeTarget::Worker {
                worker_id,
                runner_id: 1,
                runner_type: proto::jobworkerp::data::RunnerType::Command as i32,
                using: proto::DEFAULT_METHOD_NAME.to_string(),
            })
        );
        assert_eq!(
            app.function_set_app
                .validate_function_scope_tool(&scope, "scope_worker")
                .await?,
            FunctionScopeTarget::Worker {
                worker_id,
                runner_id: 1,
                runner_type: proto::jobworkerp::data::RunnerType::Command as i32,
                using: proto::DEFAULT_METHOD_NAME.to_string(),
            }
        );

        app.function_set_app
            .delete_function_set(&proto::jobworkerp::function::data::FunctionSetId {
                value: set_id,
            })
            .await?;
        app.worker_app
            .delete(&proto::jobworkerp::data::WorkerId { value: worker_id })
            .await?;

        let runner_set_id =
            create_set(&app, "scope_runner_dispatch_set", vec![target_runner(1)]).await?;
        let runner_scope = app
            .function_set_app
            .resolve_current_function_scope("scope_runner_dispatch_set")
            .await?;
        let runner_enqueue = app
            .function_set_app
            .enqueue_function_for_llm_in_scope(
                Arc::new(HashMap::new()),
                &runner_scope,
                "COMMAND",
                Some(
                    serde_json::json!({
                        "command": "echo",
                        "args": ["pinned runner"]
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                ),
                5,
            )
            .await?;
        let runner_job = app
            .job_app
            .find_job(&runner_enqueue.job_id)
            .await?
            .expect("the selected runner job was enqueued");
        let temp_worker_id = runner_job
            .data
            .as_ref()
            .and_then(|data| data.worker_id)
            .expect("the temporary worker is attached to the job");
        let temp_worker = app
            .worker_app
            .find(&temp_worker_id)
            .await?
            .expect("the temporary worker exists");
        assert_eq!(
            temp_worker.data.as_ref().and_then(|data| data.runner_id),
            Some(RunnerId { value: 1 })
        );
        if let Some(result_handle) = runner_enqueue.result_handle {
            result_handle.abort();
        }
        app.job_app.delete_job(&runner_enqueue.job_id).await?;
        app.function_set_app
            .delete_function_set(&proto::jobworkerp::function::data::FunctionSetId {
                value: runner_set_id,
            })
            .await?;
        Ok(())
    })
}

#[test]
fn scope_rejects_collisions_inside_the_set_and_with_external_runners() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = app::module::test::create_hybrid_test_app().await?;
        let worker_id = create_worker(&app, "COMMAND").await?;

        create_set(
            &app,
            "scope_internal_collision",
            vec![target_runner(1), target_worker(worker_id)],
        )
        .await?;
        let internal = app
            .function_set_app
            .resolve_current_function_scope("scope_internal_collision")
            .await;
        assert!(
            internal.is_err(),
            "same public name must not select two targets"
        );

        create_set(
            &app,
            "scope_external_collision",
            vec![target_worker(worker_id)],
        )
        .await?;
        let external = app
            .function_set_app
            .resolve_current_function_scope("scope_external_collision")
            .await;
        assert!(
            external.is_err(),
            "a runner outside the set must still collide with a selected worker"
        );
        Ok(())
    })
}

#[test]
fn method_tool_names_are_checked_against_exact_and_combined_name_routes() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = app::module::test::create_hybrid_test_app().await?;
        let runner = app
            .runner_app
            .find_runner_by_name("GRPC")
            .await?
            .expect("the GRPC runner is loaded");
        let runner_id = runner.id.expect("the runner has an ID").value;
        let set_id = create_set(
            &app,
            "scope_method_name_set",
            vec![target_runner_method(runner_id, "unary")],
        )
        .await?;

        let listed = app
            .function_set_app
            .resolve_current_function_scope("scope_method_name_set")
            .await?;
        assert_eq!(
            listed.tools.get("GRPC___unary"),
            Some(&FunctionScopeTarget::Runner {
                runner_id,
                runner_type: runner.data.expect("runner data exists").runner_type,
                using: "unary".to_string(),
            })
        );

        // The exact worker-name route and the GRPC___method runner route both
        // resolve this public name, despite only the runner being in the set.
        let external_worker = create_worker(&app, "GRPC___unary").await?;
        assert!(
            app.function_set_app
                .validate_function_scope_tool(&listed, "GRPC___unary")
                .await
                .is_err()
        );

        app.function_set_app
            .delete_function_set(&proto::jobworkerp::function::data::FunctionSetId {
                value: set_id,
            })
            .await?;
        app.worker_app
            .delete(&proto::jobworkerp::data::WorkerId {
                value: external_worker,
            })
            .await?;
        Ok(())
    })
}

#[test]
fn authoritative_resolution_reflects_updated_name_cache_and_rejects_deletion() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = app::module::test::create_hybrid_test_app().await?;
        let worker_a = create_worker(&app, "scope_a").await?;
        let worker_b = create_worker(&app, "scope_b").await?;
        let set_id = create_set(&app, "scope_cache", vec![target_worker(worker_a)]).await?;

        let cached = app
            .function_set_app
            .find_function_set_by_name("scope_cache")
            .await?
            .expect("set is present");
        assert_eq!(
            cached.data.as_ref().unwrap().targets[0]
                .function_id
                .as_ref()
                .unwrap()
                .id,
            Some(function_id::Id::WorkerId(
                proto::jobworkerp::data::WorkerId { value: worker_a }
            ))
        );

        app.function_set_app
            .update_function_set(
                &proto::jobworkerp::function::data::FunctionSetId { value: set_id },
                &Some(FunctionSetData {
                    name: "scope_cache".to_string(),
                    description: "updated".to_string(),
                    category: 0,
                    targets: vec![target_worker(worker_b)],
                }),
            )
            .await?;

        // Updates evict the name cache; the security-sensitive scope resolver
        // independently reads authoritative storage for each request.
        let still_cached = app
            .function_set_app
            .find_function_set_by_name("scope_cache")
            .await?
            .expect("the updated lookup is available");
        assert_eq!(
            still_cached.data.as_ref().unwrap().targets[0]
                .function_id
                .as_ref()
                .unwrap()
                .id,
            Some(function_id::Id::WorkerId(
                proto::jobworkerp::data::WorkerId { value: worker_b }
            ))
        );
        let current = app
            .function_set_app
            .resolve_current_function_scope("scope_cache")
            .await?;
        assert_eq!(
            current.tools.get("scope_b"),
            Some(&FunctionScopeTarget::Worker {
                worker_id: worker_b,
                runner_id: 1,
                runner_type: proto::jobworkerp::data::RunnerType::Command as i32,
                using: proto::DEFAULT_METHOD_NAME.to_string(),
            })
        );

        app.function_set_app
            .delete_function_set(&proto::jobworkerp::function::data::FunctionSetId {
                value: set_id,
            })
            .await?;
        assert!(
            app.function_set_app
                .resolve_current_function_scope("scope_cache")
                .await
                .is_err()
        );
        Ok(())
    })
}

#[test]
fn store_failure_does_not_fall_back_to_a_warmed_name_cache() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = app::module::test::create_hybrid_test_app().await?;
        let worker_id = create_worker(&app, "scope_store_failure").await?;
        let set_id =
            create_set(&app, "scope_store_failure", vec![target_worker(worker_id)]).await?;
        app.function_set_app
            .find_function_set_by_name("scope_store_failure")
            .await?
            .expect("warm the ordinary name cache");

        app.function_set_app
            .delete_function_set(&proto::jobworkerp::function::data::FunctionSetId {
                value: set_id,
            })
            .await?;

        let repository = app.function_set_app.function_set_repository();
        sqlx::query::<Rdb>("ALTER TABLE `function_set` RENAME TO `function_set_unavailable`")
            .execute(repository.db_pool())
            .await?;

        let resolution = app
            .function_set_app
            .resolve_current_function_scope("scope_store_failure")
            .await;
        sqlx::query::<Rdb>("ALTER TABLE `function_set_unavailable` RENAME TO `function_set`")
            .execute(repository.db_pool())
            .await?;
        assert!(resolution.is_err());
        Ok(())
    })
}

#[test]
fn same_job_retarget_is_rejected_but_a_fresh_scope_accepts_the_current_unique_target() -> Result<()>
{
    TEST_RUNTIME.block_on(async {
        let app = app::module::test::create_hybrid_test_app().await?;
        let worker_a = create_worker(&app, "scope_retarget").await?;
        let set_id = create_set(&app, "scope_retarget_set", vec![target_worker(worker_a)]).await?;
        let listed = app
            .function_set_app
            .resolve_current_function_scope("scope_retarget_set")
            .await?;

        app.worker_app
            .delete(&proto::jobworkerp::data::WorkerId { value: worker_a })
            .await?;
        let worker_b = create_worker(&app, "scope_retarget").await?;
        app.function_set_app
            .update_function_set(
                &proto::jobworkerp::function::data::FunctionSetId { value: set_id },
                &Some(FunctionSetData {
                    name: "scope_retarget_set".to_string(),
                    description: "retargeted".to_string(),
                    category: 0,
                    targets: vec![target_worker(worker_b)],
                }),
            )
            .await?;

        assert!(
            app.function_set_app
                .validate_function_scope_tool(&listed, "scope_retarget")
                .await
                .is_err()
        );

        let resumed = app
            .function_set_app
            .resolve_current_function_scope("scope_retarget_set")
            .await?;
        assert_eq!(
            app.function_set_app
                .validate_function_scope_tool(&resumed, "scope_retarget")
                .await?,
            FunctionScopeTarget::Worker {
                worker_id: worker_b,
                runner_id: 1,
                runner_type: proto::jobworkerp::data::RunnerType::Command as i32,
                using: proto::DEFAULT_METHOD_NAME.to_string(),
            }
        );
        Ok(())
    })
}

#[test]
fn execution_rechecks_scope_before_any_enqueue_when_the_target_changes() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = app::module::test::create_hybrid_test_app().await?;
        let worker_a = create_worker(&app, "scope_enqueue_race").await?;
        let set_id = create_set(
            &app,
            "scope_enqueue_race_set",
            vec![target_worker(worker_a)],
        )
        .await?;
        let listed = app
            .function_set_app
            .resolve_current_function_scope("scope_enqueue_race_set")
            .await?;

        let worker_b = create_worker(&app, "scope_enqueue_retarget").await?;
        app.function_set_app
            .update_function_set(
                &proto::jobworkerp::function::data::FunctionSetId { value: set_id },
                &Some(FunctionSetData {
                    name: "scope_enqueue_race_set".to_string(),
                    description: "changed after listing".to_string(),
                    category: 0,
                    targets: vec![target_worker(worker_b)],
                }),
            )
            .await?;

        let result = app
            .function_set_app
            .enqueue_function_for_llm_in_scope(
                Arc::new(HashMap::new()),
                &listed,
                "scope_enqueue_race",
                None,
                1,
            )
            .await;
        assert!(
            result.is_err(),
            "changed target must be rejected before enqueue"
        );
        Ok(())
    })
}

#[test]
fn stream_enqueue_dispatches_to_the_worker_id_selected_by_the_scope() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = app::module::test::create_hybrid_test_app().await?;
        let worker_id = create_worker(&app, "scope_dispatch_worker").await?;
        let set_id = create_set(&app, "scope_dispatch_set", vec![target_worker(worker_id)]).await?;
        let scope = app
            .function_set_app
            .resolve_current_function_scope("scope_dispatch_set")
            .await?;

        let enqueued = app
            .function_set_app
            .enqueue_function_for_llm_in_scope(
                Arc::new(HashMap::new()),
                &scope,
                "scope_dispatch_worker",
                Some(
                    serde_json::json!({
                        "command": "echo",
                        "args": ["scoped dispatch"]
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                ),
                5,
            )
            .await?;
        assert!(enqueued.is_streaming);

        let job = app
            .job_app
            .find_job(&enqueued.job_id)
            .await?
            .expect("the selected worker job was enqueued");
        assert_eq!(
            job.data.as_ref().and_then(|data| data.worker_id),
            Some(proto::jobworkerp::data::WorkerId { value: worker_id })
        );
        assert_eq!(
            job.data.as_ref().and_then(|data| data.using.as_deref()),
            Some(proto::DEFAULT_METHOD_NAME)
        );

        if let Some(result_handle) = enqueued.result_handle {
            result_handle.abort();
        }
        app.job_app.delete_job(&enqueued.job_id).await?;
        app.function_set_app
            .delete_function_set(&proto::jobworkerp::function::data::FunctionSetId {
                value: set_id,
            })
            .await?;
        app.worker_app
            .delete(&proto::jobworkerp::data::WorkerId { value: worker_id })
            .await?;
        Ok(())
    })
}

#[test]
fn scope_uses_current_worker_and_runner_data_after_cross_node_update() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = app::module::test::create_hybrid_test_app().await?;
        let worker_id = create_worker(&app, "scope_cross_node_worker").await?;
        let set_id =
            create_set(&app, "scope_cross_node_set", vec![target_worker(worker_id)]).await?;

        let runner_b = app
            .runner_app
            .find_runner_by_name("HTTP_REQUEST")
            .await?
            .expect("the HTTP_REQUEST runner is loaded");
        let runner_b_id = runner_b.id.expect("the runner has an ID").value;
        let runner_b_type = runner_b.data.expect("runner data exists").runner_type;

        // Warm the ID and name caches, then simulate an update from another node
        // that cannot invalidate this app's worker caches.
        let cached_by_name = app
            .worker_app
            .find_by_name("scope_cross_node_worker")
            .await?
            .expect("worker is cached by name");
        let cached_by_id = app
            .worker_app
            .find(&proto::jobworkerp::data::WorkerId { value: worker_id })
            .await?
            .expect("worker is cached by ID");
        app.worker_app
            .find_list(vec![], None, None, None, None, None, vec![], None, None)
            .await?;
        assert_eq!(
            cached_by_name
                .data
                .as_ref()
                .and_then(|data| data.runner_id)
                .map(|id| id.value),
            Some(1)
        );
        assert_eq!(
            cached_by_id
                .data
                .as_ref()
                .and_then(|data| data.runner_id)
                .map(|id| id.value),
            Some(1)
        );
        sqlx::query::<Rdb>("UPDATE worker SET runner_id = ? WHERE id = ?")
            .bind(runner_b_id)
            .bind(worker_id)
            .execute(app.function_set_app.function_set_repository().db_pool())
            .await?;

        let scope = app
            .function_set_app
            .resolve_current_function_scope("scope_cross_node_set")
            .await?;

        assert_eq!(
            scope.tools.get("scope_cross_node_worker"),
            Some(&FunctionScopeTarget::Worker {
                worker_id,
                runner_id: runner_b_id,
                runner_type: runner_b_type,
                using: proto::DEFAULT_METHOD_NAME.to_string(),
            })
        );
        let exposed = scope
            .functions
            .iter()
            .find(|function| function.name == "scope_cross_node_worker")
            .expect("the current worker is exposed");
        assert_eq!(exposed.runner_id, Some(RunnerId { value: runner_b_id }));
        assert_eq!(exposed.runner_type, runner_b_type);
        app.function_set_app
            .delete_function_set(&proto::jobworkerp::function::data::FunctionSetId {
                value: set_id,
            })
            .await?;
        app.worker_app
            .delete(&proto::jobworkerp::data::WorkerId { value: worker_id })
            .await?;
        Ok(())
    })
}

#[test]
fn worker_scope_lookup_reads_the_store_even_after_the_same_list_was_cached() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = app::module::test::create_hybrid_test_app().await?;
        let worker_id = create_worker(&app, "scope_fresh_list_worker").await?;
        let current = app.worker_app.find_current_list().await?;
        assert!(
            current
                .iter()
                .any(|worker| worker.id.is_some_and(|id| id.value == worker_id))
        );
        let _ = app
            .worker_app
            .find_list(vec![], None, None, None, None, None, vec![], None, None)
            .await?;
        sqlx::query::<Rdb>("UPDATE worker SET name = ? WHERE id = ?")
            .bind("scope_fresh_list_worker_renamed")
            .bind(worker_id)
            .execute(app.function_set_app.function_set_repository().db_pool())
            .await?;
        let current = app.worker_app.find_current_list().await?;
        let worker = current
            .iter()
            .find(|worker| worker.id.is_some_and(|id| id.value == worker_id))
            .unwrap();
        assert_eq!(
            worker.data.as_ref().unwrap().name,
            "scope_fresh_list_worker_renamed"
        );
        app.worker_app
            .delete(&proto::jobworkerp::data::WorkerId { value: worker_id })
            .await?;
        Ok(())
    })
}

#[test]
fn periodic_worker_cannot_be_enqueued_as_a_scoped_tool() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = app::module::test::create_hybrid_test_app().await?;
        let worker_id = create_worker(&app, "scope_periodic_worker").await?;
        let worker_id_proto = proto::jobworkerp::data::WorkerId { value: worker_id };
        let mut worker_data = app
            .worker_app
            .find(&worker_id_proto)
            .await?
            .unwrap()
            .data
            .unwrap();
        worker_data.periodic_interval = 1000;
        app.worker_app
            .update(&worker_id_proto, &Some(worker_data))
            .await?;
        let set_id = create_set(&app, "scope_periodic_set", vec![target_worker(worker_id)]).await?;
        let scope = app
            .function_set_app
            .resolve_current_function_scope("scope_periodic_set")
            .await?;
        let error = app
            .function_set_app
            .enqueue_function_for_llm_in_scope(
                Arc::new(HashMap::new()),
                &scope,
                "scope_periodic_worker",
                Some(
                    serde_json::json!({"command":"echo", "args":["not enqueued"]})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                5,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("periodic Workers"));
        app.function_set_app
            .delete_function_set(&proto::jobworkerp::function::data::FunctionSetId {
                value: set_id,
            })
            .await?;
        app.worker_app.delete(&worker_id_proto).await?;
        Ok(())
    })
}

#[test]
fn fresh_scope_accepts_replacement_worker_when_old_same_name_worker_is_deleted() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = app::module::test::create_hybrid_test_app().await?;
        let worker_a = create_worker(&app, "scope_replacement_name").await?;
        let worker_b = create_worker(&app, "scope_replacement_name_b").await?;
        let set_id =
            create_set(&app, "scope_replacement_set", vec![target_worker(worker_a)]).await?;

        app.worker_app
            .find_by_name("scope_replacement_name")
            .await?
            .expect("warm the old worker name cache");
        app.worker_app
            .find(&proto::jobworkerp::data::WorkerId { value: worker_a })
            .await?
            .expect("warm the deleted worker ID cache");
        app.worker_app
            .find(&proto::jobworkerp::data::WorkerId { value: worker_b })
            .await?
            .expect("warm the replacement worker ID cache");

        // Simulate a different node replacing A with B without invalidating the
        // current node's positive worker caches.
        let pool = app.function_set_app.function_set_repository().db_pool();
        sqlx::query::<Rdb>("DELETE FROM worker WHERE id = ?")
            .bind(worker_a)
            .execute(pool)
            .await?;
        sqlx::query::<Rdb>("UPDATE worker SET name = ? WHERE id = ?")
            .bind("scope_replacement_name")
            .bind(worker_b)
            .execute(pool)
            .await?;
        app.function_set_app
            .update_function_set(
                &proto::jobworkerp::function::data::FunctionSetId { value: set_id },
                &Some(FunctionSetData {
                    name: "scope_replacement_set".to_string(),
                    description: "replacement target".to_string(),
                    category: 0,
                    targets: vec![target_worker(worker_b)],
                }),
            )
            .await?;

        let stale_name_lookup = app
            .worker_app
            .find_by_name("scope_replacement_name")
            .await?
            .expect("the old positive name cache remains populated");
        assert_eq!(
            stale_name_lookup.id,
            Some(proto::jobworkerp::data::WorkerId { value: worker_a })
        );
        let stale_id_lookup = app
            .worker_app
            .find(&proto::jobworkerp::data::WorkerId { value: worker_b })
            .await?
            .expect("the replacement's cached data remains populated");
        assert_eq!(
            stale_id_lookup.data.as_ref().map(|data| data.name.as_str()),
            Some("scope_replacement_name_b")
        );

        let scope = app
            .function_set_app
            .resolve_current_function_scope("scope_replacement_set")
            .await?;
        assert_eq!(
            scope.tools.get("scope_replacement_name"),
            Some(&FunctionScopeTarget::Worker {
                worker_id: worker_b,
                runner_id: 1,
                runner_type: proto::jobworkerp::data::RunnerType::Command as i32,
                using: proto::DEFAULT_METHOD_NAME.to_string(),
            })
        );
        app.function_set_app
            .delete_function_set(&proto::jobworkerp::function::data::FunctionSetId {
                value: set_id,
            })
            .await?;
        app.worker_app
            .delete(&proto::jobworkerp::data::WorkerId { value: worker_b })
            .await?;
        Ok(())
    })
}

#[test]
fn scope_fails_closed_when_runner_name_cache_outlives_authoritative_runner_update() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = app::module::test::create_hybrid_test_app().await?;
        let runner = app
            .runner_app
            .find_runner_by_name("COMMAND")
            .await?
            .expect("the COMMAND runner is loaded");
        let runner_id = runner.id.expect("the runner has an ID");
        app.runner_app
            .find_runner(&runner_id)
            .await?
            .expect("warm the runner ID cache");
        app.runner_app
            .find_runner_by_name("COMMAND")
            .await?
            .expect("warm the runner name cache");
        let set_id = create_set(
            &app,
            "scope_runner_cache",
            vec![target_runner(runner_id.value)],
        )
        .await?;

        let pool = app.function_set_app.function_set_repository().db_pool();
        sqlx::query::<Rdb>("UPDATE runner SET name = ? WHERE id = ?")
            .bind("scope_runner_cache_removed")
            .bind(runner_id.value)
            .execute(pool)
            .await?;
        let resolution = app
            .function_set_app
            .resolve_current_function_scope("scope_runner_cache")
            .await;
        sqlx::query::<Rdb>("UPDATE runner SET name = ? WHERE id = ?")
            .bind("COMMAND")
            .bind(runner_id.value)
            .execute(pool)
            .await?;
        app.function_set_app
            .delete_function_set(&proto::jobworkerp::function::data::FunctionSetId {
                value: set_id,
            })
            .await?;

        assert!(
            resolution.is_err(),
            "a stale runner cache must not keep an administratively renamed target resolvable"
        );
        Ok(())
    })
}

#[test]
fn dispatch_uses_worker_snapshot_validated_before_runner_update() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app = app::module::test::create_hybrid_test_app().await?;
        let worker_id = create_worker(&app, "scope_frozen_worker").await?;
        let set_id = create_set(&app, "scope_frozen_set", vec![target_worker(worker_id)]).await?;
        let scope = app
            .function_set_app
            .resolve_current_function_scope("scope_frozen_set")
            .await?;
        let validated = app
            .function_set_app
            .validate_function_scope_target_for_dispatch(&scope, "scope_frozen_worker")
            .await?;
        let frozen_worker = validated
            .worker_snapshot
            .as_ref()
            .expect("validated Worker targets include a dispatch snapshot");
        assert_eq!(
            frozen_worker.data.as_ref().and_then(|data| data.runner_id),
            Some(RunnerId { value: 1 })
        );

        let runner_b = app
            .runner_app
            .find_runner_by_name("HTTP_REQUEST")
            .await?
            .expect("the HTTP_REQUEST runner is loaded");
        let runner_b_id = runner_b.id.expect("the runner has an ID");
        let mut updated_worker = frozen_worker
            .data
            .clone()
            .expect("the frozen worker has data");
        updated_worker.runner_id = Some(runner_b_id);
        app.worker_app
            .update(
                &proto::jobworkerp::data::WorkerId { value: worker_id },
                &Some(updated_worker),
            )
            .await?;
        assert_eq!(
            app.worker_app
                .find(&proto::jobworkerp::data::WorkerId { value: worker_id })
                .await?
                .and_then(|worker| worker.data)
                .and_then(|data| data.runner_id),
            Some(runner_b_id)
        );

        let enqueued = app
            .function_app
            .enqueue_function_for_llm_target(
                Arc::new(HashMap::new()),
                &validated.target,
                validated.worker_snapshot,
                Some(
                    serde_json::json!({
                        "command": "echo",
                        "args": ["frozen runner A"]
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                ),
                5,
            )
            .await?;
        assert!(
            enqueued.is_streaming,
            "dispatch keeps runner A's streaming semantics after the persisted worker changes"
        );
        let job = app
            .job_app
            .find_job(&enqueued.job_id)
            .await?
            .expect("the snapshot-backed job was enqueued");
        assert_eq!(
            job.data.as_ref().and_then(|data| data.worker_id),
            Some(proto::jobworkerp::data::WorkerId { value: worker_id })
        );
        assert_eq!(
            job.data
                .as_ref()
                .and_then(|data| data.overrides.as_ref())
                .and_then(|overrides| overrides.expected_runner_id),
            Some(1),
            "the queued job must preserve its validated Runner ID until dispatch"
        );

        if let Some(result_handle) = enqueued.result_handle {
            result_handle.abort();
        }
        app.job_app.delete_job(&enqueued.job_id).await?;
        app.function_set_app
            .delete_function_set(&proto::jobworkerp::function::data::FunctionSetId {
                value: set_id,
            })
            .await?;
        app.worker_app
            .delete(&proto::jobworkerp::data::WorkerId { value: worker_id })
            .await?;
        Ok(())
    })
}
