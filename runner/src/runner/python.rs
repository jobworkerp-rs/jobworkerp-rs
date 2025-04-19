use crate::jobworkerp::runner::{
    python_command_args, python_command_runner_settings, PythonCommandArgs, PythonCommandResult,
    PythonCommandRunnerSettings,
};
use crate::runner::RunnerTrait;
use crate::schema_to_json_string_option;
use anyhow::{anyhow, Context, Result};
use futures::stream::BoxStream;
use prost::Message;
use proto::jobworkerp::data::{ResultOutputItem, RunnerType, StreamingOutputType};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::process::Command;
use tokio::sync::Mutex;
use tonic::async_trait;

use super::RunnerSpec;

pub struct PythonCommandRunner {
    venv_path: Option<PathBuf>,
    temp_dir: Option<TempDir>,
    settings: Option<PythonCommandRunnerSettings>,
    process_cancel: Arc<Mutex<bool>>,
    current_process_id: Arc<Mutex<Option<u32>>>,
}

impl PythonCommandRunner {
    pub fn new() -> Self {
        PythonCommandRunner {
            venv_path: None,
            temp_dir: None,
            settings: None,
            process_cancel: Arc::new(Mutex::new(false)),
            current_process_id: Arc::new(Mutex::new(None)),
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
    fn job_args_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/python_command_args.proto").to_string()
    }
    fn result_output_proto(&self) -> Option<String> {
        Some(
            include_str!("../../protobuf/jobworkerp/runner/python_command_result.proto")
                .to_string(),
        )
    }
    fn output_type(&self) -> StreamingOutputType {
        StreamingOutputType::NonStreaming
    }
    fn settings_schema(&self) -> String {
        // XXX for right oneof structure in json schema
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
                "Failed to create virtual environment with uv: {:?}",
                uv_path
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

    async fn run(&mut self, arg: &[u8]) -> Result<Vec<Vec<u8>>> {
        let python_bin = self
            .python_bin_path()
            .ok_or_else(|| anyhow!("Virtual environment not loaded"))?;

        let job_args =
            PythonCommandArgs::decode(arg).context("Failed to decode PythonCommandArgs")?;

        let temp_dir_path = self
            .temp_dir
            .as_ref()
            .ok_or_else(|| anyhow!("Temporary directory not created"))?
            .path();

        let script_path = temp_dir_path.join("script.py");

        if let Some(script_spec) = &job_args.script {
            let script_content = match script_spec {
                python_command_args::Script::ScriptContent(script) => {
                    // content
                    script.clone()
                }
                python_command_args::Script::ScriptUrl(url) => {
                    // download from URL
                    let response = reqwest::get(url)
                        .await
                        .context(format!("Failed to download script from URL: {}", url))?;

                    if !response.status().is_success() {
                        return Err(anyhow!(
                            "Failed to download script: HTTP status {}",
                            response.status()
                        ));
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
            return Err(anyhow!("No script specified: {:?}", job_args));
        }

        let input_path = if let Some(input_data_spec) = &job_args.input_data {
            let input_path = temp_dir_path.join("input.bin");

            let input_data = match input_data_spec {
                python_command_args::InputData::DataBody(data) => data.clone(),
                python_command_args::InputData::DataUrl(url) => {
                    let response = reqwest::get(url)
                        .await
                        .context(format!("Failed to download input data from URL: {}", url))?;

                    if !response.status().is_success() {
                        return Err(anyhow!(
                            "Failed to download input data: HTTP status {}",
                            response.status()
                        ));
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

        // execute async
        let mut command = Command::new(&python_bin);
        command.arg("-u");
        command.arg(&script_path);

        // Add input file path as argument if available
        if let Some(input_path) = &input_path {
            command.arg(input_path);
        }

        // capture stdout/stderr
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());

        // env
        for (key, value) in &job_args.env_vars {
            command.env(key, value);
        }

        let child = command.spawn().context("Failed to execute Python script")?;

        *self.current_process_id.lock().await = child.id();

        let output = child
            .wait_with_output()
            .await
            .context("Failed to wait for process")?;

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

        Ok(vec![encoded_result])
    }

    async fn run_stream(&mut self, _arg: &[u8]) -> Result<BoxStream<'static, ResultOutputItem>> {
        Err(anyhow!("Stream output not supported by PythonRunner"))
    }

    async fn cancel(&mut self) {
        let mut cancel_flag = self.process_cancel.lock().await;
        *cancel_flag = true;

        // kill the current process
        if let Some(pid) = *self.current_process_id.lock().await {
            #[cfg(unix)]
            {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;
                let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
            }

            #[cfg(windows)]
            {
                use windows_sys::Win32::System::Threading::{
                    OpenProcess, TerminateProcess, PROCESS_TERMINATE,
                };
                unsafe {
                    let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
                    if handle != 0 {
                        TerminateProcess(handle, 1);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_python_runner() {
        // use tracing::Level;
        // command_utils::util::tracing::tracing_init_test(Level::DEBUG);
        // XXX use a real path or find a better way to get the path
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

            let run_result = runner.run(&args_bytes).await;
            assert!(run_result.is_ok(), "Failed to run: {:?}", run_result.err());

            let output = run_result.unwrap();
            assert!(!output.is_empty());

            let result = PythonCommandResult::decode(output[0].as_slice()).unwrap();
            let stdout = &result.output;

            assert!(stdout.contains("Hello from Python!"));
            assert!(stdout.contains("Version info:"));
            assert!(stdout.contains("Requests version:"));
        } else {
            panic!("uv not found");
        }
    }
}
