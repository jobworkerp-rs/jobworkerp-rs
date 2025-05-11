use super::{expression::UseExpression, job::JobExecutorWrapper, task::TaskExecutor};
use crate::workflow::definition::{
    transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
    workflow::{Task, WorkflowSchema},
};
use crate::workflow::execute::context::{self, TaskContext, Then, WorkflowContext, WorkflowStatus};
use anyhow::Result;
use app::module::AppModule;
use futures::{Stream, StreamExt};
use indexmap::IndexMap;
use infra_utils::infra::net::reqwest::ReqwestClient;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio_stream::wrappers::ReceiverStream;

#[derive(Debug, Clone)]
pub struct WorkflowExecutor {
    pub job_executors: Arc<JobExecutorWrapper>,
    pub http_client: ReqwestClient,
    pub workflow: Arc<WorkflowSchema>,
    pub workflow_context: Arc<RwLock<context::WorkflowContext>>,
}
impl UseJqAndTemplateTransformer for WorkflowExecutor {}
impl UseExpressionTransformer for WorkflowExecutor {}
impl UseExpression for WorkflowExecutor {}

impl WorkflowExecutor {
    pub fn new(
        app_module: Arc<AppModule>,
        http_client: ReqwestClient,
        workflow: Arc<WorkflowSchema>,
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
    /// transforms the input, executes the tasks, and provides a stream of workflow contexts
    /// updated after each task execution.
    ///
    /// # Returns
    /// A `Stream<Item = Result<Arc<RwLock<WorkflowContext>>>>` containing the updated workflow context
    /// after each task execution.
    pub fn execute_workflow(
        &self,
    ) -> impl Stream<Item = Result<Arc<RwLock<WorkflowContext>>>> + 'static {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Arc<RwLock<WorkflowContext>>>>(32);

        // Create a clone of the workflow context to send initial state
        let initial_wfc = self.workflow_context.clone();
        let workflow = self.workflow.clone();
        let job_executors = self.job_executors.clone();
        let http_client = self.http_client.clone();

        tokio::spawn(async move {
            // Set workflow to running status
            {
                let mut lock = initial_wfc.write().await;
                lock.status = WorkflowStatus::Running;
            }

            // // Send the initial workflow context
            // let _ = tx.send(Ok(initial_wfc.clone())).await;

            let input = {
                let lock = initial_wfc.read().await;
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
                            wf.output = Some(Arc::new(serde_json::json!({"error": e.to_string()})));
                            drop(wf);
                            let _ = tx.send(Ok(initial_wfc.clone())).await;
                            return;
                        }
                    }
                }
            }

            // Prepare task map
            let mut idx = 0;
            let task_map = Arc::new(workflow.do_.0.iter().fold(
                IndexMap::<String, (u32, Arc<Task>)>::new(),
                |mut acc, task| {
                    task.iter().for_each(|(name, t)| {
                        acc.insert(name.clone(), (idx, Arc::new(t.clone())));
                    });
                    idx += 1;
                    acc
                },
            ));

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
                    wf.output = Some(Arc::new(serde_json::json!({"error": e.to_string()})));
                    drop(wf);
                    let _ = tx.send(Ok(initial_wfc.clone())).await;
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
                        wf.output = Some(Arc::new(serde_json::json!({"error": e.to_string()})));
                        drop(wf);
                        let _ = tx.send(Ok(initial_wfc.clone())).await;
                        return;
                    }
                }
            } else {
                input.clone()
            };
            task_context.set_input(transformed_input);
            task_context.add_position_name("do".to_string());

            // XXX Create a new workflow executor for task execution
            let task_executor = WorkflowExecutor {
                job_executors,
                http_client,
                workflow: workflow.clone(),
                workflow_context: initial_wfc.clone(),
            };

            // Execute tasks and update workflow context after each task
            let mut task_stream = task_executor.execute_task_list(task_map, task_context);

            while let Some(tc_result) = task_stream.next().await {
                match tc_result {
                    Ok(tc) => {
                        // Send updated workflow context through the channel
                        let mut wf = initial_wfc.write().await;
                        wf.output = Some(tc.output.clone());
                        wf.position = tc.position.clone();
                        drop(wf);
                        let _ = tx.send(Ok(initial_wfc.clone())).await;
                    }
                    Err(e) => {
                        tracing::debug!("Failed to execute task list: {:#?}", e);
                        let mut wf = initial_wfc.write().await;
                        wf.status = WorkflowStatus::Faulted;
                        wf.output = Some(Arc::new(serde_json::json!({"error": e.to_string()})));
                        drop(wf);
                        let _ = tx.send(Ok(initial_wfc.clone())).await;
                        break;
                    }
                }
            }

            // Process final task context for output transformation
            let lock = initial_wfc.read().await;
            if lock.status == WorkflowStatus::Running {
                drop(lock);

                // Get the final task context

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
                                        lock.output = Some(Arc::new(
                                            serde_json::json!({"error": e.to_string()}),
                                        ));
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

                drop(lock);

                // Send final workflow context
                let _ = tx.send(Ok(initial_wfc.clone())).await;
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
        });

        ReceiverStream::new(rx)
    }

    /// Executes a task list and returns a stream of task contexts.
    ///
    /// This function iterates through the task map, executing each task and
    /// yielding the result after each task is completed.
    fn execute_task_list(
        &self,
        task_map: Arc<IndexMap<String, (u32, Arc<Task>)>>,
        parent_task_context: TaskContext,
    ) -> impl Stream<Item = Result<Arc<TaskContext>>> + '_ {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Arc<TaskContext>>>(32);

        let workflow_context = self.workflow_context.clone();
        let job_executors = self.job_executors.clone();
        let http_client = self.http_client.clone();

        tokio::spawn(async move {
            let mut prev_context = parent_task_context;

            let mut task_iterator = task_map.iter();
            let mut next_task_pair = task_iterator.next();
            let lock = workflow_context.read().await;
            let mut status = lock.status.clone();
            drop(lock);

            while next_task_pair.is_some() && status == WorkflowStatus::Running {
                if let Some((name, (pos, task))) = next_task_pair {
                    tracing::info!("Executing task: {}", name);
                    prev_context.add_position_index(*pos);
                    let task_executor = TaskExecutor::new(
                        job_executors.clone(),
                        http_client.clone(),
                        name,
                        task.clone(),
                    );

                    // Get stream from task executor
                    let mut task_stream = task_executor
                        .execute(workflow_context.clone(), Arc::new(prev_context))
                        .await;

                    let mut last_context = None;

                    // Process each item in the stream from the task
                    while let Some(result) = task_stream.next().await {
                        match result {
                            Ok(result_task_context) => {
                                // Save the last context for flow control after the stream completes
                                last_context = Some(result_task_context.clone());

                                // Create a clone to send through the channel
                                let context_to_send = Arc::new(result_task_context);
                                if tx.send(Ok(context_to_send.clone())).await.is_err() {
                                    break; // Channel closed, receiver dropped
                                }
                            }
                            Err(e) => {
                                let error = anyhow::anyhow!("Failed to execute task: {:#?}", e);
                                let _ = tx.send(Err(error)).await;
                                workflow_context.write().await.status = WorkflowStatus::Faulted;
                                break;
                            }
                        }
                    }

                    // Continue with flow control based on the last context received
                    if let Some(mut result_task_context) = last_context {
                        let flow_directive = result_task_context.flow_directive.clone();

                        match flow_directive {
                            Then::Continue => {
                                result_task_context.remove_position();
                                prev_context = result_task_context;
                                next_task_pair = task_iterator.next();
                                tracing::info!(
                                    "Task Continue next: {}",
                                    next_task_pair.map(|p| p.0).unwrap_or(&"".to_string()),
                                );
                            }
                            Then::End => {
                                prev_context = result_task_context;
                                workflow_context.write().await.status = WorkflowStatus::Completed;
                                next_task_pair = None;
                            }
                            Then::Exit => {
                                prev_context = result_task_context;
                                next_task_pair = None;
                                workflow_context.write().await.status = WorkflowStatus::Completed;
                                tracing::info!("Exit Task (main): {}", name);
                            }
                            Then::TaskName(tname) => {
                                result_task_context.remove_position();
                                prev_context = result_task_context;
                                let mut it = task_map.iter();
                                for (k, v) in it.by_ref() {
                                    if k == &tname {
                                        next_task_pair = Some((k, v));
                                        tracing::info!("Jump to Task: {}", k);
                                        break;
                                    }
                                }
                                task_iterator = it;
                            }
                        }
                    } else {
                        // No context was received from the task execution
                        break;
                    }

                    let lock = workflow_context.read().await;
                    status = lock.status.clone();
                    drop(lock);
                } else {
                    workflow_context.write().await.status = WorkflowStatus::Completed;
                    break;
                }
            }
        });

        ReceiverStream::new(rx)
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
    use crate::workflow::execute::context::TaskContext;
    use app::module::test::create_hybrid_test_app;
    use indexmap::indexmap;
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
                }),
            );
            vec![map1, map2]
        };
        let task_list = TaskList(task_map_list);

        WorkflowSchema {
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
    fn test_execute_task_list() {
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
                context.clone(),
            );

            let mut idx = 0;
            let task_map = Arc::new(workflow.do_.0.iter().fold(
                IndexMap::<String, (u32, Arc<Task>)>::new(),
                |mut acc, task| {
                    task.iter().for_each(|(name, t)| {
                        acc.insert(name.clone(), (idx, Arc::new(t.clone())));
                    });
                    idx += 1;
                    acc
                },
            ));

            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );

            executor.workflow_context.write().await.status = WorkflowStatus::Running;
            let mut task_stream = executor.execute_task_list(task_map, task_context);
            let mut final_tc = None;
            while let Some(tc) = task_stream.next().await {
                match tc {
                    Ok(tc) => {
                        final_tc = Some(tc);
                    }
                    Err(_) => {
                        final_tc = None;
                        break;
                    }
                }
            }

            assert!(final_tc.is_some());
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
                context.clone(),
            );

            executor.workflow_context.write().await.status = WorkflowStatus::Running;

            executor.cancel().await;

            let status = executor.workflow_context.read().await.status.clone();
            assert_eq!(status, WorkflowStatus::Cancelled);
        })
    }

    #[test]
    fn test_execute_task_list_with_flow_directives() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let http_client = ReqwestClient::new(
                Some("test"),
                Some(std::time::Duration::from_secs(1)),
                Some(std::time::Duration::from_secs(1)),
                Some(1),
            )
            .unwrap();

            let mut workflow = create_test_workflow();

            let task_list = TaskList(vec![
                {
                    let mut map = HashMap::new();
                    map.insert(
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
                        }),
                    );
                    map
                },
                {
                    let mut map = HashMap::new();
                    map.insert(
                        "task2".to_string(),
                        Task::SetTask(SetTask {
                            set: serde_json::Map::new(),
                            export: None,
                            if_: None,
                            input: None,
                            metadata: serde_json::Map::new(),
                            output: None,
                            then: Some(FlowDirective::Variant1("task4".to_string())),
                            timeout: None,
                        }),
                    );
                    map
                },
                {
                    let mut map = HashMap::new();
                    map.insert(
                        "task3".to_string(),
                        Task::SetTask(SetTask {
                            set: serde_json::Map::new(),
                            export: None,
                            if_: None,
                            input: None,
                            metadata: serde_json::Map::new(),
                            output: None,
                            then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Exit)),
                            timeout: None,
                        }),
                    );
                    map
                },
                {
                    let mut map = HashMap::new();
                    map.insert(
                        "task4".to_string(),
                        Task::SetTask(SetTask {
                            set: serde_json::Map::new(),
                            export: None,
                            if_: None,
                            input: None,
                            metadata: serde_json::Map::new(),
                            output: None,
                            then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                            timeout: None,
                        }),
                    );
                    map
                },
            ]);

            workflow.do_ = task_list;

            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));

            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let executor = WorkflowExecutor::new(
                app_module,
                http_client,
                Arc::new(workflow.clone()),
                input.clone(),
                context.clone(),
            );

            let mut task_map = IndexMap::<String, (u32, Arc<Task>)>::new();
            task_map.insert(
                "task1".to_string(),
                (
                    0,
                    Arc::new(Task::SetTask(SetTask {
                        set: serde_json::Map::new(),
                        export: None,
                        if_: None,
                        input: None,
                        metadata: serde_json::Map::new(),
                        output: None,
                        then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Continue)),
                        timeout: None,
                    })),
                ),
            );
            task_map.insert(
                "task2".to_string(),
                (
                    1,
                    Arc::new(Task::SetTask(SetTask {
                        set: serde_json::Map::new(),
                        export: None,
                        if_: None,
                        input: None,
                        metadata: serde_json::Map::new(),
                        output: None,
                        then: Some(FlowDirective::Variant1("task4".to_string())),
                        timeout: None,
                    })),
                ),
            );
            task_map.insert(
                "task3".to_string(),
                (
                    2,
                    Arc::new(Task::SetTask(SetTask {
                        set: serde_json::Map::new(),
                        export: None,
                        if_: None,
                        input: None,
                        metadata: serde_json::Map::new(),
                        output: None,
                        then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Exit)),
                        timeout: None,
                    })),
                ),
            );
            task_map.insert(
                "task4".to_string(),
                (
                    3,
                    Arc::new(Task::SetTask(SetTask {
                        set: serde_json::Map::new(),
                        export: None,
                        if_: None,
                        input: None,
                        metadata: serde_json::Map::new(),
                        output: None,
                        then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                        timeout: None,
                    })),
                ),
            );

            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );

            executor.workflow_context.write().await.status = WorkflowStatus::Running;
            let mut task_stream = executor.execute_task_list(Arc::new(task_map), task_context);
            let mut final_tc = None;
            while let Some(tc) = task_stream.next().await {
                match tc {
                    Ok(tc) => {
                        final_tc = Some(tc);
                    }
                    Err(_) => {
                        final_tc = None;
                        break;
                    }
                }
            }

            assert!(final_tc.is_some());
            assert_eq!(
                final_tc
                    .unwrap()
                    .prev_position(1)
                    .last()
                    .unwrap()
                    .as_str()
                    .unwrap(),
                "task4".to_string()
            );
        })
    }

    #[test]
    fn test_execute_task_list_flow_exit_and_end() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
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

            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            // exit test
            {
                let executor = WorkflowExecutor::new(
                    app_module.clone(),
                    http_client.clone(),
                    Arc::new(workflow.clone()),
                    input.clone(),
                    context.clone(),
                );

                let task_map = Arc::new(indexmap! {
                    "exit_task".to_string() => (0, Arc::new(Task::SetTask(SetTask {
                        set: serde_json::Map::new(),
                        export: None,
                        if_: None,
                        input: None,
                        metadata: serde_json::Map::new(),
                        output: None,
                        then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Exit)),
                        timeout: None,
                    }))),
                    "unreachable_task".to_string() => (1, Arc::new(Task::SetTask(SetTask {
                        set: serde_json::Map::new(),
                        export: None,
                        if_: None,
                        input: None,
                        metadata: serde_json::Map::new(),
                        output: None,
                        then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Continue)),
                        timeout: None,
                    })))
                });

                let task_context = TaskContext::new(
                    None,
                    input.clone(),
                    Arc::new(Mutex::new(serde_json::Map::new())),
                );

                // exit case
                executor.workflow_context.write().await.status = WorkflowStatus::Running;
                let mut task_stream = executor.execute_task_list(task_map, task_context);
                let mut final_tc = None;
                while let Some(tc) = task_stream.next().await {
                    match tc {
                        Ok(tc) => {
                            final_tc = Some(tc);
                        }
                        Err(_) => {
                            final_tc = None;
                            break;
                        }
                    }
                }

                assert!(final_tc.is_some());
                assert_eq!(
                    final_tc
                        .unwrap()
                        .prev_position(1)
                        .last()
                        .unwrap()
                        .as_str()
                        .unwrap(),
                    "exit_task".to_string()
                );
            }

            // End test
            {
                let executor = WorkflowExecutor::new(
                    app_module.clone(),
                    http_client.clone(),
                    Arc::new(workflow.clone()),
                    input.clone(),
                    context.clone(),
                );

                let task_map = Arc::new(indexmap! {
                    "end_task".to_string() => (0, Arc::new(Task::SetTask(SetTask {
                        set: serde_json::Map::new(),
                        export: None,
                        if_: None,
                        input: None,
                        metadata: serde_json::Map::new(),
                        output: None,
                        then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                        timeout: None,
                    })))
                });

                let task_context = TaskContext::new(
                    None,
                    input.clone(),
                    Arc::new(Mutex::new(serde_json::Map::new())),
                );
                executor.workflow_context.write().await.status = WorkflowStatus::Running;
                let mut task_stream = executor.execute_task_list(task_map, task_context);
                let mut final_tc = None;
                while let Some(tc) = task_stream.next().await {
                    match tc {
                        Ok(tc) => {
                            final_tc = Some(tc);
                        }
                        Err(_) => {
                            final_tc = None;
                            break;
                        }
                    }
                }

                assert!(final_tc.is_some());
                assert_eq!(
                    final_tc
                        .unwrap()
                        .prev_position(1)
                        .last()
                        .unwrap()
                        .as_str()
                        .unwrap(),
                    "end_task".to_string()
                );
                assert_eq!(
                    executor.workflow_context.read().await.status,
                    WorkflowStatus::Completed
                );
            }
        })
    }

    #[test]
    fn test_execute_task_list_task_name_jump() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let http_client = ReqwestClient::new(
                Some("test"),
                Some(std::time::Duration::from_secs(1)),
                Some(std::time::Duration::from_secs(1)),
                Some(1),
            )
            .unwrap();

            // TaskName フロー指示のテスト
            let input = Arc::new(serde_json::json!({"test": "input"}));
            let context = Arc::new(serde_json::json!({}));

            let workflow = Arc::new(WorkflowSchema {
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
                output: None,
                do_: TaskList(vec![]),
            });

            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let executor = WorkflowExecutor::new(
                app_module.clone(),
                http_client.clone(),
                workflow.clone(),
                input.clone(),
                context.clone(),
            );

            let task_map = Arc::new(indexmap! {
                "start_task".to_string() => (0, Arc::new(Task::SetTask(SetTask {
                    set: serde_json::Map::new(),
                    export: None,
                    if_: None,
                    input: None,
                    metadata: serde_json::Map::new(),
                    output: None,
                    then: Some(FlowDirective::Variant1("jump_target".to_string())),
                    timeout: None,
                }))),
                "skipped_task".to_string() => (1, Arc::new(Task::SetTask(SetTask {
                    set: serde_json::Map::new(),
                    export: None,
                    if_: None,
                    input: None,
                    metadata: serde_json::Map::new(),
                    output: None,
                    then: Some(FlowDirective::Variant0(FlowDirectiveEnum::Exit)),
                    timeout: None,
                }))),
                "jump_target".to_string() => (2, Arc::new(Task::SetTask(SetTask {
                    set: serde_json::Map::new(),
                    export: None,
                    if_: None,
                    input: None,
                    metadata: serde_json::Map::new(),
                    output: None,
                    then: Some(FlowDirective::Variant0(FlowDirectiveEnum::End)),
                    timeout: None,
                })))
            });

            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );
            executor.workflow_context.write().await.status = WorkflowStatus::Running;
            let mut task_stream = executor.execute_task_list(task_map, task_context);
            let mut final_tc = None;
            while let Some(tc) = task_stream.next().await {
                match tc {
                    Ok(tc) => {
                        final_tc = Some(tc);
                    }
                    Err(_) => {
                        final_tc = None;
                        break;
                    }
                }
            }

            assert!(final_tc.is_some());
            assert_eq!(
                final_tc
                    .unwrap()
                    .prev_position(1)
                    .last()
                    .unwrap()
                    .as_str()
                    .unwrap(),
                "jump_target".to_string()
            );
            assert_eq!(
                executor.workflow_context.read().await.status,
                WorkflowStatus::Completed
            );
        })
    }
}
