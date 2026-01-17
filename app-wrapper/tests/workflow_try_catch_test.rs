//! Integration tests for TryTask error handling in workflows
//! Tests try-catch-raise error handling with streaming execution
//!
//! NOTE: These tests use a multi-threaded runtime because the streaming
//! implementation with RwLock requires multiple threads to avoid deadlock.
//! The single-threaded TEST_RUNTIME causes deadlock in stream! macro contexts.

#![allow(clippy::uninlined_format_args)]

use anyhow::Result;
use app::module::test::create_hybrid_test_app;
use app_wrapper::modules::AppWrapperModule;
use app_wrapper::workflow::runner::unified::WorkflowUnifiedRunnerImpl;
use jobworkerp_runner::jobworkerp::runner::workflow_runner_settings::WorkflowSource as SettingsWorkflowSource;
use jobworkerp_runner::jobworkerp::runner::{
    WorkflowResult, WorkflowRunArgs, WorkflowRunnerSettings,
};
use jobworkerp_runner::runner::RunnerTrait;
use prost::Message;
use std::collections::HashMap;
use std::sync::Arc;

/// Create a workflow that raises an error inside try block and catches it
fn create_try_catch_workflow_json() -> String {
    r#"{
        "document": {
            "dsl": "1.0.0",
            "namespace": "test",
            "name": "try-catch-test-workflow",
            "version": "1.0.0"
        },
        "input": {
            "from": ".testInput"
        },
        "do": [
            {
                "tryWithRaise": {
                    "try": [
                        {
                            "setBeforeRaise": {
                                "set": {
                                    "before_raise": "executed"
                                }
                            }
                        },
                        {
                            "raiseError": {
                                "raise": {
                                    "error": {
                                        "type": "test-error",
                                        "status": 500,
                                        "detail": "Test error from raise task"
                                    }
                                }
                            }
                        },
                        {
                            "setAfterRaise": {
                                "set": {
                                    "after_raise": "should_not_execute"
                                }
                            }
                        }
                    ],
                    "catch": {
                        "as": "caught_error",
                        "do": [
                            {
                                "setCatchExecuted": {
                                    "set": {
                                        "catch_executed": true,
                                        "error_caught": "${$caught_error != null}"
                                    }
                                }
                            }
                        ]
                    }
                }
            },
            {
                "setFinalResult": {
                    "set": {
                        "workflow_completed": true
                    }
                }
            }
        ],
        "output": {
            "as": {
                "before_raise": "${$before_raise}",
                "catch_executed": "${$catch_executed}",
                "error_caught": "${$error_caught}",
                "workflow_completed": "${$workflow_completed}"
            }
        }
    }"#
    .to_string()
}

/// Create a workflow that raises an error and catches it with catch.do, storing error in global context
fn create_try_with_catch_do_storing_error_workflow_json() -> String {
    r#"{
        "document": {
            "dsl": "1.0.0",
            "namespace": "test",
            "name": "try-no-catch-do-workflow",
            "version": "1.0.0"
        },
        "input": {
            "from": ".testInput"
        },
        "do": [
            {
                "tryWithoutCatchDo": {
                    "try": [
                        {
                            "raiseError": {
                                "raise": {
                                    "error": {
                                        "type": "test-error",
                                        "status": 400,
                                        "detail": "Error without catch.do"
                                    }
                                }
                            }
                        }
                    ],
                    "catch": {
                        "as": "caught_error",
                        "do": [
                            {
                                "setErrorToGlobal": {
                                    "set": {
                                        "error": "${$caught_error}"
                                    }
                                }
                            }
                        ]
                    }
                }
            },
            {
                "setAfterTry": {
                    "set": {
                        "after_try_executed": true
                    }
                }
            }
        ],
        "output": {
            "as": {
                "after_try_executed": "${$after_try_executed}",
                "error": "${$error}"
            }
        }
    }"#
    .to_string()
}

/// Create a workflow with nested try blocks
fn create_nested_try_workflow_json() -> String {
    r#"{
        "document": {
            "dsl": "1.0.0",
            "namespace": "test",
            "name": "nested-try-workflow",
            "version": "1.0.0"
        },
        "input": {
            "from": ".testInput"
        },
        "do": [
            {
                "outerTry": {
                    "try": [
                        {
                            "innerTry": {
                                "try": [
                                    {
                                        "raiseInner": {
                                            "raise": {
                                                "error": {
                                                    "type": "inner-error",
                                                    "status": 500,
                                                    "detail": "Inner error"
                                                }
                                            }
                                        }
                                    }
                                ],
                                "catch": {
                                    "as": "inner_error",
                                    "do": [
                                        {
                                            "setInnerCatch": {
                                                "set": {
                                                    "inner_catch_executed": true,
                                                    "outer_catch_executed": false
                                                }
                                            }
                                        }
                                    ]
                                }
                            }
                        },
                        {
                            "setAfterInnerTry": {
                                "set": {
                                    "after_inner_try": "executed"
                                }
                            }
                        }
                    ],
                    "catch": {
                        "as": "outer_error",
                        "do": [
                            {
                                "setOuterCatch": {
                                    "set": {
                                        "outer_catch_executed": true
                                    }
                                }
                            }
                        ]
                    }
                }
            },
            {
                "setFinal": {
                    "set": {
                        "workflow_completed": true
                    }
                }
            }
        ],
        "output": {
            "as": {
                "inner_catch_executed": "${$inner_catch_executed // null}",
                "after_inner_try": "${$after_inner_try // null}",
                "outer_catch_executed": "${$outer_catch_executed // null}",
                "workflow_completed": "${$workflow_completed}"
            }
        }
    }"#
    .to_string()
}

/// Create test workflow unified runner
async fn create_test_unified_runner() -> Result<WorkflowUnifiedRunnerImpl> {
    let app_module = Arc::new(create_hybrid_test_app().await?);
    let app_wrapper_module = Arc::new(AppWrapperModule::new_by_env(None));
    WorkflowUnifiedRunnerImpl::new(app_wrapper_module, app_module)
}

/// Create runner settings with workflow definition
fn create_workflow_settings(workflow_json: &str) -> Vec<u8> {
    let settings = WorkflowRunnerSettings {
        workflow_source: Some(SettingsWorkflowSource::WorkflowData(
            workflow_json.to_string(),
        )),
    };
    settings.encode_to_vec()
}

#[tokio::test]
#[ignore = "requires backend with same db"]
async fn test_try_catch_with_raise_error() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let mut runner = create_test_unified_runner().await?;
    let workflow_json = create_try_catch_workflow_json();
    runner
        .load(create_workflow_settings(&workflow_json))
        .await?;

    let args = WorkflowRunArgs {
        workflow_source: None,
        input: r#"{"testInput": "test try-catch"}"#.to_string(),
        ..Default::default()
    };

    let args_bytes = args.encode_to_vec();
    let (result, _metadata) = runner.run(&args_bytes, HashMap::new(), None).await;

    let output = result?;
    let response = WorkflowResult::decode(&output[..])?;

    tracing::info!("Workflow result: {:?}", response);

    // Verify workflow completed successfully
    assert!(
        response.status == 0,
        "Workflow should complete successfully, got status: {}",
        response.status
    );

    // Parse output to verify error was caught
    let output_json: serde_json::Value = serde_json::from_str(&response.output)?;

    // Verify before_raise was executed
    assert_eq!(
        output_json.get("before_raise"),
        Some(&serde_json::json!("executed")),
        "before_raise should be executed"
    );

    // Verify after_raise was NOT executed (error should have stopped execution)
    assert!(
        output_json.get("after_raise").is_none(),
        "after_raise should NOT be executed"
    );

    // Verify catch.do was executed
    assert_eq!(
        output_json.get("catch_executed"),
        Some(&serde_json::json!(true)),
        "catch_executed should be true"
    );

    // Verify error was caught
    assert_eq!(
        output_json.get("error_caught"),
        Some(&serde_json::json!(true)),
        "error_caught should be true"
    );

    // Verify workflow completed after try-catch
    assert_eq!(
        output_json.get("workflow_completed"),
        Some(&serde_json::json!(true)),
        "workflow_completed should be true"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "requires backend with same db"]
async fn test_try_with_catch_do_storing_error() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let mut runner = create_test_unified_runner().await?;
    let workflow_json = create_try_with_catch_do_storing_error_workflow_json();
    runner
        .load(create_workflow_settings(&workflow_json))
        .await?;

    let args = WorkflowRunArgs {
        workflow_source: None,
        input: r#"{"testInput": "test try without catch.do"}"#.to_string(),
        ..Default::default()
    };

    let args_bytes = args.encode_to_vec();
    let (result, _metadata) = runner.run(&args_bytes, HashMap::new(), None).await;

    let output = result?;
    let response = WorkflowResult::decode(&output[..])?;

    tracing::info!("Workflow result: {:?}", response);

    // Workflow should complete successfully (error is caught but no catch.do)
    assert!(
        response.status == 0,
        "Workflow should complete successfully even without catch.do, got status: {}",
        response.status
    );

    // Parse output
    let output_json: serde_json::Value = serde_json::from_str(&response.output)?;

    // Verify after_try was executed (workflow continues after caught error)
    assert_eq!(
        output_json.get("after_try_executed"),
        Some(&serde_json::json!(true)),
        "after_try_executed should be true"
    );

    // Verify error is stored in context
    assert!(
        output_json.get("error").is_some(),
        "error should be stored in context"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "requires backend with same db"]
async fn test_nested_try_catch() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let mut runner = create_test_unified_runner().await?;
    let workflow_json = create_nested_try_workflow_json();
    runner
        .load(create_workflow_settings(&workflow_json))
        .await?;

    let args = WorkflowRunArgs {
        workflow_source: None,
        input: r#"{"testInput": "test nested try"}"#.to_string(),
        ..Default::default()
    };

    let args_bytes = args.encode_to_vec();
    let (result, _metadata) = runner.run(&args_bytes, HashMap::new(), None).await;

    let output = result?;
    let response = WorkflowResult::decode(&output[..])?;

    tracing::info!("Workflow result: {:?}", response);

    // Workflow should complete successfully
    assert!(
        response.status == 0,
        "Workflow should complete successfully, got status: {}",
        response.status
    );

    // Parse output
    let output_json: serde_json::Value = serde_json::from_str(&response.output)?;

    // Verify inner catch was executed
    assert_eq!(
        output_json.get("inner_catch_executed"),
        Some(&serde_json::json!(true)),
        "inner_catch_executed should be true"
    );

    // Verify code after inner try was executed (inner error was caught)
    assert_eq!(
        output_json.get("after_inner_try"),
        Some(&serde_json::json!("executed")),
        "after_inner_try should be executed"
    );

    // Verify outer catch was NOT executed (inner error was handled)
    // Note: output.as includes outer_catch_executed which is set to false in inner catch
    // jaq evaluates it to null because the value is false (boolean), not because it's missing
    let outer_catch = output_json.get("outer_catch_executed");
    assert!(
        outer_catch.is_none()
            || outer_catch == Some(&serde_json::json!(null))
            || outer_catch == Some(&serde_json::json!(false)),
        "outer_catch_executed should NOT be true, got: {:?}",
        outer_catch
    );

    // Verify workflow completed
    assert_eq!(
        output_json.get("workflow_completed"),
        Some(&serde_json::json!(true)),
        "workflow_completed should be true"
    );

    Ok(())
}
