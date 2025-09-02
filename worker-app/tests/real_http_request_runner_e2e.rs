//! Real E2E tests for HTTP_REQUEST runner with actual HTTP communication
//!
//! These tests make actual HTTP requests and verify real responses
//! using the JobRunner infrastructure with real test app modules.

use anyhow::Result;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_runner::jobworkerp::runner::{
    http_request_args::KeyValue, HttpRequestArgs, HttpResponseResult,
};
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

/// Create a test job with HTTP_REQUEST runner
fn create_http_request_job(
    url: &str,
    method: &str,
    headers: Vec<KeyValue>,
    body: Option<String>,
    timeout_ms: u64,
) -> Job {
    let http_args = HttpRequestArgs {
        path: url.to_string(),
        method: method.to_string(),
        headers,
        body,
        queries: vec![],
    };
    let args_bytes = ProstMessageCodec::serialize_message(&http_args).unwrap();

    Job {
        id: Some(JobId { value: 1 }),
        data: Some(JobData {
            worker_id: Some(WorkerId { value: 1 }),
            args: args_bytes,
            uniq_key: Some("real_http_test".to_string()),
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
    // Create HTTP request runner settings with httpbin.org as base_url
    let http_settings = jobworkerp_runner::jobworkerp::runner::HttpRequestRunnerSettings {
        base_url: "https://httpbin.org".to_string(),
    };
    let settings_bytes = ProstMessageCodec::serialize_message(&http_settings).unwrap();

    let worker_data = WorkerData {
        name: "real_http_test_worker".to_string(),
        runner_settings: settings_bytes,
        retry_policy: None,
        channel: Some("test".to_string()),
        response_type: ResponseType::NoResult as i32,
        store_success: false,
        store_failure: false,
        use_static: false, // Use real execution, not static pool
        ..Default::default()
    };

    let runner_data = RunnerData {
        name: RunnerType::HttpRequest.as_str_name().to_string(),
        ..Default::default()
    };

    (worker_data, runner_data)
}

/// Test basic HTTP GET request with real public API
#[tokio::test]
async fn test_real_http_get_request() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let job_runner = get_real_job_runner().await;

    // Use a reliable public API for testing
    let job = create_http_request_job(
        "/get",
        "GET",
        vec![KeyValue {
            key: "User-Agent".to_string(),
            value: "jobworkerp-rs-e2e-test".to_string(),
        }],
        None,
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

    // Verify actual HTTP response
    assert!(data.output.is_some());
    let http_result: HttpResponseResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    assert_eq!(http_result.status_code, 200);

    // Check response data
    if let Some(response_data) = &http_result.response_data {
        match response_data {
            jobworkerp_runner::jobworkerp::runner::http_response_result::ResponseData::Content(
                content,
            ) => {
                assert!(content.contains("httpbin.org"));
                assert!(content.contains("User-Agent"));
                assert!(content.contains("jobworkerp-rs-e2e-test"));
            }
            jobworkerp_runner::jobworkerp::runner::http_response_result::ResponseData::Chunk(
                chunk,
            ) => {
                let content = String::from_utf8_lossy(chunk);
                assert!(content.contains("httpbin.org"));
                assert!(content.contains("User-Agent"));
                assert!(content.contains("jobworkerp-rs-e2e-test"));
            }
        }
    }

    // Should complete within reasonable time
    assert!(elapsed_time < Duration::from_secs(10));

    println!("✓ Real HTTP_REQUEST GET request test passed");
    Ok(())
}

/// Test HTTP POST request with actual data submission
#[tokio::test]
async fn test_real_http_post_request() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    let post_data = r#"{"test": "Real HTTP E2E Test", "timestamp": "2025-01-01T00:00:00Z"}"#;
    let job = create_http_request_job(
        "/post",
        "POST",
        vec![
            KeyValue {
                key: "Content-Type".to_string(),
                value: "application/json".to_string(),
            },
            KeyValue {
                key: "Accept".to_string(),
                value: "application/json".to_string(),
            },
        ],
        Some(post_data.to_string()),
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

    // Verify HTTP POST response
    let http_result: HttpResponseResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    assert_eq!(http_result.status_code, 200);

    // Check response data
    if let Some(response_data) = &http_result.response_data {
        match response_data {
            jobworkerp_runner::jobworkerp::runner::http_response_result::ResponseData::Content(
                content,
            ) => {
                assert!(content.contains("Real HTTP E2E Test"));
                assert!(content.contains("application/json"));
                assert!(content.contains("data"));
            }
            jobworkerp_runner::jobworkerp::runner::http_response_result::ResponseData::Chunk(
                chunk,
            ) => {
                let content = String::from_utf8_lossy(chunk);
                assert!(content.contains("Real HTTP E2E Test"));
                assert!(content.contains("application/json"));
                assert!(content.contains("data"));
            }
        }
    }

    println!("✓ Real HTTP_REQUEST POST request test passed");
    Ok(())
}

/// Test HTTP request with query parameters
#[tokio::test]
async fn test_real_http_query_parameters() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    let job = create_http_request_job(
        "/get?param1=value1&param2=Real%20E2E%20Test&numeric=42",
        "GET",
        vec![KeyValue {
            key: "X-Test-Header".to_string(),
            value: "E2E-Query-Test".to_string(),
        }],
        None,
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

    // Verify query parameters were sent correctly
    let http_result: HttpResponseResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    assert_eq!(http_result.status_code, 200);

    // Check response data
    if let Some(response_data) = &http_result.response_data {
        let body_content = match response_data {
            jobworkerp_runner::jobworkerp::runner::http_response_result::ResponseData::Content(
                content,
            ) => content.clone(),
            jobworkerp_runner::jobworkerp::runner::http_response_result::ResponseData::Chunk(
                chunk,
            ) => String::from_utf8_lossy(chunk).to_string(),
        };
        assert!(body_content.contains("param1"));
        assert!(body_content.contains("value1"));
        assert!(body_content.contains("Real E2E Test"));
        assert!(body_content.contains("numeric"));
        assert!(body_content.contains("42"));
        assert!(body_content.contains("E2E-Query-Test"));
    }

    println!("✓ Real HTTP_REQUEST query parameters test passed");
    Ok(())
}

/// Test HTTP request timeout with actual slow endpoint
#[tokio::test]
async fn test_real_http_timeout() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Use httpbin.org delay endpoint that takes 5 seconds
    let job = create_http_request_job(
        "/delay/5",
        "GET",
        vec![],
        None,
        3000, // 3 second timeout - should timeout before 5 second delay
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

    // Should timeout around 3 seconds, not wait full 5 seconds
    assert!(elapsed_time >= Duration::from_secs(3));
    assert!(elapsed_time < Duration::from_secs(6));

    println!("✓ Real HTTP_REQUEST timeout test passed");
    Ok(())
}

/// Test HTTP error handling with actual error response
#[tokio::test]
async fn test_real_http_error_handling() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Request a 404 Not Found endpoint
    let job = create_http_request_job("/status/404", "GET", vec![], None, 30000);
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    // Verify execution completed (HTTP runner successful)
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32); // HTTP runner completed successfully

    // Verify 404 status code was captured
    let http_result: HttpResponseResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    assert_eq!(http_result.status_code, 404);

    println!("✓ Real HTTP_REQUEST error handling test passed");
    Ok(())
}

/// Test HTTP request with custom headers validation
#[tokio::test]
async fn test_real_http_custom_headers() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    let job = create_http_request_job(
        "/headers",
        "GET",
        vec![
            KeyValue {
                key: "X-Custom-Header-1".to_string(),
                value: "Real-E2E-Test-Value-1".to_string(),
            },
            KeyValue {
                key: "X-Custom-Header-2".to_string(),
                value: "Real-E2E-Test-Value-2".to_string(),
            },
            KeyValue {
                key: "Authorization".to_string(),
                value: "Bearer test-token-12345".to_string(),
            },
        ],
        None,
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

    // Verify custom headers were sent
    let http_result: HttpResponseResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    assert_eq!(http_result.status_code, 200);

    // Check response data
    if let Some(response_data) = &http_result.response_data {
        let body_content = match response_data {
            jobworkerp_runner::jobworkerp::runner::http_response_result::ResponseData::Content(
                content,
            ) => content.clone(),
            jobworkerp_runner::jobworkerp::runner::http_response_result::ResponseData::Chunk(
                chunk,
            ) => String::from_utf8_lossy(chunk).to_string(),
        };
        assert!(body_content.contains("X-Custom-Header-1"));
        assert!(body_content.contains("Real-E2E-Test-Value-1"));
        assert!(body_content.contains("X-Custom-Header-2"));
        assert!(body_content.contains("Real-E2E-Test-Value-2"));
        assert!(body_content.contains("Authorization"));
        assert!(body_content.contains("Bearer test-token-12345"));
    }

    println!("✓ Real HTTP_REQUEST custom headers test passed");
    Ok(())
}

/// Test HTTP PUT request with data update
#[tokio::test]
async fn test_real_http_put_request() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    let put_data = r#"{"id": 123, "name": "Real HTTP E2E Test Update", "status": "active"}"#;
    let job = create_http_request_job(
        "/put",
        "PUT",
        vec![KeyValue {
            key: "Content-Type".to_string(),
            value: "application/json".to_string(),
        }],
        Some(put_data.to_string()),
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

    // Verify PUT response
    let http_result: HttpResponseResult =
        ProstMessageCodec::deserialize_message(&data.output.unwrap().items)?;

    assert_eq!(http_result.status_code, 200);

    // Check response data
    if let Some(response_data) = &http_result.response_data {
        let body_content = match response_data {
            jobworkerp_runner::jobworkerp::runner::http_response_result::ResponseData::Content(
                content,
            ) => content.clone(),
            jobworkerp_runner::jobworkerp::runner::http_response_result::ResponseData::Chunk(
                chunk,
            ) => String::from_utf8_lossy(chunk).to_string(),
        };
        assert!(body_content.contains("Real HTTP E2E Test Update"));
        assert!(body_content.contains("application/json"));
    }

    println!("✓ Real HTTP_REQUEST PUT request test passed");
    Ok(())
}

/// Integration test demonstrating complete HTTP_REQUEST runner real workflow
#[tokio::test]
async fn test_real_http_request_runner_complete_workflow() -> Result<()> {
    println!("=== Real HTTP_REQUEST Runner Complete Workflow Test ===");

    let job_runner = get_real_job_runner().await;
    let (worker_data, runner_data) = create_test_data();
    let worker_id = WorkerId { value: 1 };

    // Test 1: Basic GET request
    let get_job = create_http_request_job("/get", "GET", vec![], None, 30000);
    let (result1, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, get_job)
        .await;
    assert_eq!(result1.data.unwrap().status, ResultStatus::Success as i32);
    println!("  ✓ Basic GET request passed");

    // Test 2: POST with JSON data
    let post_job = create_http_request_job(
        "/post",
        "POST",
        vec![KeyValue {
            key: "Content-Type".to_string(),
            value: "application/json".to_string(),
        }],
        Some(r#"{"workflow": "test", "step": 2}"#.to_string()),
        30000,
    );
    let (result2, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, post_job)
        .await;
    assert_eq!(result2.data.unwrap().status, ResultStatus::Success as i32);
    println!("  ✓ POST with JSON data passed");

    // Test 3: Headers and authentication
    let auth_job = create_http_request_job(
        "/bearer",
        "GET",
        vec![KeyValue {
            key: "Authorization".to_string(),
            value: "Bearer workflow-test-token".to_string(),
        }],
        None,
        30000,
    );
    let (result3, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, auth_job)
        .await;
    assert_eq!(result3.data.unwrap().status, ResultStatus::Success as i32);
    println!("  ✓ Headers and authentication passed");

    // Test 4: Response status verification
    let status_job = create_http_request_job("/status/201", "GET", vec![], None, 30000);
    let (result4, _) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, status_job)
        .await;
    let data4 = result4.data.unwrap();
    assert_eq!(data4.status, ResultStatus::Success as i32);

    // Verify specific status code
    let http_result: HttpResponseResult =
        ProstMessageCodec::deserialize_message(&data4.output.unwrap().items)?;
    assert_eq!(http_result.status_code, 201);
    println!("  ✓ Response status verification passed");

    println!("=== Real HTTP_REQUEST Runner Complete Workflow Test PASSED ===");
    Ok(())
}
