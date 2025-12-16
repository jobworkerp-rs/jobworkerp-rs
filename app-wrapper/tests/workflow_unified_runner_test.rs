//! Integration tests for Workflow Unified Runner (multi-method support)
//! Tests the unified WORKFLOW runner with 'run' and 'create' methods

#![allow(clippy::uninlined_format_args)]

use anyhow::Result;
use app::module::test::create_hybrid_test_app;
use app_wrapper::modules::AppWrapperModule;
use app_wrapper::workflow::runner::unified::WorkflowUnifiedRunnerImpl;
use infra_utils::infra::test::TEST_RUNTIME;
use jobworkerp_runner::jobworkerp::runner::create_workflow_args::WorkflowSource as CreateWorkflowSource;
use jobworkerp_runner::jobworkerp::runner::workflow_run_args::WorkflowSource as RunWorkflowSource;
use jobworkerp_runner::jobworkerp::runner::workflow_runner_settings::WorkflowSource as SettingsWorkflowSource;
use jobworkerp_runner::jobworkerp::runner::{
    CreateWorkflowArgs, CreateWorkflowResult, WorkflowResult, WorkflowRunArgs,
    WorkflowRunnerSettings,
};
use jobworkerp_runner::runner::RunnerTrait;
use prost::Message;
use std::collections::HashMap;
use std::sync::Arc;

/// Simple test workflow definition
fn create_simple_workflow_json() -> String {
    r#"{
        "document": {
            "dsl": "0.0.1",
            "namespace": "test",
            "name": "simple-test-workflow",
            "version": "1.0.0"
        },
        "input": {
            "from": ".testInput"
        },
        "do": [
            {
                "echo-task": {
                    "run": {
                        "runner": {
                            "name": "COMMAND",
                            "arguments": {
                                "command": "echo",
                                "args": ["test"]
                            }
                        }
                    }
                }
            }
        ]
    }"#
    .to_string()
}

/// Create test workflow unified runner
async fn create_test_unified_runner() -> Result<WorkflowUnifiedRunnerImpl> {
    let app_module = Arc::new(create_hybrid_test_app().await?);
    let app_wrapper_module = Arc::new(AppWrapperModule::new_by_env(None));
    WorkflowUnifiedRunnerImpl::new(app_wrapper_module, app_module)
}

/// Create runner settings with workflow definition (using new WorkflowRunnerSettings)
fn create_workflow_settings(workflow_json: &str) -> Vec<u8> {
    let settings = WorkflowRunnerSettings {
        workflow_source: Some(SettingsWorkflowSource::WorkflowData(
            workflow_json.to_string(),
        )),
    };
    settings.encode_to_vec()
}

#[test]
#[ignore = "need backend with same db"]
fn test_unified_runner_run_method_default() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

        let mut runner = create_test_unified_runner().await?;
        let workflow_json = create_simple_workflow_json();
        runner
            .load(create_workflow_settings(&workflow_json))
            .await?;

        // Use new WorkflowRunArgs (no workflow_source since it's in settings)
        let args = WorkflowRunArgs {
            workflow_source: None,
            input: r#"{"testInput": "Hello from unified runner"}"#.to_string(),
            ..Default::default()
        };

        let args_bytes = args.encode_to_vec();

        // Test with None (should default to "run")
        let (result, _metadata) = runner.run(&args_bytes, HashMap::new(), None).await;

        let output = result?;
        let response = WorkflowResult::decode(&output[..])?;

        tracing::info!("Workflow result: {:?}", response);
        // Verify success: output is present and status indicates success (0)
        assert!(
            !response.output.is_empty() && response.status == 0,
            "Expected non-empty output and success status (0), got output='{}', status={}",
            response.output,
            response.status
        );

        tracing::info!("Unified runner run method (default) test passed!");
        Ok(())
    })
}

#[test]
#[ignore = "need backend with same db"]
fn test_unified_runner_run_method_explicit() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

        let mut runner = create_test_unified_runner().await?;
        let workflow_json = create_simple_workflow_json();
        runner
            .load(create_workflow_settings(&workflow_json))
            .await?;

        // Use new WorkflowRunArgs (no workflow_source since it's in settings)
        let args = WorkflowRunArgs {
            workflow_source: None,
            input: r#"{"testInput": "Hello with explicit run"}"#.to_string(),
            ..Default::default()
        };

        let args_bytes = args.encode_to_vec();

        // Test with explicit "run" method
        let (result, _metadata) = runner.run(&args_bytes, HashMap::new(), Some("run")).await;

        let output = result?;
        let response = WorkflowResult::decode(&output[..])?;

        tracing::info!("Workflow result: {:?}", response);
        // Verify success: output is present and status indicates success (0)
        assert!(
            !response.output.is_empty() && response.status == 0,
            "Expected non-empty output and success status (0), got output='{}', status={}",
            response.output,
            response.status
        );

        tracing::info!("Unified runner run method (explicit) test passed!");
        Ok(())
    })
}

#[test]
fn test_unified_runner_create_method() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

        let mut runner = create_test_unified_runner().await?;
        // Create method doesn't need settings loaded
        runner.load(vec![]).await?;

        let workflow_json = create_simple_workflow_json();
        let args = CreateWorkflowArgs {
            name: "test-created-workflow".to_string(),
            workflow_source: Some(CreateWorkflowSource::WorkflowData(workflow_json)),
            worker_options: None,
        };

        let args_bytes = args.encode_to_vec();

        // Test with "create" method
        let (result, _metadata) = runner
            .run(&args_bytes, HashMap::new(), Some("create"))
            .await;

        let output = result?;
        let response = CreateWorkflowResult::decode(&output[..])?;

        tracing::info!("Create workflow result: {:?}", response);
        assert!(response.worker_id.is_some());

        tracing::info!("Unified runner create method test passed!");
        Ok(())
    })
}

#[test]
fn test_unified_runner_unknown_method_error() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

        let mut runner = create_test_unified_runner().await?;
        runner.load(vec![]).await?;

        // Use new WorkflowRunArgs with workflow in args (since settings is empty)
        let workflow_json = create_simple_workflow_json();
        let args = WorkflowRunArgs {
            workflow_source: Some(RunWorkflowSource::WorkflowData(workflow_json)),
            input: r#"{"testInput": "test"}"#.to_string(),
            ..Default::default()
        };

        let args_bytes = args.encode_to_vec();

        // Call with unknown method
        let (result, _metadata) = runner
            .run(&args_bytes, HashMap::new(), Some("unknown_method"))
            .await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Unknown method"),
            "Expected 'Unknown method' error, got: {}",
            err_msg
        );

        tracing::info!("Unknown method error test passed!");
        Ok(())
    })
}

#[test]
fn test_unified_runner_create_invalid_workflow() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

        let mut runner = create_test_unified_runner().await?;
        runner.load(vec![]).await?;

        // Invalid workflow JSON (missing required fields)
        let invalid_workflow = r#"{"invalid": "workflow"}"#;
        let args = CreateWorkflowArgs {
            name: "invalid-workflow".to_string(),
            workflow_source: Some(CreateWorkflowSource::WorkflowData(
                invalid_workflow.to_string(),
            )),
            worker_options: None,
        };

        let args_bytes = args.encode_to_vec();

        let (result, _metadata) = runner
            .run(&args_bytes, HashMap::new(), Some("create"))
            .await;

        // Should fail due to invalid workflow
        assert!(result.is_err());
        tracing::info!(
            "Invalid workflow correctly rejected: {}",
            result.unwrap_err()
        );

        tracing::info!("Create invalid workflow error test passed!");
        Ok(())
    })
}

#[test]
fn test_unified_runner_create_empty_name_error() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

        let mut runner = create_test_unified_runner().await?;
        runner.load(vec![]).await?;

        let workflow_json = create_simple_workflow_json();
        let args = CreateWorkflowArgs {
            name: "".to_string(), // Empty name should fail
            workflow_source: Some(CreateWorkflowSource::WorkflowData(workflow_json)),
            worker_options: None,
        };

        let args_bytes = args.encode_to_vec();

        let (result, _metadata) = runner
            .run(&args_bytes, HashMap::new(), Some("create"))
            .await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("empty") || err_msg.contains("name"),
            "Expected empty name error, got: {}",
            err_msg
        );

        tracing::info!("Empty name error test passed!");
        Ok(())
    })
}
