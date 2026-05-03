//! Diagnostic test: verify that the WORKFLOW runner's `run_stream` path emits an OTLP
//! span that reaches the configured collector / Langfuse.
//!
//! Background: `execute_run_stream` previously called `Self::create_context(&metadata)`
//! without ever opening a root span, so child spans created by
//! `execute_workflow_with_events` had no valid parent and the trace was lost. Mirrors
//! the LLM streaming diagnostic tests under `ollama_otel_tracing_test.rs` /
//! `genai_otel_tracing_test.rs`.
//!
//! Requires:
//! - OTLP collector reachable at OTLP_ADDR (forwarding to Langfuse)
//! - jobworkerp-rs backend with the same DB (because the workflow runs a COMMAND task)
//!
//! Run with:
//! ```sh
//! cargo test -p app-wrapper --test workflow_otel_tracing_test \
//!   -- --ignored --nocapture --test-threads=1
//! ```
//!
//! After the test finishes, manually inspect Langfuse for a `workflow.run_stream` trace.
//!
//! NOTE: like the LLM diagnostic tests, this calls `tracing_init` (NOT `tracing_init_test`)
//! so the OTLP exporter is actually initialised, and `shutdown_tracer_provider` is called
//! at the end to flush the BatchSpanProcessor before the process exits.

#![allow(clippy::uninlined_format_args)]

use anyhow::Result;
use app::module::test::create_hybrid_test_app;
use app_wrapper::modules::AppWrapperModule;
use app_wrapper::workflow::runner::unified::WorkflowUnifiedRunnerImpl;
use command_utils::util::tracing::LoggingConfig;
use futures::StreamExt;
use jobworkerp_runner::jobworkerp::runner::workflow_runner_settings::WorkflowSource as SettingsWorkflowSource;
use jobworkerp_runner::jobworkerp::runner::{WorkflowRunArgs, WorkflowRunnerSettings};
use jobworkerp_runner::runner::RunnerTrait;
use prost::Message;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::time::{Duration, timeout};

const OTLP_ADDR: &str = "http://otel-collector.default.svc.cluster.local:4317";
const TEST_TIMEOUT: Duration = Duration::from_secs(120);

async fn init_real_tracing() -> Result<bool> {
    // SAFETY: tests run single-threaded.
    unsafe { std::env::set_var("OTLP_ADDR", OTLP_ADDR) };
    let otlp_set = std::env::var("OTLP_ADDR").is_ok();
    command_utils::util::tracing::tracing_init(LoggingConfig::new()).await?;
    Ok(otlp_set)
}

fn simple_workflow_json() -> String {
    r#"{
        "document": {
            "dsl": "0.0.1",
            "namespace": "test",
            "name": "otel-tracing-stream-workflow",
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
                                "args": ["hi"]
                            }
                        }
                    }
                }
            }
        ]
    }"#
    .to_string()
}

fn workflow_settings(workflow_json: &str) -> Vec<u8> {
    WorkflowRunnerSettings {
        workflow_source: Some(SettingsWorkflowSource::WorkflowData(
            workflow_json.to_string(),
        )),
        workflow_context: None,
    }
    .encode_to_vec()
}

async fn build_runner() -> Result<WorkflowUnifiedRunnerImpl> {
    let app_module = Arc::new(create_hybrid_test_app().await?);
    let app_wrapper_module = Arc::new(AppWrapperModule::new_by_env(None));
    WorkflowUnifiedRunnerImpl::new(app_wrapper_module, app_module)
}

/// Drive `run_stream` once and confirm the call returned. Whether a span actually
/// reached Langfuse must be verified manually in the UI.
#[tokio::test]
#[ignore = "Requires backend with same DB and an OTLP collector. Inspect Langfuse afterwards."]
async fn workflow_run_stream_emits_otlp_span() -> Result<()> {
    let otlp_active = init_real_tracing().await?;
    eprintln!(
        "OTLP exporter active: {} (workflow run_stream)",
        otlp_active
    );

    let mut runner = build_runner().await?;
    runner
        .load(workflow_settings(&simple_workflow_json()))
        .await?;

    let args = WorkflowRunArgs {
        workflow_source: None,
        input: r#"{"testInput": "hi from otel test"}"#.to_string(),
        ..Default::default()
    };
    let args_bytes = args.encode_to_vec();
    let metadata: HashMap<String, String> = HashMap::new();

    // Pass `using = None` so the unified runner resolves to METHOD_RUN and dispatches
    // to execute_run_stream — the path under test.
    let mut stream =
        timeout(TEST_TIMEOUT, runner.run_stream(&args_bytes, metadata, None)).await??;

    let mut chunks: u32 = 0;
    while let Some(_item) = timeout(TEST_TIMEOUT, stream.next()).await? {
        chunks += 1;
    }
    eprintln!("workflow run_stream finished (chunks={})", chunks);

    command_utils::util::tracing::shutdown_tracer_provider();
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok(())
}
