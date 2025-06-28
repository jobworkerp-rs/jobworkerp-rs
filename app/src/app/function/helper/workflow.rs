use crate::app::{job::UseJobApp, worker::UseWorkerApp};
use anyhow::Result;
use command_utils::protobuf::ProtobufDescriptor;
use jobworkerp_base::error::JobWorkerError;
use proto::{
    jobworkerp::data::{ResponseType, RunnerData, RunnerId, WorkerData},
    ProtobufHelper,
};
use serde_json::{Map, Value};
use std::future::Future;
use tracing;

pub trait ReusableWorkflowHelper: UseJobApp + UseWorkerApp + ProtobufHelper + Send + Sync {
    const WORKFLOW_CHANNEL: Option<&str> = Some("workflow");
    fn handle_reusable_workflow<'a>(
        &'a self,
        name: &'a str,
        arguments: Option<Map<String, Value>>,
        runner_id: RunnerId,
        runner_data: RunnerData,
    ) -> impl Future<Output = Result<Value>> + Send + 'a {
        async move {
            tracing::debug!("found calling to reusable workflow: {:?}", &runner_data);
            match self
                .create_workflow(runner_id, runner_data, arguments)
                .await
            {
                Ok(_) => {
                    tracing::info!("Workflow created: {}", name);
                    Ok(serde_json::json!({"status": "ok"}))
                }
                Err(e) => {
                    tracing::error!("Failed to create workflow: {}", e);
                    Err(
                        JobWorkerError::RuntimeError(format!("Failed to create workflow: {e}"))
                            .into(),
                    )
                }
            }
        }
    }
    fn create_workflow(
        &self,
        runner_id: RunnerId,
        runner_data: RunnerData,
        definition: Option<Map<String, Value>>,
    ) -> impl Future<Output = Result<()>> + Send + '_ {
        async move {
            tracing::debug!("found calling to reusable workflow: {:?}", &runner_data);
            let arguments =
                definition.and_then(|a| self.parse_arguments_for_reusable_workflow(a).ok());

            if let Some(arguments) = arguments {
                tracing::trace!("workflow_data: {:?}", &arguments);
                let workflow_definition = serde_json::Value::Object(arguments);
                let document = workflow_definition.get("document").cloned();
                let workflow_name = document
                    .as_ref()
                    .and_then(|t| t.get("name"))
                    .and_then(|t| t.as_str().map(|s| s.to_string()))
                    .unwrap_or(runner_data.name.clone());
                let workflow_description = document
                    .as_ref()
                    .and_then(|d| d.get("summary"))
                    .and_then(|d| d.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();

                let settings = serde_json::json!({
                    "json_data": workflow_definition.to_string()
                });
                let runner_settings_descriptor =
                    Self::parse_runner_settings_schema_descriptor(&runner_data).map_err(|e| {
                        anyhow::anyhow!(
                            "Failed to parse runner_settings schema descriptor: {:#?}",
                            e
                        )
                    })?;
                let runner_settings = if let Some(ope_desc) = runner_settings_descriptor {
                    tracing::debug!("runner settings schema exists: {:#?}", &settings);
                    ProtobufDescriptor::json_value_to_message(ope_desc, &settings, true).map_err(
                        |e| anyhow::anyhow!("Failed to parse runner_settings schema: {:#?}", e),
                    )?
                } else {
                    tracing::debug!("runner settings schema empty");
                    vec![]
                };

                let data = WorkerData {
                    name: workflow_name.to_string(),
                    description: workflow_description.to_string(),
                    runner_id: Some(runner_id),
                    runner_settings,
                    channel: Self::WORKFLOW_CHANNEL.map(|s| s.to_string()),
                    response_type: ResponseType::Direct as i32,
                    broadcast_results: true,
                    ..Default::default()
                };
                if let Some(w) = self.worker_app().find_by_name(&workflow_name).await? {
                    tracing::info!("Workflow already exists: {:?}", w);
                    Err(
                        JobWorkerError::AlreadyExists(format!("Workflow already exists: {w:?}"))
                            .into(),
                    )
                } else {
                    let worker = self.worker_app().create(&data).await;
                    match worker {
                        Ok(worker) => {
                            tracing::info!("Reusable Workflow Worker created: {:?}", worker);
                            Ok(())
                        }
                        Err(e) => {
                            tracing::error!("Failed to create worker: {}", e);
                            Err(e)
                        }
                    }
                }
            } else {
                tracing::warn!("Workflow data is not found");
                Err(anyhow::anyhow!(
                    "Workflow creation requires a workflow json arguments.",
                ))
            }
        }
    }
    fn parse_as_json_and_string_with_key_or_noop(
        &self,
        key: &str,
        mut value: Map<String, Value>,
    ) -> Result<Map<String, Value>> {
        if let Some(candidate_value) = value.remove(key) {
            if candidate_value.is_object()
                && candidate_value.as_object().is_some_and(|o| !o.is_empty())
            {
                match candidate_value {
                    Value::Object(obj) => Ok(obj.clone()),
                    _ => Ok(Map::new()),
                }
            } else if candidate_value.is_string() {
                match candidate_value {
                    Value::String(s) if !s.is_empty() => {
                        let parsed = serde_json::from_str::<Value>(s.as_str()).or_else(|e| {
                            tracing::warn!("Failed to parse string as json: {}", e);
                            serde_yaml::from_str::<Value>(s.as_str()).inspect_err(|e| {
                                tracing::warn!("Failed to parse string as yaml: {}", e);
                            })
                        });
                        if let Ok(parsed_value) = parsed {
                            if parsed_value.is_object()
                                && parsed_value.as_object().is_some_and(|o| !o.is_empty())
                            {
                                match parsed_value {
                                    Value::Object(obj) => Ok(obj),
                                    _ => Ok(Map::new()),
                                }
                            } else {
                                tracing::warn!(
                                    "data is not an object(logic error): {:#?}",
                                    &parsed_value
                                );
                                Ok(value)
                            }
                        } else {
                            tracing::warn!(
                                "data string is not a valid json: {:#?}, {:?}",
                                &parsed,
                                &s
                            );
                            Ok(value)
                        }
                    }
                    _ => {
                        tracing::warn!(
                            "data is not a valid json(logic error): {:#?}",
                            &candidate_value
                        );
                        Ok(value)
                    }
                }
            } else {
                tracing::warn!(
                    "data key:{} is not a valid json: {:#?}",
                    key,
                    &candidate_value
                );
                Ok(value)
            }
        } else {
            Ok(value)
        }
    }

    fn parse_arguments_for_reusable_workflow(
        &self,
        arguments: Map<String, Value>,
    ) -> Result<Map<String, Value>> {
        let arguments = self.parse_as_json_and_string_with_key_or_noop("arguments", arguments)?;
        let arguments = self.parse_as_json_and_string_with_key_or_noop("settings", arguments)?;
        self.parse_as_json_and_string_with_key_or_noop("workflow_data", arguments)
    }
}
