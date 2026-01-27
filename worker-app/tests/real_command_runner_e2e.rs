//! Real E2E tests for COMMAND runner with actual shell execution
//!
//! These tests execute actual shell commands and verify real output
//! using the JobRunner infrastructure with real test app modules.

use anyhow::Result;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_runner::jobworkerp::runner::{CommandArgs, CommandResult};
use proto::jobworkerp::data::{
    Job, JobData, JobId, ResponseType, ResultStatus, RunnerData, RunnerType, WorkerData, WorkerId,
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::OnceCell;

// Import JobRunner infrastructure
use app::app::WorkerConfig;
use app_wrapper::runner::{RunnerFactory, UseRunnerFactory};
use command_utils::trace::Tracing;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use worker_app::worker::runner::JobRunner;
use worker_app::worker::runner::map::{RunnerFactoryWithPoolMap, UseRunnerPoolMap};
use worker_app::worker::runner::result::RunnerResultHandler;

/// Real E2E Test JobRunner using actual test infrastructure
struct RealE2EJobRunner {
    runner_factory: Arc<RunnerFactory>,
    runner_pool: RunnerFactoryWithPoolMap,
    id_generator: IdGeneratorWrapper,
}

impl RealE2EJobRunner {
    async fn new() -> Self {
        let app_module = Arc::new(app::module::test::create_hybrid_test_app().await.unwrap());
        let app_wrapper_module = Arc::new(
            app_wrapper::modules::test::create_test_app_wrapper_module(app_module.clone()),
        );
        let mcp_clients =
            Arc::new(jobworkerp_runner::runner::mcp::proxy::McpServerFactory::default());

        RealE2EJobRunner {
            runner_factory: Arc::new(RunnerFactory::new(
                app_module.clone(),
                app_wrapper_module.clone(),
                mcp_clients.clone(),
            )),
            runner_pool: RunnerFactoryWithPoolMap::new(
                Arc::new(RunnerFactory::new(
                    app_module,
                    app_wrapper_module,
                    mcp_clients,
                )),
                Arc::new(WorkerConfig::default()),
            ),
            id_generator: IdGeneratorWrapper::new_mock(),
        }
    }
}

impl jobworkerp_base::codec::UseProstCodec for RealE2EJobRunner {}
impl UseJobqueueAndCodec for RealE2EJobRunner {}
impl UseRunnerFactory for RealE2EJobRunner {
    fn runner_factory(&self) -> &RunnerFactory {
        &self.runner_factory
    }
}
impl RunnerResultHandler for RealE2EJobRunner {}
impl UseRunnerPoolMap for RealE2EJobRunner {
    fn runner_pool_map(&self) -> &RunnerFactoryWithPoolMap {
        &self.runner_pool
    }
}
impl JobRunner for RealE2EJobRunner {}
impl Tracing for RealE2EJobRunner {}
impl UseIdGenerator for RealE2EJobRunner {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}

/// Get or create the test JobRunner with real infrastructure
async fn get_real_job_runner() -> &'static RealE2EJobRunner {
    static JOB_RUNNER: OnceCell<Box<RealE2EJobRunner>> = OnceCell::const_new();
    JOB_RUNNER
        .get_or_init(|| async { Box::new(RealE2EJobRunner::new().await) })
        .await
}

/// Create a test job with COMMAND runner
fn create_command_job(command: &str, args: Vec<String>, timeout_ms: u64) -> Job {
    let command_args = CommandArgs {
        command: command.to_string(),
        args,
        with_memory_monitoring: true,
        treat_nonzero_as_error: false,
        success_exit_codes: vec![],
        working_dir: "".to_string(),
    };
    let args_bytes = ProstMessageCodec::serialize_message(&command_args).unwrap();

    Job {
        id: Some(JobId { value: 1 }),
        data: Some(JobData {
            worker_id: Some(WorkerId { value: 1 }),
            args: args_bytes,
            uniq_key: Some("real_command_test".to_string()),
            retried: 0,
            priority: 0,
            timeout: timeout_ms,
            enqueue_time: command_utils::util::datetime::now_millis(),
            run_after_time: command_utils::util::datetime::now_millis(),
            grabbed_until_time: None,
            streaming_type: 0,
            using: None,
        }),
        ..Default::default()
    }
}

/// Create test worker and runner data
fn create_test_data() -> (WorkerData, RunnerData) {
    let worker_data = WorkerData {
        name: "real_command_test_worker".to_string(),
        runner_settings: vec![],
        retry_policy: None,
        channel: Some("test".to_string()),
        response_type: ResponseType::NoResult as i32,
        store_success: false,
        store_failure: false,
        use_static: false, // Use real execution, not static pool
        ..Default::default()
    };

    let runner_data = RunnerData {
        name: RunnerType::Command.as_str_name().to_string(),
        ..Default::default()
    };

    (worker_data, runner_data)
}

/// Test basic shell command execution with real output verification
#[tokio::test]
async fn test_real_command_basic_execution() -> Result<()> {
    let job_runner = get_real_job_runner().await;
    let job = create_command_job("echo", vec!["Hello Real E2E Test".to_string()], 10000);
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let start_time = Instant::now();
    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;
    let elapsed_time = start_time.elapsed();

    assert!(result.data.is_some());
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    assert!(data.output.is_some());
    let command_result: CommandResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    assert_eq!(command_result.exit_code.unwrap_or(-1), 0);
    assert!(
        command_result
            .stdout
            .as_ref()
            .unwrap()
            .contains("Hello Real E2E Test")
    );

    assert!(command_result.max_memory_usage_kb.is_some());

    // Should complete quickly
    assert!(elapsed_time < Duration::from_secs(5));

    println!("✓ Real COMMAND basic execution test passed");
    Ok(())
}

/// Test command with actual file system operations
#[tokio::test]
async fn test_real_command_filesystem_operations() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    let temp_file = format!("/tmp/jobworkerp_test_{}", std::process::id());
    let test_content = "Real E2E Test Content";

    let job = create_command_job(
        "sh",
        vec![
            "-c".to_string(),
            format!(
                "echo '{}' > {} && cat {}",
                test_content, temp_file, temp_file
            ),
        ],
        10000,
    );
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    let command_result: CommandResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    assert_eq!(command_result.exit_code.unwrap_or(-1), 0);
    assert!(
        command_result
            .stdout
            .as_ref()
            .unwrap()
            .contains(test_content)
    );

    // Cleanup temp file
    let cleanup_job = create_command_job("rm", vec!["-f".to_string(), temp_file], 5000);
    let _ = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, cleanup_job)
        .await;

    println!("✓ Real COMMAND filesystem operations test passed");
    Ok(())
}

/// Test command with actual environment variables
#[tokio::test]
async fn test_real_command_environment_variables() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Set environment variable and verify it
    let job = create_command_job(
        "sh",
        vec![
            "-c".to_string(),
            "export TEST_VAR='Real E2E Environment' && echo $TEST_VAR".to_string(),
        ],
        10000,
    );
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    let command_result: CommandResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    assert_eq!(command_result.exit_code.unwrap_or(-1), 0);
    assert!(
        command_result
            .stdout
            .as_ref()
            .unwrap()
            .contains("Real E2E Environment")
    );

    println!("✓ Real COMMAND environment variables test passed");
    Ok(())
}

/// Test command with actual process monitoring and timeout
#[tokio::test]
async fn test_real_command_timeout_with_real_process() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Run a sleep command that will be timed out
    let job = create_command_job("sleep", vec!["5".to_string()], 2000); // 2 second timeout
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let start_time = Instant::now();
    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;
    let elapsed_time = start_time.elapsed();

    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::MaxRetry as i32);

    assert!(String::from_utf8_lossy(&data.output.unwrap().items).contains("timeout"));

    // Should timeout around 2 seconds, not wait full 5 seconds
    assert!(elapsed_time >= Duration::from_secs(2));
    assert!(elapsed_time < Duration::from_secs(4));

    println!("✓ Real COMMAND timeout with real process test passed");
    Ok(())
}

/// Test command error handling with actual failing command
#[tokio::test]
async fn test_real_command_error_handling() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Run a command that will fail
    let job = create_command_job(
        "ls",
        vec!["/nonexistent/directory/that/does/not/exist".to_string()],
        10000,
    );
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32); // Command runner completed successfully

    let command_result: CommandResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    tracing::debug!("Command result: {:?}", command_result);
    assert_ne!(command_result.exit_code.unwrap_or(0), 0); // Non-zero exit code
    // XXX change message depends on the system language and environment
    // assert!(command_result
    //     .stderr
    //     .as_ref()
    //     .unwrap()
    //     .contains("No such file or directory"));

    println!("✓ Real COMMAND error handling test passed");
    Ok(())
}

/// Test command with actual complex shell operations
#[tokio::test]
async fn test_real_command_complex_shell_operations() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Complex shell script with pipes, redirections, and multiple commands
    let script = r#"
        echo "Starting complex operations" &&
        echo -e "Line 1\nLine 2\nLine 3" | wc -l &&
        date +%Y-%m-%d &&
        echo $((2 + 3 * 4)) &&
        echo "Complex operations completed"
    "#;

    let job = create_command_job("sh", vec!["-c".to_string(), script.to_string()], 15000);
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    let command_result: CommandResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    assert_eq!(command_result.exit_code.unwrap_or(-1), 0);
    let stdout = command_result.stdout.as_ref().unwrap();

    tracing::debug!("Command output: {}", stdout);
    assert!(stdout.contains("Starting complex operations"));
    assert!(stdout.contains("3")); // wc -l output
    assert!(stdout.contains("14")); // 2 + 3 * 4 = 14
    assert!(stdout.contains("Complex operations completed"));

    assert!(stdout.chars().filter(|&c| c == '-').count() >= 2);

    println!("✓ Real COMMAND complex shell operations test passed");
    Ok(())
}

/// Integration test demonstrating complete COMMAND runner real workflow
#[tokio::test]
async fn test_real_command_runner_complete_workflow() -> Result<()> {
    println!("=== Real COMMAND Runner Complete Workflow Test ===");

    let job_runner = get_real_job_runner().await;
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    // Test 1: Basic command execution
    let basic_job = create_command_job("echo", vec!["Workflow Test 1".to_string()], 10000);
    let (result1, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, basic_job)
        .await;
    assert_eq!(result1.data.unwrap().status, ResultStatus::Success as i32);
    println!("  ✓ Basic command execution passed");

    // Test 2: File operations
    let temp_file = format!("/tmp/workflow_test_{}", std::process::id());
    let file_job = create_command_job(
        "sh",
        vec![
            "-c".to_string(),
            format!(
                "echo 'Workflow content' > {} && ls -la {}",
                temp_file, temp_file
            ),
        ],
        10000,
    );
    let (result2, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, file_job)
        .await;
    assert_eq!(result2.data.unwrap().status, ResultStatus::Success as i32);
    println!("  ✓ File operations passed");

    // Test 3: Process monitoring
    let monitor_job = create_command_job("sleep", vec!["1".to_string()], 5000);
    let start_time = Instant::now();
    let (result3, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, monitor_job)
        .await;
    let elapsed = start_time.elapsed();
    assert_eq!(result3.data.unwrap().status, ResultStatus::Success as i32);
    assert!(elapsed >= Duration::from_secs(1));
    assert!(elapsed < Duration::from_secs(2));
    println!("  ✓ Process monitoring passed");

    // Test 4: Error handling
    let error_job = create_command_job("false", vec![], 10000);
    let (result4, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, error_job)
        .await;
    let command_result: CommandResult =
        ProstMessageCodec::deserialize_message(&result4.data.unwrap().output.unwrap().items)?;
    assert_ne!(command_result.exit_code.unwrap_or(0), 0);
    println!("  ✓ Error handling passed");

    // Cleanup
    let cleanup_job = create_command_job("rm", vec!["-f".to_string(), temp_file], 5000);
    let _ = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, cleanup_job)
        .await;

    println!("=== Real COMMAND Runner Complete Workflow Test PASSED ===");
    Ok(())
}

/// Test command execution with specific working directory
#[tokio::test]
async fn test_real_command_working_directory() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Create a temporary directory using std::fs (since we are in test environment)
    // We can use a unique path in /tmp
    let temp_dir_path = format!("/tmp/jobworkerp_test_cwd_{}", std::process::id());
    std::fs::create_dir_all(&temp_dir_path).unwrap();
    // Create a file in it to verify
    let temp_file_path = format!("{}/test_file.txt", temp_dir_path);
    std::fs::write(&temp_file_path, "test content").unwrap();

    let job = Job {
        id: Some(JobId { value: 1 }),
        data: Some(JobData {
            worker_id: Some(WorkerId { value: 1 }),
            args: ProstMessageCodec::serialize_message(&CommandArgs {
                command: "ls".to_string(), // List files in the working directory
                args: vec![],
                with_memory_monitoring: true,
                treat_nonzero_as_error: false,
                success_exit_codes: vec![],
                working_dir: temp_dir_path.clone(),
            })
            .unwrap(),
            uniq_key: Some("real_command_cwd_test".to_string()),
            retried: 0,
            priority: 0,
            timeout: 10000,
            enqueue_time: command_utils::util::datetime::now_millis(),
            run_after_time: command_utils::util::datetime::now_millis(),
            grabbed_until_time: None,
            streaming_type: 0,
            using: None,
        }),
        ..Default::default()
    };

    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    let command_result: CommandResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    assert_eq!(command_result.exit_code.unwrap_or(-1), 0);
    assert!(
        command_result
            .stdout
            .as_ref()
            .unwrap()
            .contains("test_file.txt")
    );

    // Cleanup
    std::fs::remove_dir_all(&temp_dir_path).unwrap();

    println!("✓ Real COMMAND working directory test passed");
    Ok(())
}
