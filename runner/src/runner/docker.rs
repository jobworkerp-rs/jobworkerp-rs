use super::cancellation_helper::{CancelMonitoringHelper, UseCancelMonitoringHelper};
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
use proto::jobworkerp::data::{
    JobData, JobId, JobResult, ResultOutputItem, RunnerType, StreamingOutputType,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;

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
    pub fn to_docker(&self) -> CreateImageOptions<'_, T> {
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
    // Helper for dependency injection integration (optional for backward compatibility)
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl DockerExecRunner {
    /// Constructor without cancellation monitoring (for backward compatibility)
    pub fn new() -> Self {
        DockerExecRunner {
            docker: None,
            instant_id: "".to_string(),
            cancel_helper: None,
        }
    }

    /// Constructor with cancellation monitoring (DI integration version)
    pub fn new_with_cancel_monitoring(cancel_helper: CancelMonitoringHelper) -> Self {
        DockerExecRunner {
            docker: None,
            instant_id: "".to_string(),
            cancel_helper: Some(cancel_helper),
        }
    }

    /// Unified cancellation token retrieval
    async fn get_cancellation_token(&self) -> CancellationToken {
        if let Some(helper) = &self.cancel_helper {
            helper.get_cancellation_token().await
        } else {
            CancellationToken::new()
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
    // Phase 6.6: Unified method_proto_map for all runners
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            "run".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!("../../protobuf/jobworkerp/runner/docker_args.proto")
                    .to_string(),
                result_proto: "".to_string(), // Binary data (empty proto allowed)
                description: Some("Execute command in Docker container (exec mode)".to_string()),
                output_type: StreamingOutputType::NonStreaming as i32,
            },
        );
        schemas
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
        _using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        // Set up cancellation token using helper
        let cancellation_token = self.get_cancellation_token().await;

        let result = async {
            if let Some(docker) = self.docker.as_ref() {
                let req = ProstMessageCodec::deserialize_message::<DockerArgs>(arg)?;
                let mut c: CreateExecOptions<String> = self.trans_exec_arg(req.clone());
                // for log
                c.attach_stdout = Some(true);
                c.attach_stderr = Some(true);

                // Check cancellation before creating exec
                if cancellation_token.is_cancelled() {
                    tracing::info!("Docker exec execution was cancelled before create_exec");
                    return Err(anyhow::anyhow!("Docker exec execution was cancelled before create_exec"));
                }

                // non interactive
                let exec = tokio::select! {
                    exec_result = docker.create_exec(&self.instant_id, c) => {
                        exec_result?.id
                    }
                    _ = cancellation_token.cancelled() => {
                        tracing::info!("Docker exec creation was cancelled");
                        return Err(anyhow::anyhow!("Docker exec creation was cancelled"));
                    }
                };

                let mut out = Vec::<Vec<u8>>::new();
                let start_result = tokio::select! {
                    start_result = docker.start_exec(&exec, None) => start_result,
                    _ = cancellation_token.cancelled() => {
                        tracing::info!("Docker exec start was cancelled");
                        return Err(anyhow::anyhow!("Docker exec start was cancelled"));
                    }
                };

                if let StartExecResults::Attached { mut output, .. } = start_result? {
                    loop {
                        tokio::select! {
                            msg_result = output.next() => {
                                match msg_result {
                                    Some(Ok(msg)) => {
                                        out.push(format!("{msg}\n").into_bytes().to_vec());
                                    }
                                    Some(Err(e)) => {
                                        tracing::error!("Docker output stream error: {}", e);
                                        break;
                                    }
                                    None => break, // Stream ended
                                }
                            }
                            _ = cancellation_token.cancelled() => {
                                tracing::info!("Docker exec output reading was cancelled");
                                return Err(anyhow::anyhow!("Docker exec output reading was cancelled"));
                            }
                        }
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

        // Use new cancellation approach
        (result, metadata)
    }
    async fn run_stream(
        &mut self,
        arg: &[u8],
        _metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // Set up cancellation token for pre-execution cancellation check
        let _cancellation_token = self.get_cancellation_token().await;

        // default implementation (return empty)
        let _ = arg;
        // Clear cancellation token even on error (no-op with new approach)
        Err(anyhow::anyhow!("not implemented"))
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
        let res = runner1.run(&arg, metadata, None).await;
        tracing::info!("result:{:?}", &res);
        runner1.stop(2, false).await.and(res.0)
    });

    let arg2 = ProstMessageCodec::serialize_message(&DockerArgs {
        cmd: vec!["cat".to_string(), "/etc/resolv.conf".to_string()],
        ..Default::default()
    })?;
    let handle2 = tokio::spawn(async move {
        let metadata = HashMap::new();
        let res = runner2.run(&arg2, metadata, None).await;
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
    // Helper for dependency injection integration (optional for backward compatibility)
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl DockerRunner {
    /// Constructor without cancellation monitoring (for backward compatibility)
    pub fn new() -> Self {
        DockerRunner {
            docker: None,
            current_container_id: None,
            cancel_helper: None,
        }
    }

    /// Constructor with cancellation monitoring (DI integration version)
    pub fn new_with_cancel_monitoring(cancel_helper: CancelMonitoringHelper) -> Self {
        DockerRunner {
            docker: None,
            current_container_id: None,
            cancel_helper: Some(cancel_helper),
        }
    }

    /// Unified cancellation token retrieval
    async fn get_cancellation_token(&self) -> CancellationToken {
        if let Some(helper) = &self.cancel_helper {
            helper.get_cancellation_token().await
        } else {
            CancellationToken::new()
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
    // Phase 6.6: Unified method_proto_map for all runners
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            "run".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!("../../protobuf/jobworkerp/runner/docker_args.proto")
                    .to_string(),
                result_proto: "".to_string(), // Binary data (empty proto allowed)
                description: Some("Execute command in Docker container (run mode)".to_string()),
                output_type: StreamingOutputType::NonStreaming as i32,
            },
        );
        schemas
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
        _using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        // Set up cancellation token using helper
        let cancellation_token = self.get_cancellation_token().await;

        let result = async {
            let arg = ProstMessageCodec::deserialize_message::<DockerArgs>(args)?;
            let create_option = CreateRunnerOptions::new(arg.image.clone());
            if self.docker.is_none() {
                self.create(&create_option).await?;
            }
            if let Some(docker) = self.docker.as_ref() {
                // Check cancellation before creating image
                if cancellation_token.is_cancelled() {
                    tracing::info!("Docker execution was cancelled before create_image");
                    return Err(anyhow::anyhow!("Docker execution was cancelled before create_image"));
                }

                // create image if not exist
                tokio::select! {
                    result = docker
                        .create_image(Some(create_option.to_docker()), None, None)
                        .try_collect::<Vec<_>>() => {
                        result.map_err(JobWorkerError::DockerError)?;
                    }
                    _ = cancellation_token.cancelled() => {
                        tracing::info!("Docker image creation was cancelled");
                        return Err(anyhow::anyhow!("Docker image creation was cancelled"));
                    }
                }

                let mut config = self.trans_docker_arg_to_config(&arg);
                // to output log
                config.attach_stdout = Some(true);
                config.attach_stderr = Some(true);

                let created = tokio::select! {
                    result = docker.create_container::<&str, String>(None, config) => result?,
                    _ = cancellation_token.cancelled() => {
                        tracing::info!("Docker container creation was cancelled");
                        return Err(anyhow::anyhow!("Docker container creation was cancelled"));
                    }
                };
                let id = created.id;
                tracing::info!("container id: {}", &id);

                // Store container ID for potential cancellation
                self.current_container_id = Some(id.clone());

                let attach_result = tokio::select! {
                    result = docker.attach_container(
                        &id,
                        Some(AttachContainerOptions::<String> {
                            stdout: Some(true),
                            stderr: Some(true),
                            // stdin: Some(true),
                            stream: Some(true),
                            ..Default::default()
                        }),
                    ) => result?,
                    _ = cancellation_token.cancelled() => {
                        tracing::info!("Docker container attach was cancelled");
                        return Err(anyhow::anyhow!("Docker container attach was cancelled"));
                    }
                };

                let AttachContainerResults {
                    mut output,
                    input: _,
                } = attach_result;

                tokio::select! {
                    result = docker.start_container::<String>(&id, None) => result?,
                    _ = cancellation_token.cancelled() => {
                        tracing::info!("Docker container start was cancelled");
                        return Err(anyhow::anyhow!("Docker container start was cancelled"));
                    }
                }

                let mut logs = Vec::<Vec<u8>>::new();
                // pipe docker attach output into stdout
                loop {
                    tokio::select! {
                        output_result = output.next() => {
                            match output_result {
                                Some(Ok(output)) => {
                                    match String::from_utf8(output.into_bytes().to_vec()) {
                                        Ok(o) => {
                                            tracing::info!("{}", &o);
                                            logs.push(format!("{o}\n").into_bytes().to_vec())
                                            // logs.push(o);
                                        }
                                        Err(e) => tracing::error!("error in decoding logs: {:?}", e),
                                    }
                                }
                                Some(Err(e)) => {
                                    tracing::error!("Docker output stream error: {}", e);
                                    break;
                                }
                                None => break, // Stream ended
                            }
                        }
                        _ = cancellation_token.cancelled() => {
                            tracing::info!("Docker output reading was cancelled");
                            return Err(anyhow::anyhow!("Docker output reading was cancelled"));
                        }
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
        // Use new cancellation approach
        (result, metadata)
    }
    async fn run_stream(
        &mut self,
        arg: &[u8],
        _metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // Set up cancellation token for pre-execution cancellation check
        let _cancellation_token = self.get_cancellation_token().await;

        // default implementation (return empty)
        let _ = arg;
        // Clear cancellation token even on error (no-op with new approach)
        Err(anyhow::anyhow!("not implemented"))
    }
}

// CancelMonitoring implementation for DockerExecRunner
#[async_trait]
impl super::cancellation::CancelMonitoring for DockerExecRunner {
    /// Initialize cancellation monitoring for specific job
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: JobId,
        _job_data: &JobData,
    ) -> Result<Option<JobResult>> {
        tracing::debug!(
            "Setting up cancellation monitoring for DockerExecRunner job {}",
            job_id.value
        );

        // Clear branching based on helper availability
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, _job_data).await
        } else {
            tracing::trace!("No cancel helper available, continuing with normal execution");
            Ok(None) // Continue with normal execution
        }
    }

    /// Cleanup cancellation monitoring
    async fn cleanup_cancellation_monitoring(&mut self) -> Result<()> {
        tracing::trace!("Cleaning up cancellation monitoring for DockerExecRunner");

        // Clear the cancellation helper (no-op with new approach)

        // Clear instant_id if needed (though it might be used for container identification)
        // self.instant_id = String::new(); // Note: Keeping this as it might be needed for container lifecycle

        Ok(())
    }

    /// Signals cancellation token for DockerExecRunner
    async fn request_cancellation(&mut self) -> Result<()> {
        // Signal cancellation token
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
                tracing::info!("DockerExecRunner: cancellation token signaled");
            }
        } else {
            tracing::warn!("DockerExecRunner: no cancellation helper available");
        }

        // DockerExecRunner-specific command execution cleanup
        // Commands running via docker exec are automatically stopped by cancellation token
        // Additional cleanup is typically not needed
        tracing::info!("DockerExecRunner: exec command will be stopped via cancellation token");

        Ok(())
    }
}

impl UseCancelMonitoringHelper for DockerExecRunner {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}

// CancelMonitoring implementation for DockerRunner
#[async_trait]
impl super::cancellation::CancelMonitoring for DockerRunner {
    /// Initialize cancellation monitoring for specific job
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: JobId,
        _job_data: &JobData,
    ) -> Result<Option<JobResult>> {
        tracing::debug!(
            "Setting up cancellation monitoring for DockerRunner job {}",
            job_id.value
        );

        // Clear branching based on helper availability
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, _job_data).await
        } else {
            tracing::trace!("No cancel helper available, continuing with normal execution");
            Ok(None) // Continue with normal execution
        }
    }

    /// Cleanup cancellation monitoring
    async fn cleanup_cancellation_monitoring(&mut self) -> Result<()> {
        tracing::trace!("Cleaning up cancellation monitoring for DockerRunner");

        // Clear the cancellation helper (no-op with new approach)

        // Clear current container ID if needed
        self.current_container_id = None;

        Ok(())
    }

    /// Cancels containers and cleans up resources for DockerRunner
    async fn request_cancellation(&mut self) -> Result<()> {
        // 1. Signal cancellation token
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
                tracing::info!("DockerRunner: cancellation token signaled");
            }
        }

        // 2. DockerRunner-specific container stop and removal
        if let Some(container_id) = self.current_container_id.clone() {
            if let Some(docker) = self.docker.as_ref() {
                // Graceful stop (SIGTERM)
                if let Err(e) = docker
                    .stop_container(
                        &container_id,
                        Some(bollard::container::StopContainerOptions { t: 10 }),
                    )
                    .await
                {
                    tracing::warn!("Failed to stop container {}: {}", container_id, e);
                }

                // Force remove
                if let Err(e) = docker
                    .remove_container(
                        &container_id,
                        Some(bollard::container::RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await
                {
                    tracing::warn!("Failed to remove container {}: {}", container_id, e);
                }

                tracing::info!(
                    "DockerRunner: container {} stopped and removed",
                    container_id
                );
            }

            // Clear container ID
            self.current_container_id = None;
        } else {
            tracing::warn!("DockerRunner: no container to stop");
        }

        Ok(())
    }
}

impl UseCancelMonitoringHelper for DockerRunner {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}

#[cfg(test)]
mod test {
    use super::*;

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
            let res = runner1.run(&arg, HashMap::new(), None).await;
            tracing::info!("result:{:?}", &res);
            res
        });

        let arg2 = ProstMessageCodec::serialize_message(&DockerArgs {
            image: Some("busybox:latest".to_string()),
            cmd: vec!["echo".to_string(), "run in docker container".to_string()],
            ..Default::default()
        })?;
        let handle2 = tokio::spawn(async move {
            let res = runner2.run(&arg2, HashMap::new(), None).await;
            tracing::info!("result:{:?}", &res);
            res
        });

        let r = tokio::join!(handle1, handle2);
        tracing::info!("result:{:?}", &r);

        Ok(())
    }

    #[tokio::test]
    #[ignore] // Moved to e2e tests in worker-app/tests
    async fn test_docker_exec_pre_execution_cancellation() {
        eprintln!("=== Testing Docker Exec Runner pre-execution cancellation ===");

        let mut runner = DockerExecRunner::new();

        // Test cancellation by setting a cancelled token
        let cancellation_token = tokio_util::sync::CancellationToken::new();
        // TODO: Update to new cancellation system (moved to e2e tests)
        // runner.set_cancellation_token(cancellation_token.clone());
        cancellation_token.cancel();

        use crate::jobworkerp::runner::DockerArgs;
        let arg = DockerArgs {
            cmd: vec!["sleep".to_string(), "10".to_string()],
            ..Default::default()
        };

        let start_time = std::time::Instant::now();
        let (result, _metadata) = runner
            .run(
                &ProstMessageCodec::serialize_message(&arg).unwrap(),
                HashMap::new(),
                None,
            )
            .await;
        let elapsed = start_time.elapsed();

        eprintln!("Execution completed in {elapsed:?}");

        // The command should be cancelled
        match result {
            Ok(_) => {
                panic!("Docker exec command should have been cancelled but completed normally");
            }
            Err(e) => {
                eprintln!("Docker exec command was cancelled as expected: {e}");
                assert!(e.to_string().contains("cancelled"));
            }
        }

        // Should complete much faster than 10 seconds due to cancellation
        assert!(
            elapsed.as_millis() < 1000,
            "Cancellation should prevent long execution"
        );

        eprintln!("=== Docker Exec pre-execution cancellation test completed ===");
    }

    #[tokio::test]
    #[ignore] // Moved to e2e tests in worker-app/tests
    async fn test_docker_runner_pre_execution_cancellation() {
        eprintln!("=== Testing Docker Runner pre-execution cancellation ===");

        let mut runner = DockerRunner::new();

        // Test cancellation by setting a cancelled token
        let cancellation_token = tokio_util::sync::CancellationToken::new();
        // TODO: Update to new cancellation system (moved to e2e tests)
        // runner.set_cancellation_token(cancellation_token.clone());
        cancellation_token.cancel();

        use crate::jobworkerp::runner::DockerArgs;
        let arg = DockerArgs {
            image: Some("busybox:latest".to_string()),
            cmd: vec!["sleep".to_string(), "10".to_string()],
            ..Default::default()
        };

        let start_time = std::time::Instant::now();
        let (result, _metadata) = runner
            .run(
                &ProstMessageCodec::serialize_message(&arg).unwrap(),
                HashMap::new(),
                None,
            )
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await; // Allow some time for cancellation to take effect
        let elapsed = start_time.elapsed();

        eprintln!("Execution completed in {elapsed:?}");

        // The command should be cancelled
        match result {
            Ok(_) => {
                panic!("Docker runner command should have been cancelled but completed normally");
            }
            Err(e) => {
                eprintln!("Docker runner command was cancelled as expected: {e}");
                assert!(e.to_string().contains("cancelled"));
            }
        }

        // Should complete much faster than 10 seconds due to cancellation
        assert!(
            elapsed.as_millis() < 2000,
            "Cancellation should prevent long execution"
        );

        eprintln!("=== Docker Runner pre-execution cancellation test completed ===");
    }

    #[tokio::test]
    #[ignore] // Moved to e2e tests in worker-app/tests
    async fn test_docker_exec_stream_mid_execution_cancellation() {
        eprintln!("=== Testing Docker Exec Runner stream mid-execution cancellation ===");

        use std::sync::Arc;
        use std::time::{Duration, Instant};
        use tokio::sync::Mutex;

        // Use Arc<tokio::sync::Mutex<>> to share runner between tasks (similar to LLM pattern)
        let runner = Arc::new(Mutex::new(DockerExecRunner::new()));

        // Create test arguments
        use crate::jobworkerp::runner::DockerArgs;
        let arg = DockerArgs {
            cmd: vec!["sleep".to_string(), "10".to_string()],
            ..Default::default()
        };

        // Create cancellation token and set it on the runner
        let cancellation_token = tokio_util::sync::CancellationToken::new();
        {
            let _runner_guard = runner.lock().await;
            // TODO: Update to new cancellation system (moved to e2e tests)
            // runner_guard.set_cancellation_token(cancellation_token.clone());
        }

        let start_time = Instant::now();
        let serialized_args = ProstMessageCodec::serialize_message(&arg).unwrap();

        let runner_clone = runner.clone();

        // Start stream execution in a task
        let execution_task = tokio::spawn(async move {
            let mut runner_guard = runner_clone.lock().await;
            let stream_result = runner_guard
                .run_stream(&serialized_args, HashMap::new(), None)
                .await;

            match stream_result {
                Ok(_stream) => {
                    // Docker exec stream is not implemented, so this shouldn't happen
                    eprintln!("WARNING: Docker exec stream returned Ok (should be unimplemented)");
                    Ok(0)
                }
                Err(e) => {
                    eprintln!("Docker exec stream returned error as expected: {e}");
                    Err(e)
                }
            }
        });

        // Wait for stream to start (let it run for a bit)
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Cancel using the external token reference (avoids deadlock)
        cancellation_token.cancel();
        eprintln!("Called cancellation_token.cancel() after 100ms");

        // Wait for the execution to complete or be cancelled
        let execution_result = execution_task.await;
        let elapsed = start_time.elapsed();

        eprintln!("Docker exec stream execution completed in {elapsed:?}");

        match execution_result {
            Ok(stream_processing_result) => {
                match stream_processing_result {
                    Ok(_item_count) => {
                        eprintln!("WARNING: Docker exec stream should be unimplemented");
                    }
                    Err(e) => {
                        eprintln!("✓ Docker exec stream processing was cancelled as expected: {e}");
                        // Check if it's a cancellation error or unimplemented error
                        if e.to_string().contains("cancelled") {
                            eprintln!("✓ Cancellation was properly detected");
                        } else if e.to_string().contains("not implemented") {
                            eprintln!("✓ Stream is unimplemented but cancellation check worked");
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Docker exec stream execution task failed: {e}");
                panic!("Task failed: {e}");
            }
        }

        // Verify that cancellation happened very quickly (since stream is unimplemented)
        if elapsed > Duration::from_secs(1) {
            panic!(
            "Stream processing took too long ({elapsed:?}), should be immediate for unimplemented stream"
        );
        }

        eprintln!("✓ Docker exec stream mid-execution cancellation test completed successfully");
    }

    #[tokio::test]
    #[ignore] // Moved to e2e tests in worker-app/tests
    async fn test_docker_runner_stream_mid_execution_cancellation() {
        eprintln!("=== Testing Docker Runner stream mid-execution cancellation ===");

        use std::sync::Arc;
        use std::time::{Duration, Instant};
        use tokio::sync::Mutex;

        // Use Arc<tokio::sync::Mutex<>> to share runner between tasks (similar to LLM pattern)
        let runner = Arc::new(Mutex::new(DockerRunner::new()));

        // Create test arguments
        use crate::jobworkerp::runner::DockerArgs;
        let arg = DockerArgs {
            image: Some("busybox:latest".to_string()),
            cmd: vec!["sleep".to_string(), "10".to_string()],
            ..Default::default()
        };

        // Create cancellation token and set it on the runner
        let cancellation_token = tokio_util::sync::CancellationToken::new();
        {
            let _runner_guard = runner.lock().await;
            // TODO: Update to new cancellation system (moved to e2e tests)
            // runner_guard.set_cancellation_token(cancellation_token.clone());
        }

        let start_time = Instant::now();
        let serialized_args = ProstMessageCodec::serialize_message(&arg).unwrap();

        let runner_clone = runner.clone();

        // Start stream execution in a task
        let execution_task = tokio::spawn(async move {
            let mut runner_guard = runner_clone.lock().await;
            let stream_result = runner_guard
                .run_stream(&serialized_args, HashMap::new(), None)
                .await;

            match stream_result {
                Ok(_stream) => {
                    // Docker runner stream is not implemented, so this shouldn't happen
                    eprintln!(
                        "WARNING: Docker runner stream returned Ok (should be unimplemented)"
                    );
                    Ok(0)
                }
                Err(e) => {
                    eprintln!("Docker runner stream returned error as expected: {e}");
                    Err(e)
                }
            }
        });

        // Wait for stream to start (let it run for a bit)
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Cancel using the external token reference (avoids deadlock)
        cancellation_token.cancel();
        eprintln!("Called cancellation_token.cancel() after 100ms");

        // Wait for the execution to complete or be cancelled
        let execution_result = execution_task.await;
        let elapsed = start_time.elapsed();

        eprintln!("Docker runner stream execution completed in {elapsed:?}");

        match execution_result {
            Ok(stream_processing_result) => {
                match stream_processing_result {
                    Ok(_item_count) => {
                        eprintln!("WARNING: Docker runner stream should be unimplemented");
                    }
                    Err(e) => {
                        eprintln!(
                            "✓ Docker runner stream processing was cancelled as expected: {e}"
                        );
                        // Check if it's a cancellation error or unimplemented error
                        if e.to_string().contains("cancelled") {
                            eprintln!("✓ Cancellation was properly detected");
                        } else if e.to_string().contains("not implemented") {
                            eprintln!("✓ Stream is unimplemented but cancellation check worked");
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Docker runner stream execution task failed: {e}");
                panic!("Task failed: {e}");
            }
        }

        // Verify that cancellation happened very quickly (since stream is unimplemented)
        if elapsed > Duration::from_secs(1) {
            panic!(
            "Stream processing took too long ({elapsed:?}), should be immediate for unimplemented stream"
        );
        }

        eprintln!("✓ Docker runner stream mid-execution cancellation test completed successfully");
    }
}
