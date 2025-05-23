pub mod context;
pub mod expression;
pub mod ext;
pub mod job;
pub mod task;
pub mod workflow;

// execute workflow from yaml definition file

use super::definition::workflow::WorkflowSchema;
use crate::workflow::execute::context::WorkflowContext;
use crate::workflow::execute::workflow::WorkflowExecutor;
use anyhow::Result;
use app::module::AppModule;
use futures::StreamExt;
use infra_utils::infra::net::reqwest::ReqwestClient;
use std::sync::Arc;
use tokio::sync::RwLock;

// enough time for heavy task like llm but not too long
const DEFAULT_REQUEST_TIMEOUT_SEC: u32 = 1200; // 20 minutes

/// Executes a workflow schema.
///
/// This function creates a workflow executor and executes the workflow.
///
/// # Arguments
/// * `app_module` - An Arc reference to an AppModule instance.
/// * `http_client` - A ReqwestClient instance.
/// * `workflow` - An Arc reference to a WorkflowSchema instance.
/// * `workflow_file` - A string representation of the workflow file.
/// * `input` - An Arc reference to a serde_json::Value representing the input to the workflow.
/// * `context` - An Arc reference to a serde_json::Value representing the context of the workflow.
///
/// # Returns
/// A Result containing an Arc<RwLock<WorkflowContext>>.
pub async fn execute(
    app_module: Arc<AppModule>,
    http_client: ReqwestClient,
    workflow: Arc<WorkflowSchema>,
    input: Arc<serde_json::Value>,
    context: Arc<serde_json::Value>,
) -> Result<Arc<RwLock<WorkflowContext>>> {
    let workflow_executor =
        WorkflowExecutor::new(app_module, http_client, workflow.clone(), input, context);

    // Get the stream of workflow context updates
    let mut workflow_stream = workflow_executor.execute_workflow();

    // Store the final workflow context
    let mut final_context = None;

    // Process the stream of workflow context results
    while let Some(result) = workflow_stream.next().await {
        match result {
            Ok(context) => {
                final_context = Some(context);
            }
            Err(e) => {
                return Err(e.context("Failed to execute workflow"));
            }
        }
    }

    // Return the final workflow context or an error if none was received
    final_context.ok_or_else(|| anyhow::anyhow!("No workflow context was returned"))
}
