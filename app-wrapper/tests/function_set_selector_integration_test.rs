//! Integration tests for FunctionSetSelectorImpl.
//! Tests load() and run() with AppModule (SQLite), no external service required.

use anyhow::Result;
use app::app::function::function_set::FunctionSetApp;
use app::module::AppModule;
use app_wrapper::function_set_selector::FunctionSetSelectorImpl;
use infra_utils::infra::test::TEST_RUNTIME;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_runner::jobworkerp::runner::function_set_selector::{
    FunctionSetSelectorArgs, FunctionSetSelectorResult, FunctionSetSelectorSettings,
};
use jobworkerp_runner::runner::RunnerTrait;
use jobworkerp_runner::runner::cancellation_helper::CancelMonitoringHelper;
use proto::jobworkerp::data::RunnerId;
use proto::jobworkerp::function::data::{
    FunctionId, FunctionSetData, FunctionSetId, FunctionUsing, function_id,
};
use std::collections::HashMap;
use std::sync::Arc;

async fn setup_test_app_module() -> Result<AppModule> {
    app::module::test::create_hybrid_test_app().await
}

struct TestContext {
    app_module: Arc<AppModule>,
    runner: Box<dyn RunnerTrait + Send + Sync>,
    set_ids: Vec<FunctionSetId>,
}

impl TestContext {
    async fn cleanup(self) -> Result<()> {
        for id in &self.set_ids {
            let _ = self
                .app_module
                .function_set_app
                .delete_function_set(id)
                .await;
        }
        Ok(())
    }
}

async fn setup_test_env(settings: FunctionSetSelectorSettings) -> Result<TestContext> {
    let app_module = Arc::new(setup_test_app_module().await?);

    let mut set_ids = Vec::new();

    // Register two FunctionSets
    let set1_data = FunctionSetData {
        name: "selector-test-set-1".to_string(),
        description: "First test set for selector".to_string(),
        category: 1,
        targets: vec![FunctionUsing {
            function_id: Some(FunctionId {
                id: Some(function_id::Id::RunnerId(RunnerId { value: 1 })), // COMMAND
            }),
            using: None,
        }],
    };
    match app_module
        .function_set_app
        .create_function_set(&set1_data)
        .await
    {
        Ok(id) => set_ids.push(id),
        Err(e) if e.to_string().to_lowercase().contains("unique") => {
            // Already exists from a previous test run
        }
        Err(e) => return Err(e),
    }

    let set2_data = FunctionSetData {
        name: "selector-test-set-2".to_string(),
        description: "Second test set for selector".to_string(),
        category: 2,
        targets: vec![FunctionUsing {
            function_id: Some(FunctionId {
                id: Some(function_id::Id::RunnerId(RunnerId { value: 2 })), // HTTP_REQUEST
            }),
            using: None,
        }],
    };
    match app_module
        .function_set_app
        .create_function_set(&set2_data)
        .await
    {
        Ok(id) => set_ids.push(id),
        Err(e) if e.to_string().to_lowercase().contains("unique") => {}
        Err(e) => return Err(e),
    }

    // Create runner
    let cancel_helper = CancelMonitoringHelper::new(Box::new(DummyCancelManager));
    let mut runner =
        FunctionSetSelectorImpl::new_with_cancel_monitoring(app_module.clone(), cancel_helper);

    let settings_bytes = ProstMessageCodec::serialize_message(&settings)?;
    runner.load(settings_bytes).await?;

    Ok(TestContext {
        app_module,
        runner: Box::new(runner),
        set_ids,
    })
}

#[derive(Debug)]
struct DummyCancelManager;

#[async_trait::async_trait]
impl jobworkerp_runner::runner::cancellation::RunnerCancellationManager for DummyCancelManager {
    async fn setup_monitoring(
        &mut self,
        _job_id: &proto::jobworkerp::data::JobId,
        _job_data: &proto::jobworkerp::data::JobData,
    ) -> anyhow::Result<jobworkerp_runner::runner::cancellation::CancellationSetupResult> {
        Ok(jobworkerp_runner::runner::cancellation::CancellationSetupResult::MonitoringStarted)
    }

    async fn cleanup_monitoring(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn get_token(&self) -> tokio_util::sync::CancellationToken {
        tokio_util::sync::CancellationToken::new()
    }

    fn is_cancelled(&self) -> bool {
        false
    }
}

fn default_settings() -> FunctionSetSelectorSettings {
    FunctionSetSelectorSettings {
        entry_template: None,
        result_template: None,
        tool_entry_template: None,
    }
}

async fn run_selector(
    runner: &mut (dyn RunnerTrait + Send + Sync),
    args: &FunctionSetSelectorArgs,
) -> Result<FunctionSetSelectorResult> {
    let args_bytes = ProstMessageCodec::serialize_message(args)?;
    let (result, _metadata) = runner.run(&args_bytes, HashMap::new(), None).await;
    let result_bytes = result?;
    ProstMessageCodec::deserialize_message::<FunctionSetSelectorResult>(&result_bytes)
}

#[test]
fn test_function_set_selector_run_returns_all_sets() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let mut ctx = setup_test_env(default_settings()).await?;

        let args = FunctionSetSelectorArgs {
            limit: None,
            offset: None,
            category: None,
            name_filter: None,
        };

        let result = run_selector(ctx.runner.as_mut(), &args).await?;

        println!("Formatted output:\n{}", result.formatted_output);
        assert!(
            result.formatted_output.contains("selector-test-set-1"),
            "Output should contain set-1"
        );
        assert!(
            result.formatted_output.contains("selector-test-set-2"),
            "Output should contain set-2"
        );
        assert!(result.total_count >= 2, "Should have at least 2 sets");

        ctx.cleanup().await
    })
}

#[test]
fn test_function_set_selector_run_with_name_filter() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let mut ctx = setup_test_env(default_settings()).await?;

        let args = FunctionSetSelectorArgs {
            limit: None,
            offset: None,
            category: None,
            name_filter: Some("set-1".to_string()),
        };

        let result = run_selector(ctx.runner.as_mut(), &args).await?;

        println!("Filtered output:\n{}", result.formatted_output);
        assert!(
            result.formatted_output.contains("selector-test-set-1"),
            "Output should contain set-1"
        );
        assert!(
            !result.formatted_output.contains("selector-test-set-2"),
            "Output should NOT contain set-2"
        );

        ctx.cleanup().await
    })
}

#[test]
fn test_function_set_selector_run_with_category_filter() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let mut ctx = setup_test_env(default_settings()).await?;

        let args = FunctionSetSelectorArgs {
            limit: None,
            offset: None,
            category: Some(2),
            name_filter: None,
        };

        let result = run_selector(ctx.runner.as_mut(), &args).await?;

        println!("Category-filtered output:\n{}", result.formatted_output);
        assert!(
            result.formatted_output.contains("selector-test-set-2"),
            "Output should contain set-2 (category=2)"
        );
        assert!(
            !result.formatted_output.contains("selector-test-set-1"),
            "Output should NOT contain set-1 (category=1)"
        );

        ctx.cleanup().await
    })
}

#[test]
fn test_function_set_selector_run_with_limit() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let mut ctx = setup_test_env(default_settings()).await?;

        let args = FunctionSetSelectorArgs {
            limit: Some(1),
            offset: None,
            category: None,
            name_filter: None,
        };

        let result = run_selector(ctx.runner.as_mut(), &args).await?;

        println!("Limited output:\n{}", result.formatted_output);

        // Count how many "##" headers (each entry starts with "## ")
        let entry_count = result.formatted_output.matches("## ").count();
        assert_eq!(entry_count, 1, "Should only have 1 entry due to limit=1");

        ctx.cleanup().await
    })
}

#[test]
fn test_function_set_selector_run_with_custom_template() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let settings = FunctionSetSelectorSettings {
            entry_template: Some("[{{ name }}] {{ tool_count }} tools".to_string()),
            result_template: Some("Sets:\n{{ entries }}".to_string()),
            tool_entry_template: Some("* {{ tool_name }}".to_string()),
        };
        let mut ctx = setup_test_env(settings).await?;

        let args = FunctionSetSelectorArgs {
            limit: None,
            offset: None,
            category: None,
            name_filter: Some("set-1".to_string()),
        };

        let result = run_selector(ctx.runner.as_mut(), &args).await?;

        println!("Custom template output:\n{}", result.formatted_output);
        assert!(
            result.formatted_output.contains("[selector-test-set-1]"),
            "Output should use custom template format"
        );
        assert!(
            result.formatted_output.starts_with("Sets:"),
            "Output should use custom result template"
        );

        ctx.cleanup().await
    })
}
