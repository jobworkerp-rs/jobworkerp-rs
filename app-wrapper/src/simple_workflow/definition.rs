// Note: Auto-generated code package. Do not modify manually.

use anyhow::{anyhow, Result};
use infra_utils::infra::net::reqwest::{self, ReqwestClient};
use jobworkerp_runner::jobworkerp::runner::workflow_args::WorkflowSource;
use serde::de::DeserializeOwned;
use url::Url;

// Code generated by `typify` based on `json-schema/workflow.json`
pub mod transform;
pub mod workflow;

pub const RUNNER_SETTINGS_METADATA_LABEL: &str = "settings";
pub const WORKER_PARAMS_METADATA_LABEL: &str = "function";
pub const WORKER_NAME_METADATA_LABEL: &str = "name";

pub struct WorkflowLoader {
    pub http_client: reqwest::ReqwestClient,
}

impl WorkflowLoader {
    pub fn new(http_client: ReqwestClient) -> Result<Self> {
        Ok(Self { http_client })
    }
    pub async fn load_workflow_source(
        &self,
        source: &WorkflowSource,
    ) -> Result<workflow::WorkflowSchema> {
        match source {
            WorkflowSource::Url(url) => self
                .load_workflow(Some(url.as_str()), None)
                .await
                .map_err(|e| anyhow!("Failed to load workflow from url: {}", e)),
            WorkflowSource::JsonData(data) => self
                .load_workflow(None, Some(data.as_str()))
                .await
                .map_err(|e| anyhow!("Failed to load workflow from json: {}", e)),
        }
    }
    pub async fn load_workflow(
        &self,
        url_or_path: Option<&str>,
        json_or_yaml_data: Option<&str>,
    ) -> Result<workflow::WorkflowSchema> {
        let wf = if let Some(url_or_path) = url_or_path {
            tracing::debug!("workflow url_or_path: {}", url_or_path);
            self.load_url_or_path::<workflow::WorkflowSchema>(url_or_path)
                .await
        } else if let Some(data) = json_or_yaml_data {
            tracing::debug!("workflow string_data: {}", data);
            // load as do_ task list (json or yaml)
            let do_task: workflow::DoTask =
                serde_json::from_str(data).or_else(|_| serde_yaml::from_str(data))?;
            Ok(workflow::WorkflowSchema {
                do_: do_task.do_,
                input: do_task.input.unwrap_or_default(),
                output: do_task.output,
                document: workflow::Document::default(),
            })
        } else {
            Err(anyhow!("url_or_path or json_or_yaml_data is required"))
        }?;
        Ok(wf)
    }
}

pub trait UseLoadUrlOrPath {
    fn http_client(&self) -> &reqwest::ReqwestClient;
    fn load_url_or_path<T: DeserializeOwned + Clone>(
        &self,
        url_or_path: &str,
    ) -> impl std::future::Future<Output = Result<T>> + Send {
        let http_client = self.http_client().clone();
        async move {
            let body = if let Ok(url) = url_or_path.parse::<Url>() {
                let res = http_client.client().get(url.clone()).send().await?;
                if res.status().is_success() {
                    let body = res.text().await?;
                    Ok(body)
                } else {
                    Err(anyhow!(
                        "Failed to load yaml: {}, status: {}",
                        &url,
                        res.status()
                    ))
                }
            } else {
                // TODO name reference (inner yaml, public catalog)
                let body = std::fs::read_to_string(url_or_path)?;
                Ok(body)
            }?;
            let yaml: T = serde_json::from_str(&body).or_else(|e1| {
                serde_yaml::from_str(&body).map_err(|e2| {
                    anyhow!(
                        "Failed to parse data as yaml or json: {}\nas json: {:?}\nas yaml: {:?}",
                        url_or_path,
                        e1,
                        e2
                    )
                })
            })?;
            Ok(yaml)
        }
    }
}
impl UseLoadUrlOrPath for WorkflowLoader {
    fn http_client(&self) -> &reqwest::ReqwestClient {
        &self.http_client
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use infra_utils::infra::net::reqwest::ReqwestClient;

    use crate::simple_workflow::definition::workflow::{self, FunctionOptions};
    // use tracing::Level;

    // parse example flow yaml
    #[tokio::test]
    async fn test_parse_example_flow_yaml() -> Result<(), Box<dyn std::error::Error>> {
        // command_utils::util::tracing::tracing_init_test(Level::DEBUG);
        let http_client =
            ReqwestClient::new(Some("test client"), Some(Duration::from_secs(30)), Some(2))?;
        let loader = super::WorkflowLoader::new(http_client)?;
        let flow = loader
            .load_workflow(Some("test-files/ls-test.yaml"), None)
            .await?;
        println!("{:#?}", flow);
        assert_eq!(flow.document.title, Some("Workflow test (ls)".to_string()));
        assert_eq!(flow.document.name.as_str(), "ls-test");
        assert!(flow
            .input
            .schema
            .as_ref()
            .is_some_and(|s| { s.json_schema().is_some() }));

        let run_task = match &flow.do_.0[0]["ListWorker"] {
            workflow::Task::RunTask(run_task) => run_task,
            _ => panic!("unexpected task type"),
        };
        assert!(run_task.metadata.is_empty());
        assert!(run_task
            .output
            .as_ref()
            .is_some_and(|o| o.as_.as_ref().is_some_and(|s| {
                match s {
                    workflow::OutputAs::Variant0(_v) => true, // string
                    _ => false,
                }
            })));
        assert!(run_task.input.as_ref().is_none());
        match run_task.run.clone() {
            workflow::RunTaskConfiguration {
                function:
                    workflow::Function {
                        arguments,
                        options,
                        runner_name,
                        settings,
                    },
                await_,
                ..
            } => {
                assert_eq!(runner_name, "COMMAND".to_string());
                assert_eq!(
                    serde_json::Value::Object(arguments),
                    serde_json::json!({
                        "command": "ls",
                        "args": ["${.}"]
                    })
                );
                assert_eq!(serde_json::Value::Object(settings), serde_json::json!({}));
                let mut opts = FunctionOptions::default();
                opts.channel = Some("workflow".to_string());
                opts.store_failure = Some(true);
                opts.store_success = Some(true);
                opts.use_static = Some(false);
                assert_eq!(options, Some(opts));
                assert!(await_); // default true
            } // _ => panic!("unexpected script variant"),
        }
        let _for_task = match &flow.do_.0[1]["EachFileIteration"] {
            workflow::Task::ForTask(for_task) => for_task,
            f => panic!("unexpected task type: {:#?}", f),
        };
        // println!("====FOR: {:#?}", _for_task);

        Ok(())
    }
}
