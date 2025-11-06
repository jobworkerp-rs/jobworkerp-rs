use super::{expression::UseExpression, task::TaskExecutor};
use crate::modules::AppWrapperModule;
use crate::workflow::definition::{
    transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
    workflow::{self, Task, WorkflowSchema},
};
use crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId;
use crate::workflow::execute::checkpoint::CheckPointContext;
use crate::workflow::execute::context::{
    self, TaskContext, WorkflowContext, WorkflowPosition, WorkflowStatus,
};
use crate::workflow::execute::task::ExecutionId;
use anyhow::Result;
use app::app::job::execute::JobExecutorWrapper;
use app::module::AppModule;
use async_stream::stream;
use command_utils::trace::Tracing;
use futures::{Stream, StreamExt};
use jobworkerp_base::APP_WORKER_NAME;
use opentelemetry::{
    trace::{SpanRef, TraceContextExt},
    Context,
};
use proto::jobworkerp::data::StorageType;
use std::time::Duration;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, Clone)]
pub struct WorkflowExecutor {
    pub default_task_timeout_sec: u64,
    pub job_executors: Arc<JobExecutorWrapper>,
    pub workflow: Arc<WorkflowSchema>,
    pub workflow_context: Arc<RwLock<context::WorkflowContext>>,
    pub execution_id: Option<Arc<ExecutionId>>,
    pub metadata: Arc<HashMap<String, String>>,
    pub checkpoint_repository: Option<Arc<dyn CheckPointRepositoryWithId>>,
}
impl UseJqAndTemplateTransformer for WorkflowExecutor {}
impl UseExpressionTransformer for WorkflowExecutor {}
impl UseExpression for WorkflowExecutor {}
impl Tracing for WorkflowExecutor {}

const DEFAULT_TASK_TIMEOUT_SEC: u64 = 1200; // 20 minutes
const ROOT_TASK_NAME: &str = "ROOT";

impl WorkflowTracing for WorkflowExecutor {}

impl WorkflowExecutor {
    // Initializes a new WorkflowExecutor instance. (with update checkpoint context)
    #[allow(clippy::too_many_arguments)]
    pub async fn init(
        app_wrapper_module: Arc<AppWrapperModule>,
        app_module: Arc<AppModule>,
        workflow: Arc<WorkflowSchema>,
        input: Arc<serde_json::Value>,
        execution_id: Option<ExecutionId>,
        context: Arc<serde_json::Value>,
        metadata: Arc<HashMap<String, String>>,
        checkpoint: Option<CheckPointContext>,
    ) -> Result<Self, Box<workflow::Error>> {
        let checkpoint_repository: Option<Arc<dyn CheckPointRepositoryWithId>> =
            if let Some(checkpointing) = &workflow.checkpointing {
                let workflow::CheckpointConfig { enabled, storage } = checkpointing;
                if *enabled {
                    match storage {
                        &Some(workflow::CheckpointConfigStorage::Redis)
                            if app_module.config_module.storage_type() == StorageType::Scalable =>
                        {
                            app_wrapper_module
                                .repositories
                                .redis_checkpoint_repository
                                .clone()
                        }
                        _ => Some(
                            app_wrapper_module
                                .repositories
                                .memory_checkpoint_repository
                                .clone(),
                        ),
                    }
                } else {
                    None
                }
            } else {
                None
            };

        // update checkpoint context if provided, and extract the position
        if let (Some(chpoint), Some(execution_id), Some(repo)) = (
            &checkpoint,
            execution_id.as_ref(),
            checkpoint_repository.as_ref(),
        ) {
            repo.save_checkpoint_with_id(
                execution_id,
                &workflow.document.name.to_string(),
                chpoint,
            )
            .await
            .map_err(|e| {
                tracing::error!("Failed to save checkpoint: {:#?}", e);
                workflow::errors::ErrorFactory::new().service_unavailable(
                    format!("Failed to execute by jobworkerp: {e:?}"),
                    Some(
                        WorkflowPosition::new(vec![
                            serde_json::to_value(ROOT_TASK_NAME).unwrap_or(serde_json::Value::Null)
                        ])
                        .as_error_instance(),
                    ),
                    Some(format!("{e:?}")),
                )
            })?;
        } else {
            tracing::debug!("Necessary info is not provided, ignore checkpointing: execution_id={:?}, checkpoint={:?}, repository={:?}",
                execution_id, checkpoint, checkpoint_repository);
        }
        let workflow_context = Arc::new(RwLock::new(context::WorkflowContext::new(
            &workflow,
            input,
            context,
            checkpoint.map(|cp| cp.position),
        )));

        let job_executors = Arc::new(JobExecutorWrapper::new(app_module));
        // let checkpoint_epository = workflow.;
        Ok(Self {
            default_task_timeout_sec: app_wrapper_module
                .config_module
                .workflow_config
                .task_default_timeout_sec
                .unwrap_or(DEFAULT_TASK_TIMEOUT_SEC),
            job_executors,
            workflow,
            workflow_context,
            execution_id: execution_id.map(Arc::new),
            metadata,
            checkpoint_repository,
        })
    }
    pub async fn update_checkpoint_context(
        &self,
        execution_id: &ExecutionId,
        checkpoint: &CheckPointContext,
    ) -> Result<()> {
        if let Some(repo) = self.checkpoint_repository.as_ref() {
            repo.save_checkpoint_with_id(
                execution_id,
                &self.workflow.document.name.to_string(),
                checkpoint,
            )
            .await?;
        }
        Ok(())
    }
    /// Executes the workflow.
    ///
    /// This function sets the workflow status to running, validates the input schema,
    /// transforms the input, executes the tasks, and provides a stream of workflow contexts
    /// updated after each task execution.
    ///
    /// # Returns
    /// A `Stream<Item = Result<Arc<RwLock<WorkflowContext>>>>` containing the updated workflow context
    /// after each task execution.
    pub fn execute_workflow(
        &self,
        cx: Arc<opentelemetry::Context>,
    ) -> impl Stream<Item = Result<Arc<WorkflowContext>, Box<workflow::Error>>> + 'static {
        let initial_wfc = self.workflow_context.clone();
        let workflow = self.workflow.clone();
        let job_executors = self.job_executors.clone();
        let cxc = cx.clone();
        let metadata = self.metadata.clone();
        let execution_id = self.execution_id.clone();
        let default_task_timeout = Duration::from_secs(self.default_task_timeout_sec);
        let checkpoint_repository = self.checkpoint_repository.clone();

        stream! {
            // Set workflow to running status
            {
                let mut lock = initial_wfc.write().await;
                lock.status = WorkflowStatus::Running;
            }
            let span =
                Self::start_child_otel_span(&cxc, APP_WORKER_NAME, "execute_workflow".to_string());
            let ccx = Context::current_with_span(span);
            let span = ccx.span();
            let ccx = Arc::new(ccx.clone());
            tracing::debug!(
                "Executing workflow: {}, id={}",
                workflow.document.name.as_str(),
                initial_wfc.read().await.id.to_string()
            );
            let checkpoint_position = initial_wfc
                .read()
                .await
                .checkpoint_position
                .clone();
            let cp_task_context = if let (Some(execution_id), Some(checkpoint_repository), Some(pos)) =
              (&execution_id, &checkpoint_repository, &checkpoint_position)
            {
                tracing::debug!(
                    "Loading checkpoint for workflow: {}, execution_id={}",
                    workflow.document.name.as_str(),
                    execution_id.value
                );
                // Load checkpoint if available
                let workflow_name = workflow.document.name.to_string();
                match checkpoint_repository.get_checkpoint_with_id(
                    execution_id,
                    &workflow_name,
                    &pos.as_json_pointer(),
                ).await {
                    Ok(Some(checkpoint)) => {
                        tracing::debug!("Loaded checkpoint for workflow: {}, execution_id={}", &workflow_name, &execution_id.value);
                        let mut lock = initial_wfc.write().await;
                        lock.position = checkpoint.position.clone();
                        lock.input = checkpoint.workflow.input.clone();
                        lock.context_variables = Arc::new(Mutex::new((*checkpoint.workflow.context_variables).clone()));
                        drop(lock);
                        Some(TaskContext::new_from_cp(
                            None,
                            &checkpoint.task
                        ))
                    }
                    Ok(None) => {
                        tracing::warn!("No checkpoint found for workflow: {}, execution_id={}", &workflow_name, &execution_id.value);
                        None
                    }
                    Err(e) => {
                        tracing::error!("Failed to load checkpoint: {:#?}", e);
                        // yield Err(Box::new(e));
                yield Err(workflow::errors::ErrorFactory::new().service_unavailable(
                    format!("Failed to load checkpoint from execution_id: {}, workflow: {}, position: {}, error: {:#?}",
                      execution_id.value, workflow_name, &pos.as_json_pointer(), e),
                    Some(
                        WorkflowPosition::new(vec![
                            serde_json::to_value(ROOT_TASK_NAME).unwrap_or(serde_json::Value::Null)
                        ])
                        .as_error_instance(),
                    ),
                    Some(format!("{e:?}")),
                ));

                        return;
                    }
                }
            } else {
                tracing::debug!(
                    "No checkpoint available for workflow: {}, execution_id={:?}",
                    workflow.document.name.as_str(),
                    execution_id
                );
                // normal execution without checkpoint
                None
            };

            tracing::debug!(
                "Workflow context initialized: {}, id={}",
                workflow.document.name.as_str(),
                initial_wfc.read().await.id.to_string()
            );
            let input = {
                let lock = initial_wfc.read().await;
                // Record workflow metadata and input
                Self::record_workflow_metadata(&span, &workflow, &lock.id.to_string());
                Self::record_workflow_input(&span, &lock.input);
                lock.input.clone()
            };

            // Validate input schema and apply defaults
            let input_with_defaults = if let Some(schema) = workflow.input.schema.as_ref() {
                if let Some(schema_doc) = schema.json_schema() {
                    // First, validate the input
                    match jsonschema::validate(schema_doc, &input).map_err(|e| {
                        anyhow::anyhow!("Failed to validate workflow input schema: {:#?}", e)
                    }) {
                        Ok(_) => {}
                        Err(e) => {
                            tracing::debug!("Failed to validate workflow input schema: {:#?}", e);
                            let mut wf = initial_wfc.write().await;
                            wf.status = WorkflowStatus::Faulted;
                            let error_output =
                                Arc::new(serde_json::json!({"error": e.to_string()}));
                            Self::record_workflow_output(&span, &error_output, &wf.status);
                            wf.output = Some(error_output);
                            // hard copy
                            let res = Arc::new((*wf).clone());
                            drop(wf);
                            yield Ok(res);
                            return;
                        }
                    }

                    // After validation, apply default values from schema
                    match Self::apply_schema_defaults((*input).clone(), schema_doc) {
                        Ok(merged) => {
                            tracing::debug!("Schema defaults applied successfully");
                            let input_with_defaults = Arc::new(merged);
                            // Update workflow_context.input with defaults applied
                            // This ensures defaults are reflected in checkpoint saves
                            initial_wfc.write().await.input = input_with_defaults.clone();
                            input_with_defaults
                        }
                        Err(e) => {
                            // Log warning and continue with original input
                            // Schema validation already passed, so input is valid
                            tracing::warn!(
                                "Failed to apply schema defaults, continuing with original input: {:#?}",
                                e
                            );
                            input.clone()
                        }
                    }
                } else {
                    input.clone()
                }
            } else {
                input.clone()
            };
            tracing::debug!(
                "Workflow input validated and defaults applied: {}, id={}",
                workflow.document.name.as_str(),
                initial_wfc.read().await.id.to_string()
            );

            let mut task_context = cp_task_context.unwrap_or(TaskContext::new(
                None,
                input_with_defaults.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            ));

            let wfr = initial_wfc.read().await;
            let expression_result =
                WorkflowExecutor::expression(&wfr, Arc::new(task_context.clone())).await;
            drop(wfr);

            let expression = match expression_result {
                Ok(e) => e,
                Err(e) => {
                    tracing::debug!("Failed to create expression: {:#?}", e);
                    let mut wf = initial_wfc.write().await;
                    wf.status = WorkflowStatus::Faulted;
                    let error_output = Arc::new(serde_json::json!({"error": e.to_string()}));
                    Self::record_workflow_output(&span, &error_output, &wf.status);
                    wf.output = Some(error_output);
                    // hard copy
                    let res = Arc::new((*wf).clone());
                    drop(wf);
                    yield Ok(res);
                    return;
                }
            };
            tracing::debug!(
                "Expression created for workflow: {}, id={}",
                workflow.document.name.as_str(),
                initial_wfc.read().await.id.to_string()
            );

            // Transform input (using input with defaults applied)
            let transformed_input = if let Some(from) = workflow.input.from.as_ref() {
                match WorkflowExecutor::transform_input(input_with_defaults.clone(), from, &expression) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!("Failed to transform input: {:#?}", e);
                        let mut wf = initial_wfc.write().await;
                        wf.status = WorkflowStatus::Faulted;
                        let error_output = Arc::new(serde_json::json!({"error": e.to_string()}));
                        Self::record_workflow_output(&span, &error_output, &wf.status);
                        wf.output = Some(error_output);
                        // hard copy
                        let res = Arc::new((*wf).clone());
                        drop(wf);
                        yield Ok(res);
                        return;
                    }
                }
            } else {
                input_with_defaults.clone()
            };
            task_context.set_input(transformed_input);


            tracing::debug!(
                "Input transformed for workflow: {}, id={}",
                workflow.document.name.as_str(),
                initial_wfc.read().await.id.to_string()
            );
            // run workflow tasks as do_task
            let task_executor = TaskExecutor::new(
                initial_wfc.clone(),
                default_task_timeout,
                job_executors,
                checkpoint_repository.clone(),
                ROOT_TASK_NAME,
                Arc::new(Task::DoTask(workflow.create_do_task(metadata.clone()))),
                metadata.clone(),
            );
            let mut task_stream = task_executor
                .execute(ccx, Arc::new(task_context), execution_id.clone())
                .await;

            tracing::debug!(
                "Task stream created for workflow: {}, id={}",
                workflow.document.name.as_str(),
                initial_wfc.read().await.id.to_string()
            );
            while let Some(tc_result) = task_stream.next().await {
                match tc_result {
                    Ok(tc) => {
                        // Update and yield workflow context
                        let mut wf = initial_wfc.write().await;
                        wf.output = Some(tc.output.clone());
                        wf.position = tc.position.read().await.clone(); // XXX clone
                        // hard copy
                        let res = Arc::new((*wf).clone());
                        drop(wf);
                        tracing::debug!(
                            "Task executed: {}, id={}, position={}",
                            workflow.document.name.as_str(),
                            res.id.to_string(),
                            tc.position.read().await
                        );
                        yield Ok(res);
                    }
                    Err(e) => {
                        tracing::debug!("Failed to execute task list: {:#?}", e);
                        let mut wf = initial_wfc.write().await;
                        wf.status = WorkflowStatus::Faulted;
                        let error_output = Arc::new(serde_json::json!({"error": e.to_string()}));
                        Self::record_workflow_output(&span, &error_output, &wf.status);
                        wf.output = Some(error_output);
                        drop(wf);
                        Self::record_error(&span, &e.to_string());
                        yield Err(e);
                    }
                }
            }

            // Process final task context for output transformation
            let lock = initial_wfc.read().await;
            if lock.status == WorkflowStatus::Running {
                drop(lock);

                // Transform output if specified
                if let Some(output) = workflow.output.as_ref() {
                    if let Some(as_) = output.as_.as_ref() {
                        let wfr = initial_wfc.read().await;
                        let expression_result = WorkflowExecutor::expression(
                            &wfr,
                            Arc::new(TaskContext::new(
                                None,
                                wfr.input.clone(),
                                Arc::new(Mutex::new(serde_json::Map::new())),
                            )),
                        )
                        .await;
                        drop(wfr);

                        if let Ok(expression) = expression_result {
                            let mut lock = initial_wfc.write().await;
                            if let Some(output_value) = lock.output.clone() {
                                match WorkflowExecutor::transform_output(
                                    output_value,
                                    as_,
                                    &expression,
                                ) {
                                    Ok(transformed_output) => {
                                        lock.output = Some(transformed_output);
                                    }
                                    Err(e) => {
                                        tracing::debug!("Failed to transform output: {:#?}", e);
                                        lock.status = WorkflowStatus::Faulted;
                                        let error_output =
                                            Arc::new(serde_json::json!({"error": e.to_string()}));
                                        Self::record_workflow_output(
                                            &span,
                                            &error_output,
                                            &lock.status,
                                        );
                                        lock.output = Some(error_output);
                                    }
                                }
                            }
                        }
                    }
                }
                tracing::debug!(
                    "Final output transformed for workflow: {}, id={}",
                    workflow.document.name.as_str(),
                    initial_wfc.read().await.id.to_string()
                );

                let mut lock = initial_wfc.write().await;
                // Mark workflow as completed if it's still running
                if lock.status == WorkflowStatus::Running {
                    lock.status = WorkflowStatus::Completed;
                }

                // Record final output
                if let Some(output) = lock.output.as_ref() {
                    Self::record_workflow_output(&span, output, &lock.status);
                }
                // hard copy
                let res = Arc::new((*lock).clone());

                drop(lock);

                // Yield final workflow context
                yield Ok(res);
            }
            tracing::debug!(
                "Workflow execution completed: {}, id={}",
                workflow.document.name.as_str(),
                initial_wfc.read().await.id.to_string()
            );

            // Log workflow status
            let lock = initial_wfc.read().await;
            match lock.status {
                WorkflowStatus::Completed | WorkflowStatus::Running => {
                    tracing::info!(
                        "Workflow completed: id={}, doc={:#?}",
                        &lock.id,
                        &lock.document
                    );
                }
                WorkflowStatus::Faulted => {
                    tracing::error!(
                        "Workflow faulted: id={}, doc={:#?}",
                        lock.id,
                        lock.document
                    );
                }
                WorkflowStatus::Cancelled => {
                    tracing::warn!(
                        "Workflow canceled: id={}, doc={:#?}",
                        lock.id,
                        lock.document
                    );
                }
                WorkflowStatus::Pending | WorkflowStatus::Waiting => {
                    tracing::warn!(
                        "Workflow is ended in waiting yet: id={}, doc={:#?}",
                        lock.id,
                        lock.document
                    );
                }
            }
        }
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

    /// Apply default values from JSON Schema to input.
    ///
    /// This function recursively merges default values defined in the schema
    /// into the input JSON value. User-provided values take precedence over defaults.
    ///
    /// # Arguments
    /// * `input` - The input JSON value to merge defaults into
    /// * `schema` - The JSON Schema containing default value definitions
    ///
    /// # Returns
    /// A new JSON value with defaults applied, or an error if the operation fails
    fn apply_schema_defaults(
        input: serde_json::Value,
        schema: &serde_json::Value,
    ) -> Result<serde_json::Value, anyhow::Error> {
        // Only process object inputs
        let mut input_obj = match input {
            serde_json::Value::Object(obj) => obj,
            other => return Ok(other),
        };

        // Get properties from schema
        let properties = match schema.get("properties").and_then(|p| p.as_object()) {
            Some(props) => props,
            None => return Ok(serde_json::Value::Object(input_obj)),
        };

        // Process each property in the schema
        for (key, prop_schema) in properties.iter() {
            if let Some(input_val) = input_obj.get_mut(key) {
                // Value exists in input - check if it's a nested object
                if input_val.is_object() && prop_schema.get("properties").is_some() {
                    // Recursively apply defaults to nested object
                    *input_val = Self::apply_schema_defaults(input_val.clone(), prop_schema)?;
                }
            } else {
                // Value doesn't exist - apply default if available
                if let Some(default_val) = prop_schema.get("default") {
                    input_obj.insert(key.clone(), default_val.clone());
                }
            }
        }

        Ok(serde_json::Value::Object(input_obj))
    }
}

pub trait WorkflowTracing {
    /// Records workflow input to the current span using OpenTelemetry semantic conventions
    fn record_workflow_input(span: &SpanRef, input: &serde_json::Value) {
        // Record input as pretty-printed JSON - Jaeger will parse and display it nicely
        let input_str = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
        span.set_attribute(opentelemetry::KeyValue::new(
            "input.value",
            input_str.clone(),
        ));
        span.set_attribute(opentelemetry::KeyValue::new(
            "input.size",
            input_str.len() as i64,
        ));

        // Record type information and actual values for filtering and debugging
        match input {
            serde_json::Value::Object(obj) => {
                span.set_attribute(opentelemetry::KeyValue::new("input.type", "object"));
                span.set_attribute(opentelemetry::KeyValue::new(
                    "input.object.key_count",
                    obj.len() as i64,
                ));

                // Record object keys for better debugging
                let keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
                if !keys.is_empty() {
                    span.set_attribute(opentelemetry::KeyValue::new(
                        "input.object.keys",
                        format!("{keys:?}"),
                    ));
                }
            }
            serde_json::Value::Array(arr) => {
                span.set_attribute(opentelemetry::KeyValue::new("input.type", "array"));
                span.set_attribute(opentelemetry::KeyValue::new(
                    "input.array.length",
                    arr.len() as i64,
                ));
            }
            serde_json::Value::String(s) => {
                span.set_attribute(opentelemetry::KeyValue::new("input.type", "string"));
                span.set_attribute(opentelemetry::KeyValue::new(
                    "input.string.value",
                    s.clone(),
                ));
                span.set_attribute(opentelemetry::KeyValue::new(
                    "input.string.length",
                    s.len() as i64,
                ));
            }
            serde_json::Value::Number(n) => {
                span.set_attribute(opentelemetry::KeyValue::new("input.type", "number"));
                span.set_attribute(opentelemetry::KeyValue::new(
                    "input.number.value",
                    n.to_string(),
                ));
            }
            serde_json::Value::Bool(b) => {
                span.set_attribute(opentelemetry::KeyValue::new("input.type", "boolean"));
                span.set_attribute(opentelemetry::KeyValue::new("input.boolean.value", *b));
            }
            serde_json::Value::Null => {
                span.set_attribute(opentelemetry::KeyValue::new("input.type", "null"));
                span.set_attribute(opentelemetry::KeyValue::new("input.null.value", "null"));
            }
        }
    }

    /// Records workflow output to the current span using OpenTelemetry semantic conventions
    fn record_workflow_output(span: &SpanRef, output: &serde_json::Value, status: &WorkflowStatus) {
        // Record output as pretty-printed JSON - Jaeger will parse and display it nicely
        let output_str =
            serde_json::to_string_pretty(output).unwrap_or_else(|_| output.to_string());
        span.set_attribute(opentelemetry::KeyValue::new(
            "output.value",
            output_str.clone(),
        ));
        span.set_attribute(opentelemetry::KeyValue::new(
            "output.size",
            output_str.len() as i64,
        ));
        span.set_attribute(opentelemetry::KeyValue::new(
            "operation.status",
            format!("{status:?}"),
        ));

        // Set span status based on workflow status
        match status {
            WorkflowStatus::Completed => {
                span.set_attribute(opentelemetry::KeyValue::new("otel.status_code", "OK"));
            }
            WorkflowStatus::Faulted | WorkflowStatus::Cancelled => {
                span.set_attribute(opentelemetry::KeyValue::new("otel.status_code", "ERROR"));
                span.set_attribute(opentelemetry::KeyValue::new("error", true));
            }
            _ => {
                span.set_attribute(opentelemetry::KeyValue::new("otel.status_code", "UNSET"));
            }
        }

        // Record basic type information for filtering and metrics
        match output {
            serde_json::Value::Object(obj) => {
                span.set_attribute(opentelemetry::KeyValue::new("output.type", "object"));
                span.set_attribute(opentelemetry::KeyValue::new(
                    "output.object.key_count",
                    obj.len() as i64,
                ));

                // Record object keys for better debugging
                let keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
                if !keys.is_empty() {
                    span.set_attribute(opentelemetry::KeyValue::new(
                        "output.object.keys",
                        format!("{keys:?}"),
                    ));
                }

                // Handle error outputs with standard error attributes
                if obj.contains_key("error") {
                    span.set_attribute(opentelemetry::KeyValue::new("error", true));
                    span.set_attribute(opentelemetry::KeyValue::new("otel.status_code", "ERROR"));
                    if let Some(error_msg) = obj.get("error") {
                        span.set_attribute(opentelemetry::KeyValue::new(
                            "error.type",
                            "workflow_execution_error",
                        ));
                        span.set_attribute(opentelemetry::KeyValue::new(
                            "error.message",
                            error_msg.to_string(),
                        ));
                        span.set_attribute(opentelemetry::KeyValue::new(
                            "otel.status_description",
                            error_msg.to_string(),
                        ));
                    }
                }
            }
            serde_json::Value::Array(arr) => {
                span.set_attribute(opentelemetry::KeyValue::new("output.type", "array"));
                span.set_attribute(opentelemetry::KeyValue::new(
                    "output.array.length",
                    arr.len() as i64,
                ));
            }
            serde_json::Value::String(s) => {
                span.set_attribute(opentelemetry::KeyValue::new("output.type", "string"));
                span.set_attribute(opentelemetry::KeyValue::new(
                    "output.string.value",
                    s.clone(),
                ));
                span.set_attribute(opentelemetry::KeyValue::new(
                    "output.string.length",
                    s.len() as i64,
                ));
            }
            serde_json::Value::Number(n) => {
                span.set_attribute(opentelemetry::KeyValue::new("output.type", "number"));
                span.set_attribute(opentelemetry::KeyValue::new(
                    "output.number.value",
                    n.to_string(),
                ));
            }
            serde_json::Value::Bool(b) => {
                span.set_attribute(opentelemetry::KeyValue::new("output.type", "boolean"));
                span.set_attribute(opentelemetry::KeyValue::new("output.boolean.value", *b));
            }
            serde_json::Value::Null => {
                span.set_attribute(opentelemetry::KeyValue::new("output.type", "null"));
                span.set_attribute(opentelemetry::KeyValue::new("output.null.value", "null"));
            }
        }
    }

    /// Records workflow metadata to the current span
    fn record_workflow_metadata(span: &SpanRef, workflow: &WorkflowSchema, workflow_id: &str) {
        span.set_attribute(opentelemetry::KeyValue::new(
            "service.name",
            workflow.document.name.to_string(),
        ));
        span.set_attribute(opentelemetry::KeyValue::new(
            "service.version",
            workflow.document.version.to_string(),
        ));
        span.set_attribute(opentelemetry::KeyValue::new(
            "operation.id",
            workflow_id.to_string(),
        ));
        span.set_attribute(opentelemetry::KeyValue::new(
            "operation.task_count",
            workflow.do_.0.len() as i64,
        ));

        // Add service-related attributes for better organization in Jaeger
        span.set_attribute(opentelemetry::KeyValue::new(
            "service.operation",
            "workflow_execution",
        ));
        span.set_attribute(opentelemetry::KeyValue::new(
            "component",
            "workflow_executor",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::test::create_test_app_wrapper_module;
    use crate::workflow::definition::{
        workflow::{
            CheckpointConfig, CheckpointConfigStorage, Document, FlowDirective, FlowDirectiveEnum,
            Input, Output, OutputAs, SetTask, Task, TaskList, WorkflowName, WorkflowSchema,
            WorkflowVersion,
        },
        WorkflowLoader,
    };

    use app::module::test::create_hybrid_test_app;
    use futures::{pin_mut, StreamExt};
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::sync::Arc;

    fn create_test_workflow() -> WorkflowSchema {
        let task_map_list = {
            let mut map1 = HashMap::new();
            map1.insert(
                "task1".to_string(),
                Task::SetTask(SetTask {
                    set: serde_json::Map::new(),
                    export: None,
                    if_: None,
                    input: None,
                    metadata: serde_json::Map::new(),
                    output: None,
                    then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Continue)),
                    timeout: None,
                    checkpoint: false,
                }),
            );
            let mut map2 = HashMap::new();
            map2.insert(
                "task2".to_string(),
                Task::SetTask(SetTask {
                    set: serde_json::Map::new(),
                    export: None,
                    if_: None,
                    input: None,
                    metadata: serde_json::Map::new(),
                    output: None,
                    then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                    timeout: None,
                    checkpoint: false,
                }),
            );
            vec![map1, map2]
        };
        let task_list = TaskList(task_map_list);

        WorkflowSchema {
            checkpointing: None,
            document: Document {
                name: WorkflowName::from_str("test-workflow").unwrap(),
                version: WorkflowVersion::from_str("1.0.0").unwrap(),
                metadata: serde_json::Map::new(),
                ..Default::default()
            },
            input: Input {
                schema: None,
                from: None,
            },
            output: Some(Output {
                as_: None,
                schema: None,
            }),
            do_: task_list,
        }
    }

    #[test]
    fn test_apply_schema_defaults() {
        // Test case 1: Apply defaults to empty input
        let schema = serde_json::json!({
            "properties": {
                "max_iterations": {
                    "type": "integer",
                    "default": 5
                },
                "target_score": {
                    "type": "number",
                    "default": 0.85
                },
                "query": {
                    "type": "string"
                }
            }
        });

        let input = serde_json::json!({
            "query": "test query"
        });

        let result = WorkflowExecutor::apply_schema_defaults(input.clone(), &schema).unwrap();
        assert_eq!(result.get("query").unwrap(), "test query");
        assert_eq!(result.get("max_iterations").unwrap(), 5);
        assert_eq!(result.get("target_score").unwrap(), 0.85);

        // Test case 2: User-provided values should not be overwritten
        let input_with_values = serde_json::json!({
            "query": "test query",
            "max_iterations": 10
        });

        let result2 =
            WorkflowExecutor::apply_schema_defaults(input_with_values.clone(), &schema).unwrap();
        assert_eq!(result2.get("query").unwrap(), "test query");
        assert_eq!(result2.get("max_iterations").unwrap(), 10); // User value preserved
        assert_eq!(result2.get("target_score").unwrap(), 0.85); // Default applied

        // Test case 3: Nested objects
        let nested_schema = serde_json::json!({
            "properties": {
                "config": {
                    "type": "object",
                    "properties": {
                        "timeout": {
                            "type": "integer",
                            "default": 30
                        },
                        "retries": {
                            "type": "integer",
                            "default": 3
                        }
                    }
                },
                "name": {
                    "type": "string",
                    "default": "default-name"
                }
            }
        });

        let nested_input = serde_json::json!({
            "config": {
                "timeout": 60
            }
        });

        let result3 =
            WorkflowExecutor::apply_schema_defaults(nested_input.clone(), &nested_schema).unwrap();
        assert_eq!(result3.get("name").unwrap(), "default-name");
        let config = result3.get("config").unwrap().as_object().unwrap();
        assert_eq!(config.get("timeout").unwrap(), 60); // User value
        assert_eq!(config.get("retries").unwrap(), 3); // Default applied

        // Test case 4: Non-object input should pass through
        let non_object_input = serde_json::json!("string value");
        let result4 =
            WorkflowExecutor::apply_schema_defaults(non_object_input.clone(), &schema).unwrap();
        assert_eq!(result4, "string value");
    }

    #[test]
    fn test_execute_workflow() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let app_wrapper_module = Arc::new(create_test_app_wrapper_module(app_module.clone()));

            let workflow = create_test_workflow();
            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));

            let executor = WorkflowExecutor::init(
                app_wrapper_module,
                app_module,
                Arc::new(workflow.clone()),
                input.clone(),
                None,
                context.clone(),
                Arc::new(HashMap::new()),
                None,
            )
            .await
            .unwrap();

            let workflow_stream =
                executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
            pin_mut!(workflow_stream);

            let mut final_wfc = None;
            while let Some(wfc) = workflow_stream.next().await {
                match wfc {
                    Ok(wfc) => {
                        final_wfc = Some(wfc);
                    }
                    Err(_) => {
                        final_wfc = None;
                        break;
                    }
                }
            }

            assert!(final_wfc.is_some());
            let wfc = final_wfc.unwrap();
            assert_eq!(wfc.status, WorkflowStatus::Completed);
        })
    }

    #[test]
    fn test_cancel_workflow() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let app_wrapper_module = Arc::new(create_test_app_wrapper_module(app_module.clone()));
            let workflow = create_test_workflow();
            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));

            let executor = WorkflowExecutor::init(
                app_wrapper_module,
                app_module,
                Arc::new(workflow.clone()),
                input.clone(),
                None,
                context.clone(),
                Arc::new(HashMap::new()),
                None,
            )
            .await
            .unwrap();

            executor.workflow_context.write().await.status = WorkflowStatus::Running;

            executor.cancel().await;

            let status = executor.workflow_context.read().await.status.clone();
            assert_eq!(status, WorkflowStatus::Cancelled);
        })
    }

    #[test]
    fn test_workflow_with_checkpoint_memory() {
        // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let app_wrapper_module = Arc::new(create_test_app_wrapper_module(app_module.clone()));
            let workflow = create_test_checkpoint_workflow();
            let input = Arc::new(serde_json::json!({"seed": 12345}));
            let context = Arc::new(serde_json::json!({}));
            let execution_id = ExecutionId::new("test-checkpoint-execution".to_string()).unwrap();
            // First execution with checkpoint enabled
            let executor = WorkflowExecutor::init(
                app_wrapper_module.clone(),
                app_module.clone(),
                Arc::new(workflow.clone()),
                input.clone(),
                Some(execution_id.clone()),
                context.clone(),
                Arc::new(HashMap::new()),
                None,
            )
            .await
            .unwrap();

            let workflow_stream =
                executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
            pin_mut!(workflow_stream);

            let mut final_wfc = None;
            while let Some(wfc) = workflow_stream.next().await {
                match wfc {
                    Ok(wfc) => {
                        final_wfc = Some(wfc);
                    }
                    Err(e) => {
                        tracing::error!("Workflow error: {:#?}", e);
                        final_wfc = None;
                        break;
                    }
                }
            }

            assert!(final_wfc.is_some());
            tracing::debug!("First execution completed");

            let first_execution_wfc = final_wfc.unwrap();
            assert_eq!(first_execution_wfc.status, WorkflowStatus::Completed);
            let first_output = first_execution_wfc.output.as_ref().unwrap().clone();

            // Simulate checkpoint restoration by creating a checkpoint context
            let checkpoint_repo = executor.checkpoint_repository.as_ref().unwrap();
            let checkpoint_key = format!(
                "{}:{}:{}",
                workflow.document.name.as_str(),
                execution_id.value,
                "/ROOT/do/0/GenerateRandomValue"
            );
            let saved_checkpoint = checkpoint_repo
                .checkpoint_repository()
                .get_checkpoint(&checkpoint_key)
                .await
                .unwrap();

            assert!(saved_checkpoint.is_some(), "Checkpoint should be saved");

            // Second execution with checkpoint restore
            let checkpoint_context = saved_checkpoint.unwrap();
            let executor2 = WorkflowExecutor::init(
                app_wrapper_module.clone(),
                app_module.clone(),
                Arc::new(workflow.clone()),
                input.clone(),
                Some(execution_id.clone()),
                context.clone(),
                Arc::new(HashMap::new()),
                Some(checkpoint_context.clone()),
            )
            .await
            .unwrap();

            let workflow_stream2 =
                executor2.execute_workflow(Arc::new(opentelemetry::Context::current()));

            pin_mut!(workflow_stream2);

            let mut final_wfc2 = None;
            while let Some(wfc) = workflow_stream2.next().await {
                match wfc {
                    Ok(wfc) => {
                        final_wfc2 = Some(wfc);
                    }
                    Err(_e) => {
                        final_wfc2 = None;
                        break;
                    }
                }
            }

            assert!(final_wfc2.is_some());
            let second_execution_wfc = final_wfc2.unwrap();
            assert_eq!(second_execution_wfc.status, WorkflowStatus::Completed);
            let second_output = second_execution_wfc.output.as_ref().unwrap().clone();
            drop(second_execution_wfc);
            tracing::debug!("====== Second execution output: {:#?}", second_output);

            // Verify that outputs are identical due to checkpoint restoration
            assert_eq!(
                first_output, second_output,
                "Outputs should be identical when restored from checkpoint"
            );
        })
    }

    #[test]
    fn test_workflow_checkpoint_with_random_output() {
        // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let app_wrapper_module = Arc::new(create_test_app_wrapper_module(app_module.clone()));
            let workflow = create_test_random_checkpoint_workflow();
            let input = Arc::new(serde_json::json!({"base_seed": 42}));
            let context = Arc::new(serde_json::json!({}));
            let execution_id =
                ExecutionId::new("test-random-checkpoint-execution".to_string()).unwrap();

            // First execution - should generate random values and save checkpoints
            let executor = WorkflowExecutor::init(
                app_wrapper_module.clone(),
                app_module.clone(),
                Arc::new(workflow.clone()),
                input.clone(),
                Some(execution_id.clone()),
                context.clone(),
                Arc::new(HashMap::new()),
                None,
            )
            .await
            .unwrap();

            let workflow_stream =
                executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
            pin_mut!(workflow_stream);

            let mut first_execution_results = Vec::new();
            while let Some(wfc) = workflow_stream.next().await {
                match wfc {
                    Ok(wfc) => {
                        if let Some(output) = &wfc.output {
                            first_execution_results.push(output.clone());
                        }
                    }
                    Err(e) => {
                        tracing::error!("Workflow error: {:#?}", e);
                        break;
                    }
                }
            }

            assert!(!first_execution_results.is_empty());
            let first_final_output = first_execution_results.last().unwrap();

            // Get checkpoint from the middle of the workflow execution
            let checkpoint_repo = executor.checkpoint_repository.as_ref().unwrap();
            let checkpoint_key = format!(
                "{}:{}:{}",
                workflow.document.name.as_str(),
                execution_id.value,
                "/ROOT/do/1/ProcessValue"
            );
            let saved_checkpoint = checkpoint_repo
                .checkpoint_repository()
                .get_checkpoint(&checkpoint_key)
                .await
                .unwrap();

            if let Some(checkpoint_context) = saved_checkpoint {
                // Second execution with checkpoint restore from middle
                let executor2 = WorkflowExecutor::init(
                    app_wrapper_module.clone(),
                    app_module.clone(),
                    Arc::new(workflow.clone()),
                    input.clone(),
                    ExecutionId::new("test-random-checkpoint-execution-2".to_string()),
                    context.clone(),
                    Arc::new(HashMap::new()),
                    Some(checkpoint_context),
                )
                .await
                .unwrap();

                let workflow_stream2 =
                    executor2.execute_workflow(Arc::new(opentelemetry::Context::current()));
                pin_mut!(workflow_stream2);

                let mut second_execution_results = Vec::new();
                while let Some(wfc) = workflow_stream2.next().await {
                    match wfc {
                        Ok(wfc) => {
                            if let Some(output) = &wfc.output {
                                second_execution_results.push(output.clone());
                            }
                        }
                        Err(e) => {
                            tracing::error!("Workflow error: {:#?}", e);
                            break;
                        }
                    }
                }

                assert!(!second_execution_results.is_empty());
                let second_final_output = second_execution_results.last().unwrap();

                // Verify that checkpoint restoration preserves random state
                if let (Some(first_random), Some(second_random)) = (
                    first_final_output.get("random_value"),
                    second_final_output.get("random_value"),
                ) {
                    // Due to checkpoint restoration, the random values should be consistent
                    // when restored from the same checkpoint position
                    tracing::info!(
                        "First random: {}, Second random: {}",
                        first_random,
                        second_random
                    );
                }
            }
        })
    }

    fn create_test_checkpoint_workflow() -> WorkflowSchema {
        let task_map_list = vec![
            {
                let mut map = HashMap::new();
                map.insert(
                    "GenerateRandomValue".to_string(),
                    Task::SetTask(SetTask {
                        set: {
                            let mut set_map = serde_json::Map::new();
                            set_map.insert(
                                "random_value".to_string(),
                                serde_json::json!("${(.seed * 1664525 + 1013904223) % 4294967296}"),
                            );
                            set_map.insert("step".to_string(), serde_json::json!(1));
                            set_map
                        },
                        if_: None,
                        input: None,
                        output: None,
                        metadata: serde_json::Map::new(),
                        then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Continue)),
                        timeout: None,
                        export: None,
                        checkpoint: true,
                    }),
                );
                map
            },
            {
                let mut map = HashMap::new();
                map.insert(
                    "ProcessValue".to_string(),
                    Task::SetTask(SetTask {
                        set: {
                            let mut set_map = serde_json::Map::new();
                            set_map.insert(
                                "processed_value".to_string(),
                                serde_json::json!("${.random_value * 2}"),
                            );
                            set_map.insert("step".to_string(), serde_json::json!(2));
                            set_map
                        },
                        if_: None,
                        input: None,
                        output: Some(Output {
                            as_: Some(OutputAs::Variant0("${$processed_value}".to_string())),
                            ..Default::default()
                        }),
                        metadata: serde_json::Map::new(),
                        then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Continue)),
                        timeout: None,
                        export: None,
                        checkpoint: true,
                    }),
                );
                map
            },
        ];
        let task_list = TaskList(task_map_list);

        WorkflowSchema {
            document: Document {
                name: WorkflowName::try_from("checkpoint-test-workflow".to_string()).unwrap(),
                version: WorkflowVersion::try_from("1.0.0".to_string()).unwrap(),
                metadata: serde_json::Map::new(),
                ..Default::default()
            },
            input: Input {
                schema: None,
                from: None,
            },
            output: Some(Output {
                as_: None,
                schema: None,
            }),
            checkpointing: Some(CheckpointConfig {
                enabled: true,
                storage: Some(CheckpointConfigStorage::Memory),
            }),
            do_: task_list,
        }
    }

    fn create_test_random_checkpoint_workflow() -> WorkflowSchema {
        let task_map_list = vec![
            {
                let mut map = HashMap::new();
                map.insert(
                    "InitializeRandom".to_string(),
                    Task::SetTask(SetTask {
                        set: {
                            let mut set_map = serde_json::Map::new();
                            set_map.insert("seed".to_string(), serde_json::json!("${.base_seed}"));
                            set_map.insert("step".to_string(), serde_json::json!("init"));
                            set_map
                        },
                        if_: None,
                        input: None,
                        output: None,
                        metadata: serde_json::Map::new(),
                        then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Continue)),
                        timeout: None,
                        export: None,
                        checkpoint: true,
                    }),
                );
                map
            },
            {
                let mut map = HashMap::new();
                map.insert(
                    "GenerateFirstRandom".to_string(),
                    Task::SetTask(SetTask {
                        set: {
                            let mut set_map = serde_json::Map::new();
                            // Linear congruential generator for predictable "random" values
                            set_map.insert(
                                "random_value".to_string(),
                                serde_json::json!("${(.seed * 1664525 + 1013904223) % 2147483647}"),
                            );
                            set_map.insert("step".to_string(), serde_json::json!("first_random"));
                            set_map
                        },
                        if_: None,
                        input: None,
                        output: None,
                        metadata: serde_json::Map::new(),
                        then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Continue)),
                        timeout: None,
                        export: None,
                        checkpoint: true,
                    }),
                );
                map
            },
            {
                let mut map = HashMap::new();
                map.insert(
                    "GenerateSecondRandom".to_string(),
                    Task::SetTask(SetTask {
                        set: {
                            let mut set_map = serde_json::Map::new();
                            // Use previous random value as new seed
                            set_map.insert(
                                "next_seed".to_string(),
                                serde_json::json!("${.random_value}"),
                            );
                            set_map.insert(
                                "second_random".to_string(),
                                serde_json::json!(
                                    "${(.random_value * 1103515245 + 12345) % 2147483647}"
                                ),
                            );
                            set_map.insert("step".to_string(), serde_json::json!("second_random"));
                            set_map
                        },
                        if_: None,
                        input: None,
                        output: None,
                        metadata: serde_json::Map::new(),
                        then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Continue)),
                        timeout: None,
                        export: None,
                        checkpoint: true,
                    }),
                );
                map
            },
            {
                let mut map = HashMap::new();
                map.insert(
                    "FinalizeResult".to_string(),
                    Task::SetTask(SetTask {
                        set: {
                            let mut set_map = serde_json::Map::new();
                            set_map.insert(
                                "final_result".to_string(),
                                serde_json::json!({
                                    "first_random": "${.random_value}",
                                    "second_random": "${.second_random}",
                                    "combined": "${.random_value + .second_random}",
                                    "step": "final"
                                }),
                            );
                            set_map
                        },
                        if_: None,
                        input: None,
                        output: None,
                        metadata: serde_json::Map::new(),
                        then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                        timeout: None,
                        export: None,
                        checkpoint: true,
                    }),
                );
                map
            },
        ];
        let task_list = TaskList(task_map_list);

        WorkflowSchema {
            document: Document {
                dsl: "1.0.0".to_string().try_into().unwrap(),
                namespace: "test".try_into().unwrap(),
                name: WorkflowName::try_from("random-checkpoint-test-workflow".to_string())
                    .unwrap(),
                title: Some("Random Checkpoint Test Workflow".to_string()),
                summary: None,
                version: WorkflowVersion::try_from("1.0.0".to_string()).unwrap(),
                tags: serde_json::Map::new(),
                metadata: serde_json::Map::new(),
            },
            input: Input {
                schema: None,
                from: None,
            },
            output: Some(Output {
                as_: Some(workflow::OutputAs::Variant0("${.final_result}".to_string())),
                schema: None,
            }),
            checkpointing: Some(CheckpointConfig {
                enabled: true,
                storage: Some(CheckpointConfigStorage::Memory),
            }),
            do_: task_list,
        }
    }
    async fn load_test_workflow_from_yaml(yaml_path: &str) -> WorkflowSchema {
        // Use local-only loader for loading local test files (no network access needed)
        let loader = WorkflowLoader::new_local_only();
        loader
            .load_workflow(Some(yaml_path), None, false)
            .await
            .unwrap()
    }

    #[test]
    fn test_workflow_checkpoint_with_for_task() {
        // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let app_wrapper_module = Arc::new(create_test_app_wrapper_module(app_module.clone()));
            let workflow_path = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/test-files/for_task_checkpoint.yaml"
            );
            let workflow = load_test_workflow_from_yaml(workflow_path).await;
            let input = Arc::new(serde_json::json!({"items": [1, 2, 3, 4, 5]}));
            let context = Arc::new(serde_json::json!({}));
            let execution_id =
                ExecutionId::new("test-for-task-checkpoint-execution".to_string()).unwrap();

            // First execution with checkpoint enabled
            let executor = WorkflowExecutor::init(
                app_wrapper_module.clone(),
                app_module.clone(),
                Arc::new(workflow.clone()),
                input.clone(),
                Some(execution_id.clone()),
                context.clone(),
                Arc::new(HashMap::new()),
                None,
            )
            .await
            .unwrap();

            let workflow_stream =
                executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
            pin_mut!(workflow_stream);

            let mut final_wfc = None;
            while let Some(wfc) = workflow_stream.next().await {
                match wfc {
                    Ok(wfc) => {
                        final_wfc = Some(wfc);
                    }
                    Err(e) => {
                        tracing::error!("Workflow error: {:#?}", e);
                        break;
                    }
                }
            }

            assert!(final_wfc.is_some());
            let first_execution_wfc = final_wfc.unwrap();
            assert_eq!(first_execution_wfc.status, WorkflowStatus::Completed);
            let first_output = first_execution_wfc.output.as_ref().unwrap().clone();

            // Get checkpoint from inside the for loop
            let checkpoint_repo = executor.checkpoint_repository.as_ref().unwrap();
            let checkpoint_key = format!(
                "{}:{}:{}",
                workflow.document.name.as_str(),
                execution_id.value,
                "/ROOT/do/0/ProcessItems/for/do/0/ProcessItem" // Checkpoint from 3rd iteration
            );
            let saved_checkpoint = checkpoint_repo
                .checkpoint_repository()
                .get_checkpoint(&checkpoint_key)
                .await
                .unwrap();

            assert!(
                saved_checkpoint.is_some(),
                "Checkpoint should be saved inside for loop"
            );

            // Second execution with checkpoint restore from inside for loop
            let executor2 = WorkflowExecutor::init(
                app_wrapper_module.clone(),
                app_module.clone(),
                Arc::new(workflow.clone()),
                input.clone(),
                ExecutionId::new("test-for-task-checkpoint-execution-2".to_string()),
                context.clone(),
                Arc::new(HashMap::new()),
                saved_checkpoint,
            )
            .await
            .unwrap();

            let workflow_stream2 =
                executor2.execute_workflow(Arc::new(opentelemetry::Context::current()));
            pin_mut!(workflow_stream2);

            let mut final_wfc2 = None;
            while let Some(wfc) = workflow_stream2.next().await {
                match wfc {
                    Ok(wfc) => {
                        final_wfc2 = Some(wfc);
                    }
                    Err(e) => {
                        tracing::error!("Workflow error: {:#?}", e);
                        break;
                    }
                }
            }

            assert!(final_wfc2.is_some());
            let second_execution_wfc = final_wfc2.unwrap();
            assert_eq!(second_execution_wfc.status, WorkflowStatus::Completed);
            let second_output = second_execution_wfc.output.as_ref().unwrap().clone();

            // Verify that outputs match when restored from checkpoint inside for loop
            assert_eq!(
                first_output, second_output,
                "Outputs should be identical when restored from checkpoint inside for loop"
            );
        })
    }

    #[test]
    fn test_workflow_checkpoint_with_try_task() {
        // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let app_wrapper_module = Arc::new(create_test_app_wrapper_module(app_module.clone()));
            let workflow_path = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/test-files/try_task_checkpoint.yaml"
            );
            let workflow = load_test_workflow_from_yaml(workflow_path).await;
            let input = Arc::new(serde_json::json!({"should_fail": false}));
            let context = Arc::new(serde_json::json!({}));
            let execution_id =
                ExecutionId::new("test-try-task-checkpoint-execution".to_string()).unwrap();

            // First execution with checkpoint enabled
            let executor = WorkflowExecutor::init(
                app_wrapper_module.clone(),
                app_module.clone(),
                Arc::new(workflow.clone()),
                input.clone(),
                Some(execution_id.clone()),
                context.clone(),
                Arc::new(HashMap::new()),
                None,
            )
            .await
            .unwrap();

            let workflow_stream =
                executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
            pin_mut!(workflow_stream);

            let mut final_wfc = None;
            while let Some(wfc) = workflow_stream.next().await {
                match wfc {
                    Ok(wfc) => {
                        final_wfc = Some(wfc);
                    }
                    Err(e) => {
                        tracing::error!("Workflow error: {:#?}", e);
                        break;
                    }
                }
            }

            assert!(final_wfc.is_some());
            let first_execution_wfc = final_wfc.unwrap();
            assert_eq!(first_execution_wfc.status, WorkflowStatus::Completed);
            let first_output = first_execution_wfc.output.as_ref().unwrap().clone();
            drop(first_execution_wfc);

            // Get checkpoint from inside the try block
            let checkpoint_repo = executor.checkpoint_repository.as_ref().unwrap();
            let checkpoint_key = format!(
                "{}:{}:{}",
                workflow.document.name.as_str(),
                execution_id.value,
                "/ROOT/do/0/SafeProcess/try/do/1/InnerProcess" // Checkpoint from inside try block
            );
            let saved_checkpoint = checkpoint_repo
                .checkpoint_repository()
                .get_checkpoint(&checkpoint_key)
                .await
                .unwrap();

            assert!(
                saved_checkpoint.is_some(),
                "Checkpoint should be saved inside try block"
            );

            // Second execution with checkpoint restore from inside try block
            let executor2 = WorkflowExecutor::init(
                app_wrapper_module.clone(),
                app_module.clone(),
                Arc::new(workflow.clone()),
                input.clone(),
                ExecutionId::new("test-try-task-checkpoint-execution-2".to_string()),
                context.clone(),
                Arc::new(HashMap::new()),
                saved_checkpoint,
            )
            .await
            .unwrap();

            let workflow_stream2 =
                executor2.execute_workflow(Arc::new(opentelemetry::Context::current()));
            pin_mut!(workflow_stream2);

            let mut final_wfc2 = None;
            while let Some(wfc) = workflow_stream2.next().await {
                match wfc {
                    Ok(wfc) => {
                        final_wfc2 = Some(wfc);
                    }
                    Err(e) => {
                        tracing::error!("Workflow error: {:#?}", e);
                        break;
                    }
                }
            }

            assert!(final_wfc2.is_some());
            let second_execution_wfc = final_wfc2.unwrap();
            assert_eq!(second_execution_wfc.status, WorkflowStatus::Completed);
            let second_output = second_execution_wfc.output.as_ref().unwrap().clone();

            // Verify that outputs match when restored from checkpoint inside try block
            assert_eq!(
                first_output, second_output,
                "Outputs should be identical when restored from checkpoint inside try block"
            );
        })
    }

    #[test]
    fn test_workflow_checkpoint_with_deep_nested_structure() {
        // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let app_wrapper_module = Arc::new(create_test_app_wrapper_module(app_module.clone()));
            let workflow_path = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/test-files/deep_nested_checkpoint.yaml"
            );
            let workflow = load_test_workflow_from_yaml(workflow_path).await;
            let input = Arc::new(serde_json::json!({"base_value": 100}));
            let context = Arc::new(serde_json::json!({}));
            let execution_id =
                ExecutionId::new("test-deep-nested-checkpoint-execution".to_string()).unwrap();

            // First execution with checkpoint enabled
            let executor = WorkflowExecutor::init(
                app_wrapper_module.clone(),
                app_module.clone(),
                Arc::new(workflow.clone()),
                input.clone(),
                Some(execution_id.clone()),
                context.clone(),
                Arc::new(HashMap::new()),
                None,
            )
            .await
            .unwrap();

            let workflow_stream =
                executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
            pin_mut!(workflow_stream);

            let mut final_wfc = None;
            while let Some(wfc) = workflow_stream.next().await {
                match wfc {
                    Ok(wfc) => {
                        final_wfc = Some(wfc);
                    }
                    Err(e) => {
                        tracing::error!("Workflow error: {:#?}", e);
                        break;
                    }
                }
            }

            assert!(final_wfc.is_some());
            let first_execution_wfc = final_wfc.unwrap();
            assert_eq!(first_execution_wfc.status, WorkflowStatus::Completed);
            let first_output = first_execution_wfc.output.as_ref().unwrap().clone();

            // Get checkpoint from deep nested structure
            let checkpoint_repo = executor.checkpoint_repository.as_ref().unwrap();
            let checkpoint_key = format!(
                "{}:{}:{}",
                workflow.document.name.as_str(),
                execution_id.value,
                "/ROOT/do/0/OuterProcess/do/1/NestedProcess/do/1/NestedProcess/try/do/0/DeepProcess/do/2/DeepestProcess" // Very deep checkpoint
            );
            let saved_checkpoint = checkpoint_repo
                .checkpoint_repository()
                .get_checkpoint(&checkpoint_key)
                .await
                .unwrap();

            assert!(
                saved_checkpoint.is_some(),
                "Checkpoint should be saved in deep nested structure"
            );

            // Second execution with checkpoint restore from deep nested structure
            let executor2 = WorkflowExecutor::init(
                app_wrapper_module.clone(),
                app_module.clone(),
                Arc::new(workflow.clone()),
                input.clone(),
                ExecutionId::new("test-deep-nested-checkpoint-execution-2".to_string()),
                context.clone(),
                Arc::new(HashMap::new()),
                saved_checkpoint,
            )
            .await
            .unwrap();

            let workflow_stream2 =
                executor2.execute_workflow(Arc::new(opentelemetry::Context::current()));
            pin_mut!(workflow_stream2);

            let mut final_wfc2 = None;
            while let Some(wfc) = workflow_stream2.next().await {
                match wfc {
                    Ok(wfc) => {
                        final_wfc2 = Some(wfc);
                    }
                    Err(e) => {
                        tracing::error!("Workflow error: {:#?}", e);
                        break;
                    }
                }
            }

            assert!(final_wfc2.is_some());
            let second_execution_wfc = final_wfc2.unwrap();
            assert_eq!(second_execution_wfc.status, WorkflowStatus::Completed);
            let second_output = second_execution_wfc.output.as_ref().unwrap().clone();

            // Verify that outputs match when restored from deep nested checkpoint
            assert_eq!(
                first_output, second_output,
                "Outputs should be identical when restored from deep nested checkpoint"
            );
        })
    }
}
