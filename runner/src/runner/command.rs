use crate::jobworkerp::runner::{CommandArgs, CommandResult};
use crate::{schema_to_json_string, schema_to_json_string_option};

use super::{RunnerSpec, RunnerTrait};
use anyhow::{Context, Result};
use async_stream::stream;
use async_trait::async_trait;
use command_utils::trace::Tracing;
use futures::stream::BoxStream;
use jobworkerp_base::{
    codec::{ProstMessageCodec, UseProstCodec},
    error::JobWorkerError,
};
use proto::jobworkerp::data::{result_output_item::Item, StreamingOutputType};
use proto::jobworkerp::data::{ResultOutputItem, RunnerType};
use std::collections::HashMap;
use std::pin::Pin;
use std::{
    future::Future,
    mem,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use sysinfo::System;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};
use tokio_stream::StreamExt;
use tokio_util::codec::{FramedRead, LinesCodec};
use tokio_util::sync::CancellationToken;

use super::cancellation::CancelMonitoring;
use super::cancellation_helper::{CancelMonitoringHelper, UseCancelMonitoringHelper};
use proto::jobworkerp::data::{JobData, JobId, JobResult};

/**
 * CommandRunner
 * - command: command to run
 * - child: child process
 *
 * specify multiple arguments with \n separated
 */
#[async_trait]
trait CommandRunner: RunnerTrait {
    fn child(&mut self) -> &mut Option<Box<Child>>;
    fn set_child(&mut self, child: Option<Child>);
    fn consume_child(&mut self) -> Option<Box<Child>>;
}

#[derive(Debug)]
pub struct CommandRunnerImpl {
    pub process: Option<Box<Child>>,
    pub stream_process_pid: Arc<RwLock<Option<u32>>>,
    // Helper for DI integration (Optional)
    cancel_helper: Option<CancelMonitoringHelper>,
}
impl Default for CommandRunnerImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandRunnerImpl {
    /// Gracefully kill a child process with SIGTERM, fallback to SIGKILL
    async fn graceful_kill_process(child: &mut tokio::process::Child) -> bool {
        if let Some(pid) = child.id() {
            #[cfg(unix)]
            {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;

                // First try SIGTERM for graceful shutdown
                if let Err(e) = kill(Pid::from_raw(pid as i32), Signal::SIGTERM) {
                    tracing::warn!("Failed to send SIGTERM to process {}: {}", pid, e);
                    return false;
                } else {
                    tracing::debug!("Sent SIGTERM to process {} for graceful shutdown", pid);
                }

                // Wait for graceful shutdown with 5 second timeout
                match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
                    Ok(Ok(status)) => {
                        tracing::debug!(
                            "Process {} terminated gracefully with status: {}",
                            pid,
                            status
                        );
                        return true;
                    }
                    Ok(Err(e)) => {
                        tracing::warn!("Error waiting for process {}: {}", pid, e);
                    }
                    Err(_) => {
                        // Force kill with SIGKILL if timeout
                        tracing::warn!(
                            "Process {} did not terminate gracefully, force killing",
                            pid
                        );
                        if let Err(e) = kill(Pid::from_raw(pid as i32), Signal::SIGKILL) {
                            tracing::error!("Failed to send SIGKILL to process {}: {}", pid, e);
                        }
                        let _ = child.kill().await;
                    }
                }
            }
            #[cfg(not(unix))]
            {
                let _ = child.kill().await;
            }
        }
        false
    }

    /// Constructor without cancellation monitoring (backward compatibility)
    pub fn new() -> Self {
        Self {
            process: None,
            stream_process_pid: Arc::new(RwLock::new(None)),
            cancel_helper: None, // Explicitly None
        }
    }

    /// Constructor with cancellation monitoring (DI integration version)
    pub fn new_with_cancel_monitoring(cancel_helper: CancelMonitoringHelper) -> Self {
        Self {
            process: None,
            stream_process_pid: Arc::new(RwLock::new(None)),
            cancel_helper: Some(cancel_helper), // Explicitly Some
        }
    }

    /// Unified token acquisition method
    async fn get_cancellation_token(&self) -> CancellationToken {
        // Clear determination of cancellation monitoring necessity
        if let Some(helper) = &self.cancel_helper {
            helper.get_cancellation_token().await
        } else {
            // No cancellation monitoring - default token
            CancellationToken::new()
        }
    }

    pub async fn reset(&mut self) -> Result<()> {
        self.process = None;
        let mut pid = self.stream_process_pid.write().await;
        *pid = None; // Reset the stream process PID
        Ok(())
    }
}

#[async_trait]
impl CommandRunner for CommandRunnerImpl {
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

impl Tracing for CommandRunnerImpl {}

impl RunnerSpec for CommandRunnerImpl {
    fn name(&self) -> String {
        RunnerType::Command.as_str_name().to_string()
    }
    fn runner_settings_proto(&self) -> String {
        "".to_string()
    }
    // Phase 6.6: Unified method_proto_map for all runners
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            "run".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!("../../protobuf/jobworkerp/runner/command_args.proto")
                    .to_string(),
                result_proto: include_str!("../../protobuf/jobworkerp/runner/command_result.proto")
                    .to_string(),
                description: Some("Execute shell command".to_string()),
                output_type: StreamingOutputType::Both as i32,
            },
        );
        schemas
    }
    fn settings_schema(&self) -> String {
        schema_to_json_string!(crate::jobworkerp::runner::Empty, "settings_schema")
    }
    fn arguments_schema(&self) -> String {
        schema_to_json_string!(CommandArgs, "arguments_schema")
    }
    fn output_schema(&self) -> Option<String> {
        schema_to_json_string_option!(CommandResult, "output_schema")
    }
}

#[async_trait]
impl RunnerTrait for CommandRunnerImpl {
    async fn load(&mut self, _settings: Vec<u8>) -> Result<()> {
        // do nothing
        Ok(())
    }
    // arg: assumed as utf-8 string, specify multiple arguments with \n separated
    async fn run(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        // let (span, cx) =
        //     Self::tracing_span_from_metadata(&metadata, APP_WORKER_NAME, "COMMAND::run");
        // let _guard = span.enter();
        // let mut metadata = metadata.clone();
        // Self::inject_metadata_from_context(&mut metadata, &cx);

        // Clear and concise token acquisition
        let cancellation_token = self.get_cancellation_token().await;

        let res = async {
        let data =
            ProstMessageCodec::deserialize_message::<CommandArgs>(args).context("on run job")?;
        let mut command = Command::new(data.command.as_str());
        let args: &Vec<String> = &data.args;
        let mut stdout_messages = Vec::<String>::new();
        let mut stderr_messages = Vec::<String>::new();

        // Get current Unix time in milliseconds using SystemTime
        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis() as u64;

        // Start timing the command execution (for execution duration)
        let start_time = Instant::now();

        // For memory monitoring
        let max_memory = Arc::new(Mutex::new(0u64));
        // Flag to check if we should monitor memory
        let should_monitor_memory = data.with_memory_monitoring;

        tracing::info!(
            "run command: {}, args: {:?}, monitor_memory: {}, started_at: {}",
            &data.command,
            args,
            should_monitor_memory,
            started_at
        );
        // Use tokio::select! to monitor cancellation during process spawn
        let spawn_result = tokio::select! {
            spawn_result = async {
                command
                    .args(args)
                    .kill_on_drop(true)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
            } => spawn_result,
            _ = cancellation_token.cancelled() => {
                tracing::info!("Command execution was cancelled before spawn");
                return Err(anyhow::anyhow!("Command execution was cancelled before spawn"));
            }
        };

        match spawn_result {
            Ok(child) => {
                tracing::debug!("spawned child: {:?}", child);
                self.set_child(Some(child));

                // Memory monitoring task handle - initialize as None
                let memory_monitor_handle = if should_monitor_memory {
                    // Get process ID for memory monitoring
                    let pid = if let Some(c) = self.child() {
                        c.id().map(|id| id as usize)
                    } else {
                        None
                    };

                    // Set up memory monitoring if we have a valid PID
                    if let Some(process_pid) = pid {
                        let max_mem = Arc::clone(&max_memory);
                        // Create a oneshot channel to signal the monitoring task to stop
                        let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();

                        // Spawn a task to monitor memory usage
                        Some((
                            tokio::spawn(async move {
                                let mut sys = System::new();
                                let mut current_max = 0u64;

                                // Poll every 100ms for memory usage
                                loop {
                                    // Check if stop signal received
                                    if rx.try_recv().is_ok() {
                                        tracing::debug!(
                                            "Memory monitoring task received stop signal"
                                        );
                                        break;
                                    }

                                    let pids = [sysinfo::Pid::from(process_pid)];
                                    sys.refresh_processes(
                                        sysinfo::ProcessesToUpdate::Some(&pids[..]),
                                        false,
                                    );
                                    if let Some(process) = sys.process(pids[0]) {
                                        let memory = process.memory(); // in Byte
                                        tracing::info!(
                                            "Memory usage: {:.3}KB",
                                            memory as f64 / 1024.0
                                        );
                                        if memory > current_max {
                                            current_max = memory;
                                            let mut max = max_mem.lock().await;
                                            *max = current_max;
                                        }
                                    } else {
                                        // Process not found, probably exited
                                        break;
                                    }
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                }
                            }),
                            tx,
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(c) = self.child() {
                    let stdout = c.stdout.take().unwrap();
                    let stderr = c.stderr.take().unwrap();

                    let mut stdout_reader = FramedRead::new(stdout, LinesCodec::new());
                    let mut stderr_reader = FramedRead::new(stderr, LinesCodec::new());

                    // Process stdout and stderr concurrently with cancellation monitoring
                    let mut stdout_done = false;
                    let mut stderr_done = false;

                    while !stdout_done || !stderr_done {
                        tokio::select! {
                            stdout_line = stdout_reader.next(), if !stdout_done => {
                                match stdout_line {
                                    Some(Ok(line)) => stdout_messages.push(line),
                                    Some(Err(e)) => tracing::error!("command stdout line decode err: {:?}", e),
                                    None => stdout_done = true,
                                }
                            }
                            stderr_line = stderr_reader.next(), if !stderr_done => {
                                match stderr_line {
                                    Some(Ok(line)) => stderr_messages.push(line),
                                    Some(Err(e)) => tracing::error!("command stderr line decode err: {:?}", e),
                                    None => stderr_done = true,
                                }
                            }
                            _ = cancellation_token.cancelled() => {
                                tracing::info!("Command execution cancelled during output reading");
                                // Get child process and kill it
                                if let Some(mut child) = self.consume_child() {
                                    Self::graceful_kill_process(&mut child).await;
                                }
                                return Err(anyhow::anyhow!("Command execution was cancelled during output reading"));
                            }
                        }
                    }
                }

                // Wait for memory monitor to finish if it was started
                if let Some((handle, stop_tx)) = memory_monitor_handle {
                    // Signal the monitoring task to stop
                    let _ = stop_tx.send(());

                    // Create a clone of the handle for aborting if timeout occurs
                    let handle_clone = handle.abort_handle();

                    // Give it a little time to finish but don't wait forever
                    match tokio::time::timeout(Duration::from_millis(500), handle).await {
                        Ok(_) => {}
                        Err(_) => {
                            tracing::warn!("Memory monitor task didn't complete in time, aborting");
                            handle_clone.abort();
                        }
                    }
                }

                // Get the maximum memory usage - default to 0 if not monitored
                let max_memory_usage = if should_monitor_memory {
                    *max_memory.lock().await
                } else {
                    0
                };

                // Calculate execution time in milliseconds
                let execution_time_ms = start_time.elapsed().as_millis() as u64;

                // Get the exit code from the child process with cancellation monitoring
                let exit_code = match self.consume_child() {
                    Some(mut child) => {
                        // Monitor cancellation during process execution
                        tokio::select! {
                            wait_result = child.wait() => {
                                match wait_result {
                                    Ok(status) => status,
                                    Err(e) => {
                                        tracing::error!("Failed to wait for child process: {:?}", e);
                                        std::process::ExitStatus::default() // Provide a default exit status
                                    }
                                }
                            }
                            _ = cancellation_token.cancelled() => {
                                tracing::info!("Command execution was cancelled during process execution");
                                // Gracefully kill the child process using shared function
                                Self::graceful_kill_process(&mut child).await;
                                return Err(anyhow::anyhow!("Command execution was cancelled during process execution"));
                            }
                        }
                    }
                    None => {
                        tracing::warn!("Child process already completed or not available");
                        std::process::ExitStatus::default() // Provide a default exit status
                    }
                };

                tracing::info!(
                    "command has run: {}, started_at: {}ms, execution_time: {}ms, peak_memory: {}KB, stdout:{:?}, stderr:{:?}",
                    &data.command,
                    started_at,
                    execution_time_ms,
                    max_memory_usage/1024,
                    &stdout_messages,
                    &stderr_messages,
                );

                let result = CommandResult {
                    exit_code: exit_code.code(),
                    stdout: Some(stdout_messages.join("\n")),
                    stderr: Some(stderr_messages.join("\n")),
                    execution_time_ms: Some(execution_time_ms),
                    started_at: Some(started_at), // Use Unix timestamp in milliseconds
                    max_memory_usage_kb: if should_monitor_memory {
                        Some(max_memory_usage / 1024)
                    } else {
                        None
                    },
                };

                // Serialize the result
                let serialized_result = ProstMessageCodec::serialize_message(&result)?;

                Ok(serialized_result)
            }
            Err(e) => {
                tracing::error!(
                    "error in run command: {}, args: {:?}, err:{:?}",
                    &data.command,
                    &data.args,
                    &e
                );
                Err(
                    JobWorkerError::RuntimeError(std::format!("command worker error: {e:?}"))
                        .into(),
                )
            }
        }
        }.await;

        let _ = self.reset().await; // Ignore reset errors in cleanup
                                    // Result processing simplified
        (res, metadata)
    }
    async fn run_stream(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // Set up cancellation token using manager
        let cancellation_token = self.get_cancellation_token().await;

        let data = ProstMessageCodec::deserialize_message::<CommandArgs>(args)
            .context("on run_stream job")?;
        let command_str = data.command.clone();
        let args_vec = data.args.clone(); // Clone the args to own them
        let should_monitor_memory = data.with_memory_monitoring;

        // Clone stream_process_pid for use within the stream
        let stream_pid_ref = self.stream_process_pid.clone();

        // Note: cancellation token is already cloned above, no need for helper

        // Note: Cancellation manager lifecycle is now handled by DI Helper system

        // Create mutable command here
        let mut command = Command::new(command_str.as_str());

        // Get current Unix time in milliseconds using SystemTime
        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis() as u64;

        // Start timing the command execution (for execution duration)
        let start_time = Instant::now();

        // For memory monitoring
        let max_memory = Arc::new(Mutex::new(0u64));

        tracing::info!(
            "run_stream command: {}, args: {:?}, monitor_memory: {}, started_at: {}",
            &command_str,
            &args_vec,
            should_monitor_memory,
            started_at
        );
        // let (span, cx) =
        //     Self::tracing_span_from_metadata(&metadata, APP_WORKER_NAME, "COMMAND::run_stream");
        // let _guard = span.enter();
        // let mut metadata = metadata.clone();
        // Self::inject_metadata_from_context(&mut metadata, &cx);

        let trailer = Arc::new(proto::jobworkerp::data::Trailer {
            metadata: metadata.clone(),
        });

        // Stream_process_pid already cloned above

        // Create the stream - use stream! instead of try_stream! to handle errors internally
        let stream = stream! {
            match command
                .args(&args_vec)  // Use the cloned args vector
                .kill_on_drop(true)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(mut child) => {
                    tracing::debug!("spawned child in stream mode: {:?}", child);
                    let process_id = child.id();

                    // Store the process ID for cancellation from outside the stream
                    if let Some(pid) = process_id {
                        let mut pid_guard = stream_pid_ref.write().await;
                        *pid_guard = Some(pid);
                        tracing::debug!("Set stream process PID for cancellation: {}", pid);
                    }

                    // Set up memory monitoring if enabled
                    let memory_monitor_handle = if should_monitor_memory && process_id.is_some() {
                        let process_pid = process_id.unwrap() as usize;
                        let max_mem = Arc::clone(&max_memory);

                        // Create a oneshot channel to signal the monitoring task to stop
                        let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();

                        // Spawn a task to monitor memory usage
                        Some((tokio::spawn(async move {
                            let mut sys = System::new();
                            let mut current_max = 0u64;

                            // Poll every 100ms for memory usage
                            loop {
                                // Check if stop signal received
                                if rx.try_recv().is_ok() {
                                    tracing::debug!("Memory monitoring task in stream mode received stop signal");
                                    break;
                                }

                                let pids = [sysinfo::Pid::from(process_pid)];
                                sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&pids[..]), false);
                                if let Some(process) = sys.process(pids[0]) {
                                    let memory = process.memory(); // in Byte
                                    if memory > current_max {
                                        current_max = memory;
                                        let mut max = max_mem.lock().await;
                                        *max = current_max;
                                    }
                                } else {
                                    // Process not found, probably exited
                                    break;
                                }
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        }), tx))
                    } else {
                        None
                    };

                    // Take stdout and stderr
                    let mut stdout = match child.stdout.take() {
                        Some(stdout) => stdout,
                        None => {
                            // Error handling - create CommandResult with error message
                            let error_result = CommandResult {
                                exit_code: Some(-1),
                                stdout: None,
                                stderr: Some("=== FAILED TO CAPTURE STDOUT ===".to_string()),
                                execution_time_ms: None,
                                started_at: Some(started_at),
                                max_memory_usage_kb: None,
                            };

                            if let Ok(serialized) = ProstMessageCodec::serialize_message(&error_result) {
                                yield ResultOutputItem {
                                    item: Some(Item::Data(serialized)),
                                };
                            }

                            yield ResultOutputItem {
                                item: Some(Item::End((*trailer).clone() )),
                            };

                            return;
                        }
                    };

                    let mut stderr = match child.stderr.take() {
                        Some(stderr) => stderr,
                        None => {
                            // Error handling - create CommandResult with error message
                            let error_result = CommandResult {
                                exit_code: Some(-1),
                                stdout: None,
                                stderr: Some("=== FAILED TO CAPTURE STDERR ===".to_string()),
                                execution_time_ms: None,
                                started_at: Some(started_at),
                                max_memory_usage_kb: None,
                            };

                            if let Ok(serialized) = ProstMessageCodec::serialize_message(&error_result) {
                                yield ResultOutputItem {
                                    item: Some(Item::Data(serialized)),
                                };
                            }

                            yield ResultOutputItem {
                                item: Some(Item::End((*trailer).clone() )),
                            };

                            return;
                        }
                    };

                    // Create buffers for reading
                    let mut stdout_buf = Vec::with_capacity(1024);
                    let mut stderr_buf = Vec::with_capacity(1024);

                    // Track if we've seen EOF on both streams
                    let mut stdout_done = false;
                    let mut stderr_done = false;

                    // Set up buffers for processing lines
                    let mut stdout_bytes = Vec::new();
                    let mut stderr_bytes = Vec::new();

                    // Create mutable handles to track progress
                    let mut child = child;

                    // Stream processing loop
                    while !stdout_done || !stderr_done {
                        let stdout_fut = if !stdout_done {
                            // If stdout is not done, read from it
                            Box::pin(tokio::io::AsyncReadExt::read_buf(&mut stdout, &mut stdout_buf))
                                as Pin<Box<dyn Future<Output = std::io::Result<usize>> + Send>>
                        } else {
                            // Otherwise, provide a future that never completes
                            Box::pin(futures::future::pending::<std::io::Result<usize>>())
                                as Pin<Box<dyn Future<Output = std::io::Result<usize>> + Send>>
                        };

                        let stderr_fut = if !stderr_done {
                            // If stderr is not done, read from it
                            Box::pin(tokio::io::AsyncReadExt::read_buf(&mut stderr, &mut stderr_buf))
                                as Pin<Box<dyn Future<Output = std::io::Result<usize>> + Send>>
                        } else {
                            // Otherwise, provide a future that never completes
                            Box::pin(futures::future::pending::<std::io::Result<usize>>())
                                as Pin<Box<dyn Future<Output = std::io::Result<usize>> + Send>>
                        };

                        // Read from whichever stream has data available first
                        tokio::select! {
                            stdout_result = stdout_fut, if !stdout_done => {
                                match stdout_result {
                                    Ok(0) => {
                                        // EOF reached on stdout
                                        stdout_done = true;
                                    },
                                    Ok(n) => {
                                        // Process the bytes we read
                                        let new_bytes = stdout_buf.split_off(stdout_buf.len() - n);
                                        stdout_bytes.extend_from_slice(&new_bytes);

                                        // Create lines of output
                                        let mut start = 0;
                                        for (i, &b) in stdout_bytes.iter().enumerate() {
                                            if b == b'\n' {
                                                if i > start {
                                                    let line = &stdout_bytes[start..i];

                                                    // Create CommandResult with stdout data
                                                    let result = CommandResult {
                                                        exit_code: None, // Not finished yet
                                                        stdout: Some(String::from_utf8_lossy(line).to_string()),
                                                        stderr: None,
                                                        execution_time_ms: None, // Not finished yet
                                                        started_at: Some(started_at),
                                                        max_memory_usage_kb: if should_monitor_memory {
                                                            let mem = *max_memory.lock().await;
                                                            if mem > 0 { Some(mem/1024) } else { None }
                                                        } else { None },
                                                    };

                                                    // Serialize and send
                                                    if let Ok(serialized) = ProstMessageCodec::serialize_message(&result) {
                                                        yield ResultOutputItem {
                                                            item: Some(Item::Data(serialized)),
                                                        };
                                                    }
                                                }
                                                start = i + 1;
                                            }
                                        }

                                        // Keep any incomplete line for next time
                                        if start < stdout_bytes.len() {
                                            stdout_bytes = stdout_bytes[start..].to_vec();
                                        } else {
                                            stdout_bytes.clear();
                                        }
                                    },
                                    Err(e) => {
                                        tracing::error!("Error reading from stdout: {:?}", e);
                                        stdout_done = true;
                                    }
                                }
                            },
                            stderr_result = stderr_fut, if !stderr_done => {
                                match stderr_result {
                                    Ok(0) => {
                                        // EOF reached on stderr
                                        stderr_done = true;
                                    },
                                    Ok(n) => {
                                        // Process the bytes we read
                                        let new_bytes = stderr_buf.split_off(stderr_buf.len() - n);
                                        stderr_bytes.extend_from_slice(&new_bytes);

                                        // Create lines of output
                                        let mut start = 0;
                                        for (i, &b) in stderr_bytes.iter().enumerate() {
                                            if b == b'\n' {
                                                if i > start {
                                                    let line = &stderr_bytes[start..i];

                                                    // Create CommandResult with stderr data
                                                    let result = CommandResult {
                                                        exit_code: None, // Not finished yet
                                                        stdout: None,
                                                        stderr: Some(String::from_utf8_lossy(line).to_string()),
                                                        execution_time_ms: None, // Not finished yet
                                                        started_at: Some(started_at),
                                                        max_memory_usage_kb: if should_monitor_memory {
                                                            let mem = *max_memory.lock().await;
                                                            if mem > 0 { Some(mem/1024) } else { None }
                                                        } else { None },
                                                    };

                                                    // Serialize and send
                                                    if let Ok(serialized) = ProstMessageCodec::serialize_message(&result) {
                                                        yield ResultOutputItem {
                                                            item: Some(Item::Data(serialized)),
                                                        };
                                                    }
                                                }
                                                start = i + 1;
                                            }
                                        }

                                        // Keep any incomplete line for next time
                                        if start < stderr_bytes.len() {
                                            stderr_bytes = stderr_bytes[start..].to_vec();
                                        } else {
                                            stderr_bytes.clear();
                                        }
                                    },
                                    Err(e) => {
                                        tracing::error!("Error reading from stderr: {:?}", e);
                                        stderr_done = true;
                                    }
                                }
                            },
                            // Check if process exited, but continue reading output
                            exit_result = child.wait() => {
                                match exit_result {
                                    Ok(status) => {
                                        tracing::debug!("Process exited with status: {:?}", status);
                                        // Even if the process exited, we still want to read all output
                                    },
                                    Err(e) => {
                                        tracing::error!("Error waiting for process: {:?}", e);
                                    }
                                }
                            }
                            _ = cancellation_token.cancelled() => {
                                tracing::info!("Command stream execution was cancelled");
                                // Note: cancellation is already handled by the token

                                // Gracefully kill the process
                                Self::graceful_kill_process(&mut child).await;

                                // Clear the PID from stream_pid_ref
                                let mut pid_guard = stream_pid_ref.write().await;
                                *pid_guard = None;
                                drop(pid_guard);

                                break;
                            }
                        }
                    }

                    // Wait for the process to finish if it hasn't already
                    let exit_code = match child.wait().await {
                        Ok(status) => status.code(),
                        Err(e) => {
                            tracing::error!("Error waiting for process: {:?}", e);
                            None
                        }
                    };

                    // Wait for memory monitor to finish if it was started
                    if let Some((handle, stop_tx)) = memory_monitor_handle {
                        // Signal the monitoring task to stop
                        let _ = stop_tx.send(());

                        // Create a clone of the handle for aborting if timeout occurs
                        let handle_clone = handle.abort_handle();

                        // Give it a little time to finish but don't wait forever
                        match tokio::time::timeout(Duration::from_millis(500), handle).await {
                            Ok(_) => {},
                            Err(_) => {
                                tracing::warn!("Memory monitor task in stream mode didn't complete in time, aborting");
                                handle_clone.abort();
                            }
                        }
                    }

                    // Get the maximum memory usage
                    let max_memory_usage = if should_monitor_memory {
                        *max_memory.lock().await
                    } else {
                        0
                    };

                    // Calculate execution time
                    let execution_time_ms = start_time.elapsed().as_millis() as u64;

                    // Create the final result
                    tracing::info!(
                        "command_stream completed: {}, exit_code: {:?}, execution_time: {}ms, peak_memory: {}KB",
                        &data.command,
                        exit_code,
                        execution_time_ms,
                        max_memory_usage/1024
                    );

                    // Create final CommandResult with execution results (no stdout/stderr)
                    let final_result = CommandResult {
                        exit_code,
                        stdout: None,
                        stderr: None,
                        execution_time_ms: Some(execution_time_ms),
                        started_at: Some(started_at),
                        max_memory_usage_kb: if should_monitor_memory {Some(max_memory_usage/1024)} else {None},
                    };

                    // Serialize the final CommandResult
                    match ProstMessageCodec::serialize_message(&final_result) {
                        Ok(serialized) => {
                            yield ResultOutputItem {
                                item: Some(Item::Data(serialized)),
                            };
                        },
                        Err(e) => {
                            tracing::error!("Failed to serialize final CommandResult: {:?}", e);
                        }
                    }

                    // Reset stream process PID and cancellation helper
                    let mut pid_guard = stream_pid_ref.write().await;
                    *pid_guard = None;
                    drop(pid_guard);
                    // Note: token cleanup is handled by Manager

                    // Stream completed - cancellation monitoring cleanup is handled by process lifecycle
                    tracing::debug!("Stream completed for job, keeping cancellation monitoring active for process lifetime");

                    // Send the end of stream marker
                    yield ResultOutputItem {
                        item: Some(Item::End((*trailer).clone())),
                    };
                },
                Err(e) => {
                    tracing::error!(
                        "error in run_stream command: {}, args: {:?}, err:{:?}",
                        &command_str,  // Use the cloned command string
                        &args_vec,     // Use the cloned args vector
                        &e
                    );

                    // Create an error CommandResult
                    let error_result = CommandResult {
                        exit_code: Some(-1), // Conventional error code
                        stdout: None,
                        stderr: Some(format!("Command error: {e}")),
                        execution_time_ms: Some(0),
                        started_at: Some(started_at),
                        max_memory_usage_kb: None,
                    };

                    // Serialize the error CommandResult
                    match ProstMessageCodec::serialize_message(&error_result) {
                        Ok(serialized) => {
                            yield ResultOutputItem {
                                item: Some(Item::Data(serialized)),
                            };
                        },
                        Err(e) => {
                            tracing::error!("Failed to serialize error CommandResult: {:?}", e);
                        }
                    }

                    // Reset stream process PID and cancellation helper on error
                    let mut pid_guard = stream_pid_ref.write().await;
                    *pid_guard = None;
                    drop(pid_guard);
                    // Note: token cleanup is handled by Manager

                    // Stream failed - cancellation monitoring cleanup is handled by process lifecycle
                    tracing::debug!("Stream failed for job, keeping cancellation monitoring active for process lifetime");

                    // Always send end marker even on error
                    yield ResultOutputItem {
                        item: Some(Item::End((*trailer).clone())),
                    };
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

// CancelMonitoring implementation for CommandRunnerImpl
#[async_trait]
impl CancelMonitoring for CommandRunnerImpl {
    /// Initialize cancellation monitoring for specific job
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: JobId,
        job_data: &JobData,
    ) -> Result<Option<JobResult>> {
        // Clear helper availability check
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, job_data).await
        } else {
            tracing::debug!("No cancel monitoring configured for job {}", job_id.value);
            Ok(None)
        }
    }

    /// Cleanup cancellation monitoring
    async fn cleanup_cancellation_monitoring(&mut self) -> Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.cleanup_monitoring_impl().await
        } else {
            Ok(())
        }
    }

    /// Cancels active processes and cleans up resources for CommandRunnerImpl
    async fn request_cancellation(&mut self) -> Result<()> {
        // 1. Signal cancellation token
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
                tracing::info!("CommandRunner: cancellation token signaled");
            }
        }

        // 2. Terminate CommandRunner-specific processes
        if let Some(mut child) = self.consume_child() {
            Self::graceful_kill_process(&mut child).await;
            tracing::info!("CommandRunner: cancelled child process directly");
        }

        // 3. Terminate streaming processes
        let stream_pid = {
            let mut pid_guard = self.stream_process_pid.write().await;
            pid_guard.take()
        };
        if let Some(stream_pid) = stream_pid {
            #[cfg(unix)]
            {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;

                // First try SIGTERM for graceful shutdown
                if let Err(e) = kill(Pid::from_raw(stream_pid as i32), Signal::SIGTERM) {
                    tracing::warn!(
                        "Failed to send SIGTERM to stream process {}: {}",
                        stream_pid,
                        e
                    );
                } else {
                    tracing::debug!("Sent SIGTERM to stream process {}", stream_pid);
                    // Give it a moment for graceful shutdown
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    // Check if still running and force kill if needed
                    if kill(Pid::from_raw(stream_pid as i32), None).is_ok() {
                        tracing::warn!(
                            "Stream process {} still running, sending SIGKILL",
                            stream_pid
                        );
                        let _ = kill(Pid::from_raw(stream_pid as i32), Signal::SIGKILL);
                    }
                }
            }
            #[cfg(windows)]
            {
                tracing::debug!("Windows stream process termination not implemented");
            }
            tracing::info!(
                "CommandRunner: cancelled stream process by PID: {}",
                stream_pid
            );
        }

        Ok(())
    }

    /// Complete state reset for pool recycling
    /// Resets CommandRunner-specific state (stream_process_pid etc.) to prevent state contamination for next job
    async fn reset_for_pooling(&mut self) -> Result<()> {
        // Check if streaming process is still running
        let has_active_stream_process = {
            let pid_guard = self.stream_process_pid.read().await;
            pid_guard.is_some()
        };

        if has_active_stream_process {
            // Maintain cancellation monitoring during streaming process execution
            // Rely on automatic cleanup (timeout-based) when process terminates
            tracing::debug!("CommandRunner has active streaming process - keeping cancellation monitoring active");
        } else {
            // Cleanup cancellation monitoring only when process terminates
            if let Some(helper) = &mut self.cancel_helper {
                helper.reset_for_pooling_impl().await?;
            } else {
                self.cleanup_cancellation_monitoring().await?;
            }
        }

        // CommandRunner-specific state reset (non-streaming only)
        if !has_active_stream_process {
            let mut pid_guard = self.stream_process_pid.write().await;
            *pid_guard = None;
        }

        tracing::debug!(
            "CommandRunner reset for pooling (streaming active: {})",
            has_active_stream_process
        );
        Ok(())
    }
}

impl UseCancelMonitoringHelper for CommandRunnerImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}

// test CommandRunnerImpl run with command '/usr/bin/sleep' and arg '10'
#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobworkerp::runner::CommandArgs;
    use futures::StreamExt;
    use tokio::time::{sleep, Duration};

    // Use common mock from test_common module

    #[tokio::test]
    async fn test_run() {
        let mut runner = CommandRunnerImpl::new();
        let arg = CommandArgs {
            command: "/bin/echo".to_string(),
            args: vec!["Hello, World!".to_string()],
            with_memory_monitoring: true,
        };

        let res = runner
            .run(
                &ProstMessageCodec::serialize_message(&arg).unwrap(),
                HashMap::new(),
                None,
            )
            .await;

        assert!(res.0.is_ok());
        let result_bytes = res.0.unwrap();
        let result =
            ProstMessageCodec::deserialize_message::<CommandResult>(&result_bytes).unwrap();

        // Check that the output contains the expected string
        let binding = result.stdout.unwrap_or_default();
        let stdout = &binding;
        assert!(stdout.contains("Hello, World!"));

        // Verify execution time is present
        assert!(result.execution_time_ms.is_some());

        // Verify started_at is present
        assert!(result.started_at.is_some());

        // Verify memory monitoring data
        assert!(result.max_memory_usage_kb.is_some());

        // Verify exit code is 0 (success)
        assert_eq!(result.exit_code, Some(0));
    }

    #[tokio::test]
    async fn test_run_without_memory_monitoring() {
        let mut runner = CommandRunnerImpl::new();
        let arg = CommandArgs {
            command: "/bin/echo".to_string(),
            args: vec!["No memory monitoring".to_string()],
            with_memory_monitoring: false,
        };

        let res = runner
            .run(
                &ProstMessageCodec::serialize_message(&arg).unwrap(),
                HashMap::new(),
                None,
            )
            .await;

        assert!(res.0.is_ok());
        let result_bytes = res.0.unwrap();
        let result =
            ProstMessageCodec::deserialize_message::<CommandResult>(&result_bytes).unwrap();

        // Memory monitoring should be None when disabled
        assert!(result.max_memory_usage_kb.is_none());
    }

    #[tokio::test]
    async fn test_run_with_error() {
        let mut runner = CommandRunnerImpl::new();
        let arg = CommandArgs {
            command: "/bin/non_existent_command".to_string(),
            args: vec![],
            with_memory_monitoring: false,
        };

        let res = runner
            .run(
                &ProstMessageCodec::serialize_message(&arg).unwrap(),
                HashMap::new(),
                None,
            )
            .await;

        // Should return an error for non-existent command
        assert!(res.0.is_err());
    }

    #[tokio::test]
    async fn test_run_stream() {
        let mut runner = CommandRunnerImpl::new();
        let arg = CommandArgs {
            command: "/bin/echo".to_string(),
            args: vec!["Stream test".to_string()],
            with_memory_monitoring: true,
        };

        let stream_result = runner
            .run_stream(
                &ProstMessageCodec::serialize_message(&arg).unwrap(),
                HashMap::new(),
                None,
            )
            .await;

        assert!(stream_result.is_ok());
        let mut stream = stream_result.unwrap();

        let mut found_stdout_data = false;
        let mut found_final_result = false;
        let mut found_end = false;

        // Collect all items from the stream
        while let Some(item) = stream.next().await {
            match item.item {
                Some(Item::Data(data)) => {
                    // Deserialize the data as CommandResult
                    let result = ProstMessageCodec::deserialize_message::<CommandResult>(&data);
                    assert!(
                        result.is_ok(),
                        "Should be able to deserialize the data as CommandResult"
                    );

                    let result = result.unwrap();
                    println!("Stream result: exit_code: {:?}, stdout: {:?}, stderr: {:?}, execution_time_ms: {:?}",
                        result.exit_code,
                        result.stdout.as_ref(),
                        result.stderr.as_ref(),
                        result.execution_time_ms
                    );

                    // Check if this is a stdout data packet or the final result
                    if let Some(stdout) = result.stdout {
                        // This is an intermediate output, should contain our test string
                        let stdout_str = &stdout;
                        if stdout_str.contains("Stream test") {
                            found_stdout_data = true;
                        }
                    } else if result.exit_code.is_some() && result.execution_time_ms.is_some() {
                        // This is the final result packet
                        found_final_result = true;
                    }
                }
                Some(Item::End(_)) => {
                    found_end = true;
                    break;
                }
                None => {}
            }
        }

        // Verify we received both data and the end marker
        assert!(
            found_stdout_data,
            "Stream should produce CommandResult with stdout data"
        );
        assert!(
            found_final_result,
            "Stream should produce a final CommandResult with exit code and execution time"
        );
        assert!(found_end, "Stream should produce an end marker");
    }

    #[tokio::test]
    async fn test_run_stream_with_multiple_lines() {
        use std::io::{self, Write};

        let mut runner = CommandRunnerImpl::new();
        // Use a command that outputs multiple lines with sleep to demonstrate real-time streaming
        let arg = CommandArgs {
            command: "/bin/bash".to_string(),
            args: vec![
                "-c".to_string(),
                "echo 'Line 1'; sleep 0.5; echo 'Line 2'; sleep 0.5; echo 'Line 3'".to_string(),
            ],
            with_memory_monitoring: true,
        };

        eprintln!("\n=== Starting stream test with multiple lines ===");

        // Get the stream
        let stream_result = runner
            .run_stream(
                &ProstMessageCodec::serialize_message(&arg).unwrap(),
                HashMap::new(),
                None,
            )
            .await;

        assert!(stream_result.is_ok());
        let stream = stream_result.unwrap();

        // Create channels for communicating test results
        let (tx, mut rx) = tokio::sync::mpsc::channel::<(bool, bool, usize)>(10);

        // Spawn a task to process the stream so we can see real-time output
        tokio::spawn(async move {
            let mut stream = stream;
            let mut found_final_result = false;
            let mut found_end = false;
            let mut line_count = 0;
            let start_time = Instant::now();

            eprintln!("Stream processing started...");
            io::stderr().flush().unwrap();

            // Process all items from the stream
            while let Some(item) = stream.next().await {
                let elapsed = start_time.elapsed().as_millis();

                match item.item {
                    Some(Item::Data(data)) => {
                        // Deserialize the data as CommandResult
                        let result = ProstMessageCodec::deserialize_message::<CommandResult>(&data);
                        assert!(
                            result.is_ok(),
                            "Should be able to deserialize the data as CommandResult"
                        );

                        let result = result.unwrap();

                        // Check if this is a stdout/stderr data packet or the final result
                        if let Some(stdout) = result.stdout {
                            // This is an intermediate output line
                            let stdout_str = &stdout;
                            eprintln!("[{}ms] STDOUT: {}", elapsed, stdout_str.trim());
                            io::stderr().flush().unwrap();

                            if stdout_str.contains("Line") {
                                line_count += 1;
                            }
                        } else if let Some(stderr) = result.stderr {
                            // This is an error or stderr output
                            let stderr_str = &stderr;
                            eprintln!("[{}ms] STDERR: {}", elapsed, stderr_str.trim());
                            io::stderr().flush().unwrap();

                            if stderr_str.contains("Line") {
                                line_count += 1;
                            }
                        } else if result.exit_code.is_some() && result.execution_time_ms.is_some() {
                            // This is the final result packet
                            eprintln!(
                                "[{}ms] RESULT: exit_code={:?}, execution_time={}ms",
                                elapsed,
                                result.exit_code,
                                result.execution_time_ms.unwrap_or(0)
                            );
                            io::stderr().flush().unwrap();

                            found_final_result = true;
                        }

                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Some(Item::End(_)) => {
                        eprintln!("[{elapsed}ms] END marker received");
                        io::stderr().flush().unwrap();
                        found_end = true;
                        break;
                    }
                    None => {
                        eprintln!("[{elapsed}ms] Empty item received");
                        io::stderr().flush().unwrap();
                    }
                }
            }

            eprintln!(
                "Stream processing completed: lines={line_count}, final_result={found_final_result}, end_marker={found_end}"
            );
            io::stderr().flush().unwrap();

            // Send results back to the test
            let _ = tx.send((found_final_result, found_end, line_count)).await;
        });

        // Wait for the stream processing to complete and get results
        if let Some((found_final_result, found_end, line_count)) = rx.recv().await {
            // Verify results
            assert!(
                line_count >= 3,
                "Stream should capture at least 3 lines of output"
            );
            assert!(
                found_final_result,
                "Stream should produce a final CommandResult"
            );
            assert!(found_end, "Stream should produce an end marker");
            eprintln!("=== Stream test with multiple lines completed successfully ===\n");
            io::stderr().flush().unwrap();
        } else {
            panic!("Failed to receive results from stream processing task");
        }
    }

    #[tokio::test]
    async fn test_graceful_cancel() {
        eprintln!("=== Starting graceful cancellation test ===");

        // Test approach: Execute a long-running command and then test cancel on a separate runner
        // This verifies the cancel() method works when there's a child process

        let mut runner1 = CommandRunnerImpl::new();
        let mut runner2 = CommandRunnerImpl::new();

        // Use a long-running command
        let arg = CommandArgs {
            command: "/bin/sleep".to_string(),
            args: vec!["2".to_string()], // Sleep for 2 seconds
            with_memory_monitoring: false,
        };

        let arg_bytes = ProstMessageCodec::serialize_message(&arg).unwrap();
        let metadata = HashMap::new();

        // Start the command and immediately prepare for cancellation test
        let start_time = std::time::Instant::now();

        // First, start a command that we will cancel
        let execution_task =
            tokio::spawn(async move { runner1.run(&arg_bytes, metadata, None).await });

        // Wait a moment, then test cancel on the second runner (which has no active process)
        sleep(Duration::from_millis(100)).await;
        // Use new cancellation API
        use crate::runner::cancellation::CancelMonitoring;
        runner2.request_cancellation().await.unwrap(); // This should not panic
        eprintln!("Cancel on runner2 (no active process) completed successfully");

        // Wait for the original command to complete or timeout
        let result = tokio::time::timeout(Duration::from_secs(5), execution_task).await;

        let elapsed = start_time.elapsed();
        eprintln!("Total execution time: {elapsed:?}");

        match result {
            Ok(task_result) => {
                let (execution_result, _metadata) = task_result.unwrap();
                match execution_result {
                    Ok(bytes) => {
                        let command_result =
                            ProstMessageCodec::deserialize_message::<CommandResult>(&bytes)
                                .unwrap();
                        eprintln!("Exit code: {:?}", command_result.exit_code);
                        // Sleep should complete normally since we didn't cancel runner1
                        assert_eq!(
                            command_result.exit_code,
                            Some(0),
                            "Sleep should complete normally"
                        );
                    }
                    Err(e) => {
                        eprintln!("Command failed unexpectedly: {e}");
                        panic!("Sleep command should complete successfully");
                    }
                }
            }
            Err(_) => {
                panic!("Command should complete within 5 seconds");
            }
        }

        eprintln!("=== Graceful cancellation test completed ===");
    }

    // Note: Complex cancellation tests moved to app-wrapper integration tests
    // runner crate level tests focus on basic functionality only
}

// CommandRunnerImpl uses the blanket implementation of CancellableRunner
// The streaming support is provided through the UseCancelMonitoringHelper trait
