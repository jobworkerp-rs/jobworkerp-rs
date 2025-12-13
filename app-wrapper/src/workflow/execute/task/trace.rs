use crate::workflow::definition::workflow::Error;
use crate::workflow::execute::context::{TaskContext, WorkflowStreamEvent};
use command_utils::trace::Tracing;
use opentelemetry::trace::SpanRef;
use opentelemetry::KeyValue;

/// Trait for recording task execution traces to OpenTelemetry spans
pub trait TaskTracing: Tracing {
    fn record_task_span(
        span: &mut SpanRef<'_>,
        operation_name: String,
        task: &TaskContext,
        position_json_pointer: String,
    ) {
        Self::record_task_input(span, operation_name, task, position_json_pointer);
    }
    fn record_task_input(
        span: &mut SpanRef,
        operation_name: String,
        task: &TaskContext,
        position_json_pointer: String,
    ) {
        let input_json = serde_json::to_string(&task.input).unwrap_or_default();
        // Record span attributes using OpenTelemetry semantic conventions
        span.set_attributes(vec![
            KeyValue::new("operation.name", operation_name),
            KeyValue::new("workflow.task.input", input_json),
            KeyValue::new("workflow.task.position", position_json_pointer),
        ]);
        // span.record("workflow.context_variables", &context_vars_json);
    }
    fn record_task_output(
        span: &SpanRef<'_>,
        result_task: &TaskContext,
        execution_duration_ms: i64,
    ) {
        let output = &result_task.output;
        let flow_directive = &result_task.flow_directive;

        let output_json = serde_json::to_string(output).unwrap_or_default();

        // Record task execution result to span attributes
        span.set_attributes(vec![
            KeyValue::new("workflow.task.output", output_json.clone()),
            KeyValue::new(
                "workflow.task.flow_directive",
                format!("{flow_directive:?}"),
            ),
            KeyValue::new("workflow.task.duration_ms", execution_duration_ms),
            KeyValue::new("workflow.task.status", "completed"),
        ]);
    }
    #[allow(clippy::borrowed_box)]
    fn record_result(span: &SpanRef<'_>, result: Result<&WorkflowStreamEvent, &Box<Error>>) {
        match result {
            Ok(event) => {
                if let Some(task) = event.context() {
                    span.set_attributes(vec![KeyValue::new(
                        "workflow.task.output",
                        serde_json::to_string(&task.output).unwrap_or_default(),
                    )]);
                }
                span.set_status(opentelemetry::trace::Status::Ok);
            }
            Err(e) => {
                span.set_status(opentelemetry::trace::Status::error(e.to_string()));
                span.set_attribute(KeyValue::new("error", format!("{e:#?}")));
            }
        }
    }
}
