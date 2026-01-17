use super::cancellation_helper::{CancelMonitoringHelper, UseCancelMonitoringHelper};
use super::{RunnerSpec, RunnerTrait};
use crate::jobworkerp::runner::{DockerArgs, DockerRunnerSettings};
use crate::schema_to_json_string;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bollard::container::AttachContainerResults;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    AttachContainerOptions, CreateContainerOptions, CreateImageOptionsBuilder,
    RemoveContainerOptions, StartContainerOptions, StopContainerOptions,
};
use bollard::{Docker, API_DEFAULT_VERSION};
use futures::stream::BoxStream;
use futures::TryStreamExt;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    JobData, JobId, JobResult, ResultOutputItem, RunnerType, StreamingOutputType,
};
use proto::DEFAULT_METHOD_NAME;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;

// Default timeout: 1 hour (in seconds)
const DEFAULT_DOCKER_TIMEOUT_SEC: u64 = 3600;
const DEFAULT_DOCKER_SOCKET_PATH: &str = "/var/run/docker.sock";

/// Create Docker client with custom timeout, respecting DOCKER_HOST environment variable
/// and rootless Docker socket locations.
///
/// Connection priority:
/// 1. DOCKER_HOST environment variable (supports unix://, tcp://, http://, https://)
/// 2. XDG_RUNTIME_DIR/docker.sock (rootless Docker)
/// 3. /var/run/docker.sock (default)
///
/// For HTTPS connections, certificates are read from DOCKER_CERT_PATH directory
/// (expects key.pem, cert.pem, ca.pem files).
fn connect_docker_with_timeout(timeout_sec: u64) -> Result<Docker> {
    if let Ok(docker_host) = std::env::var("DOCKER_HOST") {
        if let Some(socket_path) = docker_host.strip_prefix("unix://") {
            tracing::debug!("Connecting to Docker via unix socket: {}", socket_path);
            Docker::connect_with_socket(socket_path, timeout_sec, API_DEFAULT_VERSION).map_err(
                |e| {
                    anyhow!(
                        "Failed to connect to Docker via unix socket '{}': {}",
                        socket_path,
                        e
                    )
                },
            )
        } else if docker_host.starts_with("tcp://") || docker_host.starts_with("http://") {
            // Check if TLS is requested for tcp:// connections
            let use_tls = std::env::var("DOCKER_TLS_VERIFY").is_ok()
                || std::env::var("DOCKER_CERT_PATH").is_ok();
            if use_tls {
                // Convert tcp:// to https:// for SSL connection
                let https_host = docker_host
                    .replace("tcp://", "https://")
                    .replace("http://", "https://");
                tracing::debug!(
                    "Connecting to Docker via HTTPS (TLS enabled): {}",
                    https_host
                );
                connect_with_ssl_from_env(&https_host, timeout_sec)
            } else {
                tracing::debug!("Connecting to Docker via HTTP: {}", docker_host);
                Docker::connect_with_http(&docker_host, timeout_sec, API_DEFAULT_VERSION).map_err(
                    |e| {
                        anyhow!(
                            "Failed to connect to Docker via HTTP '{}': {}",
                            docker_host,
                            e
                        )
                    },
                )
            }
        } else if docker_host.starts_with("https://") {
            tracing::debug!("Connecting to Docker via HTTPS: {}", docker_host);
            connect_with_ssl_from_env(&docker_host, timeout_sec)
        } else {
            Err(anyhow!(
                "Unsupported DOCKER_HOST scheme: '{}'. Supported schemes are: unix://, tcp://, http://, https://",
                docker_host
            ))
        }
    } else {
        connect_to_default_socket(timeout_sec)
    }
}

/// Connect to Docker via SSL/TLS using certificates from DOCKER_CERT_PATH
fn connect_with_ssl_from_env(docker_host: &str, timeout_sec: u64) -> Result<Docker> {
    let cert_path = std::env::var("DOCKER_CERT_PATH").map_err(|_| {
        anyhow!(
            "DOCKER_CERT_PATH environment variable is required for HTTPS connections to '{}'",
            docker_host
        )
    })?;

    let cert_dir = std::path::Path::new(&cert_path);
    let key_path = cert_dir.join("key.pem");
    let cert_file_path = cert_dir.join("cert.pem");
    let ca_path = cert_dir.join("ca.pem");

    Docker::connect_with_ssl(
        docker_host,
        &key_path,
        &cert_file_path,
        &ca_path,
        timeout_sec,
        API_DEFAULT_VERSION,
    )
    .map_err(|e| {
        anyhow!(
            "Failed to connect to Docker via HTTPS '{}' with certificates from '{}': {}",
            docker_host,
            cert_path,
            e
        )
    })
}

/// Connect to Docker socket using default paths (rootless or system)
fn connect_to_default_socket(timeout_sec: u64) -> Result<Docker> {
    // Check for rootless Docker socket first
    if let Ok(xdg_runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let rootless_socket = format!("{}/docker.sock", xdg_runtime_dir);
        if std::path::Path::new(&rootless_socket).exists() {
            tracing::debug!("Connecting to rootless Docker socket: {}", rootless_socket);
            return Docker::connect_with_socket(&rootless_socket, timeout_sec, API_DEFAULT_VERSION)
                .map_err(|e| {
                    anyhow!(
                        "Failed to connect to rootless Docker socket '{}': {}",
                        rootless_socket,
                        e
                    )
                });
        }
    }

    // Fall back to default system socket
    tracing::debug!(
        "Connecting to default Docker socket: {}",
        DEFAULT_DOCKER_SOCKET_PATH
    );
    Docker::connect_with_socket(DEFAULT_DOCKER_SOCKET_PATH, timeout_sec, API_DEFAULT_VERSION)
        .map_err(|e| {
            anyhow!(
                "Failed to connect to Docker socket '{}': {}",
                DEFAULT_DOCKER_SOCKET_PATH,
                e
            )
        })
}

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
    pub fn to_docker(&self) -> bollard::query_parameters::CreateImageOptions {
        let mut builder = CreateImageOptionsBuilder::default();
        if let Some(ref image) = self.from_image {
            let image_str: String = image.clone().into();
            builder = builder.from_image(&image_str);
        }
        if let Some(ref src) = self.from_src {
            let src_str: String = src.clone().into();
            builder = builder.from_src(&src_str);
        }
        if let Some(ref repo) = self.repo {
            let repo_str: String = repo.clone().into();
            builder = builder.repo(&repo_str);
        }
        if let Some(ref tag) = self.tag {
            let tag_str: String = tag.clone().into();
            builder = builder.tag(&tag_str);
        }
        if let Some(ref platform) = self.platform {
            let platform_str: String = platform.clone().into();
            builder = builder.platform(&platform_str);
        }
        builder.build()
    }
    pub fn to_docker_exec_config(&self) -> ContainerCreateBody {
        // Convert volumes HashMap to Vec<String> for bollard 0.20
        let volumes = self.volumes.as_ref().map(|v| v.keys().cloned().collect());
        ContainerCreateBody {
            image: self.from_image.clone().map(|s| s.into()),
            env: self.env.clone(),
            volumes,
            working_dir: self.working_dir.clone(),
            entrypoint: self.entrypoint.clone(),
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
//
#[derive(Debug)]
pub struct DockerExecRunner {
    docker: Option<Docker>,
    instant_id: String,
    // Helper for dependency injection integration (optional for backward compatibility)
    cancel_helper: Option<CancelMonitoringHelper>,
    // Current Docker API timeout in seconds
    current_timeout_sec: u64,
}

impl DockerExecRunner {
    /// Constructor without cancellation monitoring (for backward compatibility)
    pub fn new() -> Self {
        DockerExecRunner {
            docker: None,
            instant_id: "".to_string(),
            cancel_helper: None,
            current_timeout_sec: DEFAULT_DOCKER_TIMEOUT_SEC,
        }
    }

    /// Constructor with cancellation monitoring (DI integration version)
    pub fn new_with_cancel_monitoring(cancel_helper: CancelMonitoringHelper) -> Self {
        DockerExecRunner {
            docker: None,
            instant_id: "".to_string(),
            cancel_helper: Some(cancel_helper),
            current_timeout_sec: DEFAULT_DOCKER_TIMEOUT_SEC,
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
    pub async fn create(
        &mut self,
        image_options: &CreateRunnerOptions<String>,
        timeout_sec: u64,
    ) -> Result<()> {
        let docker = connect_docker_with_timeout(timeout_sec)?;
        self.current_timeout_sec = timeout_sec;
        match docker
            .create_image(Some(image_options.to_docker()), None, None)
            .try_collect::<Vec<_>>()
            .await
        {
            Ok(_d) => {
                let mut config = image_options.to_docker_exec_config();
                config.tty = Some(true);

                let id = docker
                    .create_container(None::<CreateContainerOptions>, config)
                    .await
                    .map_err(JobWorkerError::DockerError)?
                    .id;
                tracing::info!("container id: {}", &id);
                docker
                    .start_container(&id, None::<StartContainerOptions>)
                    .await
                    .map_err(JobWorkerError::DockerError)?;

                self.docker = Some(docker);
                self.instant_id = id;
                Ok(())
            }
            Err(e) => Err(JobWorkerError::DockerError(e).into()),
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
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            DEFAULT_METHOD_NAME.to_string(),
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
}
#[async_trait]
impl RunnerTrait for DockerExecRunner {
    // create and start container
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let op = ProstMessageCodec::deserialize_message::<DockerRunnerSettings>(&settings)?;
        let timeout_sec = op.timeout_sec.unwrap_or(DEFAULT_DOCKER_TIMEOUT_SEC);
        self.create(&op.into(), timeout_sec).await
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

                if cancellation_token.is_cancelled() {
                    tracing::info!("Docker exec execution was cancelled before create_exec");
                    return Err(JobWorkerError::CancelledError("Docker exec execution was cancelled before create_exec".to_string()).into());
                }

                // non interactive
                let exec = tokio::select! {
                    exec_result = docker.create_exec(&self.instant_id, c) => {
                        exec_result?.id
                    }
                    _ = cancellation_token.cancelled() => {
                        tracing::info!("Docker exec creation was cancelled");
                        return Err(JobWorkerError::CancelledError("Docker exec creation was cancelled".to_string()).into());
                    }
                };

                let mut out = Vec::<Vec<u8>>::new();
                let start_result = tokio::select! {
                    start_result = docker.start_exec(&exec, None) => start_result,
                    _ = cancellation_token.cancelled() => {
                        tracing::info!("Docker exec start was cancelled");
                        return Err(JobWorkerError::CancelledError("Docker exec start was cancelled".to_string()).into());
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
                                return Err(JobWorkerError::CancelledError("Docker exec output reading was cancelled".to_string()).into());
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

//
// docker one time runner (not use tty)
//
#[derive(Debug)]
pub struct DockerRunner {
    docker: Option<Docker>,
    current_container_id: Option<String>,
    // Helper for dependency injection integration (optional for backward compatibility)
    cancel_helper: Option<CancelMonitoringHelper>,
    // Current Docker API timeout in seconds
    current_timeout_sec: u64,
}

impl DockerRunner {
    /// Constructor without cancellation monitoring (for backward compatibility)
    pub fn new() -> Self {
        DockerRunner {
            docker: None,
            current_container_id: None,
            cancel_helper: None,
            current_timeout_sec: DEFAULT_DOCKER_TIMEOUT_SEC,
        }
    }

    /// Constructor with cancellation monitoring (DI integration version)
    pub fn new_with_cancel_monitoring(cancel_helper: CancelMonitoringHelper) -> Self {
        DockerRunner {
            docker: None,
            current_container_id: None,
            cancel_helper: Some(cancel_helper),
            current_timeout_sec: DEFAULT_DOCKER_TIMEOUT_SEC,
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
    pub async fn create(
        &mut self,
        image_options: &CreateRunnerOptions<String>,
        timeout_sec: u64,
    ) -> Result<()> {
        self.current_timeout_sec = timeout_sec;

        // Only create docker client and pull image if from_image or from_src is specified
        // This avoids creating an unused client when image will be specified in run()
        if image_options.from_image.is_some() || image_options.from_src.is_some() {
            let docker = connect_docker_with_timeout(timeout_sec)?;
            let image_name = image_options.from_image.clone().unwrap_or_default();

            // Check if image exists locally, pull only if not present
            let image_exists = docker.inspect_image(&image_name).await.is_ok();

            if !image_exists {
                tracing::info!("Image {} not found locally, pulling...", &image_name);
                docker
                    .create_image(Some(image_options.to_docker()), None, None)
                    .try_collect::<Vec<_>>()
                    .await
                    .map_err(JobWorkerError::DockerError)?;
            } else {
                tracing::info!("Image {} found locally, skipping pull", &image_name);
            }
            self.docker = Some(docker);
        } else {
            tracing::debug!(
                "docker image is not specified in settings, will be specified in run() method"
            );
        }
        Ok(())
    }
    fn trans_docker_arg_to_config(&self, arg: &DockerArgs) -> ContainerCreateBody {
        // Separate volumes into bind mounts (containing ':') and volume declarations (no ':')
        // This allows docker run -v style syntax: "host:container[:mode]"
        let (binds, volume_declarations): (Vec<_>, Vec<_>) =
            arg.volumes.iter().cloned().partition(|v| v.contains(':'));

        // In bollard 0.20, volumes is Vec<String> instead of HashMap
        let volumes = if volume_declarations.is_empty() {
            None
        } else {
            Some(volume_declarations)
        };

        let host_config = if binds.is_empty() {
            None
        } else {
            Some(HostConfig {
                binds: Some(binds),
                ..Default::default()
            })
        };

        ContainerCreateBody {
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
                Some(arg.exposed_ports.clone())
            },
            env: if arg.env.is_empty() {
                None
            } else {
                Some(arg.env.clone())
            },
            volumes,
            working_dir: arg.working_dir.clone(),
            entrypoint: if arg.entrypoint.is_empty() {
                None
            } else {
                Some(arg.entrypoint.clone())
            },
            network_disabled: arg.network_disabled,
            // mac_address is deprecated since API v1.44
            shell: if arg.shell.is_empty() {
                None
            } else {
                Some(arg.shell.clone())
            },
            host_config,
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
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            DEFAULT_METHOD_NAME.to_string(),
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
}

#[async_trait]
impl RunnerTrait for DockerRunner {
    // create and start container
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let op = ProstMessageCodec::deserialize_message::<DockerRunnerSettings>(&settings)?;
        let timeout_sec = op.timeout_sec.unwrap_or(DEFAULT_DOCKER_TIMEOUT_SEC);
        self.create(&op.into(), timeout_sec).await
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
            // Get timeout from DockerArgs, fallback to current settings timeout
            let timeout_sec = arg.timeout_sec.unwrap_or(self.current_timeout_sec);

            // Reconnect if timeout changed or docker not initialized
            if self.docker.is_none() || timeout_sec != self.current_timeout_sec {
                self.create(&create_option, timeout_sec).await?;
            }
            if let Some(docker) = self.docker.as_ref() {
                if cancellation_token.is_cancelled() {
                    tracing::info!("Docker execution was cancelled before create_image");
                    return Err(JobWorkerError::CancelledError("Docker execution was cancelled before create_image".to_string()).into());
                }

                // Check if image exists locally, pull only if not present
                let image_name = arg.image.clone().unwrap_or_default();
                let image_exists = docker.inspect_image(&image_name).await.is_ok();

                if !image_exists {
                    tracing::info!("Image {} not found locally, pulling...", &image_name);
                    tokio::select! {
                        result = docker
                            .create_image(Some(create_option.to_docker()), None, None)
                            .try_collect::<Vec<_>>() => {
                            result.map_err(JobWorkerError::DockerError)?;
                        }
                        _ = cancellation_token.cancelled() => {
                            tracing::info!("Docker image creation was cancelled");
                            return Err(JobWorkerError::CancelledError("Docker image creation was cancelled".to_string()).into());
                        }
                    }
                } else {
                    tracing::info!("Image {} found locally, skipping pull", &image_name);
                }

                let mut config = self.trans_docker_arg_to_config(&arg);
                // to output log
                config.attach_stdout = Some(true);
                config.attach_stderr = Some(true);

                let created = tokio::select! {
                    result = docker.create_container(None::<CreateContainerOptions>, config) => result?,
                    _ = cancellation_token.cancelled() => {
                        tracing::info!("Docker container creation was cancelled");
                        return Err(JobWorkerError::CancelledError("Docker container creation was cancelled".to_string()).into());
                    }
                };
                let id = created.id;
                tracing::info!("container id: {}", &id);

                // Store container ID for potential cancellation
                self.current_container_id = Some(id.clone());

                let attach_result = tokio::select! {
                    result = docker.attach_container(
                        &id,
                        Some(AttachContainerOptions {
                            stdout: true,
                            stderr: true,
                            stream: true,
                            ..Default::default()
                        }),
                    ) => result?,
                    _ = cancellation_token.cancelled() => {
                        tracing::info!("Docker container attach was cancelled");
                        return Err(JobWorkerError::CancelledError("Docker container attach was cancelled".to_string()).into());
                    }
                };

                let AttachContainerResults {
                    mut output,
                    input: _,
                } = attach_result;

                tokio::select! {
                    result = docker.start_container(&id, None::<StartContainerOptions>) => result?,
                    _ = cancellation_token.cancelled() => {
                        tracing::info!("Docker container start was cancelled");
                        return Err(JobWorkerError::CancelledError("Docker container start was cancelled".to_string()).into());
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
                            return Err(JobWorkerError::CancelledError("Docker output reading was cancelled".to_string()).into());
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

impl Drop for DockerExecRunner {
    fn drop(&mut self) {
        // Clean up Docker container when DockerExecRunner is dropped
        if let Some(docker) = self.docker.take() {
            let container_id = std::mem::take(&mut self.instant_id);
            if !container_id.is_empty() {
                // Spawn a background task to stop and remove the container
                tokio::spawn(async move {
                    // Graceful stop (SIGTERM with 10 second timeout)
                    if let Err(e) = docker
                        .stop_container(
                            &container_id,
                            Some(StopContainerOptions {
                                t: Some(10),
                                ..Default::default()
                            }),
                        )
                        .await
                    {
                        tracing::warn!(
                            "DockerExecRunner Drop: failed to stop container {}: {}",
                            container_id,
                            e
                        );
                    }

                    // Force remove
                    if let Err(e) = docker
                        .remove_container(
                            &container_id,
                            Some(RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await
                    {
                        tracing::warn!(
                            "DockerExecRunner Drop: failed to remove container {}: {}",
                            container_id,
                            e
                        );
                    }

                    tracing::info!(
                        "DockerExecRunner Drop: container {} stopped and removed",
                        container_id
                    );
                });
            }
        }
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
                        Some(StopContainerOptions {
                            t: Some(10),
                            ..Default::default()
                        }),
                    )
                    .await
                {
                    tracing::warn!("Failed to stop container {}: {}", container_id, e);
                }

                // Force remove
                if let Err(e) = docker
                    .remove_container(
                        &container_id,
                        Some(RemoveContainerOptions {
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

impl Drop for DockerRunner {
    fn drop(&mut self) {
        // Clean up Docker container when DockerRunner is dropped
        if let Some(docker) = self.docker.take() {
            if let Some(container_id) = self.current_container_id.take() {
                // Spawn a background task to stop and remove the container
                tokio::spawn(async move {
                    // Graceful stop (SIGTERM with 10 second timeout)
                    if let Err(e) = docker
                        .stop_container(
                            &container_id,
                            Some(StopContainerOptions {
                                t: Some(10),
                                ..Default::default()
                            }),
                        )
                        .await
                    {
                        tracing::warn!(
                            "DockerRunner Drop: failed to stop container {}: {}",
                            container_id,
                            e
                        );
                    }

                    // Force remove
                    if let Err(e) = docker
                        .remove_container(
                            &container_id,
                            Some(RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await
                    {
                        tracing::warn!(
                            "DockerRunner Drop: failed to remove container {}: {}",
                            container_id,
                            e
                        );
                    }

                    tracing::info!(
                        "DockerRunner Drop: container {} stopped and removed",
                        container_id
                    );
                });
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::runner::cancellation::{
        CancelMonitoring, CancellationSetupResult, RunnerCancellationManager,
    };
    use async_trait::async_trait;
    use proto::jobworkerp::data::{JobData, JobId};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// Test stub for RunnerCancellationManager
    /// Simple implementation that wraps a CancellationToken for unit testing
    #[derive(Debug)]
    struct TestCancellationManager {
        token: CancellationToken,
        is_cancelled: Arc<AtomicBool>,
    }

    impl TestCancellationManager {
        fn new() -> Self {
            Self {
                token: CancellationToken::new(),
                is_cancelled: Arc::new(AtomicBool::new(false)),
            }
        }

        fn new_cancelled() -> Self {
            let manager = Self::new();
            manager.token.cancel();
            manager.is_cancelled.store(true, Ordering::SeqCst);
            manager
        }
    }

    #[async_trait]
    impl RunnerCancellationManager for TestCancellationManager {
        async fn setup_monitoring(
            &mut self,
            _job_id: &JobId,
            _job_data: &JobData,
        ) -> Result<CancellationSetupResult> {
            if self.is_cancelled.load(Ordering::SeqCst) {
                Ok(CancellationSetupResult::AlreadyCancelled)
            } else {
                Ok(CancellationSetupResult::MonitoringStarted)
            }
        }

        async fn cleanup_monitoring(&mut self) -> Result<()> {
            Ok(())
        }

        async fn get_token(&self) -> CancellationToken {
            self.token.clone()
        }

        fn is_cancelled(&self) -> bool {
            self.is_cancelled.load(Ordering::SeqCst)
        }
    }

    #[tokio::test]
    #[ignore = "Requires Docker daemon"]
    async fn run_test() -> Result<()> {
        // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

        let mut runner1 = DockerRunner::new();
        runner1
            .create(
                &CreateRunnerOptions::new(Some("busybox:latest".to_string())),
                DEFAULT_DOCKER_TIMEOUT_SEC,
            )
            .await?;
        let mut runner2 = DockerRunner::new();
        runner2
            .create(
                &CreateRunnerOptions::new(Some("busybox:latest".to_string())),
                DEFAULT_DOCKER_TIMEOUT_SEC,
            )
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
    #[ignore = "Requires Docker daemon"]
    async fn test_docker_exec_pre_execution_cancellation() {
        eprintln!("=== Testing Docker Exec Runner pre-execution cancellation ===");

        // Create runner with pre-cancelled cancellation manager
        let cancel_helper =
            CancelMonitoringHelper::new(Box::new(TestCancellationManager::new_cancelled()));
        let mut runner = DockerExecRunner::new_with_cancel_monitoring(cancel_helper);

        // Initialize Docker instance first (required for cancellation check to be reached)
        runner
            .create(
                &CreateRunnerOptions::new(Some("busybox:latest".to_string())),
                DEFAULT_DOCKER_TIMEOUT_SEC,
            )
            .await
            .expect("Failed to create Docker container");

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
                let err_str = e.to_string().to_lowercase();
                assert!(
                    err_str.contains("cancel"),
                    "Error should indicate cancellation: {e}"
                );
            }
        }

        // Should complete much faster than 10 seconds due to cancellation
        assert!(
            elapsed.as_millis() < 3000,
            "Cancellation should prevent long execution, took {elapsed:?}"
        );

        // Container cleanup is handled automatically by Drop implementation

        eprintln!("=== Docker Exec pre-execution cancellation test completed ===");
    }

    #[tokio::test]
    #[ignore = "Requires Docker daemon"]
    async fn test_docker_runner_pre_execution_cancellation() {
        eprintln!("=== Testing Docker Runner pre-execution cancellation ===");

        // Create runner with pre-cancelled cancellation manager
        let cancel_helper =
            CancelMonitoringHelper::new(Box::new(TestCancellationManager::new_cancelled()));
        let mut runner = DockerRunner::new_with_cancel_monitoring(cancel_helper);

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
        let elapsed = start_time.elapsed();

        eprintln!("Execution completed in {elapsed:?}");

        // The command should be cancelled
        match result {
            Ok(_) => {
                panic!("Docker runner command should have been cancelled but completed normally");
            }
            Err(e) => {
                eprintln!("Docker runner command was cancelled as expected: {e}");
                let err_str = e.to_string().to_lowercase();
                assert!(
                    err_str.contains("cancel"),
                    "Error should indicate cancellation: {e}"
                );
            }
        }

        // Should complete much faster than 10 seconds due to cancellation
        assert!(
            elapsed.as_millis() < 3000,
            "Cancellation should prevent long execution, took {elapsed:?}"
        );

        eprintln!("=== Docker Runner pre-execution cancellation test completed ===");
    }

    /// Test that DockerExecRunner run_stream returns unimplemented error
    #[tokio::test]
    async fn test_docker_exec_stream_not_implemented() {
        eprintln!("=== Testing Docker Exec Runner stream is not implemented ===");

        let mut runner = DockerExecRunner::new();

        use crate::jobworkerp::runner::DockerArgs;
        let arg = DockerArgs {
            cmd: vec!["echo".to_string(), "test".to_string()],
            ..Default::default()
        };
        let serialized_args = ProstMessageCodec::serialize_message(&arg).unwrap();

        let stream_result = runner
            .run_stream(&serialized_args, HashMap::new(), None)
            .await;

        match stream_result {
            Ok(_stream) => {
                panic!("Docker exec stream should be unimplemented");
            }
            Err(e) => {
                eprintln!("Docker exec stream returned error as expected: {e}");
                let err_str = e.to_string().to_lowercase();
                assert!(
                    err_str.contains("not implemented") || err_str.contains("unimplemented"),
                    "Error should indicate not implemented: {e}"
                );
            }
        }

        eprintln!("✓ Docker exec stream not implemented test completed");
    }

    /// Test that DockerRunner run_stream returns unimplemented error
    #[tokio::test]
    async fn test_docker_runner_stream_not_implemented() {
        eprintln!("=== Testing Docker Runner stream is not implemented ===");

        let mut runner = DockerRunner::new();

        use crate::jobworkerp::runner::DockerArgs;
        let arg = DockerArgs {
            image: Some("busybox:latest".to_string()),
            cmd: vec!["echo".to_string(), "test".to_string()],
            ..Default::default()
        };
        let serialized_args = ProstMessageCodec::serialize_message(&arg).unwrap();

        let stream_result = runner
            .run_stream(&serialized_args, HashMap::new(), None)
            .await;

        match stream_result {
            Ok(_stream) => {
                panic!("Docker runner stream should be unimplemented");
            }
            Err(e) => {
                eprintln!("Docker runner stream returned error as expected: {e}");
                let err_str = e.to_string().to_lowercase();
                assert!(
                    err_str.contains("not implemented") || err_str.contains("unimplemented"),
                    "Error should indicate not implemented: {e}"
                );
            }
        }

        eprintln!("✓ Docker runner stream not implemented test completed");
    }

    /// Test request_cancellation for DockerExecRunner
    #[tokio::test]
    async fn test_docker_exec_request_cancellation() {
        eprintln!("=== Testing DockerExecRunner request_cancellation ===");

        let cancel_helper = CancelMonitoringHelper::new(Box::new(TestCancellationManager::new()));
        let mut runner = DockerExecRunner::new_with_cancel_monitoring(cancel_helper);

        // Request cancellation
        let result = runner.request_cancellation().await;
        assert!(result.is_ok(), "request_cancellation should succeed");

        // Verify cancellation token is now cancelled
        if let Some(helper) = runner.cancel_monitoring_helper() {
            let token = helper.get_cancellation_token().await;
            assert!(
                token.is_cancelled(),
                "Token should be cancelled after request_cancellation"
            );
        }

        eprintln!("✓ DockerExecRunner request_cancellation test completed");
    }

    /// Test request_cancellation for DockerRunner
    #[tokio::test]
    async fn test_docker_runner_request_cancellation() {
        eprintln!("=== Testing DockerRunner request_cancellation ===");

        let cancel_helper = CancelMonitoringHelper::new(Box::new(TestCancellationManager::new()));
        let mut runner = DockerRunner::new_with_cancel_monitoring(cancel_helper);

        // Request cancellation
        let result = runner.request_cancellation().await;
        assert!(result.is_ok(), "request_cancellation should succeed");

        // Verify cancellation token is now cancelled
        if let Some(helper) = runner.cancel_monitoring_helper() {
            let token = helper.get_cancellation_token().await;
            assert!(
                token.is_cancelled(),
                "Token should be cancelled after request_cancellation"
            );
        }

        eprintln!("✓ DockerRunner request_cancellation test completed");
    }

    /// Test that DockerExecRunner Drop stops and removes the container
    #[tokio::test]
    #[ignore = "Requires Docker daemon"]
    async fn test_docker_exec_runner_drop_stops_container() {
        eprintln!("=== Testing DockerExecRunner Drop stops container ===");

        let container_id: String;

        // Create runner and container in a scope so Drop is called when scope ends
        {
            let mut runner = DockerExecRunner::new();
            runner
                .create(
                    &CreateRunnerOptions::new(Some("busybox:latest".to_string())),
                    DEFAULT_DOCKER_TIMEOUT_SEC,
                )
                .await
                .expect("Failed to create Docker container");

            // Store container ID for verification
            container_id = runner.instant_id.clone();
            assert!(!container_id.is_empty(), "Container ID should be set");

            // Verify container is running
            let docker = connect_docker_with_timeout(DEFAULT_DOCKER_TIMEOUT_SEC)
                .expect("Failed to connect to Docker");
            let inspect = docker.inspect_container(&container_id, None).await;
            assert!(inspect.is_ok(), "Container should exist before Drop");

            eprintln!("Container {} created and running", container_id);

            // Runner will be dropped here, triggering cleanup
        }

        // Wait for the background cleanup task to complete
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;

        // Verify container no longer exists
        let docker = connect_docker_with_timeout(DEFAULT_DOCKER_TIMEOUT_SEC)
            .expect("Failed to connect to Docker");
        let inspect = docker.inspect_container(&container_id, None).await;
        assert!(
            inspect.is_err(),
            "Container should be removed after Drop, but it still exists"
        );

        eprintln!(
            "✓ Container {} was stopped and removed by Drop",
            container_id
        );
        eprintln!("=== DockerExecRunner Drop test completed ===");
    }

    /// Test that DockerRunner Drop stops and removes the container
    #[tokio::test]
    #[ignore = "Requires Docker daemon"]
    async fn test_docker_runner_drop_stops_container() {
        eprintln!("=== Testing DockerRunner Drop stops container ===");

        let container_id: String;

        // Create container directly and set it to DockerRunner to test Drop behavior
        {
            let docker = connect_docker_with_timeout(DEFAULT_DOCKER_TIMEOUT_SEC)
                .expect("Failed to connect to Docker");

            // Create a container that sleeps
            let config = ContainerCreateBody {
                image: Some("busybox:latest".to_string()),
                cmd: Some(vec!["sleep".to_string(), "300".to_string()]),
                ..Default::default()
            };

            let created = docker
                .create_container(None::<CreateContainerOptions>, config)
                .await
                .expect("Failed to create container");
            container_id = created.id.clone();

            // Start the container
            docker
                .start_container(&container_id, None::<StartContainerOptions>)
                .await
                .expect("Failed to start container");

            eprintln!("Container {} created and running", container_id);

            // Create runner with the container ID set
            let mut runner = DockerRunner::new();
            runner.docker = Some(docker);
            runner.current_container_id = Some(container_id.clone());

            // Drop runner here - should trigger cleanup
            drop(runner);
        }

        // Wait for the background cleanup task to complete
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;

        // Verify container no longer exists
        let docker = connect_docker_with_timeout(DEFAULT_DOCKER_TIMEOUT_SEC)
            .expect("Failed to connect to Docker");
        let inspect = docker.inspect_container(&container_id, None).await;
        assert!(
            inspect.is_err(),
            "Container should be removed after Drop, but it still exists"
        );

        eprintln!(
            "✓ Container {} was stopped and removed by Drop",
            container_id
        );
        eprintln!("=== DockerRunner Drop test completed ===");
    }
}
