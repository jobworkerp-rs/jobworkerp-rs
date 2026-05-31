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

/// run_stream cancellation: once chunks start flowing, firing the
/// CancellationToken (the same path the JobService.Delete pubsub listener
/// uses) must:
///   1. stop the host-side streaming loop promptly,
///   2. abort the spawned plugin-driving task so the plugin's FfiFuture is
///      dropped (`plugin_fut.abort()` in `run_stream_v2`),
///   3. still emit an `End` trailer so downstream gRPC consumers see a
///      well-formed termination instead of a closed stream — and the
///      trailer must carry the fallback (input) metadata because the
///      cancelled plugin never produced its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v2_plugin_run_stream_is_cancelled_by_token_and_emits_end_trailer() -> Result<()> {
    use futures::StreamExt;
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

    let token = CancellationToken::new();
    wrapper.set_cancel_helper(make_helper(token.clone()));
    let setup = wrapper
        .setup_cancellation_monitoring(JobId { value: 1 }, &JobData::default())
        .await?;
    assert!(setup.is_none(), "setup must not short-circuit");

    // sleep:5000 → 50 chunks at 100 ms each — far longer than the test
    // window so cancellation must be what terminates the stream.
    let args = b"sleep:5000".to_vec();
    let meta = HashMap::from([("trace-id".to_string(), "stream-cancel".to_string())]);
    let mut stream = wrapper
        .run_stream(&args, meta.clone(), None)
        .await
        .expect("run_stream must start");

    // Let the stream emit a few chunks so we exercise the in-flight
    // cancellation path (not the pre-stream finalize fast path).
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    while chunks.len() < 2 {
        match stream.next().await {
            Some(item) => match item.item {
                Some(result_output_item::Item::Data(d)) => chunks.push(d),
                Some(result_output_item::Item::End(_)) => {
                    panic!("stream ended before cancellation could be observed");
                }
                _ => {}
            },
            None => panic!("stream closed before any chunk arrived"),
        }
    }

    let start = Instant::now();
    token.cancel();

    // Drain the rest of the stream. The plugin may have one or two more
    // chunks queued before the cancel is observed; we only require the
    // stream to finish promptly with an End trailer.
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
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(1500),
        "run_stream cancel must terminate promptly, got {:?}",
        elapsed
    );
    assert!(
        chunks.len() < 50,
        "stream should be cut short by cancel, got {} chunks (full run = 50)",
        chunks.len()
    );
    // run_stream_v2 substitutes the input metadata on the cancel path
    // (the plugin task was aborted before it could return its own).
    assert_eq!(
        end_metadata.as_ref(),
        Some(&meta),
        "cancelled run_stream must still emit an End trailer with the fallback (input) metadata"
    );
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

/// Pre-stream Err surfaces either as an outer `Err(...)` (when the host's
/// eager peek catches it) or in the End trailer metadata under
/// `V2_STREAM_ERROR_META_KEY` — never silently as a Success without any
/// error indicator. Whichever path fires, the original message must reach
/// the consumer so the job can be marked failed downstream.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_plugin_pre_stream_error_is_observable_either_way() -> Result<()> {
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

    let args = b"err:eager-or-metadata".to_vec();
    match wrapper.run_stream(&args, HashMap::new(), None).await {
        Err(e) => {
            // Eager-Err path: outer Err must mention the plugin's message.
            assert!(
                e.to_string().contains("eager-or-metadata"),
                "outer Err must include the plugin error, got {e}",
            );
        }
        Ok(mut stream) => {
            // Metadata path: drain to the End trailer and verify the key.
            let mut end_metadata: Option<HashMap<String, String>> = None;
            while let Some(item) = stream.next().await {
                if let Some(result_output_item::Item::End(t)) = item.item {
                    end_metadata = Some(t.metadata);
                    break;
                }
            }
            let meta = end_metadata.expect("End trailer must be present");
            let err_msg = meta
                .get("jobworkerp.plugin.run_stream_error")
                .expect("error must be recorded under V2_STREAM_ERROR_META_KEY");
            assert!(
                err_msg.contains("eager-or-metadata"),
                "End trailer metadata must include the plugin error, got {err_msg}",
            );
        }
    }
    Ok(())
}

/// Minor-1 ABI smoke test: when the plugin implements
/// `setup_client_stream_channel_v2`, the host forwarder uses the new sink
/// so the plugin receives `(Vec<u8>, bool)` chunks and can return from
/// `run_stream` as soon as it observes `is_final == true`. The cancel_test
/// plugin's `feed_v2` branch echoes every chunk back appended with a
/// `1` / `0` byte; the assertions below cover both the in-band flag
/// delivery and the clean termination semantics.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v2_plugin_setup_client_stream_channel_v2_delivers_is_final() -> Result<()> {
    use futures::StreamExt;
    use jobworkerp_runner::runner::{FeedData, RunnerTrait};
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

    // Acquire the feed sender first so the run_stream task below can
    // receive chunks. `setup_client_stream_channel` returns the host-side
    // tx that bridges into the plugin's `OutputSinkWithFinal`.
    let feed_tx = wrapper
        .setup_client_stream_channel(Some("feed_v2"))
        .expect("feed_v2 sink must be available");

    // Spawn run_stream and start draining the output. The plugin echoes
    // each (data, is_final) chunk with a trailing 0/1 byte so we can
    // verify the in-band flag travelled all the way through.
    let mut wrapper_run = wrapper.clone();
    let run_handle = tokio::spawn(async move {
        wrapper_run
            .run_stream(b"", HashMap::new(), Some("feed_v2"))
            .await
    });

    // Feed two non-final chunks then a final one. The plugin's run_stream
    // must return as soon as the final chunk arrives, without depending
    // on us dropping the feed_tx.
    feed_tx
        .send(FeedData {
            data: b"a".to_vec(),
            is_final: false,
        })
        .await
        .expect("send first chunk");
    feed_tx
        .send(FeedData {
            data: b"b".to_vec(),
            is_final: false,
        })
        .await
        .expect("send second chunk");
    feed_tx
        .send(FeedData {
            data: b"c".to_vec(),
            is_final: true,
        })
        .await
        .expect("send final chunk");

    let mut stream = run_handle
        .await
        .expect("run task did not panic")
        .expect("run_stream must succeed");

    let mut chunks: Vec<Vec<u8>> = Vec::new();
    while let Some(item) = stream.next().await {
        match item.item {
            Some(result_output_item::Item::Data(d)) => chunks.push(d),
            Some(result_output_item::Item::End(_)) => break,
            _ => {}
        }
    }

    assert_eq!(
        chunks,
        vec![
            // (b"a", false) → "a" + 0x00
            b"a\x00".to_vec(),
            // (b"b", false) → "b" + 0x00
            b"b\x00".to_vec(),
            // (b"c", true)  → "c" + 0x01 — plugin returns immediately after.
            b"c\x01".to_vec(),
        ],
        "plugin must observe each (data, is_final) chunk in order and \
         echo them back, and terminate on is_final=true without us \
         dropping feed_tx",
    );

    // We still hold `feed_tx`. If we drop it now the channel closes
    // cleanly, but the plugin already returned without needing that.
    drop(feed_tx);
    Ok(())
}

/// `run_stream()` must return its `BoxStream` to the caller **without waiting
/// for the first output chunk**. Client-streaming callers (e.g.
/// `EnqueueWithClientStream`) only start sending feed data once they have
/// received the gRPC response containing the result stream — if the host
/// blocked here until the plugin produced its first chunk, plugins that
/// consume feed before emitting anything (whisper-runner with empty `args`,
/// feed-driven ASR) would deadlock against the client.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_plugin_run_stream_returns_before_first_chunk() -> Result<()> {
    use jobworkerp_runner::runner::RunnerTrait;

    let plugins = Plugins::new();
    plugins.load_plugin_files(TEST_PLUGIN_DIR).await;
    let loader = plugins.runner_plugins();
    let guard = loader.read().await;
    let mut wrapper = guard
        .find_plugin_runner_by_name("CancelTest")
        .await
        .expect("CancelTest must be findable");
    wrapper.load(vec![]).await?;

    // sleep:1000 → first chunk emitted ~100 ms after the plugin starts.
    // The stream returned by `run_stream` must arrive promptly regardless;
    // the host's job is to hand the stream back so the caller can begin
    // forwarding feed data.
    let args = b"sleep:1000".to_vec();
    let start = Instant::now();
    let _stream = wrapper
        .run_stream(&args, HashMap::new(), None)
        .await
        .expect("run_stream must succeed");
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(50),
        "run_stream must return its BoxStream before the first chunk is emitted, got {:?}",
        elapsed
    );
    Ok(())
}
