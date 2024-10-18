use std::collections::HashMap;

use super::super::Runner;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bollard::container::{
    AttachContainerOptions, AttachContainerResults, Config, RemoveContainerOptions,
    StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::CreateImageOptions;
use bollard::Docker;
use futures_util::stream::StreamExt;
use futures_util::TryStreamExt;
use infra::error::JobWorkerError;
use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
use proto::jobworkerp::data::DockerArg;
use serde::{Deserialize, Serialize};

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

// implement From for proto.jobworkerp.data.worker_operation.Operation(DockerOperation)
impl<T> From<proto::jobworkerp::data::DockerOperation> for CreateRunnerOptions<T>
where
    T: Into<String> + Serialize + std::fmt::Debug + Clone + From<String> + Default,
{
    fn from(op: proto::jobworkerp::data::DockerOperation) -> Self {
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
    docker: Docker,
    instant_id: String,
}

impl DockerExecRunner {
    // create and start container
    pub async fn new(image_options: &CreateRunnerOptions<String>) -> Result<DockerExecRunner> {
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

                Ok(DockerExecRunner {
                    docker,
                    instant_id: id,
                })
            }
            Err(e) => Err(JobWorkerError::DockerError(e).into()),
        }
    }

    pub async fn stop(&self, wait_secs: i64, force: bool) -> Result<()> {
        self.docker
            .stop_container(
                &self.instant_id,
                Some(StopContainerOptions { t: wait_secs }),
            )
            .await
            .map_err(JobWorkerError::DockerError)?;
        self.docker
            .remove_container(
                &self.instant_id,
                Some(RemoveContainerOptions {
                    force,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| JobWorkerError::DockerError(e).into())
    }
    fn trans_exec_arg(&self, arg: DockerArg) -> CreateExecOptions<String> {
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
#[async_trait]
impl Runner for DockerExecRunner {
    async fn name(&self) -> String {
        format!("DockerExecRunner: {}", &self.instant_id)
    }

    async fn run(&mut self, arg: &[u8]) -> Result<Vec<Vec<u8>>> {
        let req = JobqueueAndCodec::deserialize_message::<DockerArg>(arg)?;
        let mut c: CreateExecOptions<String> = self.trans_exec_arg(req.clone());
        // for log
        c.attach_stdout = Some(true);
        c.attach_stderr = Some(true);

        // non interactive
        let exec = self.docker.create_exec(&self.instant_id, c).await?.id;

        let mut out = Vec::<Vec<u8>>::new();
        if let StartExecResults::Attached { mut output, .. } =
            self.docker.start_exec(&exec, None).await?
        {
            while let Some(Ok(msg)) = output.next().await {
                out.push(format!("{}\n", msg).into_bytes().to_vec());
            }
            Ok(vec![out.concat()])
        } else {
            tracing::error!("unexpected error: cannot attach container (exec)");
            Err(anyhow!("unexpected error: cannot attach container (exec)"))
        }
    }

    // TODO
    async fn cancel(&mut self) {
        todo!("todo")
    }
    fn operation_proto(&self) -> String {
        include_str!("../../../../protobuf/docker_operation.proto").to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../../../../protobuf/docker_args.proto").to_string()
    }
    fn use_job_result(&self) -> bool {
        false
    }
}

// confirm local docker
#[tokio::test]
#[ignore]
async fn exec_test() -> Result<()> {
    use command_utils::util::result::FlatMap;

    let mut runner1 = DockerExecRunner::new(&CreateRunnerOptions::new(Some(
        "busybox:latest".to_string(),
    )))
    .await?;
    let mut runner2 = DockerExecRunner::new(&CreateRunnerOptions::new(Some(
        "busybox:latest".to_string(),
    )))
    .await?;
    let arg = JobqueueAndCodec::serialize_message(&DockerArg {
        cmd: vec!["ls".to_string(), "-alh".to_string(), "/etc".to_string()],
        ..Default::default()
    });
    let handle1 = tokio::spawn(async move {
        let res = runner1.run(&arg).await;
        tracing::info!("result:{:?}", &res);
        runner1.stop(2, false).await.flat_map(|_| res)
    });

    let arg2 = JobqueueAndCodec::serialize_message(&DockerArg {
        cmd: vec!["cat".to_string(), "/etc/resolv.conf".to_string()],
        ..Default::default()
    });
    let handle2 = tokio::spawn(async move {
        let res = runner2.run(&arg2).await;
        tracing::info!("result:{:?}", &res);
        runner2.stop(2, true).await.flat_map(|_| res)
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
    docker: Docker,
}

impl DockerRunner {
    pub async fn new(image_options: &CreateRunnerOptions<String>) -> Result<DockerRunner> {
        let docker = Docker::connect_with_socket_defaults().unwrap();
        docker
            .create_image(Some(image_options.to_docker()), None, None)
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| JobWorkerError::DockerError(e).into())
            .map(|_| DockerRunner { docker })
    }
    fn trans_docker_arg_to_config(&self, arg: &DockerArg) -> Config<String> {
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

#[async_trait]
impl Runner for DockerRunner {
    async fn name(&self) -> String {
        "DockerRunner".to_string()
    }
    async fn run(&mut self, arg: &[u8]) -> Result<Vec<Vec<u8>>> {
        let arg = JobqueueAndCodec::deserialize_message::<DockerArg>(arg)?;
        let mut config = self.trans_docker_arg_to_config(&arg);
        // to output log
        config.attach_stdout = Some(true);
        config.attach_stderr = Some(true);

        let created = self
            .docker
            .create_container::<&str, String>(None, config)
            .await?;
        let id = created.id;
        tracing::info!("container id: {}", &id);

        let AttachContainerResults {
            mut output,
            input: _,
        } = self
            .docker
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

        self.docker.start_container::<String>(&id, None).await?;

        let mut logs = Vec::<Vec<u8>>::new();
        // pipe docker attach output into stdout
        while let Some(Ok(output)) = output.next().await {
            match String::from_utf8(output.into_bytes().to_vec()) {
                Ok(o) => {
                    tracing::info!("{}", &o);
                    logs.push(format!("{}\n", o).into_bytes().to_vec())
                    // logs.push(o);
                }
                Err(e) => tracing::error!("error in decoding logs: {:?}", e),
            }
        }
        // remove container if persist to running
        self.docker
            .remove_container(
                &id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await?;

        Ok(vec![logs.concat()])
    }
    // TODO
    async fn cancel(&mut self) {
        todo!("todo")
    }
    fn operation_proto(&self) -> String {
        include_str!("../../../../protobuf/docker_operation.proto").to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../../../../protobuf/docker_args.proto").to_string()
    }
    fn use_job_result(&self) -> bool {
        false
    }
}

#[tokio::test]
#[ignore]
async fn run_test() -> Result<()> {
    // common::util::tracing::tracing_init_test(tracing::Level::INFO);

    let mut runner1 = DockerRunner::new(&CreateRunnerOptions::new(Some(
        "busybox:latest".to_string(),
    )))
    .await?;
    let mut runner2 = DockerRunner::new(&CreateRunnerOptions::new(Some(
        "busybox:latest".to_string(),
    )))
    .await?;
    let arg = JobqueueAndCodec::serialize_message(&DockerArg {
        image: Some("busybox:latest".to_string()),
        cmd: vec!["ls".to_string(), "-alh".to_string(), "/".to_string()],
        ..Default::default()
    });
    let handle1 = tokio::spawn(async move {
        let res = runner1.run(&arg).await;
        tracing::info!("result:{:?}", &res);
        res
    });

    let arg2 = JobqueueAndCodec::serialize_message(&DockerArg {
        image: Some("busybox:latest".to_string()),
        cmd: vec!["echo".to_string(), "run in docker container".to_string()],
        ..Default::default()
    });
    let handle2 = tokio::spawn(async move {
        let res = runner2.run(&arg2).await;
        tracing::info!("result:{:?}", &res);
        res
    });

    let r = tokio::join!(handle1, handle2);
    tracing::info!("result:{:?}", &r);

    Ok(())
}
