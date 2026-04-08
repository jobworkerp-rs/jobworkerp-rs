//! E2E tests for EnqueueWithClientStream.
//!
//! These tests exercise the full dispatcher pipeline with hello_runner's
//! "feed_hello" method, verifying that client streaming feed data flows
//! through RedisFeedPublisher (Scalable) → feed_bridge → runner → streaming output.
//!
//! All sub-tests share a single `start_test_worker` call to avoid
//! accumulating leaked dispatchers (Box::leak in start_test_worker),
//! which would exhaust the shared DB connection pool.
//! Must run with --test-threads=1.
//! Requires Redis (uses create_hybrid_test_app / Scalable mode).

use anyhow::Result;
use futures::StreamExt;
use proto::jobworkerp::data::result_output_item;
use proto::jobworkerp::data::{ResponseType, StreamingType, WorkerData};
use std::collections::HashMap;
use std::sync::Arc;
use tests_with_worker::start_test_worker;
use tokio::time::Duration;

/// Encode a string as HelloArgs protobuf (field 1, string).
fn encode_hello_args(s: &str) -> Vec<u8> {
    use prost::encoding::{WireType, encode_key, encode_varint};
    let bytes = s.as_bytes();
    let mut buf = Vec::new();
    encode_key(1, WireType::LengthDelimited, &mut buf);
    encode_varint(bytes.len() as u64, &mut buf);
    buf.extend_from_slice(bytes);
    buf
}

/// Decode HelloRunnerResult protobuf (field 1, string) to String.
fn decode_hello_result(data: &[u8]) -> String {
    use prost::encoding::{WireType, decode_key, decode_varint};
    let mut buf = data;
    let key = decode_key(&mut buf);
    if !matches!(key, Ok((1, WireType::LengthDelimited))) {
        return String::new();
    }
    if let Ok(len) = decode_varint(&mut buf) {
        let len = len as usize;
        if buf.len() >= len {
            return String::from_utf8_lossy(&buf[..len]).to_string();
        }
    }
    String::new()
}

/// Ensure the HelloPlugin runner exists in DB and return its ID.
///
/// In test setup, plugins are loaded into memory by setup_test_*_module
/// but not inserted into DB. We insert a runner row directly using
/// INSERT OR IGNORE, then find by ID (bypassing name cache).
async fn find_or_create_hello_runner_id(
    app_module: &app::module::AppModule,
) -> Result<proto::jobworkerp::data::RunnerId> {
    let plugin_path = find_hello_plugin_path()?;
    let rdb_module = app_module
        .repositories
        .rdb_module
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No RDB module available"))?;

    // Generate a stable ID for the runner
    let runner_id = app_module.job_app.generate_job_id()?.value;

    use infra::infra::runner::rdb::RunnerRepository;
    let runner_row = infra::infra::runner::rows::RunnerRow {
        id: runner_id,
        name: "HelloPlugin".to_string(),
        description: "Hello plugin for E2E test".to_string(),
        definition: plugin_path,
        r#type: proto::jobworkerp::data::RunnerType::Plugin as i32,
        created_at: command_utils::util::datetime::now_millis(),
    };
    // INSERT OR IGNORE: succeeds if new, silently skips if name already exists
    let _ = rdb_module.runner_repository.create(&runner_row).await;

    // Find by direct DB query (bypasses moka name cache)
    let runner = rdb_module
        .runner_repository
        .find_by_name("HelloPlugin")
        .await?
        .ok_or_else(|| anyhow::anyhow!("HelloPlugin runner not found after insert"))?;
    runner
        .id
        .ok_or_else(|| anyhow::anyhow!("HelloPlugin runner has no ID"))
}

/// Find the hello_runner dylib path from TEST_PLUGIN_DIR locations.
fn find_hello_plugin_path() -> Result<String> {
    let ext = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    };

    let dirs = app::module::test::TEST_PLUGIN_DIR;
    for dir in dirs.split(',') {
        let path = std::path::Path::new(dir).join(format!("libplugin_runner_hello.{}", ext));
        if path.exists() {
            return Ok(path.to_string_lossy().to_string());
        }
    }
    Err(anyhow::anyhow!(
        "HelloPlugin dylib not found in TEST_PLUGIN_DIR: {}",
        dirs
    ))
}

/// Load HelloPlugin into the AppModule's runner_factory plugins.
///
/// `create_hybrid_test_app` creates an empty `Plugins` instance for
/// `config_module.runner_factory`, so plugin runners are not available
/// to the worker dispatcher. This helper loads them explicitly.
async fn load_hello_plugin(app_module: &app::module::AppModule) {
    let plugins = &app_module.config_module.runner_factory.plugins;
    plugins
        .load_plugin_files(app::module::test::TEST_PLUGIN_DIR)
        .await;
}

/// Create a worker configured for feed_hello method.
async fn create_feed_hello_worker(
    app_module: &app::module::AppModule,
    runner_id: proto::jobworkerp::data::RunnerId,
) -> Result<proto::jobworkerp::data::WorkerId> {
    let worker_data = WorkerData {
        name: format!(
            "feed_hello_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ),
        runner_id: Some(runner_id),
        runner_settings: Vec::new(),
        use_static: true,
        response_type: ResponseType::Direct as i32,
        store_success: false,
        store_failure: false,
        ..Default::default()
    };
    app_module.worker_app.create(&worker_data).await
}

// ---------------------------------------------------------------------------
// Sub-test functions called from the single entry-point test below.
// Each is a standalone async fn receiving shared AppModule & runner/worker IDs.
// ---------------------------------------------------------------------------

/// Normal flow: enqueue with reserved_job_id + feed via Redis List → verify streaming output.
async fn sub_test_normal_flow(
    app_module: &Arc<app::module::AppModule>,
    runner_id: &proto::jobworkerp::data::RunnerId,
) -> Result<()> {
    let worker_id = create_feed_hello_worker(app_module, *runner_id).await?;

    // Pre-allocate job ID
    let job_id = app_module.job_app.generate_job_id()?;

    let feed_pub = app_module.feed_publisher.clone();
    let feed_job_id = job_id;

    let enqueue_handle = {
        let app = app_module.clone();
        let wid = worker_id;
        let jid = job_id;
        async move {
            app.job_app
                .enqueue_job(
                    Arc::new(HashMap::new()),
                    Some(&wid),
                    None,
                    encode_hello_args("World"),
                    None,
                    0,
                    0,
                    30000, // 30s timeout
                    Some(jid),
                    StreamingType::Response,
                    Some("feed_hello".to_string()),
                    None, // overrides
                )
                .await
        }
    };

    let feed_handle = async move {
        // Brief delay to let enqueue proceed and dispatcher start processing.
        // Redis List buffering ensures no data loss even if we publish before
        // the feed bridge starts BLPOP.
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Send feed chunks
        feed_pub
            .publish_feed(&feed_job_id, encode_hello_args("chunk1"), false)
            .await?;

        tokio::time::sleep(Duration::from_millis(50)).await;

        feed_pub
            .publish_feed(&feed_job_id, encode_hello_args("chunk2"), false)
            .await?;

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Send final
        feed_pub
            .publish_feed(&feed_job_id, Vec::new(), true)
            .await?;

        Ok::<(), anyhow::Error>(())
    };

    let (enqueue_result, feed_result) = tokio::join!(enqueue_handle, feed_handle);
    feed_result?;
    let (_job_id, _job_result, stream) = enqueue_result?;

    let stream = stream.expect("Direct + Response streaming should return a stream");

    // Collect all stream items with timeout
    let items: Vec<_> = tokio::time::timeout(Duration::from_secs(30), stream.collect())
        .await
        .expect("Stream collection timed out");

    // Extract text from Data items
    let mut collected_text = String::new();
    for item in &items {
        if let Some(result_output_item::Item::Data(data)) = &item.item {
            collected_text.push_str(&decode_hello_result(data));
        }
    }

    tracing::info!("sub_test_normal_flow: collected text: {}", collected_text);

    assert!(
        collected_text.contains("Hello World! "),
        "Output should contain greeting. Got: {}",
        collected_text
    );
    assert!(
        collected_text.contains("chunk1"),
        "Output should contain fed data 'chunk1'. Got: {}",
        collected_text
    );
    assert!(
        collected_text.contains("chunk2"),
        "Output should contain fed data 'chunk2'. Got: {}",
        collected_text
    );

    // Verify End trailer exists
    let end_count = items
        .iter()
        .filter(|i| matches!(&i.item, Some(result_output_item::Item::End(_))))
        .count();
    assert_eq!(end_count, 1, "Should have exactly 1 End trailer");

    Ok(())
}

/// Empty feed: enqueue + immediate final → verify greeting only.
async fn sub_test_empty_feed(
    app_module: &Arc<app::module::AppModule>,
    runner_id: &proto::jobworkerp::data::RunnerId,
) -> Result<()> {
    let worker_id = create_feed_hello_worker(app_module, *runner_id).await?;

    let job_id = app_module.job_app.generate_job_id()?;

    let feed_pub = app_module.feed_publisher.clone();
    let feed_job_id = job_id;

    let enqueue_handle = {
        let app = app_module.clone();
        let wid = worker_id;
        let jid = job_id;
        async move {
            app.job_app
                .enqueue_job(
                    Arc::new(HashMap::new()),
                    Some(&wid),
                    None,
                    encode_hello_args("World"),
                    None,
                    0,
                    0,
                    30000,
                    Some(jid),
                    StreamingType::Response,
                    Some("feed_hello".to_string()),
                    None, // overrides
                )
                .await
        }
    };

    let feed_handle = async move {
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Send final immediately (no data chunks)
        feed_pub
            .publish_feed(&feed_job_id, Vec::new(), true)
            .await?;

        Ok::<(), anyhow::Error>(())
    };

    let (enqueue_result, feed_result) = tokio::join!(enqueue_handle, feed_handle);
    feed_result?;
    let (_job_id, _job_result, stream) = enqueue_result?;

    let stream = stream.expect("Direct + Response streaming should return a stream");

    let items: Vec<_> = tokio::time::timeout(Duration::from_secs(30), stream.collect())
        .await
        .expect("Stream collection timed out");

    let mut collected_text = String::new();
    for item in &items {
        if let Some(result_output_item::Item::Data(data)) = &item.item {
            collected_text.push_str(&decode_hello_result(data));
        }
    }

    tracing::info!("sub_test_empty_feed: collected text: {}", collected_text);

    assert!(
        collected_text.contains("Hello World! "),
        "Output should contain greeting. Got: {}",
        collected_text
    );

    let end_count = items
        .iter()
        .filter(|i| matches!(&i.item, Some(result_output_item::Item::End(_))))
        .count();
    assert_eq!(end_count, 1, "Should have exactly 1 End trailer");

    Ok(())
}

/// Error case: check_worker_streaming rejects non-client-streaming method
/// when require_client_stream=true is requested.
async fn sub_test_require_check_error(
    app_module: &Arc<app::module::AppModule>,
    runner_id: &proto::jobworkerp::data::RunnerId,
) -> Result<()> {
    let worker_id = create_feed_hello_worker(app_module, *runner_id).await?;

    // "run" method does not support client streaming
    let result = app_module
        .worker_app
        .check_worker_streaming(&worker_id, true, Some(true), Some("run"))
        .await;

    assert!(
        result.is_err(),
        "check_worker_streaming should reject 'run' method with require_client_stream=true"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("client streaming"),
        "Error should mention client streaming. Got: {}",
        err_msg
    );

    // Reverse guard: "feed_hello" requires client streaming, so non-client-stream call should fail
    let result2 = app_module
        .worker_app
        .check_worker_streaming(&worker_id, true, Some(false), Some("feed_hello"))
        .await;

    assert!(
        result2.is_err(),
        "check_worker_streaming should reject 'feed_hello' when require_client_stream=false"
    );
    let err_msg2 = result2.unwrap_err().to_string();
    assert!(
        err_msg2.contains("EnqueueWithClientStream"),
        "Error should mention EnqueueWithClientStream. Got: {}",
        err_msg2
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Single test entry point — starts one worker, runs all sub-tests, shuts down.
// ---------------------------------------------------------------------------

/// Unified E2E test that shares a single start_test_worker invocation
/// across all client-stream sub-tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_client_stream_e2e() -> Result<()> {
    // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);

    let app_module = Arc::new(app::module::test::create_hybrid_test_app().await?);
    load_hello_plugin(&app_module).await;
    let worker_handle = start_test_worker(app_module.clone()).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let runner_id = find_or_create_hello_runner_id(&app_module).await?;

    // Run all sub-tests sequentially, sharing the same worker.
    sub_test_normal_flow(&app_module, &runner_id).await?;
    sub_test_empty_feed(&app_module, &runner_id).await?;
    sub_test_require_check_error(&app_module, &runner_id).await?;

    worker_handle.shutdown().await;
    Ok(())
}
