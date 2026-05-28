//! V2 plugin (cooperative cancellation) end-to-end tests.
//!
//! Uses the `plugin_runner_cancel_test` dylib, which must be built before
//! running these tests (cargo test typically picks it up when the package is
//! a workspace member, but if a `.so` is missing the loader skips it).

use anyhow::Result;
use jobworkerp_runner::runner::cancellation::CancelMonitoring;
use jobworkerp_runner::runner::cancellation_helper::{
    CancelMonitoringHelper, UseCancelMonitoringHelper,
};
use jobworkerp_runner::runner::plugins::Plugins;
use jobworkerp_runner::runner::test_common::mock::MockCancellationManager;
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use proto::jobworkerp::data::{JobData, JobId, JobResult};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const TEST_PLUGIN_DIR: &str = "./target/debug,../target/debug,../target/release,./target/release";

fn make_helper(token: CancellationToken) -> CancelMonitoringHelper {
    CancelMonitoringHelper::new(Box::new(MockCancellationManager::new_with_token(token)))
}

#[tokio::test]
async fn v2_plugin_is_recognized_as_multi_method_v2() -> Result<()> {
    let plugins = Plugins::new();
    let loaded = plugins.load_plugin_files(TEST_PLUGIN_DIR).await;
    let v2 = loaded
        .iter()
        .find(|p| p.name == "CancelTest")
        .expect("CancelTest plugin (V2) should be loaded");
    assert_eq!(v2.name, "CancelTest");

    let loader = plugins.runner_plugins();
    let guard = loader.read().await;
    let wrapper = guard
        .find_plugin_runner_by_name("CancelTest")
        .await
        .expect("CancelTest must be findable");
    // Sanity: a v2 plugin must self-report as a plugin runner (its name matches).
    assert_eq!(wrapper.name(), "CancelTest");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_plugin_run_returns_completed_when_not_cancelled() -> Result<()> {
    let plugins = Plugins::new();
    plugins.load_plugin_files(TEST_PLUGIN_DIR).await;
    let loader = plugins.runner_plugins();
    let guard = loader.read().await;
    let mut wrapper = guard
        .find_plugin_runner_by_name("CancelTest")
        .await
        .expect("CancelTest must be findable");
    wrapper.load(vec![]).await?;

    // Short sleep so the test is quick; no cancellation.
    let args = b"sleep:100".to_vec();
    let (r, _meta) = wrapper.run(&args, HashMap::new(), None).await;
    assert_eq!(r.unwrap(), b"completed".to_vec());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_plugin_run_is_cancelled_by_token() -> Result<()> {
    let plugins = Plugins::new();
    plugins.load_plugin_files(TEST_PLUGIN_DIR).await;
    let loader = plugins.runner_plugins();
    let guard = loader.read().await;
    let mut wrapper = guard
        .find_plugin_runner_by_name("CancelTest")
        .await
        .expect("CancelTest must be findable");
    wrapper.load(vec![]).await?;

    // Attach a cancel helper backed by a token we can fire from the test.
    let token = CancellationToken::new();
    wrapper.set_cancel_helper(make_helper(token.clone()));

    // setup_cancellation_monitoring must push the token into the V2 plugin
    // BEFORE run() acquires the variant write lock on a blocking thread.
    let job_id = JobId { value: 1 };
    let job_data = JobData::default();
    let setup = wrapper
        .setup_cancellation_monitoring(job_id, &job_data)
        .await?;
    assert!(setup.is_none(), "setup must not short-circuit");

    // Wrap into Arc<Mutex> so we can race run() against request_cancellation.
    let wrapper = Arc::new(Mutex::new(wrapper));

    // 5s sleep would be the no-cancel completion time; cancel must abort in under 1s.
    let args = b"sleep:5000".to_vec();
    let runner = wrapper.clone();
    let run_handle =
        tokio::spawn(async move { runner.lock().await.run(&args, HashMap::new(), None).await });

    // Let run() reach its tokio::select! await point.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let start = Instant::now();
    token.cancel();
    let (r, _meta) = run_handle.await.expect("run task must not panic");
    let elapsed = start.elapsed();

    assert!(
        r.is_err(),
        "cancelled run() must return Err, got Ok({:?})",
        r.ok()
    );
    assert!(
        elapsed < Duration::from_millis(1000),
        "cooperative cancel must abort promptly, got {:?}",
        elapsed
    );
    Ok(())
}

#[tokio::test]
async fn v2_plugin_setup_without_helper_is_noop() -> Result<()> {
    let plugins = Plugins::new();
    plugins.load_plugin_files(TEST_PLUGIN_DIR).await;
    let loader = plugins.runner_plugins();
    let guard = loader.read().await;
    let mut wrapper = guard
        .find_plugin_runner_by_name("CancelTest")
        .await
        .expect("CancelTest must be findable");
    wrapper.load(vec![]).await?;

    // No cancel_helper attached: setup must return Ok(None) without panicking
    // and the wrapper must not expose any helper.
    let res: Result<Option<JobResult>> = wrapper
        .setup_cancellation_monitoring(JobId { value: 1 }, &JobData::default())
        .await;
    assert!(res?.is_none());
    assert!(wrapper.cancel_monitoring_helper().is_none());
    Ok(())
}

/// V2 plugins are async — the wrapper awaits an `FfiFuture` rather than
/// blocking on a sync FFI call. On timeout the caller drops the future, the
/// guard drops with it, and the variant write lock is reclaimed immediately,
/// so the wrapper instance can be reused without being discarded.
#[tokio::test]
async fn v2_plugin_should_not_detach_on_timeout() -> Result<()> {
    use jobworkerp_runner::runner::RunnerSpec;

    let plugins = Plugins::new();
    plugins.load_plugin_files(TEST_PLUGIN_DIR).await;
    let loader = plugins.runner_plugins();
    let guard = loader.read().await;
    let wrapper = guard
        .find_plugin_runner_by_name("CancelTest")
        .await
        .expect("CancelTest must be findable");
    assert!(
        !wrapper.should_detach_on_timeout(),
        "V2 plugin should NOT require detach on timeout"
    );
    Ok(())
}

/// V2 run_stream emits chunks via the host-provided sender and the final
/// metadata via the returned FfiFuture. Verify the host receives chunks in
/// order and gets the End trailer with the plugin's final metadata.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_plugin_run_stream_emits_chunks_and_end_trailer() -> Result<()> {
    use futures::StreamExt;
    use jobworkerp_runner::runner::RunnerTrait;
    use proto::jobworkerp::data::result_output_item;

    let plugins = Plugins::new();
    plugins.load_plugin_files(TEST_PLUGIN_DIR).await;
    let loader = plugins.runner_plugins();
    let guard = loader.read().await;
    let mut wrapper = guard
        .find_plugin_runner_by_name("CancelTest")
        .await
        .expect("CancelTest must be findable");
    wrapper.load(vec![]).await?;

    // sleep:300 → 3 chunks at 100ms each (per plugin sample implementation).
    let args = b"sleep:300".to_vec();
    let meta = HashMap::from([("trace-id".to_string(), "abc".to_string())]);
    let mut stream = wrapper
        .run_stream(&args, meta.clone(), None)
        .await
        .expect("run_stream must succeed");

    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut end_metadata: Option<HashMap<String, String>> = None;
    while let Some(item) = stream.next().await {
        match item.item {
            Some(result_output_item::Item::Data(d)) => chunks.push(d),
            Some(result_output_item::Item::End(t)) => {
                end_metadata = Some(t.metadata);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(chunks.len(), 3, "expected 3 chunks, got {}", chunks.len());
    assert_eq!(chunks[0], b"0".to_vec());
    assert_eq!(chunks[2], b"2".to_vec());
    // Plugin returns the input metadata unchanged as the End trailer.
    assert_eq!(end_metadata.as_ref(), Some(&meta));
    Ok(())
}

/// Drop-on-timeout MUST release the variant write lock. Without that
/// behaviour, a second `run()` on the same wrapper would block until the
/// first (5s) call completed; with it, the second call only waits for the
/// plugin task to observe the dropped JoinHandle (negligible) and then
/// proceeds.
///
/// Note: the plugin-side task continues running on the plugin runtime until
/// it either reaches its natural completion or observes a cancellation
/// token. This test does NOT signal the token, so the plugin task continues
/// in the background; we only assert that the wrapper's lock is freed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v2_plugin_releases_lock_on_timeout() -> Result<()> {
    let plugins = Plugins::new();
    plugins.load_plugin_files(TEST_PLUGIN_DIR).await;
    let loader = plugins.runner_plugins();
    let guard = loader.read().await;
    let mut wrapper = guard
        .find_plugin_runner_by_name("CancelTest")
        .await
        .expect("CancelTest must be findable");
    wrapper.load(vec![]).await?;

    // Race a long-running run() against a tight host-side timeout. Dropping
    // the future must release the variant write lock immediately.
    let args = b"sleep:5000".to_vec();
    let mut w_clone = wrapper.clone();
    let first = tokio::time::timeout(
        Duration::from_millis(150),
        w_clone.run(&args, HashMap::new(), None),
    )
    .await;
    assert!(first.is_err(), "first run() must time out and be dropped");

    // If the lock had stayed held by the dropped future, this second run()
    // would block on `variant.write().await` for the full 5 seconds.
    let start = Instant::now();
    let args2 = b"sleep:50".to_vec();
    let (r, _) = wrapper.run(&args2, HashMap::new(), None).await;
    let elapsed = start.elapsed();
    assert!(
        r.is_ok(),
        "second run() must complete normally, got {:?}",
        r.err()
    );
    assert!(
        elapsed < Duration::from_millis(1500),
        "second run() blocked on lock from timed-out call: {:?}",
        elapsed
    );
    Ok(())
}
