use super::super::Runner;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bollard::container::{
    AttachContainerOptions, AttachContainerResults, Config, RemoveContainerOptions,
    StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::CreateImageOptions;
use bollard::models::ContainerConfig;
use bollard::Docker;
use futures_util::stream::StreamExt;
use futures_util::TryStreamExt;
use infra::error::JobWorkerError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CreateImageOptionsForDeserialize<T>
where
    T: Into<String> + Serialize + Clone,
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
}
impl<T> CreateImageOptionsForDeserialize<T>
where
    T: Into<String> + Serialize + Clone + Default,
{
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
    pub async fn new(operation: &str) -> Result<DockerExecRunner> {
        let docker = Docker::connect_with_socket_defaults().unwrap();
        let image_options =
            serde_json::from_str::<CreateImageOptionsForDeserialize<String>>(operation)?;
        match docker
            .create_image(Some(image_options.to_docker()), None, None)
            .try_collect::<Vec<_>>()
            .await
        {
            Ok(_d) => {
                let config = Config {
                    image: image_options.from_image.clone(),
                    tty: Some(true),
                    ..Default::default()
                };

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
}
impl DockerExecRunner {
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
}
#[async_trait]
// arg: {headers:{<headers map>}, queries:[<query string array>], body: <body string or struct>}
impl Runner for DockerExecRunner {
    async fn name(&self) -> String {
        format!("DockerExecRunner: {}", &self.instant_id)
    }

    async fn run(&mut self, arg: Vec<u8>) -> Result<Vec<Vec<u8>>> {
        let mut c: CreateExecOptions<String> = serde_json::from_slice(arg.as_ref())?;
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
}

// confirm local docker
#[tokio::test]
#[ignore]
async fn exec_test() -> Result<()> {
    use command_utils::util::result::FlatMap;

    let mut runner1 = DockerExecRunner::new(
        r#"{
        "FromImage":"busybox:latest"
    }"#,
    )
    .await?;
    let mut runner2 = DockerExecRunner::new(
        r#"{
        "FromImage":"busybox:latest"
    }"#,
    )
    .await?;
    let handle1 = tokio::spawn(async move {
        let res = runner1
            .run(r#"{"Cmd": ["ls", "-alh", "/etc"]}"#.as_bytes().to_vec())
            .await;
        tracing::info!("result:{:?}", &res);
        runner1.stop(2, false).await.flat_map(|_| res)
    });

    let handle2 = tokio::spawn(async move {
        let res = runner2
            .run(r#"{"Cmd": ["cat", "/etc/resolv.conf"]}"#.as_bytes().to_vec())
            .await;
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
    pub async fn new(operation: &str) -> Result<DockerRunner> {
        let docker = Docker::connect_with_socket_defaults().unwrap();
        let image_options =
            serde_json::from_str::<CreateImageOptionsForDeserialize<String>>(operation)?;
        docker
            .create_image(Some(image_options.to_docker()), None, None)
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| JobWorkerError::DockerError(e).into())
            .map(|_| DockerRunner { docker })
    }
}

#[async_trait]
// arg: {headers:{<headers map>}, queries:[<query string array>], body: <body string or struct>}
impl Runner for DockerRunner {
    async fn name(&self) -> String {
        "DockerRunner".to_string()
    }
    async fn run(&mut self, arg: Vec<u8>) -> Result<Vec<Vec<u8>>> {
        let mut c: ContainerConfig = serde_json::from_slice(arg.as_ref())?;
        // to output log
        c.attach_stdout = Some(true);
        c.attach_stderr = Some(true);

        let config = Config::from(c);
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
}

#[tokio::test]
#[ignore]
async fn run_test() -> Result<()> {
    // common::util::tracing::tracing_init_test(tracing::Level::INFO);

    let mut runner1 = DockerRunner::new(
        r#"{
        "FromImage":"busybox:latest"
    }"#,
    )
    .await?;
    let mut runner2 = DockerRunner::new(
        r#"{
        "FromImage":"busybox:latest"
    }"#,
    )
    .await?;
    let handle1 = tokio::spawn(async move {
        let res = runner1
            .run(
                r#"{"Image":"busybox:latest","Cmd": ["ls", "-alh", "/"]}"#
                    .as_bytes()
                    .to_vec(),
            )
            .await;
        tracing::info!("result:{:?}", &res);
        res
    });

    let handle2 = tokio::spawn(async move {
        let res = runner2
            .run(
                r#"{"Image":"busybox:latest","Cmd": ["echo", "run in docker container"]}"#
                    .as_bytes()
                    .to_vec(),
            )
            .await;
        tracing::info!("result:{:?}", &res);
        res
    });

    let r = tokio::join!(handle1, handle2);
    tracing::info!("result:{:?}", &r);

    Ok(())
}
