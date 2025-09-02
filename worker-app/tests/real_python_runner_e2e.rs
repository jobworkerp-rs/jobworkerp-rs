//! Real E2E tests for PYTHON_COMMAND runner with actual Python execution
//!
//! These tests execute actual Python scripts and verify real output
//! using the JobRunner infrastructure with real test app modules.

use anyhow::Result;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_runner::jobworkerp::runner::{PythonCommandArgs, PythonCommandResult};
use proto::jobworkerp::data::{
    Job, JobData, JobId, ResponseType, ResultStatus, RunnerData, RunnerType, WorkerData, WorkerId,
};
use std::collections::HashMap;
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

/// Create a test job with PYTHON_COMMAND runner
fn create_python_job(script: &str, timeout_ms: u64) -> Job {
    let python_args = PythonCommandArgs {
        env_vars: HashMap::new(),
        script: Some(
            jobworkerp_runner::jobworkerp::runner::python_command_args::Script::ScriptContent(
                script.to_string(),
            ),
        ),
        with_stderr: true,
        input_data: None,
    };
    let args_bytes = ProstMessageCodec::serialize_message(&python_args).unwrap();

    Job {
        id: Some(JobId { value: 1 }),
        data: Some(JobData {
            worker_id: Some(WorkerId { value: 1 }),
            args: args_bytes,
            uniq_key: Some("real_python_test".to_string()),
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
        name: "real_python_test_worker".to_string(),
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
        name: RunnerType::PythonCommand.as_str_name().to_string(),
        ..Default::default()
    };

    (worker_data, runner_data)
}

/// Test basic Python script execution with real output verification
#[tokio::test]
async fn test_real_python_basic_execution() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let job_runner = get_real_job_runner().await;
    let script = r#"
print("Hello Real Python E2E Test")
print("Python version check successful")
"#;
    let job = create_python_job(script, 15000);
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

    // Verify actual Python output
    assert!(data.output.is_some());
    let command_result: PythonCommandResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    tracing::debug!("Command result: {:#?}", command_result);
    assert_eq!(command_result.exit_code, 0);
    let stdout = command_result.output;
    assert!(stdout.contains("Hello Real Python E2E Test"));
    assert!(stdout.contains("Python version check successful"));

    // Should complete within reasonable time
    assert!(elapsed_time < Duration::from_secs(10));

    println!("✓ Real PYTHON_COMMAND basic execution test passed");
    Ok(())
}

/// Test Python script with actual mathematical computations
#[tokio::test]
async fn test_real_python_mathematical_computations() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    let script = r#"
import math

# Mathematical computations
result1 = sum(range(1000))  # Sum of numbers 0-999
result2 = math.pi * 100     # Pi calculation
result3 = 2 ** 20          # Power calculation

print(f"Sum result: {result1}")
print(f"Pi calculation: {result2:.5f}")
print(f"Power calculation: {result3}")

# Verify results
assert result1 == 499500, f"Expected 499500, got {result1}"
assert abs(result2 - 314.15926) < 0.01, f"Pi calculation error: {result2}"
assert result3 == 1048576, f"Expected 1048576, got {result3}"

print("All mathematical computations verified successfully")
"#;

    let job = create_python_job(script, 15000);
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    // Verify successful execution
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    // Verify actual computational results
    let command_result: PythonCommandResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    assert_eq!(command_result.exit_code, 0);
    let stdout = command_result.output;

    println!("Command stdout:\n{stdout}");
    // Verify computational outputs
    assert!(stdout.contains("Sum result: 499500"));
    assert!(stdout.contains("Pi calculation: 314.15927"));
    assert!(stdout.contains("Power calculation: 1048576"));
    assert!(stdout.contains("All mathematical computations verified successfully"));

    println!("✓ Real PYTHON_COMMAND mathematical computations test passed");
    Ok(())
}

/// Test Python script with actual file system operations
#[tokio::test]
async fn test_real_python_filesystem_operations() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    let temp_file = format!("/tmp/python_e2e_test_{}.txt", std::process::id());
    let script = format!(
        r#"
import os
import json

# File system operations
temp_file = "{temp_file}"
test_data = {{"message": "Real Python E2E Test", "numbers": [1, 2, 3, 4, 5]}}

# Write JSON data to file
with open(temp_file, 'w') as f:
    json.dump(test_data, f, indent=2)

# Verify file exists
assert os.path.exists(temp_file), f"File {{temp_file}} was not created"

# Read and verify data
with open(temp_file, 'r') as f:
    loaded_data = json.load(f)

assert loaded_data == test_data, f"Data mismatch: {{loaded_data}} != {{test_data}}"

# File operations verification
file_size = os.path.getsize(temp_file)
print(f"Created file: {{temp_file}}")
print(f"File size: {{file_size}} bytes")
print(f"File content: {{loaded_data}}")

# Cleanup
os.remove(temp_file)
assert not os.path.exists(temp_file), f"File {{temp_file}} was not cleaned up"

print("File system operations completed successfully")
"#,
    );

    let job = create_python_job(&script, 15000);
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    // Verify successful execution
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    // Verify file operations worked
    let command_result: PythonCommandResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    assert_eq!(command_result.exit_code, 0);
    let stdout = command_result.output;

    assert!(stdout.contains(&format!("Created file: {temp_file}",)));
    assert!(stdout.contains("File size:"));
    assert!(stdout.contains("Real Python E2E Test"));
    assert!(stdout.contains("File system operations completed successfully"));

    println!("✓ Real PYTHON_COMMAND filesystem operations test passed");
    Ok(())
}

/// Test Python script with actual package imports and data processing
#[tokio::test]
async fn test_real_python_package_imports_and_data_processing() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    let script = r#"
import json
import datetime
import base64
import hashlib
import urllib.parse

# Data processing with standard library
data = {
    "timestamp": datetime.datetime.now().isoformat(),
    "test_id": "real_python_e2e",
    "values": [10, 20, 30, 40, 50]
}

# JSON processing
json_str = json.dumps(data, indent=2)
parsed_data = json.loads(json_str)

# Base64 encoding/decoding
message = "Real Python E2E Test Data Processing"
encoded = base64.b64encode(message.encode()).decode()
decoded = base64.b64decode(encoded).decode()

assert decoded == message, f"Base64 decode failed: {decoded} != {message}"

# Hash calculation
hash_obj = hashlib.sha256(message.encode())
hash_hex = hash_obj.hexdigest()

# URL encoding
url_data = urllib.parse.quote(message)

# Statistics
values = parsed_data["values"]
total = sum(values)
average = total / len(values)
max_val = max(values)
min_val = min(values)

print(f"Original message: {message}")
print(f"Base64 encoded: {encoded}")
print(f"SHA256 hash: {hash_hex}")
print(f"URL encoded: {url_data}")
print(f"Statistics - Total: {total}, Average: {average}, Max: {max_val}, Min: {min_val}")
print(f"Data timestamp: {parsed_data['timestamp']}")

# Verification
assert total == 150, f"Expected total 150, got {total}"
assert average == 30, f"Expected average 30, got {average}"
assert max_val == 50, f"Expected max 50, got {max_val}"
assert min_val == 10, f"Expected min 10, got {min_val}"

print("Data processing with packages completed successfully")
"#;

    let job = create_python_job(script, 20000);
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    // Verify successful execution
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    // Verify package imports and data processing worked
    let command_result: PythonCommandResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    assert_eq!(command_result.exit_code, 0);
    let stdout = command_result.output;

    assert!(stdout.contains("Original message: Real Python E2E Test Data Processing"));
    assert!(stdout.contains("Base64 encoded:"));
    assert!(stdout.contains("SHA256 hash:"));
    assert!(stdout.contains("URL encoded:"));
    assert!(stdout.contains("Statistics - Total: 150, Average: 30"));
    assert!(stdout.contains("Data timestamp:"));
    assert!(stdout.contains("Data processing with packages completed successfully"));

    println!("✓ Real PYTHON_COMMAND package imports and data processing test passed");
    Ok(())
}

/// Test Python error handling with actual Python exceptions
#[tokio::test]
async fn test_real_python_error_handling() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    let script = r#"
# This script will intentionally cause a Python error
print("Starting error test")

# Division by zero error
try:
    result = 10 / 0
except ZeroDivisionError as e:
    print(f"Caught expected error: {e}")

# File not found error
try:
    with open("/nonexistent/file/path.txt", "r") as f:
        content = f.read()
except FileNotFoundError as e:
    print(f"Caught expected file error: {e}")

# Index error
try:
    my_list = [1, 2, 3]
    value = my_list[10]
except IndexError as e:
    print(f"Caught expected index error: {e}")

print("Error handling test completed successfully")

# Now cause an unhandled error to test error propagation
raise ValueError("Intentional error for E2E testing")
"#;

    let job = create_python_job(script, 15000);
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    // Verify execution completed (even with error)
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32); // Runner completed successfully

    // Verify Python script itself failed
    let command_result: PythonCommandResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    assert_ne!(command_result.exit_code, 0); // Non-zero exit code

    let stdout = &command_result.output;
    let stderr = command_result.output_stderr.as_ref().unwrap();

    // Verify error handling worked before the unhandled error
    assert!(stdout.contains("Starting error test"));
    assert!(stdout.contains("Caught expected error: division by zero"));
    assert!(stdout.contains("Caught expected file error:"));
    assert!(stdout.contains("Caught expected index error:"));
    assert!(stdout.contains("Error handling test completed successfully"));

    // Verify unhandled error was captured
    assert!(stderr.contains("ValueError: Intentional error for E2E testing"));

    println!("✓ Real PYTHON_COMMAND error handling test passed");
    Ok(())
}

/// Test Python timeout with actual long-running script
#[tokio::test]
async fn test_real_python_timeout() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    let script = r#"
import time

print("Starting long-running Python script")
print("This script will be terminated by timeout")

# Long computation that will be interrupted
for i in range(10):
    print(f"Processing step {i+1}/10")
    time.sleep(1)  # Sleep for 1 second each iteration

print("This should not be reached due to timeout")
"#;

    let job = create_python_job(script, 3000); // 3 second timeout
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
    assert!(elapsed_time < Duration::from_secs(9));

    println!("✓ Real PYTHON_COMMAND timeout test passed");
    Ok(())
}

/// Integration test demonstrating complete PYTHON_COMMAND runner real workflow
#[tokio::test]
async fn test_real_python_runner_complete_workflow() -> Result<()> {
    println!("=== Real PYTHON_COMMAND Runner Complete Workflow Test ===");

    let job_runner = get_real_job_runner().await;
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    // Test 1: Basic Python execution
    let basic_script = r#"print("Workflow Test 1: Basic Python")"#;
    let basic_job = create_python_job(basic_script, 10000);
    let (result1, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, basic_job)
        .await;
    assert_eq!(result1.data.unwrap().status, ResultStatus::Success as i32);
    println!("  ✓ Basic Python execution passed");

    // Test 2: Mathematical computation
    let math_script = r#"
import math
result = math.sqrt(16) + math.pow(2, 3)
print(f"Mathematical result: {result}")
assert result == 12.0, f"Expected 12.0, got {result}"
"#;
    let math_job = create_python_job(math_script, 10000);
    let (result2, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, math_job)
        .await;
    assert_eq!(result2.data.unwrap().status, ResultStatus::Success as i32);
    println!("  ✓ Mathematical computation passed");

    // Test 3: Data processing
    let data_script = r#"
import json
data = {"test": "workflow", "values": [1, 2, 3]}
json_str = json.dumps(data)
parsed = json.loads(json_str)
print(f"Data processing: {parsed}")
"#;
    let data_job = create_python_job(data_script, 10000);
    let (result3, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, data_job)
        .await;
    assert_eq!(result3.data.unwrap().status, ResultStatus::Success as i32);
    println!("  ✓ Data processing passed");

    // Test 4: Memory monitoring verification
    let memory_script = r#"
# Create some data to use memory
data = [i**2 for i in range(1000)]
total = sum(data)
print(f"Memory test completed with result: {total}")
"#;
    let memory_job = create_python_job(memory_script, 10000);
    let (result4, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, memory_job)
        .await;
    let data4 = result4.data.unwrap();
    assert_eq!(data4.status, ResultStatus::Success as i32);

    let _command_result: PythonCommandResult =
        ProstMessageCodec::deserialize_message(&data4.output.unwrap().items)?;

    println!("=== Real PYTHON_COMMAND Runner Complete Workflow Test PASSED ===");
    Ok(())
}
