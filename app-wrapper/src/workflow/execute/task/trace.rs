use crate::workflow::definition::workflow::Error;
use crate::workflow::execute::context::TaskContext;
use infra_utils::infra::trace::Tracing;
use jobworkerp_base::APP_WORKER_NAME;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Trait for recording task execution traces to OpenTelemetry spans
pub trait TaskTracing: Tracing {
    fn create_task_span(
        cx: &opentelemetry::Context,
        span_name: String,
        operation_name: &String,
        task: &TaskContext,
    ) -> (tracing::Span, opentelemetry::Context) {
        let (mut span, ccx) = Self::child_tracing_span(cx, APP_WORKER_NAME, span_name);

        // Use tracing helper to record input information
        Self::record_task_input(&mut span, operation_name, task);

        (span, ccx)
    }
    fn record_task_input(span: &mut tracing::Span, operation_name: &String, task: &TaskContext) {
        let input_json = serde_json::to_string(&task.input).unwrap_or_default();

        // Record span attributes using OpenTelemetry semantic conventions
        span.record("operation.name", operation_name);
        span.record("workflow.task.input", &input_json);
        // span.record("workflow.context_variables", &context_vars_json);
    }
    fn record_task_output(
        span: &tracing::Span,
        result_task: &TaskContext,
        execution_duration_ms: i64,
    ) {
        let output = &result_task.output;
        let flow_directive = &result_task.flow_directive;

        let output_json = serde_json::to_string(output).unwrap_or_default();

        // Record task execution result to span attributes
        span.record("workflow.task.output", &output_json);
        span.record(
            "workflow.task.flow_directive",
            format!("{:?}", flow_directive),
        );
        span.record("workflow.task.duration_ms", execution_duration_ms);
        span.record("workflow.task.status", "completed");
    }
    #[allow(clippy::borrowed_box)]
    fn record_result(span: &tracing::Span, result: Result<&TaskContext, &Box<Error>>) {
        match result {
            Ok(task) => {
                span.record("workflow.task.status", "success");
                span.record(
                    "workflow.task.output",
                    serde_json::to_string(&task.output).unwrap_or_default(),
                );
            }
            Err(e) => {
                span.set_status(opentelemetry::trace::Status::error(e.to_string()));
                span.record("error", format!("{:#?}", e));
            }
        }
    }
}
