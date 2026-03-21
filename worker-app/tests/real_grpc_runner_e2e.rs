//! Real E2E tests for GRPC runner (multi-method) with actual gRPC communication
//!
//! These tests make actual gRPC requests to a running jobworkerp-rs server
//! and verify real responses using the JobRunner infrastructure.
//!
//! Requires: A running gRPC server at localhost:9000

use anyhow::Result;
use futures::StreamExt;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_runner::jobworkerp::runner::grpc::{
    GrpcArgs, GrpcRunnerSettings, GrpcStreamingResult, GrpcUnaryResult, grpc_args,
    grpc_streaming_result, grpc_unary_result,
};
use jobworkerp_runner::runner::FeedData;
use proto::jobworkerp::data::{
    Job, JobData, JobId, ResponseType, ResultStatus, RunnerData, RunnerType, StreamingType,
    WorkerData, WorkerId, result_output_item,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{OnceCell, mpsc};

use app::app::WorkerConfig;
use app_wrapper::runner::{RunnerFactory, UseRunnerFactory};
use command_utils::trace::Tracing;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::{IdGeneratorWrapper, UseIdGenerator};
use worker_app::worker::runner::JobRunner;
use worker_app::worker::runner::map::{RunnerFactoryWithPoolMap, UseRunnerPoolMap};
use worker_app::worker::runner::result::RunnerResultHandler;

const GRPC_HOST: &str = "localhost";
const GRPC_PORT: u32 = 9000;

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
impl JobRunner for RealE2EJobRunner {
    fn register_feed_sender(&self, _job_id: i64, _sender: mpsc::Sender<FeedData>) {}
    fn unregister_feed_sender(&self, _job_id: i64) {}
}
impl Tracing for RealE2EJobRunner {}
impl UseIdGenerator for RealE2EJobRunner {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}

async fn get_real_job_runner() -> &'static RealE2EJobRunner {
    static JOB_RUNNER: OnceCell<Box<RealE2EJobRunner>> = OnceCell::const_new();
    JOB_RUNNER
        .get_or_init(|| async { Box::new(RealE2EJobRunner::new().await) })
        .await
}

fn create_grpc_settings(use_reflection: bool) -> Vec<u8> {
    let settings = GrpcRunnerSettings {
        host: format!("http://{GRPC_HOST}"),
        port: GRPC_PORT,
        tls: false,
        timeout_ms: Some(10000),
        max_message_size: None,
        auth_token: None,
        tls_config: None,
        use_reflection: Some(use_reflection),
        method: None,
        metadata: HashMap::new(),
        timeout: None,
        as_json: None,
    };
    ProstMessageCodec::serialize_message(&settings).unwrap()
}

fn create_grpc_job(
    method: &str,
    request: Option<grpc_args::Request>,
    as_json: bool,
    timeout_ms: i64,
    using: Option<&str>,
) -> Job {
    let grpc_args = GrpcArgs {
        method: Some(method.to_string()),
        request,
        metadata: HashMap::new(),
        timeout: Some(timeout_ms),
        as_json: Some(as_json),
    };
    let args_bytes = ProstMessageCodec::serialize_message(&grpc_args).unwrap();

    Job {
        id: Some(JobId { value: 1 }),
        data: Some(JobData {
            worker_id: Some(WorkerId { value: 1 }),
            args: args_bytes,
            uniq_key: Some("real_grpc_test".to_string()),
            retried: 0,
            priority: 0,
            timeout: timeout_ms as u64,
            enqueue_time: command_utils::util::datetime::now_millis(),
            run_after_time: command_utils::util::datetime::now_millis(),
            grabbed_until_time: None,
            streaming_type: 0,
            using: using.map(|s| s.to_string()),
        }),
        ..Default::default()
    }
}

fn create_test_data(use_reflection: bool) -> (WorkerData, RunnerData) {
    let settings_bytes = create_grpc_settings(use_reflection);

    let worker_data = WorkerData {
        name: "real_grpc_test_worker".to_string(),
        runner_settings: settings_bytes,
        retry_policy: None,
        channel: Some("test".to_string()),
        response_type: ResponseType::NoResult as i32,
        store_success: false,
        store_failure: false,
        use_static: false,
        ..Default::default()
    };

    let runner_data = RunnerData {
        name: RunnerType::Grpc.as_str_name().to_string(),
        ..Default::default()
    };

    (worker_data, runner_data)
}

/// Test gRPC unary call without reflection (raw protobuf bytes)
#[ignore = "Requires running gRPC server at localhost:9000"]
#[tokio::test]
async fn test_grpc_unary_without_reflection() -> Result<()> {
    // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let job_runner = get_real_job_runner().await;

    // Encode RunnerId { value: 1 } as raw protobuf bytes
    use prost::Message;
    let runner_id = proto::jobworkerp::data::RunnerId { value: 1 };
    let mut buf = Vec::with_capacity(runner_id.encoded_len());
    runner_id.encode(&mut buf)?;

    let job = create_grpc_job(
        "jobworkerp.service.RunnerService/Find",
        Some(grpc_args::Request::Body(buf)),
        false,
        10000,
        Some("unary"),
    );
    let (worker_data, runner_data) = create_test_data(false);
    let worker_id = WorkerId { value: 1 };

    let start_time = Instant::now();
    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;
    let elapsed = start_time.elapsed();

    assert!(result.data.is_some());
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    let output = data.output.unwrap();
    let grpc_result: GrpcUnaryResult = ProstMessageCodec::deserialize_message(&output.items)?;

    assert_eq!(grpc_result.code, tonic::Code::Ok as i32);
    let body = match &grpc_result.response_data {
        Some(grpc_unary_result::ResponseData::Body(b)) => b.clone(),
        other => panic!("Expected Body variant, got {:?}", other),
    };
    assert!(!body.is_empty());
    assert!(elapsed < Duration::from_secs(10));

    println!(
        "grpc unary result (no reflection): code={}, body_len={}",
        grpc_result.code,
        body.len()
    );
    println!("test_grpc_unary_without_reflection passed");
    Ok(())
}

/// Test gRPC unary call with reflection (JSON input/output)
#[ignore = "Requires running gRPC server at localhost:9000 with reflection enabled"]
#[tokio::test]
async fn test_grpc_unary_with_reflection() -> Result<()> {
    // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let job_runner = get_real_job_runner().await;

    let job = create_grpc_job(
        "jobworkerp.service.RunnerService/Find",
        Some(grpc_args::Request::JsonBody(
            r#"{"value": "1"}"#.to_string(),
        )),
        true,
        10000,
        Some("unary"),
    );
    let (worker_data, runner_data) = create_test_data(true);
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    assert!(result.data.is_some());
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    let output = data.output.unwrap();
    let grpc_result: GrpcUnaryResult = ProstMessageCodec::deserialize_message(&output.items)?;

    assert_eq!(grpc_result.code, tonic::Code::Ok as i32);
    let json_body = match &grpc_result.response_data {
        Some(grpc_unary_result::ResponseData::JsonBody(j)) => j.clone(),
        other => panic!(
            "Expected JsonBody variant when as_json=true with reflection, got {:?}",
            other
        ),
    };

    println!("grpc unary result (reflection): json_body={}", json_body);

    println!("test_grpc_unary_with_reflection passed");
    Ok(())
}

/// Test gRPC unary with default using (None should default to "unary")
#[ignore = "Requires running gRPC server at localhost:9000"]
#[tokio::test]
async fn test_grpc_unary_default_using() -> Result<()> {
    // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let job_runner = get_real_job_runner().await;

    // using=None should default to "unary"
    let job = create_grpc_job(
        "jobworkerp.service.RunnerService/Find",
        Some(grpc_args::Request::JsonBody(
            r#"{"value": "1"}"#.to_string(),
        )),
        true,
        10000,
        None,
    );
    let (worker_data, runner_data) = create_test_data(true);
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    assert!(result.data.is_some());
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    let output = data.output.unwrap();
    let grpc_result: GrpcUnaryResult = ProstMessageCodec::deserialize_message(&output.items)?;

    assert_eq!(grpc_result.code, tonic::Code::Ok as i32);

    println!("test_grpc_unary_default_using passed");
    Ok(())
}

/// Test gRPC server streaming call with ListRunners (server streaming via run())
#[ignore = "Requires running gRPC server at localhost:9000 with streaming endpoint"]
#[tokio::test]
async fn test_grpc_streaming_via_run() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let job_runner = get_real_job_runner().await;

    // Use RunnerService/FindList which returns a list (server streaming if available)
    let job = create_grpc_job(
        "jobworkerp.service.RunnerService/FindList",
        Some(grpc_args::Request::JsonBody("{}".to_string())),
        true,
        10000,
        Some("streaming"),
    );

    let mut streaming_job_data = job.data.clone().unwrap();
    streaming_job_data.streaming_type = StreamingType::Internal as i32;
    let streaming_job = Job {
        data: Some(streaming_job_data),
        ..job
    };

    let (worker_data, runner_data) = create_test_data(true);
    let worker_id = WorkerId { value: 1 };

    let (_result, stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, streaming_job)
        .await;

    // Internal streaming: stream must be returned with collected result
    let stream = stream.expect("Internal streaming should return a stream");
    let mut stream = stream;
    let mut data_count = 0;
    let mut got_final_collected = false;

    while let Some(item) = stream.next().await {
        match item.item {
            Some(result_output_item::Item::Data(data)) => {
                data_count += 1;
                assert!(
                    !data.is_empty(),
                    "Data item {} should not be empty",
                    data_count
                );
            }
            Some(result_output_item::Item::End(_)) => {
                break;
            }
            Some(result_output_item::Item::FinalCollected(collected)) => {
                let streaming_result: GrpcStreamingResult =
                    ProstMessageCodec::deserialize_message(&collected)?;
                assert_eq!(
                    streaming_result.code,
                    tonic::Code::Ok as i32,
                    "Streaming result should have OK status"
                );
                let bodies = match &streaming_result.response_data {
                    Some(grpc_streaming_result::ResponseData::Bodies(b)) => &b.items,
                    other => panic!("Expected Bodies variant, got {:?}", other),
                };
                assert!(
                    !bodies.is_empty(),
                    "Streaming result should contain at least one body"
                );
                for (i, body) in bodies.iter().enumerate() {
                    assert!(!body.is_empty(), "Body {} should not be empty", i);
                }
                println!(
                    "Collected streaming result: bodies={}, code={}",
                    bodies.len(),
                    streaming_result.code
                );
                got_final_collected = true;
                break;
            }
            None => {}
        }
    }

    assert!(
        data_count > 0 || got_final_collected,
        "Should receive Data items or FinalCollected, got data_count={}, got_final_collected={}",
        data_count,
        got_final_collected
    );

    println!("test_grpc_streaming_via_run passed");
    Ok(())
}

/// Test gRPC streaming call with run_stream() (response streaming)
#[ignore = "Requires running gRPC server at localhost:9000 with streaming endpoint"]
#[tokio::test]
async fn test_grpc_streaming_response() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let job_runner = get_real_job_runner().await;

    let job = create_grpc_job(
        "jobworkerp.service.RunnerService/FindList",
        Some(grpc_args::Request::JsonBody("{}".to_string())),
        false,
        10000,
        Some("streaming"),
    );

    let mut streaming_job_data = job.data.clone().unwrap();
    streaming_job_data.streaming_type = StreamingType::Response as i32;
    let streaming_job = Job {
        data: Some(streaming_job_data),
        ..job
    };

    // Use reflection=true so that "{}" is properly converted to an empty protobuf message
    let (worker_data, runner_data) = create_test_data(true);
    let worker_id = WorkerId { value: 1 };

    let (_result, stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, streaming_job)
        .await;

    // Response streaming: stream must be returned
    let stream = stream.expect("Response streaming should return a stream");
    let mut stream = stream;
    let mut data_count = 0;
    let mut got_end = false;

    while let Some(item) = stream.next().await {
        match item.item {
            Some(result_output_item::Item::Data(data)) => {
                data_count += 1;
                assert!(
                    !data.is_empty(),
                    "Stream data item {} should not be empty",
                    data_count
                );
            }
            Some(result_output_item::Item::End(trailer)) => {
                got_end = true;
                println!("Stream ended with trailer metadata: {:?}", trailer.metadata);
                break;
            }
            Some(result_output_item::Item::FinalCollected(_)) => {
                panic!("Response streaming should not produce FinalCollected");
            }
            None => {}
        }
    }

    assert!(
        data_count > 0,
        "Should receive at least one Data item from server streaming, got {}",
        data_count
    );
    assert!(got_end, "Stream should end with an End item");
    println!("Streaming response: received {} data items", data_count);

    println!("test_grpc_streaming_response passed");
    Ok(())
}

/// Test gRPC server streaming without reflection (raw protobuf bytes)
/// Empty protobuf message = empty bytes
#[ignore = "Requires running gRPC server at localhost:9000 with streaming endpoint"]
#[tokio::test]
async fn test_grpc_streaming_via_run_without_reflection() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Empty protobuf message = empty bytes
    let job = create_grpc_job(
        "jobworkerp.service.RunnerService/FindList",
        Some(grpc_args::Request::Body(Vec::new())),
        false,
        10000,
        Some("streaming"),
    );

    let mut streaming_job_data = job.data.clone().unwrap();
    streaming_job_data.streaming_type = StreamingType::Internal as i32;
    let streaming_job = Job {
        data: Some(streaming_job_data),
        ..job
    };

    let (worker_data, runner_data) = create_test_data(false);
    let worker_id = WorkerId { value: 1 };

    let (_, stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, streaming_job)
        .await;

    let stream = stream.expect("Internal streaming should return a stream");
    let mut stream = stream;
    let mut data_count = 0;
    let mut got_final_collected = false;

    while let Some(item) = stream.next().await {
        match item.item {
            Some(result_output_item::Item::Data(data)) => {
                data_count += 1;
                assert!(
                    !data.is_empty(),
                    "Data item {} should not be empty",
                    data_count
                );
            }
            Some(result_output_item::Item::End(_)) => {
                break;
            }
            Some(result_output_item::Item::FinalCollected(collected)) => {
                let streaming_result: GrpcStreamingResult =
                    ProstMessageCodec::deserialize_message(&collected)?;
                assert_eq!(streaming_result.code, tonic::Code::Ok as i32);
                match &streaming_result.response_data {
                    Some(grpc_streaming_result::ResponseData::Bodies(b)) => {
                        assert!(!b.items.is_empty(), "Should have collected bodies");
                    }
                    Some(grpc_streaming_result::ResponseData::JsonBody(_)) => {
                        panic!("json_body should not be set without reflection");
                    }
                    None => panic!("response_data should not be None"),
                }
                got_final_collected = true;
                break;
            }
            None => {}
        }
    }

    assert!(
        data_count > 0 || got_final_collected,
        "Should receive Data items or FinalCollected"
    );

    println!("test_grpc_streaming_via_run_without_reflection passed");
    Ok(())
}

/// Test gRPC response streaming without reflection (raw protobuf bytes)
#[ignore = "Requires running gRPC server at localhost:9000 with streaming endpoint"]
#[tokio::test]
async fn test_grpc_streaming_response_without_reflection() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    let job = create_grpc_job(
        "jobworkerp.service.RunnerService/FindList",
        Some(grpc_args::Request::Body(Vec::new())),
        false,
        10000,
        Some("streaming"),
    );

    let mut streaming_job_data = job.data.clone().unwrap();
    streaming_job_data.streaming_type = StreamingType::Response as i32;
    let streaming_job = Job {
        data: Some(streaming_job_data),
        ..job
    };

    let (worker_data, runner_data) = create_test_data(false);
    let worker_id = WorkerId { value: 1 };

    let (_, stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, streaming_job)
        .await;

    let stream = stream.expect("Response streaming should return a stream");
    let mut stream = stream;
    let mut data_count = 0;
    let mut got_end = false;

    while let Some(item) = stream.next().await {
        match item.item {
            Some(result_output_item::Item::Data(data)) => {
                data_count += 1;
                assert!(
                    !data.is_empty(),
                    "Stream data item {} should not be empty",
                    data_count
                );
            }
            Some(result_output_item::Item::End(_)) => {
                got_end = true;
                break;
            }
            Some(result_output_item::Item::FinalCollected(_)) => {
                panic!("Response streaming should not produce FinalCollected");
            }
            None => {}
        }
    }

    assert!(
        data_count > 0,
        "Should receive at least one Data item, got {}",
        data_count
    );
    assert!(got_end, "Stream should end with an End item");

    println!(
        "test_grpc_streaming_response_without_reflection passed: {} data items",
        data_count
    );
    Ok(())
}

/// Test gRPC error handling with non-existent service
#[ignore = "Requires running gRPC server at localhost:9000"]
#[tokio::test]
async fn test_grpc_error_handling() -> Result<()> {
    command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let job_runner = get_real_job_runner().await;

    let job = create_grpc_job(
        "nonexistent.Service/Method",
        Some(grpc_args::Request::Body(Vec::new())),
        false,
        5000,
        Some("unary"),
    );
    let (worker_data, runner_data) = create_test_data(false);
    let worker_id = WorkerId { value: 1 };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    assert!(result.data.is_some());
    let data = result.data.unwrap();
    // The runner itself succeeds (returns GrpcUnaryResult with error code)
    assert_eq!(data.status, ResultStatus::Success as i32);

    let output = data.output.unwrap();
    let grpc_result: GrpcUnaryResult = ProstMessageCodec::deserialize_message(&output.items)?;

    // gRPC should return Unimplemented or NotFound
    assert_ne!(grpc_result.code, tonic::Code::Ok as i32);
    println!(
        "Error handling: code={}, message={:?}",
        grpc_result.code, grpc_result.message
    );

    println!("test_grpc_error_handling passed");
    Ok(())
}

fn create_grpc_settings_with_defaults(
    use_reflection: bool,
    method: Option<&str>,
    timeout: Option<i64>,
    as_json: Option<bool>,
) -> Vec<u8> {
    let settings = GrpcRunnerSettings {
        host: format!("http://{GRPC_HOST}"),
        port: GRPC_PORT,
        tls: false,
        timeout_ms: Some(10000),
        max_message_size: None,
        auth_token: None,
        tls_config: None,
        use_reflection: Some(use_reflection),
        method: method.map(|s| s.to_string()),
        metadata: HashMap::new(),
        timeout,
        as_json,
    };
    ProstMessageCodec::serialize_message(&settings).unwrap()
}

/// Test gRPC unary call with settings defaults (method/timeout/as_json set in settings, omitted in args)
#[ignore = "Requires running gRPC server at localhost:9000 with reflection enabled"]
#[tokio::test]
async fn test_grpc_unary_with_settings_defaults() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    let settings_bytes = create_grpc_settings_with_defaults(
        true,
        Some("jobworkerp.service.RunnerService/Find"),
        Some(10000),
        Some(true),
    );

    let worker_data = WorkerData {
        name: "grpc_settings_defaults_test".to_string(),
        runner_settings: settings_bytes,
        retry_policy: None,
        channel: Some("test".to_string()),
        response_type: ResponseType::NoResult as i32,
        store_success: false,
        store_failure: false,
        use_static: false,
        ..Default::default()
    };
    let runner_data = RunnerData {
        name: RunnerType::Grpc.as_str_name().to_string(),
        ..Default::default()
    };
    let worker_id = WorkerId { value: 1 };

    // Args: method/timeout/as_json are all omitted — settings defaults should apply
    let grpc_args = GrpcArgs {
        method: None,
        request: Some(grpc_args::Request::JsonBody(
            r#"{"value": "1"}"#.to_string(),
        )),
        metadata: HashMap::new(),
        timeout: None,
        as_json: None,
    };
    let args_bytes = ProstMessageCodec::serialize_message(&grpc_args).unwrap();

    let job = Job {
        id: Some(JobId { value: 1 }),
        data: Some(JobData {
            worker_id: Some(WorkerId { value: 1 }),
            args: args_bytes,
            uniq_key: Some("grpc_settings_defaults_test".to_string()),
            retried: 0,
            priority: 0,
            timeout: 10000,
            enqueue_time: command_utils::util::datetime::now_millis(),
            run_after_time: command_utils::util::datetime::now_millis(),
            grabbed_until_time: None,
            streaming_type: 0,
            using: Some("unary".to_string()),
        }),
        ..Default::default()
    };

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    assert!(result.data.is_some());
    let data = result.data.unwrap();
    assert_eq!(data.status, ResultStatus::Success as i32);

    let output = data.output.unwrap();
    let grpc_result: GrpcUnaryResult = ProstMessageCodec::deserialize_message(&output.items)?;

    assert_eq!(grpc_result.code, tonic::Code::Ok as i32);
    // as_json=true from settings, so response should be JSON
    let json_body = match &grpc_result.response_data {
        Some(grpc_unary_result::ResponseData::JsonBody(j)) => j.clone(),
        other => panic!(
            "Expected JsonBody variant when as_json=true via settings, got {:?}",
            other
        ),
    };
    assert!(!json_body.is_empty());

    println!("grpc unary with settings defaults: json_body={}", json_body);
    println!("test_grpc_unary_with_settings_defaults passed");
    Ok(())
}

/// Test gRPC connection failure returns response_data=None and code=Internal
#[tokio::test]
async fn test_grpc_connection_failure() -> Result<()> {
    let job_runner = get_real_job_runner().await;

    // Use a port where no server is listening
    let settings = GrpcRunnerSettings {
        host: "http://localhost".to_string(),
        port: 19999,
        tls: false,
        timeout_ms: Some(2000),
        max_message_size: None,
        auth_token: None,
        tls_config: None,
        use_reflection: Some(false),
        method: None,
        metadata: HashMap::new(),
        timeout: None,
        as_json: None,
    };
    let settings_bytes = ProstMessageCodec::serialize_message(&settings)?;

    let worker_data = WorkerData {
        name: "grpc_connection_failure_test".to_string(),
        runner_settings: settings_bytes,
        retry_policy: None,
        channel: Some("test".to_string()),
        response_type: ResponseType::NoResult as i32,
        store_success: false,
        store_failure: false,
        use_static: false,
        ..Default::default()
    };
    let runner_data = RunnerData {
        name: RunnerType::Grpc.as_str_name().to_string(),
        ..Default::default()
    };
    let worker_id = WorkerId { value: 999 };

    let job = create_grpc_job(
        "some.Service/Method",
        Some(grpc_args::Request::Body(Vec::new())),
        false,
        2000,
        Some("unary"),
    );

    let (result, _stream) = job_runner
        .run_job(&runner_data, &worker_id, &worker_data, job)
        .await;

    assert!(result.data.is_some());
    let data = result.data.unwrap();
    // Connection failure causes a fatal error at the runner load stage
    assert_eq!(
        data.status,
        ResultStatus::FatalError as i32,
        "Connection failure should result in FatalError"
    );

    let output = data.output.unwrap();
    let error_message = String::from_utf8_lossy(&output.items);
    assert!(
        !error_message.is_empty(),
        "Error output should contain a message"
    );

    println!(
        "Connection failure: status=FatalError, error={}",
        error_message
    );
    println!("test_grpc_connection_failure passed");
    Ok(())
}
