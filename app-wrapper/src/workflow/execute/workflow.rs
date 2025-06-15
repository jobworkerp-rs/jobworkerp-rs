use super::{expression::UseExpression, job::JobExecutorWrapper, task::TaskExecutor};
use crate::workflow::execute::context::{self, TaskContext, WorkflowContext, WorkflowStatus};
use crate::workflow::execute::task::ExecutionId;
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self, Task, WorkflowSchema},
    },
    execute::checkpoint::repository::CheckPointRepositoryWithIdImpl,
};
use anyhow::Result;
use app::module::AppModule;
use async_stream::stream;
use futures::{Stream, StreamExt};
use infra_utils::infra::cache::MokaCacheConfig;
use infra_utils::infra::{net::reqwest::ReqwestClient, trace::Tracing};
use jobworkerp_base::APP_WORKER_NAME;
use opentelemetry::{
    trace::{SpanRef, TraceContextExt},
    Context,
};
use proto::jobworkerp::data::StorageType;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, Clone)]
pub struct WorkflowExecutor {
    pub job_executors: Arc<JobExecutorWrapper>,
    pub http_client: ReqwestClient,
    pub workflow: Arc<WorkflowSchema>,
    pub workflow_context: Arc<RwLock<context::WorkflowContext>>,
    pub execution_id: Option<Arc<ExecutionId>>,
    pub metadata: Arc<HashMap<String, String>>,
    pub checkpoint_repository: Option<Arc<CheckPointRepositoryWithIdImpl>>,
}
impl UseJqAndTemplateTransformer for WorkflowExecutor {}
impl UseExpressionTransformer for WorkflowExecutor {}
impl UseExpression for WorkflowExecutor {}
impl Tracing for WorkflowExecutor {}

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
                        format!("{:?}", keys),
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
            format!("{:?}", status),
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
                        format!("{:?}", keys),
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

impl WorkflowTracing for WorkflowExecutor {}

impl WorkflowExecutor {
    pub fn new(
        app_module: Arc<AppModule>,
        http_client: ReqwestClient,
        workflow: Arc<WorkflowSchema>,
        input: Arc<serde_json::Value>,
        execution_id: Option<ExecutionId>,
        context: Arc<serde_json::Value>,
        metadata: Arc<HashMap<String, String>>,
    ) -> Result<Self> {
        let workflow_context = Arc::new(RwLock::new(context::WorkflowContext::new(
            &workflow, input, context,
        )));
        let checkpoint_repository = if let Some(checkpointing) = &workflow.checkpointing {
            let workflow::CheckpointConfig { enabled, storage } = checkpointing;
            if *enabled {
                match storage {
                    &Some(workflow::CheckpointConfigStorage::Redis)
                        if app_module.config_module.storage_type() == StorageType::Scalable =>
                    {
                        let redis_pool = app_module
                                    .repositories
                                    .redis_module
                                    .as_ref()
                                    .map(|r| r.redis_pool)
                                    .ok_or(anyhow::anyhow!(
                                        "Redis pool is not available in the app module (even if scalable storage)"
                                    ))?;
                        Some(Arc::new(CheckPointRepositoryWithIdImpl::new_redis(
                            redis_pool, None, // No TTL for Redis storage
                        )))
                    }
                    _ => Some(Arc::new(CheckPointRepositoryWithIdImpl::new_memory(
                        &MokaCacheConfig {
                            num_counters: 1000,
                            ttl: None, // No TTL for memory storage
                        },
                    ))),
                }
            } else {
                None
            }
        } else {
            None
        };
        let job_executors = Arc::new(JobExecutorWrapper::new(app_module));
        // let checkpoint_epository = workflow.;
        Ok(Self {
            job_executors,
            http_client,
            workflow,
            workflow_context,
            execution_id: execution_id.map(Arc::new),
            metadata,
            checkpoint_repository,
        })
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
    ) -> impl Stream<Item = Result<Arc<RwLock<WorkflowContext>>, Box<workflow::Error>>> + 'static
    {
        let initial_wfc = self.workflow_context.clone();
        let workflow = self.workflow.clone();
        let job_executors = self.job_executors.clone();
        let http_client = self.http_client.clone();
        let cxc = cx.clone();
        let metadata = self.metadata.clone();
        let execution_id = self.execution_id.clone();

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

            let input = {
                let lock = initial_wfc.read().await;
                // Record workflow metadata and input
                Self::record_workflow_metadata(&span, &workflow, &lock.id.to_string());
                Self::record_workflow_input(&span, &lock.input);
                lock.input.clone()
            };

            // Validate input schema
            if let Some(schema) = workflow.input.schema.as_ref() {
                if let Some(schema) = schema.json_schema() {
                    match jsonschema::validate(schema, &input).map_err(|e| {
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
                            drop(wf);
                            yield Ok(initial_wfc.clone());
                            return;
                        }
                    }
                }
            }

            let mut task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );

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
                    drop(wf);
                    yield Ok(initial_wfc.clone());
                    return;
                }
            };

            // Transform input
            let transformed_input = if let Some(from) = workflow.input.from.as_ref() {
                match WorkflowExecutor::transform_input(input.clone(), from, &expression) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!("Failed to transform input: {:#?}", e);
                        let mut wf = initial_wfc.write().await;
                        wf.status = WorkflowStatus::Faulted;
                        let error_output = Arc::new(serde_json::json!({"error": e.to_string()}));
                        Self::record_workflow_output(&span, &error_output, &wf.status);
                        wf.output = Some(error_output);
                        drop(wf);
                        yield Ok(initial_wfc.clone());
                        return;
                    }
                }
            } else {
                input.clone()
            };
            task_context.set_input(transformed_input);

            // run workflow tasks as do_task
            let task_executor = TaskExecutor::new(
                job_executors,
                http_client,
                None,
                "ROOT",
                Arc::new(Task::DoTask(workflow.create_do_task(metadata.clone()))),
                metadata.clone(),
            );
            let mut task_stream = task_executor
                .execute(ccx, initial_wfc.clone(), Arc::new(task_context), execution_id.clone())
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
                        drop(wf);
                        yield Ok(initial_wfc.clone());
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

                let mut lock = initial_wfc.write().await;
                // Mark workflow as completed if it's still running
                if lock.status == WorkflowStatus::Running {
                    lock.status = WorkflowStatus::Completed;
                }

                // Record final output
                if let Some(output) = lock.output.as_ref() {
                    Self::record_workflow_output(&span, output, &lock.status);
                }

                drop(lock);

                // Yield final workflow context
                yield Ok(initial_wfc.clone());
            }

            // Log workflow status
            let lock = initial_wfc.read().await;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::definition::workflow::{
        Document, FlowDirective, FlowDirectiveEnum, Input, Output, SetTask, Task, TaskList,
        WorkflowName, WorkflowSchema, WorkflowVersion,
    };
    use app::module::test::create_hybrid_test_app;
    use futures::pin_mut;
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
    fn test_execute_workflow() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let http_client = ReqwestClient::new(
                Some("test"),
                Some(std::time::Duration::from_secs(1)),
                Some(std::time::Duration::from_secs(1)),
                Some(1),
            )
            .unwrap();

            let workflow = create_test_workflow();
            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));

            let executor = WorkflowExecutor::new(
                app_module,
                http_client,
                Arc::new(workflow.clone()),
                input.clone(),
                None,
                context.clone(),
                Arc::new(HashMap::new()),
            )
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
            let lock = wfc.read().await;
            assert_eq!(lock.status, WorkflowStatus::Completed);
        })
    }

    #[test]
    fn test_cancel_workflow() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let http_client = ReqwestClient::new(
                Some("test"),
                Some(std::time::Duration::from_secs(1)),
                Some(std::time::Duration::from_secs(1)),
                Some(1),
            )
            .unwrap();

            let workflow = create_test_workflow();
            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));

            let executor = WorkflowExecutor::new(
                app_module,
                http_client,
                Arc::new(workflow.clone()),
                input.clone(),
                None,
                context.clone(),
                Arc::new(HashMap::new()),
            )
            .unwrap();

            executor.workflow_context.write().await.status = WorkflowStatus::Running;

            executor.cancel().await;

            let status = executor.workflow_context.read().await.status.clone();
            assert_eq!(status, WorkflowStatus::Cancelled);
        })
    }
}
