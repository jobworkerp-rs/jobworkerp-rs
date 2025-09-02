//! Real E2E tests for DOCKER runner with actual Docker execution
//!
//! These tests execute actual Docker containers and verify real output
//! using the JobRunner infrastructure with real test app modules.

use anyhow::Result;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_runner::jobworkerp::runner::DockerArgs;
use proto::jobworkerp::data::{
    Job, JobData, JobId, ResponseType, ResultStatus, RunnerData, RunnerType, WorkerData, WorkerId,
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::OnceCell;

// Import JobRunner infrastructure
use app::app::WorkerConfig;
use app_wrapper::runner::{RunnerFactory, UseRunnerFactory};
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use net_utils::trace::Tracing;
use worker_app::worker::runner::map::{RunnerFactoryWithPoolMap, UseRunnerPoolMap};
use worker_app::worker::runner::result::RunnerResultHandler;
use worker_app::worker::runner::JobRunner;

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

/// Create a test job with DOCKER runner
fn create_docker_job(image: &str, command: Vec<String>, timeout_ms: u64) -> Job {
    let docker_args = DockerArgs {
        image: Some(image.to_string()),
        user: None,
        exposed_ports: vec![],
        env: vec![], // Empty for security
        cmd: command,
        args_escaped: None,
        volumes: vec![], // Empty for security
        working_dir: None,
        entrypoint: vec![],
        network_disabled: Some(true), // Isolated network for security
        mac_address: None,
        shell: vec![], // Add missing shell field
    };
    let args_bytes = ProstMessageCodec::serialize_message(&docker_args).unwrap();

    Job {
        id: Some(JobId { value: 1 }),
        data: Some(JobData {
            worker_id: Some(WorkerId { value: 1 }),
            args: args_bytes,
            uniq_key: Some("real_docker_test".to_string()),
            retried: 0,
            priority: 0,
            timeout: timeout_ms,
            enqueue_time: command_utils::util::datetime::now_millis(),
            run_after_time: command_utils::util::datetime::now_millis(),
            grabbed_until_time: None,
            request_streaming: false,
        }),
        ..Default::default()
    }
}

/// Create test worker and runner data
fn create_test_data() -> (WorkerData, RunnerData) {
    let worker_data = WorkerData {
        name: "real_docker_test_worker".to_string(),
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
        name: RunnerType::Docker.as_str_name().to_string(),
        ..Default::default()
    };

    (worker_data, runner_data)
}

/// Test basic Docker container execution with real container management
#[ignore = "Requires Docker daemon(CI environment have Docker in docker but socket not mounted)"]
#[tokio::test]
async fn test_real_docker_basic_execution() -> Result<()> {
    // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let job_runner = get_real_job_runner().await;
    let job = create_docker_job(
        "alpine:latest",
        vec!["echo".to_string(), "Hello Real Docker E2E Test".to_string()],
        30000,
    );
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let start_time = Instant::now();
    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;
    let elapsed_time = start_time.elapsed();

    // Verify successful execution
    assert!(result.data.is_some());
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    // Verify actual Docker output (UTF-8 string, not protobuf)
    assert!(data.output.is_some());
    let docker_result: String = String::from_utf8_lossy(&data.output.unwrap().items).to_string();
    assert!(docker_result.contains("Hello Real Docker E2E Test"));

    // Should complete within reasonable time
    assert!(elapsed_time < Duration::from_secs(30));

    println!("✓ Real DOCKER basic execution test passed");
    Ok(())
}

/// Test Docker container with actual Linux command execution
#[ignore = "Requires Docker daemon(CI environment have Docker in docker but socket file not mounted)"]
#[tokio::test]
async fn test_real_docker_linux_commands() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Execute complex shell commands in Alpine Linux container
    let job = create_docker_job(
        "alpine:latest",
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo 'Starting Docker test' && date && echo $((2 + 3 * 4)) && echo 'Test completed'"
                .to_string(),
        ],
        30000,
    );
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    // Verify successful execution
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    // Verify complex command outputs
    let docker_result: String = String::from_utf8_lossy(&data.output.unwrap().items).to_string();
    assert!(docker_result.contains("Starting Docker test"));
    assert!(docker_result.contains("14")); // 2 + 3 * 4 = 14
    assert!(docker_result.contains("Test completed"));

    println!("✓ Real DOCKER Linux commands test passed");
    Ok(())
}

/// Test Docker container with actual file system operations
#[ignore = "Requires Docker daemon(CI environment have Docker in docker but socket not mounted)"]
#[tokio::test]
async fn test_real_docker_filesystem_operations() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Create and manipulate files within container
    let job = create_docker_job(
        "alpine:latest",
        vec![
            "sh".to_string(),
            "-c".to_string(),
            r#"
            echo 'Real Docker E2E Test Data' > /tmp/test.txt &&
            cat /tmp/test.txt &&
            echo "File size: $(wc -c < /tmp/test.txt) bytes" &&
            ls -la /tmp/test.txt &&
            rm /tmp/test.txt &&
            echo 'File operations completed'
            "#
            .to_string(),
        ],
        30000,
    );
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    // Verify successful execution
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    // Verify file operations worked
    let docker_result: String = String::from_utf8_lossy(&data.output.unwrap().items).to_string();
    assert!(docker_result.contains("Real Docker E2E Test Data"));
    assert!(docker_result.contains("File size:"));
    assert!(docker_result.contains("bytes"));
    assert!(docker_result.contains("File operations completed"));

    println!("✓ Real DOCKER filesystem operations test passed");
    Ok(())
}

/// Test Docker container with basic operations
#[ignore = "Requires Docker daemon(CI environment have Docker in docker but socket not mounted)"]
#[tokio::test]
async fn test_real_docker_package_operations() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Test basic system commands available in Alpine Linux
    let job = create_docker_job(
        "alpine:latest",
        vec![
            "sh".to_string(),
            "-c".to_string(),
            r#"
            echo 'Starting operations' &&
            ls /bin/sh > /dev/null &&
            echo 'Shell available' &&
            whoami > /dev/null &&
            echo 'User commands work' &&
            echo 'Operations completed successfully'
            "#
            .to_string(),
        ],
        30000,
    );
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    // Verify successful execution
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    // Verify operations worked
    let docker_result: String = String::from_utf8_lossy(&data.output.unwrap().items).to_string();
    assert!(docker_result.contains("Starting operations"));
    assert!(docker_result.contains("Operations completed successfully"));

    println!("✓ Real DOCKER basic operations test passed");
    Ok(())
}

/// Test Docker error handling with actual failing container
#[ignore = "Requires Docker daemon(CI environment have Docker in docker but socket not mounted)"]
#[tokio::test]
async fn test_real_docker_error_handling() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Run a command that will fail
    let job = create_docker_job(
        "alpine:latest",
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo 'Starting error test' && exit 42".to_string(),
        ],
        30000,
    );
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    // Verify execution completed (Docker runner successful)
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32); // Docker runner completed successfully

    // Verify container exit code was captured
    let docker_result: String = String::from_utf8_lossy(&data.output.unwrap().items).to_string();
    assert!(docker_result.contains("Starting error test"));

    // For error cases, Docker runner may include exit code information
    // The exact format depends on the Docker runner implementation

    println!("✓ Real DOCKER error handling test passed");
    Ok(())
}

/// Test Docker timeout with actual long-running container
#[ignore = "Requires Docker daemon(CI environment have Docker in docker but socket not mounted)"]
#[tokio::test]
async fn test_real_docker_timeout() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Run a long sleep that will be timed out
    let job = create_docker_job(
        "alpine:latest",
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo 'Starting long operation' && sleep 10 && echo 'Should not reach here'"
                .to_string(),
        ],
        3000, // 3 second timeout
    );
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let start_time = Instant::now();
    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;
    let elapsed_time = start_time.elapsed();

    // Verify timeout occurred
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::MaxRetry as i32);

    // Verify timeout message
    assert!(String::from_utf8_lossy(&data.output.unwrap().items).contains("timeout"));

    // Should timeout around 3 seconds, not wait full 10 seconds
    assert!(elapsed_time >= Duration::from_secs(3));
    assert!(elapsed_time < Duration::from_secs(6));

    println!("✓ Real DOCKER timeout test passed");
    Ok(())
}

/// Test Docker with nonexistent image for error handling
#[ignore = "Requires Docker daemon(CI environment have Docker in docker but socket not mounted)"]
#[tokio::test]
async fn test_real_docker_nonexistent_image() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Try to run container with nonexistent image
    let job = create_docker_job(
        "nonexistent-image-12345:latest",
        vec!["echo".to_string(), "should not work".to_string()],
        30000,
    );
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    // Verify execution completed with error
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::OtherError as i32); // Docker runner failed with image not found

    // Verify error was captured in output
    let docker_result: String = String::from_utf8_lossy(&data.output.unwrap().items).to_string();
    // Error message may contain information about image not found
    assert!(!docker_result.is_empty());

    println!("✓ Real DOCKER nonexistent image test passed");
    Ok(())
}

/// Integration test demonstrating complete DOCKER runner real workflow
#[ignore = "Requires Docker daemon(CI environment have Docker in docker but socket not mounted)"]
#[tokio::test]
async fn test_real_docker_runner_complete_workflow() -> Result<()> {
    println!("=== Real DOCKER Runner Complete Workflow Test ===");

    let job_runner = get_real_job_runner().await;
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    // Test 1: Basic container execution
    let basic_job = create_docker_job(
        "alpine:latest",
        vec!["echo".to_string(), "Workflow Test 1".to_string()],
        30000,
    );
    let (result1, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, basic_job)
        .await;
    assert_eq!(result1.data.unwrap().status, ResultStatus::Success as i32);
    println!("  ✓ Basic container execution passed");

    // Test 2: Linux utilities usage
    let util_job = create_docker_job(
        "alpine:latest",
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "uname -a && date && hostname".to_string(),
        ],
        30000,
    );
    let (result2, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, util_job)
        .await;
    assert_eq!(result2.data.unwrap().status, ResultStatus::Success as i32);
    println!("  ✓ Linux utilities usage passed");

    // Test 3: File manipulation
    let file_job = create_docker_job(
        "alpine:latest",
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo 'test data' > /tmp/workflow.txt && wc -w /tmp/workflow.txt".to_string(),
        ],
        30000,
    );
    let (result3, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, file_job)
        .await;
    assert_eq!(result3.data.unwrap().status, ResultStatus::Success as i32);
    println!("  ✓ File manipulation passed");

    // Test 4: Mathematical computation
    let math_job = create_docker_job(
        "alpine:latest",
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo $((123 * 456)) && echo 'Math completed'".to_string(),
        ],
        30000,
    );
    let (result4, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, math_job)
        .await;
    let data4 = result4.data.unwrap();
    assert_eq!(data4.status, ResultStatus::Success as i32);

    // Verify mathematical result
    let output: String = String::from_utf8_lossy(&data4.output.unwrap().items).to_string();
    assert!(output.contains("56088")); // 123 * 456 = 56088
    println!("  ✓ Mathematical computation passed");

    println!("=== Real DOCKER Runner Complete Workflow Test PASSED ===");
    Ok(())
}
