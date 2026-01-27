//! Real E2E tests for INLINE_WORKFLOW runner with actual workflow execution
//!
//! These tests execute actual workflows and verify real execution
//! using the JobRunner infrastructure with real test app modules.

use anyhow::Result;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_runner::jobworkerp::runner::{InlineWorkflowArgs, WorkflowResult};
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

/// Create a test job with INLINE_WORKFLOW runner
fn create_inline_workflow_job(workflow_yaml: &str, timeout_ms: u64) -> Job {
    let workflow_args = InlineWorkflowArgs {
        workflow_source: Some(
            jobworkerp_runner::jobworkerp::runner::inline_workflow_args::WorkflowSource::WorkflowData(
                workflow_yaml.to_string(),
            ),
        ),
        input: r#"{"test": "input"}"#.to_string(),
        workflow_context: None,
        execution_id: None,
        from_checkpoint: None,
    };
    let args_bytes = ProstMessageCodec::serialize_message(&workflow_args).unwrap();

    Job {
        id: Some(JobId { value: 1 }),
        data: Some(JobData {
            worker_id: Some(WorkerId { value: 1 }),
            args: args_bytes,
            uniq_key: Some("real_workflow_test".to_string()),
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
        name: "real_workflow_test_worker".to_string(),
        runner_settings: vec![],
        retry_policy: None,
        channel: None,
        response_type: ResponseType::Direct as i32,
        store_success: false,
        store_failure: false,
        use_static: true, // Use static pool for better performance
        ..Default::default()
    };

    let runner_data = RunnerData {
        name: RunnerType::InlineWorkflow.as_str_name().to_string(),
        ..Default::default()
    };

    (worker_data, runner_data)
}

/// Test basic inline workflow execution with real workflow engine
#[tokio::test]
#[ignore = "This test requires real infrastructure and may take time to run"]
async fn test_real_inline_workflow_basic_execution() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let job_runner = get_real_job_runner().await;

    // Simple workflow with command execution
    let workflow_yaml = r#"
document:
  dsl: "1.0.0"
  namespace: "default"
  name: "real-e2e-basic-workflow"
  version: "1.0.0"
input:
  schema:
    document:
      type: object
do:
  - HelloWorld:
      run:
        runner:
          name: "COMMAND"
          arguments:
            command: "echo"
            args: ["Hello Real Workflow E2E Test"]
            with_memory_monitoring: true
          options:
            useStatic: false
            storeSuccess: true
            storeFailure: true
"#;

    let job = create_inline_workflow_job(workflow_yaml, 60000);
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
    let workflow_result: WorkflowResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    assert!(!workflow_result.id.is_empty());
    assert!(
        workflow_result
            .output
            .contains("Hello Real Workflow E2E Test")
    );

    // Should complete within reasonable time
    assert!(elapsed_time < Duration::from_secs(10));

    println!("✓ Real INLINE_WORKFLOW basic execution test passed");
    Ok(())
}

/// Test inline workflow with sequential steps
#[tokio::test]
#[ignore = "This test requires real infrastructure and may take time to run"]
async fn test_real_inline_workflow_sequential_steps() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Workflow with multiple sequential steps
    let workflow_yaml = r#"
document:
  dsl: "1.0.0"
  namespace: "default"
  name: "real-e2e-sequential-workflow"
  version: "1.0.0"
input:
  schema:
    document:
      type: object
do:
  - SequentialSteps:
      do:
        - StepOne:
            run:
              runner:
                name: "COMMAND"
                arguments:
                  command: "echo"
                  args: ["Step 1: Starting workflow"]
                  with_memory_monitoring: true
        - StepTwo:
            run:
              runner:
                name: "COMMAND"
                arguments:
                  command: "echo"
                  args: ["Step 2: Middle of workflow"]
                  with_memory_monitoring: true
        - StepThree:
            run:
              runner:
                name: "COMMAND"
                arguments:
                  command: "echo"
                  args: ["Step 3: Completing workflow"]
                  with_memory_monitoring: true
"#;

    let job = create_inline_workflow_job(workflow_yaml, 45000);
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    let workflow_result: WorkflowResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    let output = &workflow_result.output;
    assert!(output.contains("Step 1: Starting workflow"));
    assert!(output.contains("Step 2: Middle of workflow"));
    assert!(output.contains("Step 3: Completing workflow"));

    println!("✓ Real INLINE_WORKFLOW sequential steps test passed");
    Ok(())
}

/// Test inline workflow with data transformation
#[tokio::test]
#[ignore = "This test requires real infrastructure and may take time to run"]
async fn test_real_inline_workflow_data_transformation() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Workflow with data processing and transformation
    let workflow_yaml = r#"
document:
  dsl: "1.0.0"
  namespace: "default"
  name: "real-e2e-data-workflow"
  version: "1.0.0"
input:
  schema:
    document:
      type: object
do:
  - DataProcessing:
      do:
        - GenerateData:
            run:
              runner:
                name: "COMMAND"
                arguments:
                  command: "echo"
                  args: ["Real E2E Workflow Data: 123"]
                  with_memory_monitoring: true
        - ProcessData:
            run:
              runner:
                name: "COMMAND"
                arguments:
                  command: "sh"
                  args: ["-c", "echo 'Processing data...' && echo $((123 * 2))"]
                  with_memory_monitoring: true
        - FinalizeData:
            run:
              runner:
                name: "COMMAND"
                arguments:
                  command: "sh"
                  args: ["-c", "echo 'Final result: 246'"]
                  with_memory_monitoring: true
"#;

    let job = create_inline_workflow_job(workflow_yaml, 45000);
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    let workflow_result: WorkflowResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    let output = &workflow_result.output;
    assert!(output.contains("Real E2E Workflow Data: 123"));
    assert!(output.contains("Processing data"));
    assert!(output.contains("246")); // 123 * 2
    assert!(output.contains("Final result"));

    println!("✓ Real INLINE_WORKFLOW data transformation test passed");
    Ok(())
}

/// Test inline workflow with parallel execution
#[tokio::test]
#[ignore = "This test requires real infrastructure and may take time to run"]
async fn test_real_inline_workflow_parallel_execution() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Workflow with parallel actions
    let workflow_yaml = r#"
document:
  dsl: "1.0.0"
  namespace: "default"
  name: "real-e2e-parallel-workflow"
  version: "1.0.0"
input:
  schema:
    document:
      type: object
do:
  - ParallelProcessing:
      fork:
        branches:
          - TaskA:
              run:
                runner:
                  name: "COMMAND"
                  arguments:
                    command: "echo"
                    args: ["Parallel Task A completed"]
                    with_memory_monitoring: true
          - TaskB:
              run:
                runner:
                  name: "COMMAND"
                  arguments:
                    command: "echo"
                    args: ["Parallel Task B completed"]
                    with_memory_monitoring: true
          - TaskC:
              run:
                runner:
                  name: "COMMAND"
                  arguments:
                    command: "echo"
                    args: ["Parallel Task C completed"]
                    with_memory_monitoring: true
"#;

    let job = create_inline_workflow_job(workflow_yaml, 30000);
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let start_time = Instant::now();
    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;
    let elapsed_time = start_time.elapsed();

    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    let workflow_result: WorkflowResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    let output = &workflow_result.output;
    assert!(output.contains("Parallel Task A completed"));
    assert!(output.contains("Parallel Task B completed"));
    assert!(output.contains("Parallel Task C completed"));

    // Parallel execution should be faster than sequential (but still allow some overhead)
    assert!(elapsed_time < Duration::from_secs(8));

    println!("✓ Real INLINE_WORKFLOW parallel execution test passed");
    Ok(())
}

/// Test inline workflow timeout with actual long-running workflow
#[tokio::test]
async fn test_real_inline_workflow_timeout() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Workflow with long-running task that will timeout
    let workflow_yaml = r#"
document:
  dsl: "1.0.0"
  namespace: "default"
  name: "real-e2e-timeout-workflow"
  version: "1.0.0"
input:
  schema:
    document:
      type: object
do:
  - LongTask:
      run:
        runner:
          name: "COMMAND"
          arguments:
            command: "sleep"
            args: ["10"]
            with_memory_monitoring: true
"#;

    let job = create_inline_workflow_job(workflow_yaml, 3000); // 3 second timeout
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let start_time = Instant::now();
    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;
    let elapsed_time = start_time.elapsed();

    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::MaxRetry as i32);

    // Should timeout around 3 seconds, not wait full 10 seconds
    println!("elapsed_time: {}s", elapsed_time.as_secs_f64());
    assert!(elapsed_time >= Duration::from_secs(3));
    assert!(elapsed_time < Duration::from_secs(6));

    println!("✓ Real INLINE_WORKFLOW timeout test passed");
    Ok(())
}

/// Test inline workflow error handling with actual failing step
#[tokio::test]
#[ignore = "This test requires real infrastructure and may take time to run"]
async fn test_real_inline_workflow_error_handling() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Workflow with step that will fail
    let workflow_yaml = r#"
document:
  dsl: "1.0.0"
  namespace: "default"
  name: "real-e2e-error-workflow"
  version: "1.0.0"
input:
  schema:
    document:
      type: object
do:
  - ErrorHandling:
      do:
        - SuccessfulStep:
            run:
              runner:
                name: "COMMAND"
                arguments:
                  command: "echo"
                  args: ["Step 1 successful"]
                  with_memory_monitoring: true
        - FailingStep:
            run:
              runner:
                name: "COMMAND"
                arguments:
                  command: "sh"
                  args: ["-c", "echo 'About to fail' && exit 1"]
                  with_memory_monitoring: true
"#;

    let job = create_inline_workflow_job(workflow_yaml, 30000);
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32); // Workflow runner completed

    let workflow_result: WorkflowResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    let output = &workflow_result.output;
    assert!(output.contains("Step 1 successful"));
    assert!(output.contains("About to fail"));

    // Workflow should indicate error occurred
    assert!(
        workflow_result.error_message.is_some()
            || workflow_result.output.contains("error")
            || workflow_result.output.contains("fail")
    );

    println!("✓ Real INLINE_WORKFLOW error handling test passed");
    Ok(())
}

/// Integration test demonstrating complete INLINE_WORKFLOW runner real workflow
#[tokio::test]
#[ignore = "This test requires real infrastructure and may take time to run"]
async fn test_real_inline_workflow_runner_complete_workflow() -> Result<()> {
    println!("=== Real INLINE_WORKFLOW Runner Complete Workflow Test ===");

    let job_runner = get_real_job_runner().await;
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    // Test 1: Basic workflow execution
    let basic_workflow = r#"
document:
  dsl: "1.0.0"
  namespace: "default"
  name: "complete-workflow-test-1"
  version: "1.0.0"
input:
  schema:
    document:
      type: object
do:
  - BasicStep:
      run:
        runner:
          name: "COMMAND"
          arguments:
            command: "echo"
            args: ["Complete Workflow Test 1"]
            with_memory_monitoring: true
"#;
    let basic_job = create_inline_workflow_job(basic_workflow, 30000);
    let (result1, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, basic_job)
        .await;
    assert_eq!(result1.data.unwrap().status, ResultStatus::Success as i32);
    println!("  ✓ Basic workflow execution passed");

    // Test 2: Multi-step workflow
    let multi_step_workflow = r#"
document:
  dsl: "1.0.0"
  namespace: "default"
  name: "complete-workflow-test-2"
  version: "1.0.0"
input:
  schema:
    document:
      type: object
do:
  - MultiStep:
      do:
        - FirstStep:
            run:
              runner:
                name: "COMMAND"
                arguments:
                  command: "echo"
                  args: ["Complete Test Step 1"]
                  with_memory_monitoring: true
        - SecondStep:
            run:
              runner:
                name: "COMMAND"
                arguments:
                  command: "echo"
                  args: ["Complete Test Step 2"]
                  with_memory_monitoring: true
"#;
    let multi_job = create_inline_workflow_job(multi_step_workflow, 45000);
    let (result2, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, multi_job)
        .await;
    assert_eq!(result2.data.unwrap().status, ResultStatus::Success as i32);
    println!("  ✓ Multi-step workflow passed");

    // Test 3: Workflow with computation
    let computation_workflow = r#"
document:
  dsl: "1.0.0"
  namespace: "default"
  name: "complete-workflow-test-3"
  version: "1.0.0"
input:
  schema:
    document:
      type: object
do:
  - Compute:
      run:
        runner:
          name: "COMMAND"
          arguments:
            command: "sh"
            args: ["-c", "echo Result: $((42 * 2))"]
            with_memory_monitoring: true
"#;
    let computation_job = create_inline_workflow_job(computation_workflow, 30000);
    let (result3, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, computation_job)
        .await;
    let data3 = result3.data.unwrap();
    assert_eq!(data3.status, ResultStatus::Success as i32);

    let workflow_result: WorkflowResult =
        ProstMessageCodec::deserialize_message(&data3.output.unwrap().items)?;
    assert!(workflow_result.output.contains("84")); // 42 * 2
    println!("  ✓ Workflow with computation passed");

    println!("=== Real INLINE_WORKFLOW Runner Complete Workflow Test PASSED ===");
    Ok(())
}
