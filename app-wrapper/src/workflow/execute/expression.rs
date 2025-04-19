use super::context::{TaskContext, WorkflowContext};
use crate::workflow::definition::workflow;
use anyhow::Result;
use std::{collections::BTreeMap, sync::Arc};

pub trait UseExpression {
    fn expression(
        workflow_context: &WorkflowContext,
        task_context: Arc<TaskContext>,
    ) -> impl std::future::Future<
        Output = Result<BTreeMap<String, Arc<serde_json::Value>>, Box<workflow::Error>>,
    > + Send {
        async move {
            let mut expression = BTreeMap::<String, Arc<serde_json::Value>>::new();
            expression.insert("input".to_string(), task_context.input.clone());
            expression.insert("output".to_string(), task_context.output.clone());
            expression.insert(
                "context".to_string(),
                Arc::new(serde_json::to_value(workflow_context).map_err(|e| {
                    // anyhow::anyhow!("Failed to serialize workflow context: {:#?}", e)
                    workflow::errors::ErrorFactory::create_from_serde_json(
                        &e,
                        Some("failed to serialize workflow context"),
                        None,
                        None,
                    )
                })?),
            );
            expression.insert(
                "runtime".to_string(),
                Arc::new(
                    serde_json::to_value(workflow_context.to_runtime_descriptor()).map_err(
                        |e| {
                            workflow::errors::ErrorFactory::create_from_serde_json(
                                &e,
                                Some("Failed to serialize runtime descriptor"),
                                None,
                                None,
                            )
                        },
                    )?,
                ),
            );
            expression.insert(
                "workflow".to_string(),
                Arc::new(
                    serde_json::to_value(workflow_context.to_descriptor()).map_err(|e| {
                        workflow::errors::ErrorFactory::create_from_serde_json(
                            &e,
                            Some("Failed to serialize workflow descriptor"),
                            None,
                            None,
                        )
                    })?,
                ),
            );
            expression.insert(
                "task".to_string(),
                Arc::new(
                    serde_json::to_value(task_context.to_descriptor()).map_err(|e| {
                        workflow::errors::ErrorFactory::create_from_serde_json(
                            &e,
                            Some("Failed to serialize task descriptor"),
                            None,
                            None,
                        )
                    })?,
                ),
            );
            {
                task_context
                    .context_variables
                    .lock()
                    .await
                    .iter()
                    .for_each(|(k, v)| {
                        expression.insert(k.clone(), Arc::new(v.clone()));
                    });
            }
            workflow_context
                .context_variables
                .lock()
                .await
                .iter()
                .for_each(|(k, v)| {
                    expression.insert(k.clone(), Arc::new(v.clone()));
                });
            Ok(expression)
        }
    }
}
