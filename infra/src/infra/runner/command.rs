use super::Runner;
use crate::jobworkerp::runner::{CommandArg, CommandOperation};
use crate::{
    error::JobWorkerError,
    infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec},
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use proto::jobworkerp::data::RunnerType;
use std::{mem, process::Stdio};
use tokio::process::{Child, Command};
use tokio_stream::StreamExt;
use tokio_util::codec::{FramedRead, LinesCodec};

/**
 * CommandRunner
 * - command: command to run
 * - child: child process
 *
 * specify multiple arguments with \n separated     
 */
#[async_trait]
trait CommandRunner: Runner {
    fn command(&self) -> Command;
    fn child(&mut self) -> &mut Option<Box<Child>>;
    fn set_child(&mut self, child: Option<Child>);
    fn consume_child(&mut self) -> Option<Box<Child>>;
}

#[derive(Debug)]
pub struct CommandRunnerImpl {
    pub process: Option<Box<Child>>,
    pub command: Box<String>,
}
impl CommandRunnerImpl {
    pub fn new() -> Self {
        Self {
            process: None,
            command: Box::new("".to_string()),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.command.is_empty()
    }
}

impl Default for CommandRunnerImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for CommandRunnerImpl {
    fn clone(&self) -> Self {
        Self {
            process: None, // cannot clone process
            command: self.command.clone(),
        }
    }
}

#[async_trait]
impl CommandRunner for CommandRunnerImpl {
    fn command(&self) -> Command {
        Command::new(self.command.as_ref())
    }

    fn child(&mut self) -> &mut Option<Box<Child>> {
        &mut self.process
    }

    fn consume_child(&mut self) -> Option<Box<Child>> {
        let mut p = None;
        mem::swap(&mut self.process, &mut p);
        p
    }

    fn set_child(&mut self, child: Option<Child>) {
        self.process = child.map(Box::new);
    }
}

#[async_trait]
impl Runner for CommandRunnerImpl {
    fn name(&self) -> String {
        RunnerType::Command.as_str_name().to_string()
    }
    async fn load(&mut self, operation: Vec<u8>) -> Result<()> {
        let data = JobqueueAndCodec::deserialize_message::<CommandOperation>(&operation)
            .context("on run job")?;
        self.command = Box::new(data.name);
        Ok(())
    }
    // arg: assumed as utf-8 string, specify multiple arguments with \n separated
    async fn run(&mut self, arg: &[u8]) -> Result<Vec<Vec<u8>>> {
        if self.is_empty() {
            return Err(JobWorkerError::RuntimeError("command is empty".to_string()).into());
        }
        let data =
            JobqueueAndCodec::deserialize_message::<CommandArg>(arg).context("on run job")?;
        let args: &Vec<String> = &data.args;
        let mut messages = Vec::<Vec<u8>>::new();
        tracing::info!("run command: {}, args: {:?}", &self.command, args);
        match self
            .command()
            .args(args)
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .spawn()
        {
            Ok(child) => {
                tracing::debug!("spawned child: {:?}", child);
                self.set_child(Some(child));
                if let Some(c) = self.child() {
                    let stdout = c.stdout.take().unwrap();
                    let mut reader = FramedRead::new(stdout, LinesCodec::new());

                    while let Some(line) = reader.next().await {
                        match line {
                            Ok(l) => messages.push(format!("{}\n", l).bytes().collect()),
                            Err(e) => tracing::error!("command line decode err: {:?}", e),
                        }
                    }
                }
                tracing::info!("command has run: {}, result:{:?}", &self.command, &messages);
                self.set_child(None);
                // XXX divide each line as vec element in response? (can select with specifing by option?)
                Ok(vec![messages.concat()])
            }
            Err(e) => {
                tracing::error!("error in run command: {}, err:{:?}", &self.command, &e);
                Err(
                    JobWorkerError::RuntimeError(std::format!("command worker error: {:?}", e))
                        .into(),
                )
            }
        }
    }
    async fn cancel(&mut self) {
        if let Some(c) = self.consume_child() {
            drop(c);
        }
    }
    fn operation_proto(&self) -> String {
        include_str!("../../../protobuf/jobworkerp/runner/command_operation.proto").to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../../../protobuf/jobworkerp/runner/command_args.proto").to_string()
    }
    fn result_output_proto(&self) -> Option<String> {
        Some("".to_string())
    }
    fn use_job_result(&self) -> bool {
        false
    }
}

// test CommandRunnerImpl run with command '/usr/bin/sleep' and arg '10'
#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobworkerp::runner::CommandArg;
    use tokio::time::{sleep, Duration};

    #[ignore = "outbound network test"]
    #[tokio::test]
    async fn test_run() {
        let mut runner = CommandRunnerImpl {
            process: None,
            command: Box::new("/usr/bin/curl".to_string()),
        };
        let arg = CommandArg {
            args: vec!["-vvv".to_string(), "https://www.google.com".to_string()],
        };
        let res = runner.run(&JobqueueAndCodec::serialize_message(&arg)).await;
        assert!(res.is_ok());
        let r = res.unwrap().pop().unwrap();
        let mes = String::from_utf8_lossy(r.as_ref()).to_string();
        // print!("====== res: {:?}", &mes);

        assert!(!mes.is_empty());
    }

    #[tokio::test]
    async fn test_cancel() {
        let mut runner = CommandRunnerImpl {
            process: None,
            command: Box::new("/bin/sleep".to_string()),
        };
        let arg = CommandArg {
            args: vec!["10".to_string()],
        };
        let res = runner.run(&JobqueueAndCodec::serialize_message(&arg)).await;

        print!("====== run and cancel res: {:?}", res);
        assert!(res.is_ok());
        assert!(res.unwrap()[0].is_empty());
        runner.cancel().await;
        sleep(Duration::from_secs(1)).await;
        assert!(runner.process.is_none());
    }
}
