//! Every built-in `collect_stream` must treat a failed End as a failure,
//! whichever representation (typed field, v1 JSON, legacy plugin key) carries it.

use std::collections::HashMap;

use futures::StreamExt;
use futures::stream::BoxStream;
use jobworkerp_runner::runner::RunnerSpec;
use jobworkerp_runner::runner::command::CommandRunnerImpl;
use jobworkerp_runner::runner::grpc::GrpcRunnerSpecImpl;
use jobworkerp_runner::runner::llm::LLMCompletionRunnerSpecImpl;
use jobworkerp_runner::runner::llm_chat::LLMChatRunnerSpecImpl;
use jobworkerp_runner::runner::python::PythonCommandRunner;
use jobworkerp_runner::runner::request::RequestRunner;
use jobworkerp_runner::runner::workflow_unified::WorkflowUnifiedRunnerSpecImpl;
use proto::jobworkerp::data::{ResultOutputItem, StreamErrorCode, Trailer};
use proto::stream_error::{
    LEGACY_PLUGIN_STREAM_ERROR_KEY, STREAM_ERROR_METADATA_KEY, StreamFailedError, end_item,
    error_end_item,
};

fn failed_ends() -> Vec<(&'static str, ResultOutputItem)> {
    let json_only = |value: &str| {
        end_item(Trailer {
            metadata: HashMap::from([(STREAM_ERROR_METADATA_KEY.to_string(), value.to_string())]),
            ..Default::default()
        })
    };
    vec![
        (
            "typed",
            error_end_item(
                HashMap::new(),
                StreamErrorCode::ExecutionFailed,
                "failed",
                "TEST",
            ),
        ),
        (
            "json",
            json_only(r#"{"version":1,"code":"TIMEOUT","message":"idle","origin":"TEST"}"#),
        ),
        ("malformed-json", json_only("not-json")),
        (
            "legacy-plugin-key",
            end_item(Trailer {
                metadata: HashMap::from([(
                    LEGACY_PLUGIN_STREAM_ERROR_KEY.to_string(),
                    "plugin failed".to_string(),
                )]),
                ..Default::default()
            }),
        ),
    ]
}

fn specs() -> Vec<(&'static str, Box<dyn RunnerSpec>, Option<&'static str>)> {
    vec![
        ("COMMAND", Box::new(CommandRunnerImpl::new()), None),
        ("PYTHON_COMMAND", Box::new(PythonCommandRunner::new()), None),
        ("HTTP_REQUEST", Box::new(RequestRunner::new()), None),
        ("GRPC", Box::new(GrpcRunnerSpecImpl::new()), None),
        (
            "LLM completion",
            Box::new(LLMCompletionRunnerSpecImpl::new()),
            None,
        ),
        ("LLM chat", Box::new(LLMChatRunnerSpecImpl::new()), None),
        (
            "WORKFLOW run",
            Box::new(WorkflowUnifiedRunnerSpecImpl::new()),
            Some("run"),
        ),
        (
            "WORKFLOW create",
            Box::new(WorkflowUnifiedRunnerSpecImpl::new()),
            Some("create"),
        ),
    ]
}

#[tokio::test]
async fn collect_stream_rejects_every_failed_end_representation() {
    for (runner, spec, using) in specs() {
        for (kind, end) in failed_ends() {
            let stream: BoxStream<'static, ResultOutputItem> = futures::stream::iter([end]).boxed();
            let error = match spec.collect_stream(stream, using).await {
                Ok(_) => panic!("{runner} accepted a {kind} failed End"),
                Err(error) => error,
            };
            assert!(
                error.downcast_ref::<StreamFailedError>().is_some(),
                "{runner} must surface the stream failure for {kind}: {error:#}"
            );
        }
    }
}
