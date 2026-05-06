use crate::workflow::definition::workflow::Error;
use crate::workflow::execute::context::{TaskContext, WorkflowStreamEvent};
use command_utils::trace::Tracing;
use opentelemetry::KeyValue;
use opentelemetry::trace::SpanRef;

// Span attribute keys. Centralised so renames stay consistent across
// production code and the tests that assert on these keys.
pub(crate) const ATTR_OPERATION_NAME: &str = "operation.name";
pub(crate) const ATTR_TASK_INPUT: &str = "workflow.task.input";
pub(crate) const ATTR_TASK_POSITION: &str = "workflow.task.position";
pub(crate) const ATTR_TASK_OUTPUT: &str = "workflow.task.output";
pub(crate) const ATTR_TASK_FLOW_DIRECTIVE: &str = "workflow.task.flow_directive";
pub(crate) const ATTR_TASK_DURATION_MS: &str = "workflow.task.duration_ms";
pub(crate) const ATTR_TASK_STATUS: &str = "workflow.task.status";
pub(crate) const ATTR_FOR_INDEX: &str = "workflow.task.for.index";
pub(crate) const ATTR_FOR_ITEM: &str = "workflow.task.for.item";

fn json_attr(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

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
        // Skip the JSON serialisation when the span is non-recording (sampled
        // out or no-op tracer). `task.input` can be large (e.g. LLM payloads),
        // so this gate is a meaningful per-task save on the hot path.
        if !span.is_recording() {
            return;
        }
        span.set_attributes(vec![
            KeyValue::new(ATTR_OPERATION_NAME, operation_name),
            KeyValue::new(ATTR_TASK_INPUT, json_attr(&task.input)),
            KeyValue::new(ATTR_TASK_POSITION, position_json_pointer),
        ]);
    }
    /// Record per-iteration metadata on a `for` task's branch span.
    /// `workflow.task.input` is intentionally not set here; the inner do/run
    /// task spans record the real (post-`from`) input themselves.
    fn record_for_item(
        span: &mut SpanRef,
        operation_name: String,
        item: &serde_json::Value,
        index: usize,
        position_json_pointer: String,
    ) {
        if !span.is_recording() {
            return;
        }
        span.set_attributes(vec![
            KeyValue::new(ATTR_OPERATION_NAME, operation_name),
            KeyValue::new(ATTR_TASK_POSITION, position_json_pointer),
            KeyValue::new(ATTR_FOR_INDEX, index as i64),
            KeyValue::new(ATTR_FOR_ITEM, json_attr(item)),
        ]);
    }
    fn record_task_output(
        span: &SpanRef<'_>,
        result_task: &TaskContext,
        execution_duration_ms: i64,
    ) {
        if !span.is_recording() {
            return;
        }
        span.set_attributes(vec![
            KeyValue::new(ATTR_TASK_OUTPUT, json_attr(&result_task.output)),
            KeyValue::new(
                ATTR_TASK_FLOW_DIRECTIVE,
                format!("{:?}", &result_task.flow_directive),
            ),
            KeyValue::new(ATTR_TASK_DURATION_MS, execution_duration_ms),
            KeyValue::new(ATTR_TASK_STATUS, "completed"),
        ]);
    }
    #[allow(clippy::borrowed_box)]
    fn record_result(span: &SpanRef<'_>, result: Result<&WorkflowStreamEvent, &Box<Error>>) {
        match result {
            Ok(event) => {
                if span.is_recording()
                    && let Some(task) = event.context()
                {
                    span.set_attributes(vec![KeyValue::new(
                        ATTR_TASK_OUTPUT,
                        json_attr(&task.output),
                    )]);
                }
                // Deliberately do NOT call set_status(Status::Ok). OpenTelemetry's
                // status order is Ok > Error > Unset, so an Ok set here would
                // mask any Error set earlier in the same span — which happens
                // when an iteration emits an Ok intermediate event before
                // failing, or when an Err is later wrapped into an Ok event by
                // onError=continue. Leaving the status Unset on success is the
                // OTel convention and lets record_error remain authoritative.
            }
            Err(e) => {
                span.set_status(opentelemetry::trace::Status::error(e.to_string()));
                if span.is_recording() {
                    span.set_attribute(KeyValue::new("error", format!("{e:#?}")));
                }
            }
        }
    }
}
