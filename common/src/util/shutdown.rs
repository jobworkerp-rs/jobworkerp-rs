// use signal_hook::{consts::SIGINT, iterator::Signals};
// ref: https://tokio.rs/tokio/topics/shutdown
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

// 全てのShutdownLockが消えるまで待つWait
#[derive(Debug, Clone)]
pub struct ShutdownLock {
    /// lock shutdown
    lock_sender: UnboundedSender<()>,
}

impl ShutdownLock {
    pub fn new(sender: UnboundedSender<()>) -> ShutdownLock {
        ShutdownLock {
            lock_sender: sender,
        }
    }
    pub fn unlock(self) {
        drop(self.lock_sender)
    }
    pub fn is_shutdown(&self) -> bool {
        self.lock_sender.is_closed()
    }
}

// 全てのShutdownLockが消えるまで待つWait
#[derive(Debug)]
pub struct ShutdownWait {
    /// The receive for waiting shutdown.
    wait_receiver: UnboundedReceiver<()>,
}

impl ShutdownWait {
    pub fn new(wait_receiver: UnboundedReceiver<()>) -> ShutdownWait {
        ShutdownWait { wait_receiver }
    }
    pub async fn wait(&mut self) {
        let r = self.wait_receiver.recv().await;
        tracing::debug!("shutdown wait released: {:?}", r);
    }
}

pub fn create_lock_and_wait() -> (ShutdownLock, ShutdownWait) {
    let (send, recv) = mpsc::unbounded_channel();
    (ShutdownLock::new(send), ShutdownWait::new(recv))
}
