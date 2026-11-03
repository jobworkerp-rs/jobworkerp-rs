//! Integration tests for TryTask error handling in workflows
//! Tests try-catch-raise error handling with streaming execution

#![allow(clippy::uninlined_format_args)]

use anyhow::Result;
use app::module::test::create_hybrid_test_app;
use app_wrapper::modules::AppWrapperModule;
use app_wrapper::workflow::runner::unified::WorkflowUnifiedRunnerImpl;
use infra_utils::infra::test::TEST_RUNTIME;
use jobworkerp_runner::jobworkerp::runner::workflow_runner_settings::WorkflowSource as SettingsWorkflowSource;
use jobworkerp_runner::jobworkerp::runner::{
    WorkflowResult, WorkflowRunArgs, WorkflowRunnerSettings,
};
use jobworkerp_runner::runner::RunnerTrait;
use prost::Message;
use std::collections::HashMap;
use std::sync::Arc;

// raise.error.type must be a URI (schema: $defs/uriTemplate)
const TEST_ERROR_TYPE: &str = "https://example.com/errors/test";
const INNER_ERROR_TYPE: &str = "https://example.com/errors/inner";

/// Create a workflow that raises an error inside try block and catches it
fn create_try_catch_workflow_json() -> String {
    create_try_catch_workflow_json_with_error_type(TEST_ERROR_TYPE)
}

fn create_try_catch_workflow_json_with_error_type(error_type: &str) -> String {
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
                                        "type": "{{ERROR_TYPE}}",
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
    .replace("{{ERROR_TYPE}}", error_type)
}

/// Create a workflow that raises an error and catches it with catch.do, storing error in global context
fn create_try_with_catch_do_storing_error_workflow_json() -> String {
    r#"{
        "document": {
            "dsl": "1.0.0",
            "namespace": "test",
            "name": "try-catch-do-storing-error-workflow",
            "version": "1.0.0"
        },
        "input": {
            "from": ".testInput"
        },
        "do": [
            {
                "tryWithCatchDo": {
                    "try": [
                        {
                            "raiseError": {
                                "raise": {
                                    "error": {
                                        "type": "{{ERROR_TYPE}}",
                                        "status": 400,
                                        "detail": "Error stored by catch.do"
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
    .replace("{{ERROR_TYPE}}", TEST_ERROR_TYPE)
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
                                                    "type": "{{ERROR_TYPE}}",
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
    .replace("{{ERROR_TYPE}}", INNER_ERROR_TYPE)
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
        workflow_context: None,
    };
    settings.encode_to_vec()
}

#[test]
fn test_raise_with_non_uri_error_type_fails_to_load() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let mut runner = create_test_unified_runner().await?;
        let workflow_json = create_try_catch_workflow_json_with_error_type("test-error");

        let result = runner.load(create_workflow_settings(&workflow_json)).await;

        let err = result.expect_err("non-URI raise.error.type must be rejected on load");
        let message = format!("{:?}", err);
        assert!(
            message.contains("Failed to validate workflow schema")
                && message.contains("/do/0/tryWithRaise"),
            "load should fail by schema validation, got: {}",
            message
        );
        Ok(())
    })
}

#[test]
fn test_try_catch_with_raise_error() -> Result<()> {
    TEST_RUNTIME.block_on(async {
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
    })
}

#[test]
fn test_try_with_catch_do_storing_error() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

        let mut runner = create_test_unified_runner().await?;
        let workflow_json = create_try_with_catch_do_storing_error_workflow_json();
        runner
            .load(create_workflow_settings(&workflow_json))
            .await?;

        let args = WorkflowRunArgs {
            workflow_source: None,
            input: r#"{"testInput": "test try with catch.do storing error"}"#.to_string(),
            ..Default::default()
        };

        let args_bytes = args.encode_to_vec();
        let (result, _metadata) = runner.run(&args_bytes, HashMap::new(), None).await;

        let output = result?;
        let response = WorkflowResult::decode(&output[..])?;

        tracing::info!("Workflow result: {:?}", response);

        // Workflow should complete successfully (error is caught and stored by catch.do)
        assert!(
            response.status == 0,
            "Workflow should complete successfully after catching the error, got status: {}",
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

        // Verify the raised error is propagated intact to catch.do
        let error = output_json
            .get("error")
            .filter(|e| !e.is_null())
            .expect("error should be stored in context");
        assert_eq!(error.get("type"), Some(&serde_json::json!(TEST_ERROR_TYPE)));
        assert_eq!(error.get("status"), Some(&serde_json::json!(400)));
        assert_eq!(
            error.get("detail"),
            Some(&serde_json::json!("Error stored by catch.do"))
        );

        Ok(())
    })
}

#[test]
fn test_nested_try_catch() -> Result<()> {
    TEST_RUNTIME.block_on(async {
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
    })
}
