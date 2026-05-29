//! Micro-benchmark comparing the OutputSink-based V2 send path to a
//! raw `tokio::mpsc::Sender`. The goal is to confirm that the
//! `Box<dyn Future>` allocation introduced by `OutputSink::send_raw` does
//! not regress the streaming hot path beyond a tolerable factor.
//!
//! Run with: `cargo run --release --bin sink_throughput`
//!
//! The bench is wired as a `bin` target rather than a criterion bench so
//! it can be executed without adding a heavyweight dependency. The
//! acceptance criterion is documented inline: if the new path is more
//! than 1.5x slower than the baseline we should switch to a `try_send` +
//! `wait_writable` design in a follow-up PR.

use jobworkerp_runner::runner::plugins::ffi::OutputSink;
use std::time::Instant;
use tokio::sync::mpsc;

const CHUNK_COUNT: usize = 10_000;
const CHUNK_SIZE: usize = 10;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let baseline = baseline_tokio_send().await;
    let new_path = output_sink_send().await;

    println!("baseline tokio::mpsc::Sender::send : {baseline:?}");
    println!("OutputSink::send_raw                : {new_path:?}");

    let ratio = new_path.as_secs_f64() / baseline.as_secs_f64();
    println!("ratio new / baseline = {ratio:.3}");

    if ratio > 1.5 {
        println!(
            "WARNING: OutputSink::send_raw is {ratio:.3}x slower than the \
             tokio baseline; consider switching to try_send + wait_writable."
        );
        std::process::exit(2);
    }
    println!("PASS: ratio within budget (<= 1.5x).");
}

async fn baseline_tokio_send() -> std::time::Duration {
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });

    let chunk: Vec<u8> = vec![0u8; CHUNK_SIZE];
    let start = Instant::now();
    for _ in 0..CHUNK_COUNT {
        tx.send(chunk.clone()).await.expect("baseline send");
    }
    drop(tx);
    drain.await.expect("drain task");
    start.elapsed()
}

async fn output_sink_send() -> std::time::Duration {
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });

    let sink = OutputSink::from_sender(tx);
    let chunk: Vec<u8> = vec![0u8; CHUNK_SIZE];
    let start = Instant::now();
    for _ in 0..CHUNK_COUNT {
        let res = sink.send_raw(chunk.clone()).await;
        if let jobworkerp_runner::runner::plugins::ffi::FfiResult::Err(_) = res {
            panic!("OutputSink::send_raw failed");
        }
    }
    drop(sink);
    drain.await.expect("drain task");
    start.elapsed()
}
