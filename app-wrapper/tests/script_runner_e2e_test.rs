/// End-to-End tests for Script Runner (Python) implementation
///
/// These tests verify the complete workflow execution from YAML definition
/// to Python script execution, including:
/// 1. Inline script execution with arguments
/// 2. External script download and execution (HTTPS only)
/// 3. Base64 encoding of arguments in real execution
/// 4. Malicious payload rejection
/// 5. Timeout and cancellation
/// 6. use_static pooling behavior
/// 7. Error handling and reporting
///
/// Run with: cargo test --package app-wrapper --test script_runner_e2e_test -- --ignored --test-threads=1
use anyhow::Result;
use app::module::test::create_hybrid_test_app;
use app::module::AppModule;
use app_wrapper::modules::{AppWrapperConfigModule, AppWrapperModule, AppWrapperRepositoryModule};
use app_wrapper::workflow::definition::WorkflowLoader;
use app_wrapper::workflow::execute::task::run::script::ScriptTaskExecutor;
use app_wrapper::workflow::execute::workflow::WorkflowExecutor;
use app_wrapper::workflow::WorkflowConfig;
use futures::{pin_mut, StreamExt};
use infra_utils::infra::test::TEST_RUNTIME;
use net_utils::net::reqwest::ReqwestClient;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// Create test app wrapper module (equivalent to app_wrapper::modules::test::create_test_app_wrapper_module)
fn create_test_app_wrapper_module(app_module: Arc<AppModule>) -> AppWrapperModule {
    let workflow_config = Arc::new(WorkflowConfig::new(
        Some(120),                           // task_default_timeout
        Some("test-user-agent".to_string()), // http_user_agent
        Some(60),                            // http_timeout_sec
        Some(120),                           // checkpoint_expire_sec
        Some(1000),                          // checkpoint_max_count
    ));
    let config_module = Arc::new(AppWrapperConfigModule::new(workflow_config));
    let repositories = Arc::new(AppWrapperRepositoryModule::new(
        config_module.clone(),
        app_module
            .repositories
            .redis_module
            .as_ref()
            .map(|r| r.redis_pool),
    ));
    AppWrapperModule {
        config_module,
        repositories,
    }
}

/// Helper function to create and execute script workflow using WorkflowExecutor
async fn execute_script_workflow(
    app: Arc<AppModule>,
    workflow_json: serde_json::Value,
    input_data: serde_json::Value,
) -> Result<serde_json::Value> {
    let app_wrapper_module = Arc::new(create_test_app_wrapper_module(app.clone()));

    // Create HTTP client for workflow loader
    let http_client = ReqwestClient::new(
        Some("test-script-runner"),
        Some(std::time::Duration::from_secs(30)),
        Some(std::time::Duration::from_secs(30)),
        Some(1),
    )?;

    // Load workflow from JSON
    let workflow_yaml = serde_yaml::to_string(&workflow_json)?;

    // Debug: Print the generated YAML
    println!("=== Generated Workflow YAML ===");
    println!("{}", workflow_yaml);
    println!("================================\n");

    let loader = WorkflowLoader::new(http_client.clone())?;
    let workflow = loader
        .load_workflow(None, Some(&workflow_yaml), false)
        .await?;

    // Initialize WorkflowExecutor
    let executor = WorkflowExecutor::init(
        app_wrapper_module,
        app,
        http_client,
        Arc::new(workflow),
        Arc::new(input_data.clone()),
        None,                     // execution_id
        Arc::new(json!({})),      // context
        Arc::new(HashMap::new()), // metadata
        None,                     // checkpoint
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to initialize WorkflowExecutor: {:?}", e))?;

    // Execute workflow and collect results
    let workflow_stream = executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
    pin_mut!(workflow_stream);

    let mut final_output = None;
    while let Some(wfc_result) = workflow_stream.next().await {
        match wfc_result {
            Ok(wfc) => {
                if let Some(output) = &wfc.output {
                    final_output = Some((**output).clone());
                }
            }
            Err(e) => {
                return Err(anyhow::anyhow!("Workflow execution failed: {:?}", e));
            }
        }
    }

    final_output.ok_or_else(|| anyhow::anyhow!("No output produced by workflow"))
}

/// Helper function to create test workflow JSON
fn create_script_workflow(
    name: &str,
    script_code: &str,
    arguments: serde_json::Value,
    metadata: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut task = json!({
        "run": {
            "script": {
                "language": "python",
                "code": script_code,
                "arguments": arguments
            }
        }
    });

    if let Some(meta) = metadata {
        task["metadata"] = meta;
    }

    let workflow = json!({
        "document": {
            "dsl": "1.0.0",
            "namespace": "test-script-runner",
            "name": name,
            "version": "1.0.0",
            "summary": format!("E2E test workflow: {}", name)
        },
        "input": {
            "from": ".testInput"
        },
        "do": [{
            "ScriptTask": task
        }]
    });

    workflow
}

/// Test 1: Basic inline script execution with Base64-encoded arguments
#[test]
#[ignore = "Requires jobworkerp backend, uv and Python installation"]
fn test_inline_script_with_base64_arguments() -> Result<()> {
    println!("🧪 Test 1: Inline script with Base64-encoded arguments");

    TEST_RUNTIME.block_on(async {
        let app = Arc::new(create_hybrid_test_app().await?);

        let script_code = r#"
import json
import sys

# Arguments should be injected via Base64 decoding
# message and count variables should be directly accessible
print(json.dumps({
    "message": message,
    "count": count,
    "doubled": count * 2
}))
"#;

        let arguments = json!({
            "message": "Hello from Python!",
            "count": 42
        });

        let workflow = create_script_workflow(
            "base64-args-test",
            script_code,
            arguments,
            Some(json!({
                "python.version": "3.12"
            })),
        );

        let result = execute_script_workflow(app, workflow, json!({"testInput": {}})).await?;

        println!("✅ Test 1 execution completed");
        println!("   - Output: {}", serde_json::to_string_pretty(&result)?);

        // Validate output structure
        assert_eq!(result["message"], "Hello from Python!");
        assert_eq!(result["count"], 42);
        assert_eq!(result["doubled"], 84);

        println!("✅ Test 1: Base64 encoding with arguments succeeded");

        Ok(())
    })
}

/// Test 2: Triple-quote injection attack should be blocked via Base64 encoding
#[test]
#[ignore = "Requires jobworkerp backend, uv and Python installation"]
fn test_triple_quote_injection_blocked() -> Result<()> {
    println!("🧪 Test 2: Triple-quote injection attack blocked via Base64");

    TEST_RUNTIME.block_on(async {
        let app = Arc::new(create_hybrid_test_app().await?);

        let script_code = r#"
import json

# This should safely receive a payload with triple quotes as a string
# The payload should NOT break out of Base64 encoding
print(json.dumps({
    "payload_received": payload,
    "payload_length": len(payload),
    "has_triple_quotes": "'''" in payload,
    "execution_safe": True
}))
"#;

        // Payload with triple quotes that would break traditional string escaping
        // This does NOT contain dangerous functions, just demonstrates triple-quote handling
        let triple_quote_payload = r#"'''
This is a test string with triple quotes
that would break naive string escaping.
Code example: print('hello')
'''
"#;

        let arguments = json!({
            "payload": triple_quote_payload
        });

        let workflow = create_script_workflow("injection-test", script_code, arguments, None);

        let result = execute_script_workflow(app, workflow, json!({"testInput": {}})).await?;

        println!("✅ Test 2 execution completed");
        println!("   - Output: {}", serde_json::to_string_pretty(&result)?);

        // Verify the payload was received as string data with triple quotes intact
        assert_eq!(result["execution_safe"], true);
        assert_eq!(result["has_triple_quotes"], true);
        assert!(result["payload_received"].as_str().unwrap().contains("'''"));
        assert!(result["payload_received"]
            .as_str()
            .unwrap()
            .contains("Code example"));

        println!("✅ Test 2: Triple-quote payload was safely handled via Base64 encoding");

        Ok(())
    })
}

/// Test 3: Dangerous function detection in arguments
#[test]
#[ignore = "Requires uv and Python installation"]
fn test_dangerous_function_rejection() -> Result<()> {
    println!("🧪 Test 3: Dangerous function in arguments should be rejected");

    TEST_RUNTIME.block_on(async {
        let app = Arc::new(create_hybrid_test_app().await?);

        let script_code = r#"
import json
print(json.dumps({"result": "ok"}))
"#;

        // Arguments containing dangerous functions
        let dangerous_arguments = json!({
            "cmd": "eval('malicious_code')",
            "system_call": "os.system('ls')"
        });

        let workflow = create_script_workflow(
            "dangerous-func-test",
            script_code,
            dangerous_arguments,
            None,
        );

        let result = execute_script_workflow(app, workflow, json!({"testInput": {}})).await;

        // This should fail with a validation error
        assert!(
            result.is_err(),
            "Expected validation error for dangerous functions"
        );
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("Dangerous")
                || error_msg.contains("eval")
                || error_msg.contains("system"),
            "Error message should mention dangerous functions: {}",
            error_msg
        );

        println!("✅ Test 3: Dangerous functions were correctly rejected");

        Ok(())
    })
}

/// Test 4: External script download with HTTPS validation
#[test]
#[ignore = "Requires network access and valid HTTPS URL"]
fn test_external_script_https_only() -> Result<()> {
    println!("🧪 Test 4: External script HTTPS validation");

    TEST_RUNTIME.block_on(async {
        let app = Arc::new(create_hybrid_test_app().await?);

        // Test case 1: HTTP URL (should fail)
        let http_workflow = json!({
            "document": {
                "dsl": "1.0.0",
                "namespace": "test-script-runner",
                "name": "external-http-test",
                "version": "1.0.0"
            },
            "input": {},
            "do": [{
                "HttpTest": {
                    "run": {
                        "script": {
                            "language": "python",
                            "source": {
                                "endpoint": "http://insecure.com/script.py"
                            },
                            "arguments": {}
                        }
                    }
                }
            }]
        });

        let result = execute_script_workflow(app.clone(), http_workflow, json!({})).await;

        assert!(result.is_err(), "HTTP URL should be rejected");
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("HTTPS") || error_msg.contains("https"),
            "Error should mention HTTPS requirement: {}",
            error_msg
        );

        // Test case 2: file:// URL (should fail)
        let file_workflow = json!({
            "document": {
                "dsl": "1.0.0",
                "namespace": "test-script-runner",
                "name": "external-file-test",
                "version": "1.0.0"
            },
            "input": {},
            "do": [{
                "FileTest": {
                    "run": {
                        "script": {
                            "language": "python",
                            "source": {
                                "endpoint": "file:///etc/passwd"
                            },
                            "arguments": {}
                        }
                    }
                }
            }]
        });

        let result2 = execute_script_workflow(app, file_workflow, json!({})).await;

        assert!(result2.is_err(), "file:// URL should be rejected");

        println!("✅ Test 4: Non-HTTPS URLs were correctly rejected");

        Ok(())
    })
}

/// Test 5: Python package installation via metadata
#[test]
#[ignore = "Requires jobworkerp backend, uv and Python installation"]
fn test_python_package_installation() -> Result<()> {
    println!("🧪 Test 5: Python package installation via metadata");

    TEST_RUNTIME.block_on(async {
        let app = Arc::new(create_hybrid_test_app().await?);

        let script_code = r#"
import json
import requests  # This package should be installed via metadata

# Make a simple request to verify requests is available
response = requests.get("https://httpbin.org/get", timeout=5)
print(json.dumps({
    "requests_available": True,
    "status_code": response.status_code,
    "requests_version": requests.__version__
}))
"#;

        let metadata = json!({
            "python.version": "3.12",
            "python.packages": "requests"
        });

        let workflow = create_script_workflow(
            "package-install-test",
            script_code,
            json!({}),
            Some(metadata),
        );

        let result = execute_script_workflow(app, workflow, json!({"testInput": {}})).await?;

        println!("✅ Test 5 execution completed");
        println!("   - Output: {}", serde_json::to_string_pretty(&result)?);

        assert_eq!(result["requests_available"], true);
        assert_eq!(result["status_code"], 200);

        println!("✅ Test 5: Python package installation succeeded");

        Ok(())
    })
}

/// Test 6: Script execution timeout
#[test]
#[ignore = "Requires uv and Python installation"]
fn test_script_execution_timeout() -> Result<()> {
    println!("🧪 Test 6: Script execution timeout");

    TEST_RUNTIME.block_on(async {
        let app = Arc::new(create_hybrid_test_app().await?);

        // Script that sleeps for 10 seconds
        let long_running_script = r#"
import time
import json

print(json.dumps({"status": "starting"}))
time.sleep(10)  # This should exceed the timeout
print(json.dumps({"status": "completed"}))
"#;

        let workflow = json!({
            "document": {
                "dsl": "1.0.0",
                "namespace": "test-script-runner",
                "name": "timeout-test",
                "version": "1.0.0"
            },
            "input": {},
            "do": [{
                "TimeoutTest": {
                    "timeout": {
                        "after": {
                            "seconds": 2
                        }
                    },
                    "run": {
                        "script": {
                            "language": "python",
                            "code": long_running_script,
                            "arguments": {}
                        }
                    }
                }
            }]
        });

        let result = execute_script_workflow(app, workflow, json!({})).await;

        // This should timeout
        assert!(result.is_err(), "Long-running script should timeout");
        let error_msg = result.unwrap_err().to_string();
        println!("   - Timeout error (expected): {}", error_msg);

        println!("✅ Test 6: Script timeout was correctly enforced");

        Ok(())
    })
}

/// Test 7: use_static pooling behavior
#[test]
#[ignore = "Requires jobworkerp backend, uv and Python installation"]
fn test_use_static_pooling() -> Result<()> {
    println!("🧪 Test 7: use_static pooling behavior");

    TEST_RUNTIME.block_on(async {
        let app = Arc::new(create_hybrid_test_app().await?);

        let script_code = r#"
import json
import time

start_time = time.time()
# Simple computation
result = sum(range(1000))
end_time = time.time()

print(json.dumps({
    "result": result,
    "execution_time_ms": (end_time - start_time) * 1000
}))
"#;

        let metadata_with_pooling = json!({
            "script.use_static": true,
            "python.version": "3.12"
        });

        let workflow = create_script_workflow(
            "pooling-test",
            script_code,
            json!({}),
            Some(metadata_with_pooling),
        );

        // First execution
        let result1 =
            execute_script_workflow(app.clone(), workflow.clone(), json!({"testInput": {}}))
                .await?;

        println!("✅ First execution completed");
        println!("   - Output: {}", serde_json::to_string_pretty(&result1)?);

        // Second execution (should reuse venv)
        let result2 = execute_script_workflow(app, workflow, json!({"testInput": {}})).await?;

        println!("✅ Second execution completed");
        println!("   - Output: {}", serde_json::to_string_pretty(&result2)?);

        // Both should produce the same result
        assert_eq!(result1["result"], result2["result"]);

        println!("✅ Test 7: use_static pooling behavior verified");

        Ok(())
    })
}

/// Test 8: Nested JSON validation
#[test]
#[ignore = "Requires uv and Python installation"]
fn test_nested_json_validation() -> Result<()> {
    println!("🧪 Test 8: Nested JSON validation");

    TEST_RUNTIME.block_on(async {
        let app = Arc::new(create_hybrid_test_app().await?);

        let script_code = r#"
import json

# Access nested data structure
result = {
    "user_name": data["user"]["name"],
    "user_age": data["user"]["age"],
    "items_count": len(data["items"])
}

print(json.dumps(result))
"#;

        // Nested structure with dangerous content
        let nested_arguments = json!({
            "data": {
                "user": {
                    "name": "Alice",
                    "age": 30
                },
                "items": [
                    "item1",
                    "eval('malicious')",  // Should be detected
                    "item3"
                ]
            }
        });

        let workflow = create_script_workflow(
            "nested-validation-test",
            script_code,
            nested_arguments,
            None,
        );

        let result = execute_script_workflow(app, workflow, json!({"testInput": {}})).await;

        // This should fail validation
        assert!(
            result.is_err(),
            "Nested dangerous content should be detected"
        );
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("Dangerous") || error_msg.contains("eval"),
            "Error should mention dangerous content: {}",
            error_msg
        );

        println!("✅ Test 8: Nested dangerous content was correctly detected");

        Ok(())
    })
}

/// Test 9: Maximum nesting depth limit
#[test]
#[ignore = "Requires uv and Python installation"]
fn test_max_nesting_depth_limit() -> Result<()> {
    println!("🧪 Test 9: Maximum nesting depth limit");

    TEST_RUNTIME.block_on(async {
        let app = Arc::new(create_hybrid_test_app().await?);

        // Create deeply nested structure (depth > MAX_RECURSIVE_DEPTH)
        let mut deeply_nested = json!({"level_0": "value"});
        for i in 1..(ScriptTaskExecutor::MAX_RECURSIVE_DEPTH + 1) {
            deeply_nested = json!({
                format!("level_{}", i): deeply_nested
            });
        }

        let script_code = r#"
import json
print(json.dumps({"result": "ok"}))
"#;

        let workflow = create_script_workflow(
            "max-depth-test",
            script_code,
            json!({"data": deeply_nested}),
            None,
        );

        let result = execute_script_workflow(app, workflow, json!({"testInput": {}})).await;

        // This should fail due to max depth
        assert!(
            result.is_err(),
            "Deeply nested structure should exceed max depth"
        );
        let error_msg = result.unwrap_err().to_string();
        assert!(
            error_msg.contains("depth") || error_msg.contains("nest"),
            "Error should mention depth limit: {}",
            error_msg
        );

        println!("✅ Test 9: Maximum nesting depth limit was enforced");

        Ok(())
    })
}

/// Test 10: Error handling and reporting
#[test]
#[ignore = "Requires uv and Python installation"]
fn test_error_handling_and_reporting() -> Result<()> {
    println!("🧪 Test 10: Error handling and reporting");

    TEST_RUNTIME.block_on(async {
        let app = Arc::new(create_hybrid_test_app().await?);

        // Script that raises an exception
        let error_script = r#"
import json

# This will raise a ZeroDivisionError
result = 10 / 0
print(json.dumps({"result": result}))
"#;

        let workflow = create_script_workflow("error-test", error_script, json!({}), None);

        let result = execute_script_workflow(app, workflow, json!({"testInput": {}})).await;

        // This should fail with ZeroDivisionError
        assert!(result.is_err(), "Script with error should fail");
        let error_msg = result.unwrap_err().to_string();
        println!("   - Error message: {}", error_msg);
        assert!(
            error_msg.contains("ZeroDivisionError") || error_msg.contains("division"),
            "Error should mention ZeroDivisionError: {}",
            error_msg
        );

        println!("✅ Test 10: Error handling and reporting verified");

        Ok(())
    })
}

/// Test 11: Python identifier validation
#[test]
#[ignore = "Requires uv and Python installation"]
fn test_python_identifier_validation() -> Result<()> {
    println!("🧪 Test 11: Python identifier validation");

    TEST_RUNTIME.block_on(async {
        let app = Arc::new(create_hybrid_test_app().await?);

        let script_code = r#"
import json
# This should receive valid identifiers
print(json.dumps({"valid": valid_name, "result": "ok"}))
"#;

        // Test case 1: Valid identifier (should succeed)
        let valid_workflow = create_script_workflow(
            "valid-identifier-test",
            script_code,
            json!({"valid_name": "test_value"}),
            None,
        );

        let result1 =
            execute_script_workflow(app.clone(), valid_workflow, json!({"testInput": {}})).await?;

        assert_eq!(result1["valid"], "test_value");
        println!("✅ Valid identifier accepted");

        // Test case 2: Invalid identifier (starts with digit, should fail)
        let invalid_workflow = create_script_workflow(
            "invalid-identifier-test",
            script_code,
            json!({"1invalid": "value"}),
            None,
        );

        let result2 =
            execute_script_workflow(app.clone(), invalid_workflow, json!({"testInput": {}})).await;

        assert!(result2.is_err(), "Invalid identifier should be rejected");
        println!("✅ Invalid identifier rejected");

        // Test case 3: Python keyword (should fail)
        let keyword_workflow = create_script_workflow(
            "keyword-identifier-test",
            script_code,
            json!({"class": "value"}),
            None,
        );

        let result3 =
            execute_script_workflow(app, keyword_workflow, json!({"testInput": {}})).await;

        assert!(result3.is_err(), "Python keyword should be rejected");
        println!("✅ Python keyword rejected");

        println!("✅ Test 11: Python identifier validation verified");

        Ok(())
    })
}

/// Test 12: Concurrent script execution
#[test]
#[ignore = "Requires uv and Python installation"]
fn test_concurrent_script_execution() -> Result<()> {
    println!("🧪 Test 12: Concurrent script execution");

    TEST_RUNTIME.block_on(async {
        let app = Arc::new(create_hybrid_test_app().await?);

        let script_code = r#"
import json
import time
import random

# Simulate some work
time.sleep(random.uniform(0.1, 0.3))
result = worker_id * 2

print(json.dumps({"worker_id": worker_id, "result": result}))
"#;

        // Create multiple workflows with different worker IDs
        let mut tasks = Vec::new();
        for i in 1..=5 {
            let app_clone = app.clone();
            let workflow = create_script_workflow(
                &format!("concurrent-test-{}", i),
                script_code,
                json!({"worker_id": i}),
                None,
            );

            tasks.push(tokio::spawn(async move {
                execute_script_workflow(app_clone, workflow, json!({"testInput": {}})).await
            }));
        }

        // Wait for all tasks to complete
        let results = futures::future::join_all(tasks).await;

        // Verify all executions succeeded
        for (i, result) in results.iter().enumerate() {
            let worker_id = (i + 1) as i64;
            match result {
                Ok(Ok(output)) => {
                    assert_eq!(output["worker_id"], worker_id);
                    assert_eq!(output["result"], worker_id * 2);
                    println!("   - Worker {}: Success", worker_id);
                }
                Ok(Err(e)) => {
                    panic!("Worker {} failed: {:?}", worker_id, e);
                }
                Err(e) => {
                    panic!("Worker {} task panicked: {:?}", worker_id, e);
                }
            }
        }

        println!("✅ Test 12: All 5 concurrent executions succeeded");

        Ok(())
    })
}

/// Integration test setup documentation
#[test]
fn test_e2e_test_setup_instructions() {
    println!("\n📚 Script Runner E2E Test Setup Instructions\n");
    println!("Prerequisites:");
    println!("  1. Install uv: https://github.com/astral-sh/uv");
    println!("     - macOS/Linux: curl -LsSf https://astral.sh/uv/install.sh | sh");
    println!("     - Windows: powershell -c \"irm https://astral.sh/uv/install.ps1 | iex\"");
    println!("  2. Ensure uv is in PATH: uv --version");
    println!("  3. Python 3.8-3.13 should be available via uv");
    println!("\nRun tests:");
    println!("  cargo test --package app-wrapper --test script_runner_e2e_test -- --ignored --test-threads=1");
    println!("\nRun specific test:");
    println!("  cargo test --package app-wrapper test_inline_script_with_base64_arguments -- --ignored --nocapture");
    println!("\nNote:");
    println!("  - Tests are marked with #[ignore] to avoid CI failures");
    println!("  - Use --test-threads=1 to avoid database conflicts");
    println!("  - Use --nocapture to see detailed output");
    println!("\n✅ Test setup instructions displayed");
}
