//! Integration test: secrets passed in the workflow context's `_secrets` never
//! reach the debug logs, even when a task hands them to a job argument whose
//! name does not look like a credential (a COMMAND `stdin`, a WORKFLOW runner's
//! `workflow_data` setting).
//!
//! Run with:
//! ```sh
//! cargo test --package tests-with-worker --test app_wrapper -- \
//!     workflow_secrets_log_masking_test --ignored --test-threads=1 --nocapture
//! ```
//!
//! Prerequisites: Redis must be accessible (Hybrid/Scalable mode). The backend
//! worker is started automatically by `start_test_worker`.

use anyhow::Result;
use app_wrapper::modules::test::create_test_app_wrapper_module;
use app_wrapper::workflow::definition::WorkflowLoader;
use app_wrapper::workflow::execute::workflow::WorkflowExecutor;
use futures::{StreamExt, pin_mut};
use infra_utils::infra::test::TEST_RUNTIME;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tests_with_worker::start_test_worker;

const TOKEN: &str = "ctx-secret-token-4f9c2a";

const WORKFLOW_YAML: &str = r#"document:
  dsl: 1.0.0
  namespace: test-secrets
  name: secrets-log-masking
  version: 1.0.0
do:
  - useToken:
      run:
        runner:
          name: COMMAND
          arguments:
            command: sh
            args: ["-c", "read token; test -n \"$token\""]
            stdin: '${$_secrets.token}'
            treat_nonzero_as_error: true
  - settingsToken:
      run:
        runner:
          name: WORKFLOW
          settings:
            workflow_data: '${ {document: {dsl: "1.0.0", namespace: "test-secrets", name: "child", version: "1.0.0", summary: $_secrets.token}, "do": [{noop: {set: {done: true}}}]} | tojson }'
          arguments:
            input: "{}"
  - keepToken:
      set:
        header: '${"token " + $_secrets.token}'
      # The exported value lands in the context the following logs print.
      export:
        as: '${ {auth: ("token " + $_secrets.token)} }'
"#;

#[test]
#[ignore = "Requires Redis backend worker"]
fn test_context_secrets_are_masked_in_debug_logs() -> Result<()> {
    let (logs, subscriber) = app::module::test::capture_logs(tracing::Level::DEBUG);

    TEST_RUNTIME.block_on(async {
        // The workflow engine runs in this task, so a thread-local subscriber
        // sees its logs (the worker's own threads are not captured).
        let _guard = tracing::subscriber::set_default(subscriber);
        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
        let worker_handle = start_test_worker(app_module.clone()).await?;

        let workflow = WorkflowLoader::new_local_only()
            .load_workflow(None, Some(WORKFLOW_YAML), false)
            .await?;
        let executor = WorkflowExecutor::init(
            Arc::new(create_test_app_wrapper_module(app_module.clone())),
            app_module,
            Arc::new(workflow),
            Arc::new(json!({})),
            None,
            Arc::new(json!({"_secrets": {"token": TOKEN}})),
            Arc::new(HashMap::new()),
            None,
        )
        .await
        .map_err(|e| anyhow::anyhow!("Failed to initialize WorkflowExecutor: {e:?}"))?;

        let stream = executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
        pin_mut!(stream);
        let mut output = None;
        while let Some(result) = stream.next().await {
            let wfc = result.map_err(|e| anyhow::anyhow!("Workflow execution failed: {e:?}"))?;
            output = wfc.output.clone();
        }
        worker_handle.shutdown().await;

        // The token reached the job and the task output unmasked.
        assert_eq!(
            output.as_deref(),
            Some(&json!({"header": format!("token {TOKEN}")}))
        );
        Ok::<_, anyhow::Error>(())
    })?;

    let logs = logs.contents();
    // Both the engine's and the app layer's argument logs were written.
    assert!(logs.contains("transformed arguments"), "{logs}");
    assert!(logs.contains("job args"), "{logs}");
    assert!(logs.contains("runner settings schema exists"), "{logs}");
    assert!(logs.contains("***REDACTED***"), "{logs}");
    let leaked: Vec<&str> = logs.lines().filter(|line| line.contains(TOKEN)).collect();
    assert!(
        leaked.is_empty(),
        "logs leaked the context secret: {leaked:#?}"
    );
    Ok(())
}
