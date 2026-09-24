use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use super::FeedData;

pub(super) struct FeedTaskGuard(JoinHandle<()>);

impl FeedTaskGuard {
    pub(super) fn spawn(
        receiver: mpsc::Receiver<FeedData>,
        sink: ExecInputSink,
        cancel: CancellationToken,
    ) -> Self {
        Self(tokio::spawn(forward_client_feed(receiver, sink, cancel)))
    }
}

impl Drop for FeedTaskGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[async_trait]
pub(super) trait ClientInputSink: Send {
    async fn write(&mut self, bytes: &[u8]) -> Result<()>;
    async fn close(&mut self) -> Result<()>;
}

pub(super) struct ExecInputSink(pub(super) microsandbox::sandbox::exec::ExecSink);

#[async_trait]
impl ClientInputSink for ExecInputSink {
    async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.0
            .write(bytes)
            .await
            .context("failed to write guest stdin")
    }

    async fn close(&mut self) -> Result<()> {
        self.0.close().await.context("failed to close guest stdin")
    }
}

pub(super) async fn forward_client_feed<S: ClientInputSink>(
    mut receiver: mpsc::Receiver<FeedData>,
    mut sink: S,
    cancel: CancellationToken,
) {
    loop {
        let feed = tokio::select! {
            _ = cancel.cancelled() => break,
            feed = receiver.recv() => feed,
        };
        let Some(feed) = feed else {
            if let Err(error) = sink.close().await {
                tracing::debug!(%error, "closing sandbox stdin after client feed ended failed");
            }
            break;
        };
        if let Err(error) = sink.write(&feed.data).await {
            tracing::warn!(%error, "sandbox stdin write failed; continuing to consume client feed");
        }
        if feed.is_final {
            // The SDK close frame alone does not end a guest PTY read. EOT
            // flushes an unterminated canonical line, then ends the next read.
            if let Err(error) = sink.write(b"\x04\x04").await {
                tracing::debug!(%error, "sending guest PTY EOF failed");
            }
            if let Err(error) = sink.close().await {
                tracing::debug!(%error, "closing sandbox stdin after final client feed failed");
            }
            break;
        }
    }
}
