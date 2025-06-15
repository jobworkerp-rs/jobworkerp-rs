pub mod checkpoint;
pub mod context;
pub mod expression;
pub mod job;
pub mod task;
pub mod workflow;

// execute workflow from yaml definition file

use super::definition::workflow::WorkflowSchema;
use crate::workflow::execute::context::WorkflowContext;
use crate::workflow::execute::workflow::WorkflowExecutor;
use anyhow::anyhow;
use anyhow::Result;
use app::module::AppModule;
use futures::pin_mut;
use futures::StreamExt;
use infra_utils::infra::net::reqwest::ReqwestClient;
use infra_utils::infra::trace::Tracing;
use opentelemetry::trace::TraceContextExt;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// enough time for heavy task like llm but not too long
const DEFAULT_REQUEST_TIMEOUT_SEC: u32 = 1200; // 20 minutes

struct TracingImpl;
impl infra_utils::infra::trace::Tracing for TracingImpl {}
/// Executes a workflow schema.
///
/// This function creates a workflow executor and executes the workflow.
///
/// # Arguments
/// * `app_module` - An Arc reference to an AppModule instance.
/// * `http_client` - A ReqwestClient instance.
/// * `workflow` - An Arc reference to a WorkflowSchema instance.
/// * `input` - An Arc reference to a serde_json::Value representing the input to the workflow.
/// * `context` - An Arc reference to a serde_json::Value representing the context of the workflow.
/// * `metadata` - A HashMap containing metadata for tracing from request metadata.
///
/// # Returns
/// A Result containing an Arc<RwLock<WorkflowContext>>.
pub async fn execute(
    app_module: Arc<AppModule>,
    http_client: ReqwestClient,
    workflow: Arc<WorkflowSchema>,
    input: Arc<serde_json::Value>,
    context: Arc<serde_json::Value>,
    metadata: HashMap<String, String>,
) -> Result<Arc<RwLock<WorkflowContext>>> {
    // let span =
    //     TracingImpl::tracing_span_from_metadata(&metadata, "workflow-execute", "execute_workflow");
    // let _ = span.enter();
    // let cx = span.context();
    let span =
        TracingImpl::otel_span_from_metadata(&metadata, "workflow-execute", "execute_workflow");
    let cx = opentelemetry::Context::current_with_span(span);

    let workflow_executor = WorkflowExecutor::new(
        app_module,
        http_client,
        workflow.clone(),
        input,
        None, // no checkpointing
        context,
        Arc::new(metadata.clone()),
    )?;
    // Get the stream of workflow context updates
    let workflow_stream = workflow_executor.execute_workflow(Arc::new(cx));
    pin_mut!(workflow_stream);

    // Store the final workflow context
    let mut final_context = None;

    // Process the stream of workflow context results
    while let Some(result) = workflow_stream.next().await {
        match result {
            Ok(context) => {
                final_context = Some(context);
            }
            Err(e) => {
                return Err(anyhow!("Failed to execute workflow: {:?}", e));
            }
        }
    }

    // Return the final workflow context or an error if none was received
    final_context.ok_or_else(|| anyhow::anyhow!("No workflow context was returned"))
}
