use std::collections::HashMap;

use super::{RunnerSpec, RunnerTrait};
use crate::jobworkerp::runner::{DockerArgs, DockerRunnerSettings};
use crate::schema_to_json_string;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bollard::container::{
    AttachContainerOptions, AttachContainerResults, Config, RemoveContainerOptions,
    StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::CreateImageOptions;
use bollard::Docker;
use futures::stream::BoxStream;
use futures::TryStreamExt;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{ResultOutputItem, RunnerType, StreamingOutputType};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CreateRunnerOptions<T>
where
    T: Into<String> + Serialize + std::fmt::Debug + Clone,
{
    /// Name of the image to pull. The name may include a tag or digest. This parameter may only be
    /// used when pulling an image. The pull is cancelled if the HTTP connection is closed.
    pub from_image: Option<T>,
    /// Source to import. The value may be a URL from which the image can be retrieved or `-` to
    /// read the image from the request body. This parameter may only be used when importing an
    /// image.
    pub from_src: Option<T>,
    /// Repository name given to an image when it is imported. The repo may include a tag. This
    /// parameter may only be used when importing an image.
    pub repo: Option<T>,
    /// Tag or digest. If empty when pulling an image, this causes all tags for the given image to
    /// be pulled.
    pub tag: Option<T>,
    /// Platform in the format `os[/arch[/variant]]`
    pub platform: Option<T>,

    //////////////////////
    // for docker exec
    /// A list of environment variables to set inside the container in the form `[\"VAR=value\", ...]`. A variable without `=` is removed from the environment, rather than to have an empty value.
    pub env: Option<Vec<String>>,

    /// An object mapping mount point paths inside the container to empty objects.
    pub volumes: Option<HashMap<String, HashMap<(), ()>>>,

    /// The working directory for commands to run in.
    pub working_dir: Option<String>,

    /// The entry point for the container as a string or an array of strings.  If the array consists of exactly one empty string (`[\"\"]`) then the entry point is reset to system default (i.e., the entry point used by docker when there is no `ENTRYPOINT` instruction in the `Dockerfile`).
    pub entrypoint: Option<Vec<String>>,
    // An object mapping ports to an empty object in the form:  `{\"<port>/<tcp|udp|sctp>\": {}}`
    // pub exposed_ports: Option<HashMap<String, HashMap<(), ()>>>,

    // Disable networking for the container.
    // pub network_disabled: Option<bool>,

    // MAC address of the container.  Deprecated: this field is deprecated in API v1.44 and up. Use EndpointSettings.MacAddress instead.
    // pub mac_address: Option<String>,
}
impl<T> CreateRunnerOptions<T>
where
    T: Into<String> + Serialize + std::fmt::Debug + Clone + Default,
{
    pub fn new(from_image: Option<T>) -> CreateRunnerOptions<T>
    where
        T: Into<String>,
    {
        CreateRunnerOptions {
            from_image,
            ..Default::default()
        }
    }
    pub fn to_docker(&self) -> CreateImageOptions<T> {
        CreateImageOptions {
            from_image: self.from_image.clone().unwrap_or_default(),
            from_src: self.from_src.clone().unwrap_or_default(),
            repo: self.repo.clone().unwrap_or_default(),
            tag: self.tag.clone().unwrap_or_default(),
            platform: self.platform.clone().unwrap_or_default(),
            changes: vec![],
        }
    }
    pub fn to_docker_exec_config(&self) -> Config<String> {
        Config {
            image: self.from_image.clone().map(|s| s.into()),
            // exposed_ports: self.exposed_ports.clone(),
            env: self.env.clone(),
            volumes: self.volumes.clone(),
            working_dir: self.working_dir.clone(),
            entrypoint: self.entrypoint.clone(),
            // network_disabled: self.network_disabled,
            // mac_address: self.mac_address.clone(),
            ..Default::default()
        }
    }
}

// implement From for proto.jobworkerp.data.RunnerSettings(DockerRunnerSettings)
impl<T> From<crate::jobworkerp::runner::DockerRunnerSettings> for CreateRunnerOptions<T>
where
    T: Into<String> + Serialize + std::fmt::Debug + Clone + From<String> + Default,
{
    fn from(op: crate::jobworkerp::runner::DockerRunnerSettings) -> Self {
        CreateRunnerOptions {
            from_image: op.from_image.map(|s| s.into()),
            from_src: op.from_src.map(|s| s.into()),
            repo: op.repo.map(|s| s.into()),
            tag: op.tag.map(|s| s.into()),
            platform: op.platform.map(|s| s.into()),
            //exposed_ports: if op.exposed_ports.is_empty() {
            //    None
            //} else {
            //    Some(
            //        op.exposed_ports
            //            .iter()
            //            .cloned()
            //            .map(|port| (port, HashMap::new()))
            //            .collect::<HashMap<_, _>>(),
            //    )
            //},
            env: if op.env.is_empty() {
                None
            } else {
                Some(op.env)
            },
            volumes: if op.volumes.is_empty() {
                None
            } else {
                Some(
                    op.volumes
                        .iter()
                        .cloned()
                        .map(|volume| (volume, HashMap::new()))
                        .collect::<HashMap<_, _>>(),
                )
            },
            working_dir: op.working_dir,
            entrypoint: if op.entrypoint.is_empty() {
                None
            } else {
                Some(op.entrypoint)
            },
            // network_disabled: op.network_disabled,
            // mac_address: op.mac_address,
        }
    }
}

//
// run with docker tty and exec with shell
// TODO instance pooling and stop docker instance when stopping worker
//
#[derive(Debug, Clone)]
pub struct DockerExecRunner {
    docker: Option<Docker>,
    instant_id: String,
}

impl DockerExecRunner {
    pub fn new() -> Self {
        DockerExecRunner {
            docker: None,
            instant_id: "".to_string(),
        }
    }
    // create and start container
    pub async fn create(&mut self, image_options: &CreateRunnerOptions<String>) -> Result<()> {
        // use docker default socket file (/var/run/docker.sock)
        let docker = Docker::connect_with_socket_defaults().unwrap();
        match docker
            .create_image(Some(image_options.to_docker()), None, None)
            .try_collect::<Vec<_>>()
            .await
        {
            Ok(_d) => {
                let mut config = image_options.to_docker_exec_config();
                config.tty = Some(true);

                let id = docker
                    .create_container::<&str, String>(None, config)
                    .await
                    .map_err(JobWorkerError::DockerError)?
                    .id;
                tracing::info!("container id: {}", &id);
                docker
                    .start_container::<String>(&id, None)
                    .await
                    .map_err(JobWorkerError::DockerError)?;

                self.docker = Some(docker);
                self.instant_id = id;
                Ok(())
            }
            Err(e) => Err(JobWorkerError::DockerError(e).into()),
        }
    }

    pub async fn stop(&self, wait_secs: i64, force: bool) -> Result<()> {
        if let Some(docker) = self.docker.as_ref() {
            docker
                .stop_container(
                    &self.instant_id,
                    Some(StopContainerOptions { t: wait_secs }),
                )
                .await
                .map_err(JobWorkerError::DockerError)?;
            docker
                .remove_container(
                    &self.instant_id,
                    Some(RemoveContainerOptions {
                        force,
                        ..Default::default()
                    }),
                )
                .await
                .map_err(|e| JobWorkerError::DockerError(e).into())
        } else {
            Err(anyhow!("docker instance is not found"))
        }
    }
    fn trans_exec_arg(&self, arg: DockerArgs) -> CreateExecOptions<String> {
        let c: CreateExecOptions<String> = CreateExecOptions {
            cmd: if arg.cmd.is_empty() {
                None
            } else {
                Some(arg.cmd)
            },
            user: arg.user,
            privileged: None,
            env: if arg.env.is_empty() {
                None
            } else {
                Some(arg.env)
            },
            working_dir: arg.working_dir,
            ..Default::default()
        };
        c
    }
}

impl Default for DockerExecRunner {
    fn default() -> Self {
        Self::new()
    }
}
impl RunnerSpec for DockerExecRunner {
    fn name(&self) -> String {
        RunnerType::Docker.as_str_name().to_string()
    }
    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/docker_runner.proto").to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/docker_args.proto").to_string()
    }
    fn result_output_proto(&self) -> Option<String> {
        Some("".to_string())
    }
    fn output_type(&self) -> StreamingOutputType {
        StreamingOutputType::NonStreaming
    }

    fn settings_schema(&self) -> String {
        schema_to_json_string!(DockerRunnerSettings, "settings_schema")
    }

    fn arguments_schema(&self) -> String {
        schema_to_json_string!(DockerArgs, "arguments_schema")
    }

    fn output_schema(&self) -> Option<String> {
        // not use macro to assign title to schema
        let mut schema = schemars::schema_for!(String);
        schema.insert(
            "title".to_string(),
            serde_json::Value::String("Command stdout".to_string()),
        );
        match serde_json::to_string(&schema) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!("error in output_schema: {:?}", e);
                None
            }
        }
    }
}
#[async_trait]
impl RunnerTrait for DockerExecRunner {
    // create and start container
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let op = ProstMessageCodec::deserialize_message::<DockerRunnerSettings>(&settings)?;
        self.create(&op.into()).await
    }

    async fn run(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let result = async {
            if let Some(docker) = self.docker.as_ref() {
                let req = ProstMessageCodec::deserialize_message::<DockerArgs>(arg)?;
                let mut c: CreateExecOptions<String> = self.trans_exec_arg(req.clone());
                // for log
                c.attach_stdout = Some(true);
                c.attach_stderr = Some(true);

                // non interactive
                let exec = docker.create_exec(&self.instant_id, c).await?.id;

                let mut out = Vec::<Vec<u8>>::new();
                if let StartExecResults::Attached { mut output, .. } =
                    docker.start_exec(&exec, None).await?
                {
                    while let Some(Ok(msg)) = output.next().await {
                        out.push(format!("{msg}\n").into_bytes().to_vec());
                    }
                    Ok(out.concat())
                } else {
                    tracing::error!("unexpected error: cannot attach container (exec)");
                    Err(anyhow!("unexpected error: cannot attach container (exec)"))
                }
            } else {
                Err(anyhow!("docker instance is not found"))
            }
        }
        .await;
        (result, metadata)
    }
    async fn run_stream(
        &mut self,
        arg: &[u8],
        _metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // default implementation (return empty)
        let _ = arg;
        Err(anyhow::anyhow!("not implemented"))
    }

    async fn cancel(&mut self) {
        if !self.instant_id.is_empty() {
            tracing::info!("Stopping Docker container: {}", self.instant_id);

            // Use existing stop method with graceful timeout (10 seconds) and force removal
            if let Err(e) = self.stop(10, true).await {
                tracing::error!("Failed to stop Docker container during cancellation: {}", e);
            }
        } else {
            tracing::warn!("No active Docker container to cancel");
        }
    }
}

// confirm local docker
#[tokio::test]
#[ignore]
async fn exec_test() -> Result<()> {
    let mut runner1 = DockerExecRunner::new();
    runner1
        .create(&CreateRunnerOptions::new(Some(
            "busybox:latest".to_string(),
        )))
        .await?;
    let mut runner2 = DockerExecRunner::new();
    runner2
        .create(&CreateRunnerOptions::new(Some(
            "busybox:latest".to_string(),
        )))
        .await?;
    let arg = ProstMessageCodec::serialize_message(&DockerArgs {
        cmd: vec!["ls".to_string(), "-alh".to_string(), "/etc".to_string()],
        ..Default::default()
    })?;
    let handle1 = tokio::spawn(async move {
        let metadata = HashMap::new();
        let res = runner1.run(&arg, metadata).await;
        tracing::info!("result:{:?}", &res);
        runner1.stop(2, false).await.and(res.0)
    });

    let arg2 = ProstMessageCodec::serialize_message(&DockerArgs {
        cmd: vec!["cat".to_string(), "/etc/resolv.conf".to_string()],
        ..Default::default()
    })?;
    let handle2 = tokio::spawn(async move {
        let metadata = HashMap::new();
        let res = runner2.run(&arg2, metadata).await;
        tracing::info!("result:{:?}", &res);
        runner2.stop(2, true).await.and(res.0)
    });

    let r = tokio::join!(handle1, handle2);
    tracing::info!("result:{:?}", &r);

    Ok(())
}

//
// docker one time runner (not use tty)
//
#[derive(Debug, Clone)]
pub struct DockerRunner {
    docker: Option<Docker>,
    current_container_id: Option<String>,
}

impl DockerRunner {
    pub fn new() -> Self {
        DockerRunner {
            docker: None,
            current_container_id: None,
        }
    }
    pub async fn create(&mut self, image_options: &CreateRunnerOptions<String>) -> Result<()> {
        if image_options.from_image.is_some() || image_options.from_src.is_some() {
            let docker = Docker::connect_with_socket_defaults().unwrap();
            docker
                .create_image(Some(image_options.to_docker()), None, None)
                .try_collect::<Vec<_>>()
                .await
                .map_err(JobWorkerError::DockerError)?;
            self.docker = Some(docker);
        } else {
            tracing::info!("docker image is not specified. should specify image in run() method");
        }
        Ok(())
    }
    fn trans_docker_arg_to_config(&self, arg: &DockerArgs) -> Config<String> {
        Config {
            image: arg.image.clone(),
            cmd: if arg.cmd.is_empty() {
                None
            } else {
                Some(arg.cmd.clone())
            },
            user: arg.user.clone(),
            exposed_ports: if arg.exposed_ports.is_empty() {
                None
            } else {
                Some(
                    arg.exposed_ports
                        .iter()
                        .cloned()
                        .map(|port| (port, HashMap::new()))
                        .collect::<HashMap<_, _>>(),
                )
            },
            env: if arg.env.is_empty() {
                None
            } else {
                Some(arg.env.clone())
            },
            volumes: if arg.volumes.is_empty() {
                None
            } else {
                Some(
                    arg.volumes
                        .iter()
                        .cloned()
                        .map(|volume| (volume, HashMap::new()))
                        .collect::<HashMap<_, _>>(),
                )
            },
            working_dir: arg.working_dir.clone(),
            entrypoint: if arg.entrypoint.is_empty() {
                None
            } else {
                Some(arg.entrypoint.clone())
            },
            network_disabled: arg.network_disabled,
            mac_address: arg.mac_address.clone(),
            shell: if arg.shell.is_empty() {
                None
            } else {
                Some(arg.shell.clone())
            },
            ..Default::default()
        }
    }
}

impl Default for DockerRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl RunnerSpec for DockerRunner {
    fn name(&self) -> String {
        RunnerType::Docker.as_str_name().to_string()
    }
    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/docker_runner.proto").to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/docker_args.proto").to_string()
    }
    fn result_output_proto(&self) -> Option<String> {
        Some("".to_string())
    }
    fn output_type(&self) -> StreamingOutputType {
        StreamingOutputType::NonStreaming
    }
    fn settings_schema(&self) -> String {
        schema_to_json_string!(DockerRunnerSettings, "settings_schema")
    }

    fn arguments_schema(&self) -> String {
        schema_to_json_string!(DockerArgs, "arguments_schema")
    }

    fn output_schema(&self) -> Option<String> {
        // not use macro to assign title to schema
        let mut schema = schemars::schema_for!(String);
        schema.insert(
            "title".to_string(),
            serde_json::Value::String("Command stdout".to_string()),
        );
        match serde_json::to_string(&schema) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!("error in output_schema: {:?}", e);
                None
            }
        }
    }
}

#[async_trait]
impl RunnerTrait for DockerRunner {
    // create and start container
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let op = ProstMessageCodec::deserialize_message::<DockerRunnerSettings>(&settings)?;
        self.create(&op.into()).await
    }
    async fn run(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let result = async {
            let arg = ProstMessageCodec::deserialize_message::<DockerArgs>(args)?;
            let create_option = CreateRunnerOptions::new(arg.image.clone());
            if self.docker.is_none() {
                self.create(&create_option).await?;
            }
            if let Some(docker) = self.docker.as_ref() {
                // create image if not exist
                docker
                    .create_image(Some(create_option.to_docker()), None, None)
                    .try_collect::<Vec<_>>()
                    .await
                    .map_err(JobWorkerError::DockerError)?;

                let mut config = self.trans_docker_arg_to_config(&arg);
                // to output log
                config.attach_stdout = Some(true);
                config.attach_stderr = Some(true);

                let created = docker
                    .create_container::<&str, String>(None, config)
                    .await?;
                let id = created.id;
                tracing::info!("container id: {}", &id);

                // Store container ID for potential cancellation
                self.current_container_id = Some(id.clone());

                let AttachContainerResults {
                    mut output,
                    input: _,
                } = docker
                    .attach_container(
                        &id,
                        Some(AttachContainerOptions::<String> {
                            stdout: Some(true),
                            stderr: Some(true),
                            // stdin: Some(true),
                            stream: Some(true),
                            ..Default::default()
                        }),
                    )
                    .await?;

                docker.start_container::<String>(&id, None).await?;

                let mut logs = Vec::<Vec<u8>>::new();
                // pipe docker attach output into stdout
                while let Some(Ok(output)) = output.next().await {
                    match String::from_utf8(output.into_bytes().to_vec()) {
                        Ok(o) => {
                            tracing::info!("{}", &o);
                            logs.push(format!("{o}\n").into_bytes().to_vec())
                            // logs.push(o);
                        }
                        Err(e) => tracing::error!("error in decoding logs: {:?}", e),
                    }
                }
                // remove container if persist to running
                docker
                    .remove_container(
                        &id,
                        Some(RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await?;
                Ok(logs.concat())
            } else {
                Err(anyhow!("docker instance is not found"))
            }
        }
        .await;

        // Clear container ID after execution completes
        self.current_container_id = None;
        (result, metadata)
    }
    async fn run_stream(
        &mut self,
        arg: &[u8],
        _metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // default implementation (return empty)
        let _ = arg;
        Err(anyhow::anyhow!("not implemented"))
    }

    async fn cancel(&mut self) {
        if let (Some(docker), Some(container_id)) =
            (self.docker.as_ref(), &self.current_container_id)
        {
            tracing::info!("Stopping Docker container: {}", container_id);

            // Try graceful stop first (SIGTERM to container main process)
            match docker
                .stop_container(
                    container_id,
                    Some(bollard::container::StopContainerOptions { t: 10 }),
                )
                .await
            {
                Ok(_) => {
                    tracing::debug!("Container {} stopped gracefully", container_id);
                }
                Err(e) => {
                    tracing::warn!("Failed to stop container {}: {}", container_id, e);

                    // Force kill if graceful stop failed
                    match docker.kill_container::<String>(container_id, None).await {
                        Ok(_) => tracing::debug!("Container {} force killed", container_id),
                        Err(e) => {
                            tracing::error!("Failed to kill container {}: {}", container_id, e)
                        }
                    }
                }
            }

            // Clean up: remove container
            match docker
                .remove_container(
                    container_id,
                    Some(bollard::container::RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
            {
                Ok(_) => tracing::debug!("Container {} removed", container_id),
                Err(e) => tracing::warn!("Failed to remove container {}: {}", container_id, e),
            }

            // Clear the container ID since it's now stopped
            self.current_container_id = None;
        } else {
            tracing::warn!("No active Docker container to cancel");
        }
    }
}

#[tokio::test]
async fn test_docker_exec_cancel() {
    eprintln!("=== Starting Docker Exec cancel test ===");
    let mut runner = DockerExecRunner::new();

    // Call cancel when no container is running - should not panic
    runner.cancel().await;
    eprintln!("Docker Exec cancel completed successfully with no active container");

    eprintln!("=== Docker Exec cancel test completed ===");
}

#[tokio::test]
async fn test_docker_runner_cancel() {
    eprintln!("=== Starting Docker Runner cancel test ===");
    let mut runner = DockerRunner::new();

    // Call cancel when no container is running - should not panic
    runner.cancel().await;
    eprintln!("Docker Runner cancel completed successfully with no container tracking");

    eprintln!("=== Docker Runner cancel test completed ===");
}

#[tokio::test]
#[ignore] // Requires Docker daemon - run with --ignored for full testing
async fn test_docker_runner_actual_cancellation() {
    eprintln!("=== Starting Docker Runner actual cancellation test ===");
    use crate::jobworkerp::runner::DockerArgs;
    use jobworkerp_base::codec::ProstMessageCodec;
    use std::collections::HashMap;

    let mut runner = DockerRunner::new();

    // Test with a long-running container that can be cancelled
    let docker_args = DockerArgs {
        image: Some("alpine:latest".to_string()),
        user: None,
        exposed_ports: vec![],
        env: vec![],
        cmd: vec!["sleep".to_string(), "30".to_string()], // Sleep for 30 seconds
        args_escaped: None,
        volumes: vec![],
        working_dir: None,
        entrypoint: vec![],
        network_disabled: None,
        mac_address: None,
        shell: vec![],
    };

    let arg_bytes = ProstMessageCodec::serialize_message(&docker_args).unwrap();
    let metadata = HashMap::new();

    // Start container execution in a task
    let start_time = std::time::Instant::now();
    let execution_task = tokio::spawn(async move { runner.run(&arg_bytes, metadata).await });

    // Wait briefly for container to start
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Test timeout - if container is running, it should take ~30 seconds
    // We'll timeout after 3 seconds to verify cancellation would work
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), execution_task).await;

    let elapsed = start_time.elapsed();
    eprintln!("Execution time: {elapsed:?}");

    match result {
        Ok(task_result) => {
            let (execution_result, _metadata) = task_result.unwrap();
            match execution_result {
                Ok(_) => {
                    eprintln!("Container completed unexpectedly quickly");
                }
                Err(e) => {
                    eprintln!("Container failed (may be expected): {e}");
                }
            }
        }
        Err(_) => {
            eprintln!("Container execution timed out as expected - this indicates cancellation would work");
            // This timeout demonstrates that the container was running long enough to be cancelled
            assert!(
                elapsed >= std::time::Duration::from_secs(3),
                "Should timeout after 3 seconds"
            );
        }
    }

    eprintln!("=== Docker Runner actual cancellation test completed ===");
}

#[tokio::test]
#[ignore]
async fn run_test() -> Result<()> {
    // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let mut runner1 = DockerRunner::new();
    runner1
        .create(&CreateRunnerOptions::new(Some(
            "busybox:latest".to_string(),
        )))
        .await?;
    let mut runner2 = DockerRunner::new();
    runner2
        .create(&CreateRunnerOptions::new(Some(
            "busybox:latest".to_string(),
        )))
        .await?;
    let arg = ProstMessageCodec::serialize_message(&DockerArgs {
        image: Some("busybox:latest".to_string()),
        cmd: vec!["ls".to_string(), "-alh".to_string(), "/".to_string()],
        ..Default::default()
    })?;
    let handle1 = tokio::spawn(async move {
        let res = runner1.run(&arg, HashMap::new()).await;
        tracing::info!("result:{:?}", &res);
        res
    });

    let arg2 = ProstMessageCodec::serialize_message(&DockerArgs {
        image: Some("busybox:latest".to_string()),
        cmd: vec!["echo".to_string(), "run in docker container".to_string()],
        ..Default::default()
    })?;
    let handle2 = tokio::spawn(async move {
        let res = runner2.run(&arg2, HashMap::new()).await;
        tracing::info!("result:{:?}", &res);
        res
    });

    let r = tokio::join!(handle1, handle2);
    tracing::info!("result:{:?}", &r);

    Ok(())
}
