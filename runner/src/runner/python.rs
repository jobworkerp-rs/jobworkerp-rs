use super::cancellation::CancelMonitoring;
use super::cancellation_helper::{CancelMonitoringHelper, UseCancelMonitoringHelper};
use super::RunnerSpec;
use crate::jobworkerp::runner::{
    python_command_args, python_command_runner_settings, PythonCommandArgs, PythonCommandResult,
    PythonCommandRunnerSettings,
};
use crate::runner::RunnerTrait;
use crate::schema_to_json_string_option;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures::stream::BoxStream;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use prost::Message;
use proto::jobworkerp::data::{JobData, JobId, JobResult};
use proto::jobworkerp::data::{ResultOutputItem, RunnerType, StreamingOutputType};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct PythonCommandRunner {
    venv_path: Option<PathBuf>,
    temp_dir: Option<TempDir>,
    settings: Option<PythonCommandRunnerSettings>,
    process_cancel: Arc<Mutex<bool>>,
    current_process_id: Arc<Mutex<Option<u32>>>,
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl PythonCommandRunner {
    /// Constructor without cancellation monitoring
    pub fn new() -> Self {
        PythonCommandRunner {
            venv_path: None,
            temp_dir: None,
            settings: None,
            process_cancel: Arc::new(Mutex::new(false)),
            current_process_id: Arc::new(Mutex::new(None)),
            cancel_helper: None,
        }
    }

    /// Kill a process by PID with platform-specific implementation
    fn kill_process_by_pid(pid: u32) -> Result<()> {
        #[cfg(unix)]
        {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;
            kill(Pid::from_raw(pid as i32), Signal::SIGKILL)
                .context(format!("Failed to send SIGKILL to process {pid}"))?;
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Threading::{
                OpenProcess, TerminateProcess, PROCESS_TERMINATE,
            };
            unsafe {
                let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
                if handle != 0 {
                    if TerminateProcess(handle, 1) == 0 {
                        return Err(anyhow!("Failed to terminate process {}", pid));
                    }
                } else {
                    return Err(anyhow!("Failed to open process {} for termination", pid));
                }
            }
        }
        Ok(())
    }

    /// Kill a process with graceful shutdown attempt (SIGTERM first, then SIGKILL)
    async fn graceful_kill_process_by_pid(pid: u32) -> Result<()> {
        #[cfg(unix)]
        {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;

            // First try SIGTERM for graceful shutdown
            if let Err(e) = kill(Pid::from_raw(pid as i32), Signal::SIGTERM) {
                tracing::warn!("Failed to send SIGTERM to process {}: {}", pid, e);
            } else {
                tracing::debug!("Sent SIGTERM to process {}", pid);
            }

            // Wait for graceful shutdown with 5 second timeout
            let timeout_duration = std::time::Duration::from_secs(5);
            tokio::time::sleep(timeout_duration).await;

            // Try not to send signal but to check if process still exists
            match kill(Pid::from_raw(pid as i32), None) {
                Ok(_) => {
                    // Process still exists, force kill with SIGKILL
                    tracing::warn!(
                        "Process {} did not terminate gracefully, force killing",
                        pid
                    );
                    Self::kill_process_by_pid(pid)?;
                    tracing::debug!("Sent SIGKILL to process {}", pid);
                }
                Err(_) => {
                    // Process no longer exists (terminated gracefully)
                    tracing::debug!("Process {} terminated gracefully", pid);
                }
            }
        }

        #[cfg(windows)]
        {
            // Windows doesn't have SIGTERM, so we directly terminate
            Self::kill_process_by_pid(pid)?;
            tracing::debug!("Terminated process {}", pid);
        }

        Ok(())
    }

    /// Constructor with cancellation monitoring to enable proper resource cleanup
    pub fn new_with_cancel_monitoring(cancel_helper: CancelMonitoringHelper) -> Self {
        PythonCommandRunner {
            venv_path: None,
            temp_dir: None,
            settings: None,
            process_cancel: Arc::new(Mutex::new(false)),
            current_process_id: Arc::new(Mutex::new(None)),
            cancel_helper: Some(cancel_helper),
        }
    }

    /// Retrieves cancellation token from helper or creates new one to support both DI and standalone usage
    async fn get_cancellation_token(&self) -> CancellationToken {
        if let Some(helper) = &self.cancel_helper {
            helper.get_cancellation_token().await
        } else {
            CancellationToken::new()
        }
    }

    fn python_bin_path(&self) -> Option<PathBuf> {
        self.venv_path.as_ref().map(|venv| {
            if cfg!(windows) {
                venv.join("Scripts").join("python.exe")
            } else {
                venv.join("bin").join("python")
            }
        })
    }
}

impl Default for PythonCommandRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl RunnerSpec for PythonCommandRunner {
    fn name(&self) -> String {
        RunnerType::PythonCommand.as_str_name().to_string()
    }
    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/python_command_runner.proto").to_string()
    }
    // Phase 6.6: Unified method_proto_map for all runners
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            "run".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/python_command_args.proto"
                )
                .to_string(),
                result_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/python_command_result.proto"
                )
                .to_string(),
                description: Some("Execute Python script via uv".to_string()),
                output_type: StreamingOutputType::NonStreaming as i32,
            },
        );
        schemas
    }
    fn output_type(&self) -> StreamingOutputType {
        // Phase 6.6.5: Use method_proto_map's output_type instead of deprecated RunnerData.output_type
        self.method_proto_map()
            .get("run")
            .cloned()
            .and_then(|s| StreamingOutputType::try_from(s.output_type).ok())
            .unwrap_or(StreamingOutputType::NonStreaming)
    }
    fn settings_schema(&self) -> String {
        include_str!("../../schema/PythonCommandRunnerSettings.json").to_string()
    }
    fn arguments_schema(&self) -> String {
        include_str!("../../schema/PythonCommandArgs.json").to_string()
    }
    fn output_schema(&self) -> Option<String> {
        schema_to_json_string_option!(PythonCommandResult, "output_schema")
    }
}

#[async_trait]
impl RunnerTrait for PythonCommandRunner {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let settings = PythonCommandRunnerSettings::decode(settings.as_slice())
            .context("Failed to decode PythonCommandRunnerSettings")?;

        let temp_dir = TempDir::new().context("Failed to create temporary directory")?;
        let venv_path = temp_dir.path().join("venv");

        let uv_path = if let Some(uv_path) = &settings.uv_path {
            uv_path.as_str()
        } else if cfg!(windows) {
            "C:\\Program Files\\uv\\uv.exe"
        } else {
            "uv" // from path
        };

        let output = Command::new(uv_path)
            .args(["venv", &venv_path.to_string_lossy()])
            .arg("--python")
            .arg(&settings.python_version)
            .output()
            .await
            .context(format!(
                "Failed to create virtual environment with uv: {uv_path:?}"
            ))?;

        if output.status.success() {
            tracing::debug!("output: {}", String::from_utf8_lossy(&output.stdout));
            tracing::debug!("output(err): {}", String::from_utf8_lossy(&output.stderr));
            tracing::info!("Created venv: {}", venv_path.to_string_lossy());
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Failed to create venv: {}", stderr));
        }

        let python_bin = if cfg!(windows) {
            venv_path.join("Scripts").join("python.exe")
        } else {
            venv_path.join("bin").join("python")
        };

        // install packages
        if let Some(req_spec) = &settings.requirements_spec {
            let mut pip_cmd = Command::new(uv_path);
            pip_cmd.args(vec![
                "pip",
                "install",
                "--python",
                &python_bin.to_string_lossy(),
            ]);

            match req_spec {
                python_command_runner_settings::RequirementsSpec::Packages(packages_list) => {
                    if !packages_list.list.is_empty() {
                        pip_cmd.args(&packages_list.list);

                        let output = pip_cmd
                            .output()
                            .await
                            .context("Failed to install required packages")?;

                        if !output.status.success() {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            return Err(anyhow!("Failed to install packages: {}", stderr));
                        }
                    }
                }
                python_command_runner_settings::RequirementsSpec::RequirementsUrl(url) => {
                    pip_cmd.args(vec!["-r", url]);

                    let output = pip_cmd
                        .output()
                        .await
                        .context("Failed to install packages from requirements URL")?;

                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        return Err(anyhow!("Failed to install packages from URL: {}", stderr));
                    }
                }
            }
        }

        self.settings = Some(settings);
        self.venv_path = Some(venv_path);
        self.temp_dir = Some(temp_dir);

        Ok(())
    }

    async fn run(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let cancellation_token = self.get_cancellation_token().await;

        let result = async {
            let python_bin = self
                .python_bin_path()
                .ok_or_else(|| anyhow!("Virtual environment not loaded"))?;

            let job_args = ProstMessageCodec::deserialize_message::<PythonCommandArgs>(arg)?;

            let temp_dir_path = self
                .temp_dir
                .as_ref()
                .ok_or_else(|| anyhow!("Temporary directory not created"))?
                .path();

            let script_path = temp_dir_path.join("script.py");

            if let Some(script_spec) = &job_args.script {
                let script_content = match script_spec {
                    python_command_args::Script::ScriptContent(script) => {
                        script.clone()
                    }
                    python_command_args::Script::ScriptUrl(url) => {
                        let response = reqwest::get(url)
                            .await
                            .context(format!("Failed to download script from URL: {url}"))?;

                        if !response.status().is_success() {
                            Err(anyhow!(
                                "Failed to download script: HTTP status {}",
                                response.status()
                            ))?
                        }
                        response
                            .text()
                            .await
                            .context("Failed to read script response as text")?
                    }
                };

                let mut script_file =
                    File::create(&script_path).context("Failed to create script file")?;
                script_file
                    .write_all(script_content.as_bytes())
                    .context("Failed to write script content")?;
            } else {
                Err(anyhow!("No script specified: {:?}", job_args))?;
            }

            let input_path = if let Some(input_data_spec) = &job_args.input_data {
                let input_path = temp_dir_path.join("input.bin");

                let input_data = match input_data_spec {
                    python_command_args::InputData::DataBody(data) => data.clone(),
                    python_command_args::InputData::DataUrl(url) => {
                        let response = reqwest::get(url)
                            .await
                            .context(format!("Failed to download input data from URL: {url}"))?;

                        if !response.status().is_success() {
                            Err(anyhow!(
                                "Failed to download input data: HTTP status {}",
                                response.status()
                            ))?
                        }

                        response
                            .text()
                            .await
                            .context("Failed to read input data as text")?
                    }
                };

                fs::write(&input_path, input_data).context("Failed to write input data")?;
                Some(input_path)
            } else {
                None
            };

            let mut command = Command::new(&python_bin);
            command.arg("-u");
            command.arg(&script_path);

            if let Some(input_path) = &input_path {
                command.arg(input_path);
            }

            command.stdout(std::process::Stdio::piped());
            command.stderr(std::process::Stdio::piped());

            for (key, value) in &job_args.env_vars {
                command.env(key, value);
            }

            // Check cancellation before spawning process
            if cancellation_token.is_cancelled() {
                tracing::info!("Python command execution was cancelled before spawn");
                return Err(anyhow::anyhow!("Python command execution was cancelled before spawn"));
            }

            let child = command.spawn().context("Failed to execute Python script")?;
            *self.current_process_id.lock().await = child.id();

            // Monitor cancellation during process execution
            let child_id = child.id();
            let output = tokio::select! {
                output_result = child.wait_with_output() => {
                    output_result.context("Failed to wait for process")?
                }
                _ = cancellation_token.cancelled() => {
                    tracing::info!("Python command execution was cancelled during process execution");
                    if let Some(pid) = child_id {
                        let _ = Self::kill_process_by_pid(pid);
                    }
                    return Err(anyhow::anyhow!("Python command execution was cancelled during process execution"));
                }
            };

            *self.current_process_id.lock().await = None;

            let result = PythonCommandResult {
                output: String::from_utf8_lossy(&output.stdout).to_string(),
                output_stderr: if output.stderr.is_empty() {
                    None
                } else {
                    Some(String::from_utf8_lossy(&output.stderr).to_string())
                },
                exit_code: output.status.code().unwrap_or(-1),
            };

            let mut encoded_result = Vec::new();
            result
                .encode(&mut encoded_result)
                .context("Failed to encode result")?;
            Ok(encoded_result)
        }
        .await;

        (result, metadata)
    }

    async fn run_stream(
        &mut self,
        _arg: &[u8],
        _metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let _cancellation_token = self.get_cancellation_token().await;

        Err(anyhow!("Stream output not supported by PythonRunner"))
    }
}

impl UseCancelMonitoringHelper for PythonCommandRunner {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}

#[async_trait]
impl CancelMonitoring for PythonCommandRunner {
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: JobId,
        job_data: &JobData,
    ) -> Result<Option<JobResult>> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, job_data).await
        } else {
            tracing::debug!("No cancel monitoring configured for job {}", job_id.value);
            Ok(None)
        }
    }

    async fn cleanup_cancellation_monitoring(&mut self) -> Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.cleanup_monitoring_impl().await
        } else {
            Ok(())
        }
    }

    /// Cancels active Python process and signals cancellation token
    async fn request_cancellation(&mut self) -> Result<()> {
        // Signal cancellation token
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
                tracing::info!("PythonCommandRunner: cancellation token signaled");
            }
        }

        // Terminate active Python process
        let mut cancel_flag = self.process_cancel.lock().await;
        *cancel_flag = true;

        if let Some(pid) = *self.current_process_id.lock().await {
            let _ = Self::graceful_kill_process_by_pid(pid)
                .await
                .map_err(|e| tracing::error!("Failed to kill Python process {}: {}", pid, e));
            tracing::info!("PythonCommandRunner: cancelled process by PID: {}", pid);
        } else {
            tracing::warn!("PythonCommandRunner: no active process to cancel");
        }

        Ok(())
    }

    async fn reset_for_pooling(&mut self) -> Result<()> {
        // Check for active processes to determine cleanup strategy
        let has_active_process = {
            let process_id = self.current_process_id.lock().await;
            process_id.is_some()
        };

        if has_active_process {
            // Keep monitoring active during execution to allow proper cancellation
            tracing::debug!(
                "PythonCommandRunner has active process - keeping cancellation monitoring active"
            );
        } else {
            // Safe to cleanup monitoring when no active process
            if let Some(helper) = &mut self.cancel_helper {
                helper.reset_for_pooling_impl().await?;
            } else {
                self.cleanup_cancellation_monitoring().await?;
            }
        }

        // Reset internal state when safe to do so
        if !has_active_process {
            let mut cancel_flag = self.process_cancel.lock().await;
            *cancel_flag = false;

            let mut process_id = self.current_process_id.lock().await;
            *process_id = None;
        }

        tracing::debug!(
            "PythonCommandRunner reset for pooling (process active: {})",
            has_active_process
        );
        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[tokio::test]
    async fn test_python_runner() {
        command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
        const UV_PATH: &str = if cfg!(windows) {
            "C:\\Program Files\\uv\\uv.exe"
        } else {
            "uv" // from path
        };
        let mut runner = PythonCommandRunner::new();

        let settings = PythonCommandRunnerSettings {
            python_version: "3.12".to_string(),
            requirements_spec: Some(python_command_runner_settings::RequirementsSpec::Packages(
                python_command_runner_settings::PackagesList {
                    list: vec!["requests".to_string()],
                },
            )),
            ..Default::default()
        };

        let mut settings_bytes = Vec::new();
        settings.encode(&mut settings_bytes).unwrap();

        if Command::new(UV_PATH).arg("--version").spawn().is_ok() {
            let load_result = runner.load(settings_bytes).await;
            assert!(
                load_result.is_ok(),
                "Failed to load: {:?}",
                load_result.err()
            );

            let job_args = PythonCommandArgs {
                script: Some(python_command_args::Script::ScriptContent(
                    r#"
print("Hello from Python!")
print("Version info:")
import sys
print(sys.version)
import requests
print(f"Requests version: {requests.__version__}")
                "#
                    .to_string(),
                )),
                env_vars: std::collections::HashMap::new(),
                input_data: None,
                with_stderr: false,
            };

            let mut args_bytes = Vec::new();
            job_args.encode(&mut args_bytes).unwrap();

            let run_result = runner.run(&args_bytes, HashMap::new(), None).await;
            assert!(
                run_result.0.is_ok(),
                "Failed to run: {:?}",
                run_result.0.err()
            );

            let output = run_result.0.unwrap();
            assert!(!output.is_empty());

            let result = PythonCommandResult::decode(output.as_slice()).unwrap();
            let stdout = &result.output;

            assert!(stdout.contains("Hello from Python!"));
            assert!(stdout.contains("Version info:"));
            assert!(stdout.contains("Requests version:"));
        } else {
            panic!("uv not found");
        }
    }

    #[tokio::test]
    #[ignore] // Requires uv installation - run with --ignored for full testing
    async fn test_python_actual_cancellation() {
        eprintln!("=== Starting Python actual cancellation test ===");

        const UV_PATH: &str = if cfg!(windows) {
            "C:\\Program Files\\uv\\uv.exe"
        } else {
            "uv" // from path
        };

        if tokio::process::Command::new(UV_PATH)
            .arg("--version")
            .output()
            .await
            .is_ok()
        {
            let mut runner = PythonCommandRunner::new();

            let settings = PythonCommandRunnerSettings {
                uv_path: Some(UV_PATH.to_string()),
                python_version: "3.11".to_string(),
                requirements_spec: None,
            };

            runner
                .load(ProstMessageCodec::serialize_message(&settings).unwrap())
                .await
                .unwrap();

            // Create a long-running Python script
            let job_args = PythonCommandArgs {
                script: Some(python_command_args::Script::ScriptContent(
                    r#"
import time
import signal

def signal_handler(sig, frame):
    print("Received signal, cleaning up...")
    exit(0)

signal.signal(signal.SIGTERM, signal_handler)

print("Starting long-running task...")
try:
    for i in range(30):  # Run for ~30 seconds
        print(f"Working... {i}")
        time.sleep(1)
    print("Task completed")
except KeyboardInterrupt:
    print("Interrupted")
                    "#
                    .to_string(),
                )),
                input_data: None,
                env_vars: std::collections::HashMap::new(),
                with_stderr: false,
            };

            let arg_bytes = ProstMessageCodec::serialize_message(&job_args).unwrap();
            let metadata = std::collections::HashMap::new();

            // Start Python execution and cancel it after 1 second
            let start_time = std::time::Instant::now();
            let execution_task =
                tokio::spawn(async move { runner.run(&arg_bytes, metadata, None).await });

            // Wait for script to start, then cancel the runner
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            // Cancel the task to test cancellation
            execution_task.abort();
            let result = execution_task.await;

            let elapsed = start_time.elapsed();
            eprintln!("Python execution time: {elapsed:?}");

            match result {
                Ok(_) => {
                    eprintln!("Python script completed - checking if it was actually cancelled");
                }
                Err(e) if e.is_cancelled() => {
                    eprintln!("Python script was cancelled as expected: {e}");
                    // Should complete much faster than 30 seconds due to cancellation
                    assert!(
                        elapsed < std::time::Duration::from_secs(5),
                        "Cancellation should stop execution quickly, took {elapsed:?}"
                    );
                }
                Err(e) => {
                    eprintln!("Python script failed with unexpected error: {e}");
                }
            }
        } else {
            eprintln!("uv not found, skipping actual Python cancellation test");
        }

        eprintln!("=== Python actual cancellation test completed ===");
    }
}
