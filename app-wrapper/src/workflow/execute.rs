pub mod context;
pub mod job;
pub mod task;
pub mod workflow;

// execute workflow from yaml definition file

use anyhow::Result;
use app::module::AppModule;
use infra_utils::infra::net::reqwest::ReqwestClient;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use workflow::WorkflowExecutor;

// enough time for heavy task like llm but not too long
const DEFAULT_REQUEST_TIMEOUT_SEC: u32 = 1200; // 20 minutes
const DEFAULT_USER_AGENT: &str = "simple-workflow";

pub async fn execute_workflow(
    app_module: Arc<AppModule>,
    workflow_file: &str,
    input: serde_json::Value,
) -> Result<Arc<RwLock<context::WorkflowContext>>> {
    let http_client = ReqwestClient::new(
        Some(DEFAULT_USER_AGENT),
        Some(Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SEC as u64)),
        Some(2),
    )?;
    let workflow = super::definition::WorkflowLoader::new(http_client.clone())?
        .load_workflow(Some(workflow_file), None)
        .await?;
    tracing::trace!("Workflow: {:#?}", workflow);
    let mut executor = WorkflowExecutor::new(
        app_module,
        http_client,
        Arc::new(workflow),
        Arc::new(input),
        Arc::new(serde_json::Value::Null),
    );
    let res = executor.execute_workflow().await;
    tracing::debug!("Workflow: {}, result: {:#?}", workflow_file, &res);
    tracing::info!("Workflow result: {}", res.read().await.output_string());
    Ok(res)
}
