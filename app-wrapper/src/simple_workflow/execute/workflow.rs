use super::{job::JobExecutorWrapper, task::TaskExecutor};
use crate::simple_workflow::definition::{
    transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
    workflow::{ServerlessWorkflow, Task},
};
use crate::simple_workflow::execute::context::{
    self, TaskContext, Then, UseExpression, WorkflowContext, WorkflowStatus,
};
use anyhow::Result;
use app::module::AppModule;
use indexmap::IndexMap;
use infra_utils::infra::net::reqwest::ReqwestClient;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, Clone)]
pub struct WorkflowExecutor {
    pub job_executors: Arc<JobExecutorWrapper>,
    pub http_client: ReqwestClient,
    pub workflow: ServerlessWorkflow,
    pub workflow_context: Arc<RwLock<context::WorkflowContext>>,
}
impl UseJqAndTemplateTransformer for WorkflowExecutor {}
impl UseExpressionTransformer for WorkflowExecutor {}
impl UseExpression for WorkflowExecutor {}

impl WorkflowExecutor {
    pub fn new(
        app_module: Arc<AppModule>,
        http_client: ReqwestClient,
        workflow: ServerlessWorkflow,
        input: Arc<serde_json::Value>,
        context: Arc<serde_json::Value>,
    ) -> Self {
        let workflow_context = Arc::new(RwLock::new(context::WorkflowContext::new(
            &workflow, input, context,
        )));
        let job_executors = Arc::new(JobExecutorWrapper::new(app_module));
        Self {
            job_executors,
            http_client,
            workflow,
            workflow_context,
        }
    }

    /// Executes the workflow.
    ///
    /// This function sets the workflow status to running, validates the input schema,
    /// transforms the input, executes the tasks, and updates the workflow context with the output.
    ///
    /// # Returns
    /// An `Arc<RwLock<WorkflowContext>>` containing the updated workflow context.
    pub async fn execute_workflow(&mut self) -> Arc<RwLock<WorkflowContext>> {
        // execute workflow
        self.workflow_context.write().await.status = WorkflowStatus::Running;
        let lock = self.workflow_context.read().await;
        let input = lock.input.clone();
        drop(lock);

        if let Some(schema) = self.workflow.input.as_ref().and_then(|i| i.schema.as_ref()) {
            if let Some(schema) = schema.json_schema() {
                match jsonschema::validate(schema, &input).map_err(|e| {
                    anyhow::anyhow!("Failed to validate workflow input schema: {:#?}", e)
                }) {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::debug!("Failed to validate workflow input schema: {:#?}", e);
                        let mut wf = self.workflow_context.write().await;
                        wf.status = WorkflowStatus::Faulted;
                        wf.output = Some(Arc::new(serde_json::json!({"error": e.to_string()})));
                        drop(wf);
                        return self.workflow_context.clone();
                    }
                }
            }
        }
        let task_map = Arc::new(self.workflow.do_.0.iter().fold(
            IndexMap::<String, Arc<Task>>::new(),
            |mut acc, task| {
                task.iter().for_each(|(name, t)| {
                    acc.insert(name.clone(), Arc::new(t.clone()));
                });
                acc
            },
        ));
        let mut task_context = TaskContext::new(
            None,
            input.clone(),
            Arc::new(Mutex::new(serde_json::Map::new())),
        );

        let wfr = self.workflow_context.read().await;
        let expression = match self.expression(&wfr, Arc::new(task_context.clone())).await {
            Ok(e) => {
                drop(wfr);
                e
            }
            Err(e) => {
                drop(wfr);
                tracing::debug!("Failed to create expression: {:#?}", e);
                let mut wf = self.workflow_context.write().await;
                wf.status = WorkflowStatus::Faulted;
                wf.output = Some(Arc::new(serde_json::json!({"error": e.to_string()})));
                drop(wf);
                return self.workflow_context.clone();
            }
        };

        // Transform input
        let transformed_input =
            if let Some(from) = self.workflow.input.as_ref().and_then(|i| i.from.as_ref()) {
                match Self::transform_input(input.clone(), from, &expression) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!("Failed to transform input: {:#?}", e);
                        let mut wf = self.workflow_context.write().await;
                        wf.status = WorkflowStatus::Faulted;
                        wf.output = Some(Arc::new(serde_json::json!({"error": e.to_string()})));
                        drop(wf);
                        return self.workflow_context.clone();
                    }
                }
            } else {
                input.clone()
            };
        task_context.set_input(transformed_input);
        // Execute tasks
        match self.execute_task_list(task_map.clone(), task_context).await {
            Ok(tc) => {
                // XXX validate by output schema?
                // transform output
                let out = if let Some(as_) =
                    self.workflow.output.as_ref().and_then(|o| o.as_.as_ref())
                {
                    match Self::transform_output(tc.raw_output.clone(), as_, &expression) {
                        Ok(v) => Some(v),
                        Err(e) => {
                            tracing::debug!("Failed to transform output: {:#?}", e);
                            let mut wf = self.workflow_context.write().await;
                            wf.status = WorkflowStatus::Faulted;
                            wf.output = Some(Arc::new(serde_json::json!({"error": e.to_string()})));
                            drop(wf);
                            return self.workflow_context.clone();
                        }
                    }
                } else {
                    Some(tc.raw_output.clone())
                };
                let mut wf = self.workflow_context.write().await;
                wf.output = out;
                wf.status = WorkflowStatus::Completed;
            }
            Err(e) => {
                tracing::debug!("Failed to execute task list: {:#?}", e);
                let mut wf = self.workflow_context.write().await;
                wf.status = WorkflowStatus::Faulted;
                wf.output = Some(Arc::new(serde_json::json!({"error": e.to_string()})));
                drop(wf);
                return self.workflow_context.clone();
            }
        };
        // tracing log for workflow status
        let lock = self.workflow_context.read().await;
        match lock.status {
            WorkflowStatus::Completed | WorkflowStatus::Running => {
                tracing::info!(
                    "Workflow completed: id={}, doc={:#?}",
                    &lock.id,
                    &lock.definition.document
                );
            }
            WorkflowStatus::Faulted => {
                tracing::error!(
                    "Workflow faulted: id={}, doc={:#?}",
                    lock.id,
                    lock.definition.document
                );
            }
            WorkflowStatus::Cancelled => {
                // TODO: cancel workflow
                tracing::warn!(
                    "Workflow canceled: id={}, doc={:#?}",
                    lock.id,
                    lock.definition.document
                );
            }
            WorkflowStatus::Pending | WorkflowStatus::Waiting => {
                tracing::warn!(
                    "Workflow is ended in waiting yet: id={}, doc={:#?}",
                    lock.id,
                    lock.definition.document
                );
            }
        }
        drop(lock);
        self.workflow_context.clone()
    }

    async fn execute_task_list(
        &self,
        task_map: Arc<IndexMap<String, Arc<Task>>>,
        parent_task_context: TaskContext,
    ) -> Result<Arc<TaskContext>> {
        let mut prev_context = Arc::new(parent_task_context);

        let mut task_iterator = task_map.iter();
        let mut next_task_pair = task_iterator.next();
        let lock = self.workflow_context.read().await;
        let mut status = lock.status.clone();
        drop(lock);
        while next_task_pair.is_some() && status == WorkflowStatus::Running {
            if let Some((name, task)) = next_task_pair {
                // parent_task.position().addIndex(iter.previousIndex());
                tracing::info!("Executing task: {}: input={:#}", name, &prev_context.output);
                let task_executor = TaskExecutor::new(
                    self.job_executors.clone(),
                    self.http_client.clone(),
                    name,
                    task.clone(),
                );
                let result_task_context = task_executor
                    .execute(self.workflow_context.clone(), prev_context.clone())
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to execute task: {:#?}", e))?;
                let flow_directive = &result_task_context.flow_directive;
                match &flow_directive {
                    Then::Continue => {
                        prev_context = result_task_context;
                        next_task_pair = task_iterator.next();
                        tracing::info!(
                            "Task Continue next: {}",
                            next_task_pair.map(|p| p.0).unwrap_or(&"".to_string()),
                        );
                    }
                    Then::End => {
                        prev_context = result_task_context;
                        self.workflow_context.write().await.status = WorkflowStatus::Completed;
                        next_task_pair = None;
                    }
                    Then::Exit => {
                        prev_context = result_task_context;
                        next_task_pair = None;
                        tracing::info!("Exit Task: {}", name);
                    }
                    // TODO test (not used yet)
                    Then::TaskName(tname) => {
                        let mut it = task_map.iter();
                        for (k, v) in it.by_ref() {
                            if k == tname {
                                next_task_pair = Some((k, v));
                                tracing::info!("Jump to Task: {}", k);
                                break;
                            }
                        }
                        task_iterator = it;
                    }
                }
                let lock = self.workflow_context.read().await;
                status = lock.status.clone();
                drop(lock);
            } else {
                break;
            }
        }
        Ok(prev_context)
    }
    /// Cancels the workflow if it is running, pending, or waiting.
    ///
    /// This function sets the workflow status to `Cancelled` and logs the cancellation.
    pub async fn cancel(&self) {
        let mut lock = self.workflow_context.write().await;
        tracing::info!(
            "Cancel workflow: {}, id={}",
            self.workflow.document.name.to_string(),
            self.workflow_context.read().await.id.to_string()
        );
        if lock.status == WorkflowStatus::Running
            || lock.status == WorkflowStatus::Pending
            || lock.status == WorkflowStatus::Waiting
        {
            lock.status = WorkflowStatus::Cancelled;
        }
    }
}
