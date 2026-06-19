//! Integration tests for the workflow `run.script` task (Python).
//!
//! These drive a full workflow through `WorkflowExecutor` with a real backend
//! worker (started by `start_test_worker`), so the PYTHON_COMMAND runner job is
//! actually dispatched and executed. They verify the `return` shaping
//! (stdout/stderr/code/all/none) and `await: false` semantics added for
//! spec-compliant run.script.
//!
//! Run with:
//! ```sh
//! cargo test --package tests-with-worker --test app_wrapper -- \
//!     run_script_workflow_integration_test --ignored --test-threads=1 --nocapture
//! ```
//!
//! Prerequisites:
//! - Redis must be accessible (Hybrid/Scalable mode).
//! - `uv` and Python must be installed (the PYTHON_COMMAND runner uses uv).
//! - Backend worker is started automatically by `start_test_worker`.
#![allow(clippy::uninlined_format_args)]

use anyhow::Result;
use app::module::AppModule;
use app_wrapper::modules::test::create_test_app_wrapper_module;
use app_wrapper::workflow::definition::WorkflowLoader;
use app_wrapper::workflow::execute::workflow::WorkflowExecutor;
use futures::{StreamExt, pin_mut};
use infra_utils::infra::test::TEST_RUNTIME;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tests_with_worker::start_test_worker;

/// Build a single-task workflow YAML that runs a Python script, optionally
/// setting run-level `return` / `await`. The script `code` is inlined as a YAML
/// block scalar so no YAML serializer dependency is needed.
fn script_workflow_yaml(
    name: &str,
    script_code: &str,
    return_: Option<&str>,
    await_: Option<bool>,
) -> String {
    let mut run_opts = String::new();
    if let Some(r) = return_ {
        run_opts.push_str(&format!("        return: {r}\n"));
    }
    if let Some(a) = await_ {
        run_opts.push_str(&format!("        await: {a}\n"));
    }
    // Indent the script body under a literal block scalar (8 spaces).
    let indented_code = script_code
        .lines()
        .map(|l| format!("            {l}"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"document:
  dsl: 1.0.0
  namespace: test-run-script
  name: {name}
  version: 1.0.0
do:
  - ScriptTask:
      run:
{run_opts}        script:
          language: python
          code: |
{indented_code}
          arguments: {{}}
"#
    )
}

/// Load the workflow, run it to completion with a live worker, and return the
/// final task output.
async fn run_script_workflow(
    app_module: Arc<AppModule>,
    workflow_yaml: &str,
    input: serde_json::Value,
) -> Result<serde_json::Value> {
    let app_wrapper_module = Arc::new(create_test_app_wrapper_module(app_module.clone()));

    let loader = WorkflowLoader::new_local_only();
    let workflow = loader
        .load_workflow(None, Some(workflow_yaml), false)
        .await?;

    let executor = WorkflowExecutor::init(
        app_wrapper_module,
        app_module,
        Arc::new(workflow),
        Arc::new(input),
        None,
        Arc::new(json!({})),
        Arc::new(HashMap::new()),
        None,
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to initialize WorkflowExecutor: {:?}", e))?;

    let stream = executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
    pin_mut!(stream);

    let mut final_output = None;
    while let Some(result) = stream.next().await {
        match result {
            Ok(wfc) => {
                if let Some(output) = &wfc.output {
                    final_output = Some((**output).clone());
                }
            }
            Err(e) => return Err(anyhow::anyhow!("Workflow execution failed: {:?}", e)),
        }
    }
    final_output.ok_or_else(|| anyhow::anyhow!("No output produced by workflow"))
}

/// `return: stdout` (the default) returns the raw stdout string and does NOT
/// parse JSON-looking output (guards the dropped JSON-parse default).
#[test]
#[ignore = "Requires Redis backend worker, uv and Python installation"]
fn test_run_script_return_stdout_is_raw_string() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let worker_handle = start_test_worker(app_module.clone()).await?;

        let yaml = script_workflow_yaml("return-stdout", "print('{\"a\": 1}')", None, None);
        let result = run_script_workflow(app_module, &yaml, json!({})).await;

        worker_handle.shutdown().await;
        // Raw stdout incl. trailing newline; NOT parsed into an object.
        assert_eq!(result?, json!("{\"a\": 1}\n"));
        Ok(())
    })
}

/// `return: code` returns the exit code as a number (0 for a successful script).
#[test]
#[ignore = "Requires Redis backend worker, uv and Python installation"]
fn test_run_script_return_code() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let worker_handle = start_test_worker(app_module.clone()).await?;

        let yaml = script_workflow_yaml("return-code", "print('hello')", Some("code"), None);
        let result = run_script_workflow(app_module, &yaml, json!({})).await;

        worker_handle.shutdown().await;
        assert_eq!(result?, json!(0));
        Ok(())
    })
}

/// `return: all` returns the `{code, stdout, stderr}` process result object.
#[test]
#[ignore = "Requires Redis backend worker, uv and Python installation"]
fn test_run_script_return_all() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let worker_handle = start_test_worker(app_module.clone()).await?;

        let code = "import sys\nprint('out')\nprint('err', file=sys.stderr)";
        let yaml = script_workflow_yaml("return-all", code, Some("all"), None);
        let result = run_script_workflow(app_module, &yaml, json!({})).await;

        worker_handle.shutdown().await;
        let result = result?;
        assert_eq!(result["code"], json!(0));
        assert_eq!(result["stdout"].as_str().unwrap().trim(), "out");
        assert!(
            result["stderr"].as_str().unwrap().contains("err"),
            "stderr should contain 'err', got: {:?}",
            result["stderr"]
        );
        Ok(())
    })
}

/// `return: stderr` returns the stderr string.
#[test]
#[ignore = "Requires Redis backend worker, uv and Python installation"]
fn test_run_script_return_stderr() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let worker_handle = start_test_worker(app_module.clone()).await?;

        let code = "import sys\nprint('to stdout')\nprint('to stderr', file=sys.stderr)";
        let yaml = script_workflow_yaml("return-stderr", code, Some("stderr"), None);
        let result = run_script_workflow(app_module, &yaml, json!({})).await;

        worker_handle.shutdown().await;
        let result = result?;
        assert!(
            result.as_str().unwrap().contains("to stderr"),
            "expected stderr content, got: {result:?}"
        );
        Ok(())
    })
}

/// `return: none` passes the task input through unchanged.
#[test]
#[ignore = "Requires Redis backend worker, uv and Python installation"]
fn test_run_script_return_none_passes_input_through() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let worker_handle = start_test_worker(app_module.clone()).await?;

        let yaml = script_workflow_yaml("return-none", "print('ignored')", Some("none"), None);
        let result = run_script_workflow(app_module, &yaml, json!({"keep": 1})).await;

        worker_handle.shutdown().await;
        assert_eq!(result?, json!({"keep": 1}));
        Ok(())
    })
}

/// `await: false` runs fire-and-forget and outputs the current task input
/// instead of awaiting the script result.
#[test]
#[ignore = "Requires Redis backend worker, uv and Python installation"]
fn test_run_script_await_false_fire_and_forget() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let worker_handle = start_test_worker(app_module.clone()).await?;

        let yaml = script_workflow_yaml("await-false", "print('not awaited')", None, Some(false));
        let result = run_script_workflow(app_module, &yaml, json!({"passthrough": true})).await;

        worker_handle.shutdown().await;
        // await:false outputs the transformed task input, not the script output.
        assert_eq!(result?, json!({"passthrough": true}));
        Ok(())
    })
}

/// A non-zero exit fails the task (run.script has no treatNonzeroAsError /
/// successExitCodes) even with `return: code`.
#[test]
#[ignore = "Requires Redis backend worker, uv and Python installation"]
fn test_run_script_nonzero_exit_fails_task() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let worker_handle = start_test_worker(app_module.clone()).await?;

        let yaml = script_workflow_yaml(
            "nonzero-exit",
            "import sys\nsys.exit(3)",
            Some("code"),
            None,
        );
        let result = run_script_workflow(app_module, &yaml, json!({})).await;

        worker_handle.shutdown().await;
        assert!(
            result.is_err(),
            "non-zero exit should fail the task before return is applied"
        );
        Ok(())
    })
}
