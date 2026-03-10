//! Tests that verify the backend worker correctly processes jobs.
//! These tests do not require an LLM server - they directly enqueue
//! COMMAND runner jobs and verify the results.

use anyhow::Result;
use app::app::function::FunctionApp;
use app::app::function::function_set::FunctionSetApp;
use proto::jobworkerp::data::RunnerId;
use proto::jobworkerp::function::data::{FunctionId, FunctionSetData, FunctionUsing, function_id};
use std::collections::HashMap;
use std::sync::Arc;
use tests_with_worker::start_test_worker;
use tokio::time::{Duration, timeout};

/// Test that the backend worker can execute a COMMAND runner job
/// via call_function_for_llm (the same path used by LLM tool calling).
#[tokio::test]
async fn test_worker_executes_command_via_function() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
    let _worker_handle = start_test_worker(app_module.clone()).await?;

    // Wait briefly for dispatcher to be ready
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Create a function set targeting the COMMAND runner (runner_id=1)
    match app_module
        .function_set_app
        .create_function_set(&FunctionSetData {
            name: "worker_backend_test".to_string(),
            description: "Test set for worker backend verification".to_string(),
            category: 0,
            targets: vec![FunctionUsing {
                function_id: Some(FunctionId {
                    id: Some(function_id::Id::RunnerId(RunnerId { value: 1 })),
                }),
                using: None,
            }],
        })
        .await
    {
        Ok(_) => {}
        Err(e) if e.to_string().to_lowercase().contains("unique") => {}
        Err(e) => return Err(e),
    }

    // Call function_for_llm directly - this enqueues a job and waits for the result
    let metadata = Arc::new(HashMap::new());
    let arguments: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({
            "command": "echo",
            "args": ["hello_from_worker_test"]
        }))?;

    let result = timeout(
        Duration::from_secs(30),
        app_module
            .function_app
            .call_function_for_llm(metadata, "COMMAND", Some(arguments), 30),
    )
    .await??;

    let output = result.to_string();
    println!("Function result: {}", output);
    assert!(
        output.contains("hello_from_worker_test"),
        "Output should contain echo text, got: {}",
        output
    );

    println!("test_worker_executes_command_via_function passed");
    Ok(())
}
