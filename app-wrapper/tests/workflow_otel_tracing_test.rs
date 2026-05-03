//! Diagnostic test: verify that the WORKFLOW runner's `run_stream` path emits an OTLP
//! span that reaches the configured collector / Langfuse.
//!
//! Background: `execute_run_stream` previously called `Self::create_context(&metadata)`
//! without ever opening a root span, so child spans created by
//! `execute_workflow_with_events` had no valid parent and the trace was lost. Mirrors
//! the LLM streaming diagnostic tests under `ollama_otel_tracing_test.rs` /
//! `genai_otel_tracing_test.rs`.
//!
//! The workflow under test uses only `set` tasks so it can be exercised in-process
//! without a running job worker. Tests that exercise WORKFLOW with COMMAND/job-based
//! tasks need a backend dispatcher (see `tests-with-worker` for that style); we
//! intentionally avoid that here because we only care about whether the streaming
//! tracing path emits a span.
//!
//! Requires:
//! - OTLP collector reachable at OTLP_ADDR (forwarding to Langfuse)
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
use infra_utils::infra::test::TEST_RUNTIME;
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
    // Use a `set` task so the workflow runs entirely in-process without a job
    // dispatcher. Job-based tasks (COMMAND etc.) would block waiting for a backend
    // worker that this diagnostic test does not start, masking whether the streaming
    // tracing wiring itself works.
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
                "echo-set": {
                    "set": {
                        "echoed": "${.testInput}"
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

/// Drive both `run` and `run_stream` once each so we can compare the
/// `langfuse.observation.input` / `.output` attributes attached to their root spans
/// in Langfuse. Combined into a single `#[test]` because `tracing_init` installs a
/// global subscriber that cannot be re-installed in the same process, and
/// `shutdown_tracer_provider` permanently tears the exporter down.
///
/// Uses `TEST_RUNTIME.block_on` (a multi-thread runtime) instead of `#[tokio::test]`
/// because the workflow stream takes RwLock-protected state internally and the
/// default current_thread runtime deadlocks while polling it — see the comment on
/// `infra_utils::infra::test::TEST_RUNTIME` for the details.
#[test]
#[ignore = "Requires an OTLP collector. Inspect Langfuse afterwards."]
fn workflow_emits_otlp_spans_with_io() -> Result<()> {
    TEST_RUNTIME.block_on(async {
        let otlp_active = init_real_tracing().await?;
        eprintln!("OTLP exporter active: {}", otlp_active);

        let mut runner = build_runner().await?;
        runner
            .load(workflow_settings(&simple_workflow_json()))
            .await?;

        // --- non-streaming path ---
        {
            let args = WorkflowRunArgs {
                workflow_source: None,
                input: r#"{"testInput": "hi from otel test (non-stream)"}"#.to_string(),
                ..Default::default()
            };
            let (result, _meta) = timeout(
                TEST_TIMEOUT,
                runner.run(&args.encode_to_vec(), HashMap::new(), None),
            )
            .await?;
            let bytes = result?;
            eprintln!("workflow run returned ({} bytes)", bytes.len());
            assert!(!bytes.is_empty(), "run produced empty output");
        }

        // --- streaming path ---
        {
            let args = WorkflowRunArgs {
                workflow_source: None,
                input: r#"{"testInput": "hi from otel test (stream)"}"#.to_string(),
                ..Default::default()
            };
            let mut stream = timeout(
                TEST_TIMEOUT,
                runner.run_stream(&args.encode_to_vec(), HashMap::new(), None),
            )
            .await??;
            let mut chunks: u32 = 0;
            while let Some(_item) = timeout(TEST_TIMEOUT, stream.next()).await? {
                chunks += 1;
            }
            eprintln!("workflow run_stream finished (chunks={})", chunks);
            assert!(chunks > 0, "stream produced no chunks");
        }

        // BatchSpanProcessor::shutdown drains synchronously, so no extra sleep needed.
        command_utils::util::tracing::shutdown_tracer_provider();
        Ok(())
    })
}
