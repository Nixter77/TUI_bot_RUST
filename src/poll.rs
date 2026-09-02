//! Background snapshot pull so REST cannot freeze the TUI key loop.

use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

fn lock_poison<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One pull from the poller thread. `tradfi` is merged on the TUI thread.
pub struct Pulled<T> {
    pub snapshot: T,
    pub tradfi: Vec<String>,
}

pub struct SnapshotPoller<T> {
    wake_tx: Option<Sender<()>>,
    latest: Arc<Mutex<Option<Pulled<T>>>>,
    join: Option<JoinHandle<()>>,
}

impl<T: Send + 'static> SnapshotPoller<T> {
    pub fn start(interval: Duration, mut pull: impl FnMut() -> Pulled<T> + Send + 'static) -> io::Result<Self> {
        let (wake_tx, wake_rx) = mpsc::channel();
        let latest = Arc::new(Mutex::new(None));
        let slot = latest.clone();
        let join = thread::Builder::new()
            .name("snapshot-poll".into())
            .spawn(move || loop {
                match wake_rx.recv_timeout(interval) {
                    Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                // One panicking pull must not kill the thread until morning.
                if let Ok(pulled) = catch_unwind(AssertUnwindSafe(|| pull())) {
                    *lock_poison(&slot) = Some(pulled);
                }
            })?;
        Ok(Self {
            wake_tx: Some(wake_tx),
            latest,
            join: Some(join),
        })
    }

    /// Wake the poller immediately (refresh / flatten).
    pub fn bump(&self) {
        if let Some(tx) = &self.wake_tx {
            let _ = tx.send(());
        }
    }

    /// Take the newest snapshot. None if still pulling or already consumed.
    pub fn take(&self) -> Option<Pulled<T>> {
        lock_poison(&self.latest).take()
    }

    pub fn stop(&mut self) {
        self.wake_tx.take();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl<T> Drop for SnapshotPoller<T> {
    fn drop(&mut self) {
        self.wake_tx.take();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}
