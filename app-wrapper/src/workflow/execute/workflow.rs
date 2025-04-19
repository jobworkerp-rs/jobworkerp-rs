use super::{expression::UseExpression, job::JobExecutorWrapper, task::TaskExecutor};
use crate::workflow::definition::{
    transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
    workflow::{Task, WorkflowSchema},
};
use crate::workflow::execute::context::{self, TaskContext, Then, WorkflowContext, WorkflowStatus};
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

        if let Some(schema) = self.workflow.input.schema.as_ref() {
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
        let mut idx = 0;
        let task_map = Arc::new(self.workflow.do_.0.iter().fold(
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

        let wfr = self.workflow_context.read().await;
        let expression = match Self::expression(&wfr, Arc::new(task_context.clone())).await {
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
        let transformed_input = if let Some(from) = self.workflow.input.from.as_ref() {
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
        task_context.add_position_name("do".to_string()).await;
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
        task_map: Arc<IndexMap<String, (u32, Arc<Task>)>>,
        parent_task_context: TaskContext,
    ) -> Result<Arc<TaskContext>> {
        let mut prev_context = Arc::new(parent_task_context);

        let mut task_iterator = task_map.iter();
        let mut next_task_pair = task_iterator.next();
        let lock = self.workflow_context.read().await;
        let mut status = lock.status.clone();
        drop(lock);

        while next_task_pair.is_some() && status == WorkflowStatus::Running {
            if let Some((name, (pos, task))) = next_task_pair {
                // parent_task.position().addIndex(iter.previousIndex());
                tracing::info!("Executing task: {}", name);
                prev_context.add_position_index(*pos).await;
                let task_executor = TaskExecutor::new(
                    self.job_executors.clone(),
                    self.http_client.clone(),
                    name,
                    task.clone(),
                );
                let result_task_context = task_executor
                    .execute(self.workflow_context.clone(), prev_context)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to execute task: {:#?}", e))?;
                let flow_directive = result_task_context.flow_directive.clone();
                match flow_directive {
                    Then::Continue => {
                        result_task_context.remove_position().await;

                        prev_context = Arc::new(result_task_context);
                        next_task_pair = task_iterator.next();
                        tracing::info!(
                            "Task Continue next: {}",
                            next_task_pair.map(|p| p.0).unwrap_or(&"".to_string()),
                        );
                    }
                    Then::End => {
                        prev_context = Arc::new(result_task_context);
                        self.workflow_context.write().await.status = WorkflowStatus::Completed;
                        next_task_pair = None;
                    }
                    Then::Exit => {
                        prev_context = Arc::new(result_task_context);
                        next_task_pair = None;
                        tracing::info!("Exit Task: {}", name);
                    }
                    Then::TaskName(tname) => {
                        result_task_context.remove_position().await;

                        prev_context = Arc::new(result_task_context);
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
                    then: Some(FlowDirective {
                        subtype_0: Some(FlowDirectiveEnum::Continue),
                        subtype_1: None,
                    }),
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
                    then: Some(FlowDirective {
                        subtype_0: Some(FlowDirectiveEnum::End),
                        subtype_1: None,
                    }),
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
            let result = executor.execute_task_list(task_map, task_context).await;

            assert!(result.is_ok());
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
                            then: Some(FlowDirective {
                                subtype_0: Some(FlowDirectiveEnum::Continue),
                                subtype_1: None,
                            }),
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
                            then: Some(FlowDirective {
                                // remove flatten from subtype_1 (can only flatten structs and maps (got a string) )
                                // (from auto-generated code)
                                subtype_1: Some("task4".to_string()),
                                subtype_0: None,
                            }),
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
                            then: Some(FlowDirective {
                                subtype_0: Some(FlowDirectiveEnum::Exit),
                                subtype_1: None,
                            }),
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
                            then: Some(FlowDirective {
                                subtype_0: Some(FlowDirectiveEnum::End),
                                subtype_1: None,
                            }),
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
                        then: Some(FlowDirective {
                            subtype_0: Some(FlowDirectiveEnum::Continue),
                            subtype_1: None,
                        }),
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
                        then: Some(FlowDirective {
                            subtype_1: Some("task4".to_string()),
                            subtype_0: None,
                        }),
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
                        then: Some(FlowDirective {
                            subtype_0: Some(FlowDirectiveEnum::Exit),
                            subtype_1: None,
                        }),
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
                        then: Some(FlowDirective {
                            subtype_0: Some(FlowDirectiveEnum::End),
                            subtype_1: None,
                        }),
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
            let tc = executor
                .execute_task_list(Arc::new(task_map), task_context)
                .await
                .unwrap();

            // task4
            assert_eq!(
                tc.prev_position(2).await.last().unwrap().as_str().unwrap(),
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
                        then: Some(FlowDirective {
                            subtype_0: Some(FlowDirectiveEnum::Exit),
                            subtype_1: None,
                        }),
                        timeout: None,
                    }))),
                    "unreachable_task".to_string() => (1, Arc::new(Task::SetTask(SetTask {
                        set: serde_json::Map::new(),
                        export: None,
                        if_: None,
                        input: None,
                        metadata: serde_json::Map::new(),
                        output: None,
                        then: Some(FlowDirective {
                            subtype_0: Some(FlowDirectiveEnum::Continue),
                            subtype_1: None,
                        }),
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
                let result = executor
                    .execute_task_list(task_map, task_context)
                    .await
                    .unwrap();
                assert_eq!(
                    result
                        .prev_position(2)
                        .await
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
                        then: Some(FlowDirective {
                            subtype_0: Some(FlowDirectiveEnum::End),
                            subtype_1: None,
                        }),
                        timeout: None,
                    })))
                });

                let task_context = TaskContext::new(
                    None,
                    input.clone(),
                    Arc::new(Mutex::new(serde_json::Map::new())),
                );
                executor.workflow_context.write().await.status = WorkflowStatus::Running;
                let result = executor
                    .execute_task_list(task_map, task_context)
                    .await
                    .unwrap();
                assert_eq!(
                    // /0/end_task/set -> /
                    result
                        .prev_position(2)
                        .await
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
                    then: Some(FlowDirective {
                        // remove flatten from subtype_1 (can only flatten structs and maps (got a string) )
                        // (from auto-generated code)
                        subtype_1: Some("jump_target".to_string()),
                        subtype_0: None,
                    }),
                    timeout: None,
                }))),
                "skipped_task".to_string() => (1, Arc::new(Task::SetTask(SetTask {
                    set: serde_json::Map::new(),
                    export: None,
                    if_: None,
                    input: None,
                    metadata: serde_json::Map::new(),
                    output: None,
                    then: Some(FlowDirective {
                        subtype_0: Some(FlowDirectiveEnum::Exit),
                        subtype_1: None,
                    }),
                    timeout: None,
                }))),
                "jump_target".to_string() => (2, Arc::new(Task::SetTask(SetTask {
                    set: serde_json::Map::new(),
                    export: None,
                    if_: None,
                    input: None,
                    metadata: serde_json::Map::new(),
                    output: None,
                    then: Some(FlowDirective {
                        subtype_0: Some(FlowDirectiveEnum::End),
                        subtype_1: None,
                    }),
                    timeout: None,
                })))
            });

            let task_context = TaskContext::new(
                None,
                input.clone(),
                Arc::new(Mutex::new(serde_json::Map::new())),
            );
            executor.workflow_context.write().await.status = WorkflowStatus::Running;
            let result = executor
                .execute_task_list(task_map, task_context)
                .await
                .unwrap();

            assert_eq!(
                result
                    .prev_position(2)
                    .await
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
