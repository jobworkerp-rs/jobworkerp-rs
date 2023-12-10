/*
// TODO implement and TEST
use crate::worker::runner::Runner;
use anyhow::Result;
use async_trait::async_trait;
// use common::util::option::ToResult;
// use futures::{StreamExt, TryStreamExt};
// use infra::error::JobWorkerError;
//use k8s_openapi::api::batch::v1::Job;
use serde::Deserialize;
// use serde_json::json;

// use kube::{
//     api::{Api, DeleteParams, PostParams, WatchEvent, WatchParams},
//     Client,
// };

// TODO use if you need (not using in default)
// parsed operation (for worker)
#[derive(Debug, Deserialize, Clone)]
pub struct K8SOperation {
    namespace: Option<String>,
    //    job: Job, // yaml or json
}

#[derive(Debug, Clone)]
pub struct K8SRunner {
    // api: Api<Job>,
    //    job: Job,
}

impl K8SRunner {
    async fn new(operation: &str) -> Result<K8SRunner> {
        // let client = Client::try_default()
        //     .await
        //     .map_err(JobWorkerError::KubeClientError)?;
        // let parsed =
        //     serde_json::from_str::<K8SOperation>(operation).map_err(JobWorkerError::SerdeJsonError);
        // let parsed = if parsed.is_err() {
        //     serde_yaml::from_str::<K8SOperation>(operation).map_err(JobWorkerError::SerdeYamlError)
        // } else {
        //     parsed
        // }?;
        // // check job name existance
        // if parsed.job.metadata.name.is_none() {
        //     return Err(JobWorkerError::ParseError(format!(
        //         "k8s job name not found: {:?}",
        //         &parsed,
        //     ))
        //     .into());
        // }

        // TODO
        // let namespace = parsed.namespace.unwrap_or_else(|| "default".into());
        Ok(Self {
            // api: Api::namespaced(client, &namespace),
            // job: parsed.job,
        })
    }
}

#[async_trait]
impl Runner for K8SRunner {
    fn name(&self) -> String {
        format!(
            "K8SRunner",
            // self.job.metadata.name.clone().unwrap_or_default()
        )
    }
    async fn run(&mut self, _arg: Vec<u8>) -> Result<Vec<Vec<u8>>> {
        tracing::info!("run k8s worker");
        // let pp = PostParams::default();
        // let jn = &self
        //     .job
        //     .metadata
        //     .name
        //     .clone()
        //     .to_result(|| JobWorkerError::ParseError("cannot find job name".to_string()))?;

        // self.api
        //     .create(&pp, &self.job)
        //     .await
        //     .map_err(JobWorkerError::KubeClientError)?;

        // // See if it ran to completion
        // let wp = WatchParams::default()
        //     .fields(&format!("metadata.name={}", jn))
        //     .timeout(20);
        // let mut stream = self.api.watch(&wp, "").await?.boxed();

        // //これで大丈夫か検証する? (defaultが空なのは安全か?)
        // while let Some(status) = stream.try_next().await? {
        //     match status {
        //         WatchEvent::Added(s) => {
        //             tracing::info!("Added {:?}", s.metadata)
        //         }
        //         WatchEvent::Modified(s) => {
        //             let current_status = s.status.clone().expect("Status is missing");
        //             match current_status.completion_time {
        //                 Some(t) => {
        //                     tracing::info!("Modified: {:?} is complete: {:?}", s.metadata, t);
        //                     break;
        //                 }
        //                 _ => tracing::info!("Modified: {:?} is running", s.metadata),
        //             }
        //         }
        //         WatchEvent::Deleted(s) => tracing::info!("Deleted {:?}", s.metadata),
        //         WatchEvent::Error(s) => tracing::error!("error event: {:?}", s),
        //         _ => {} // Bookmark? (unexpected?)
        //     }
        // }
        // // TODO get process log and return

        // // Clean up the old job record..
        // tracing::info!("Deleting the job record.");
        // self.api
        //     .delete(jn, &DeleteParams::background().dry_run())
        //     .await?;
        // self.api.delete(jn, &DeleteParams::background()).await?;
        Ok(vec![vec![1u8]]) // success
    }

    fn cancel(&mut self) {
        todo!()
    }
}

// // 用意できるまでignore
// // need to be available k8s locally (kubectl)
// #[tokio::test]
// #[ignore]
// async fn runner_test() -> Result<()> {
//     use serde_json::json;
//     let job_name = "test-job";
//     let job_json = json!({
//         "apiVersion": "batch/v1",
//         "kind": "Job",
//         "metadata": {
//             "name": &job_name
//         },
//         "spec": {
//             "template": {
//                 "metadata": {
//                     "name": "sleep-job-pod"
//                 },
//                 "spec": {
//                     "containers": [{
//                         "name": "sleep",
//                         "image": "alpine:latest",
//                         "command": ["sh", "-c"],
//                         "args": [
//                           "echo Hello World;",
//                           "echo ${ARGS};",
//                           "sleep 3;",
//                         ]
//                     }],
//                     "restartPolicy": "Never",
//                 }
//             }
//         }
//     });

//     let op = format!(
//         r#"{{
//     "namespace": "default",
//     "job": {}
//     }}"#,
//         job_json
//     );
//     println!("operation: {}", &op);
//     let mut runner = K8SRunner::new(op.as_str()).await?;
//     let res = runner.run("".as_bytes().to_vec()).await;
//     tracing::info!("result: {:?}", res);
//     assert!(res.is_ok());
//     Ok(())
// }
 */
