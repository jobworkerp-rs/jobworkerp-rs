use anyhow::{Context, Result};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use super::FeedData;

pub(super) struct FeedTaskGuard(JoinHandle<()>);

impl FeedTaskGuard {
    pub(super) fn spawn(
        receiver: mpsc::Receiver<FeedData>,
        sink: ExecInputSink,
        control: microsandbox::ExecControl,
        cancel: CancellationToken,
    ) -> Self {
        Self(tokio::spawn(forward_client_feed(
            receiver,
            sink,
            Some(Arc::new(ExecPtyControl(control))),
            cancel,
        )))
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

#[async_trait]
pub(super) trait PtyControl: Send + Sync {
    async fn resize(&self, rows: u16, cols: u16) -> Result<()>;
}

struct ExecPtyControl(microsandbox::ExecControl);

#[async_trait]
impl PtyControl for ExecPtyControl {
    async fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        self.0
            .resize(rows, cols)
            .await
            .context("failed to resize guest PTY")
    }
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
    control: Option<Arc<dyn PtyControl>>,
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
        if cancel.is_cancelled() {
            break;
        }
        if let Err(error) = feed.validate() {
            tracing::warn!(%error, "discarding invalid client feed frame");
            if let Err(close_error) = sink.close().await {
                tracing::debug!(%close_error, "closing stdin after invalid feed frame failed");
            }
            break;
        }
        if let Some(resize) = &feed.pty_resize_control {
            let Some(control) = control.as_ref() else {
                tracing::warn!(
                    "discarding PTY resize frame without a control bound to this execution"
                );
                if let Err(error) = sink.close().await {
                    tracing::debug!(%error, "closing stdin after unbound PTY resize failed");
                }
                break;
            };
            let (rows, cols) = match (u16::try_from(resize.rows), u16::try_from(resize.cols)) {
                (Ok(rows), Ok(cols)) => (rows, cols),
                _ => {
                    tracing::warn!(
                        rows = resize.rows,
                        cols = resize.cols,
                        "discarding PTY resize dimensions unsupported by the SDK"
                    );
                    continue;
                }
            };
            if let Err(error) = control.resize(rows, cols).await {
                tracing::warn!(%error, rows, cols, "guest PTY resize failed");
            }
            continue;
        }
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
