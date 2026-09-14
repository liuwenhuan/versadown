use std::collections::HashSet;
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;
use uuid::Uuid;

const REFRESH_INTERVAL: Duration = Duration::from_millis(150);

#[derive(Default)]
struct PendingRefreshes {
    roots: HashSet<Uuid>,
    worker_running: bool,
}

/// One pending entry per directory and one worker, regardless of child count.
/// Store only IDs so the consumer projects current state at flush time.
#[derive(Default)]
pub(crate) struct CloudDirectoryRefreshes {
    pending: Mutex<PendingRefreshes>,
}

impl CloudDirectoryRefreshes {
    /// Returns true only when the caller must start the flush worker.
    pub(crate) fn queue(&self, root: Uuid) -> bool {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        pending.roots.insert(root);
        if pending.worker_running {
            false
        } else {
            pending.worker_running = true;
            true
        }
    }

    pub(crate) async fn flush_pending<F, Fut>(&self, mut flush: F)
    where
        F: FnMut(Vec<Uuid>) -> Fut,
        Fut: Future<Output = ()>,
    {
        loop {
            // New events do not reset this deadline. Slow projections also
            // cannot overlap, and receive a full interval before the next run.
            tokio::time::sleep(REFRESH_INTERVAL).await;
            let roots = {
                let mut pending = self
                    .pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if pending.roots.is_empty() {
                    // Clearing the worker flag and checking for queued work
                    // share a lock so an arriving event cannot lose its wakeup.
                    pending.worker_running = false;
                    return;
                }
                pending.roots.drain().collect()
            };
            flush(roots).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn directory_burst_flushes_once_and_reads_latest_state() {
        let refreshes = Arc::new(CloudDirectoryRefreshes::default());
        let root = Uuid::new_v4();
        assert!(refreshes.queue(root));
        for _ in 0..1000 {
            assert!(!refreshes.queue(root));
        }
        let latest = Arc::new(Mutex::new(0));
        let output = Arc::new(Mutex::new(Vec::new()));
        let worker = {
            let refreshes = refreshes.clone();
            let latest = latest.clone();
            let output = output.clone();
            tokio::spawn(async move {
                refreshes
                    .flush_pending(|roots| {
                        output
                            .lock()
                            .unwrap()
                            .push((roots, *latest.lock().unwrap()));
                        std::future::ready(())
                    })
                    .await;
            })
        };
        tokio::task::yield_now().await;
        *latest.lock().unwrap() = 1000;
        tokio::time::advance(Duration::from_millis(149)).await;
        tokio::task::yield_now().await;
        assert!(output.lock().unwrap().is_empty());
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(*output.lock().unwrap(), vec![(vec![root], 1000)]);
        worker.await.unwrap();
        assert!(refreshes.queue(root), "idle worker must allow a new burst");
    }

    #[tokio::test(start_paused = true)]
    async fn continuous_updates_flush_without_resetting_the_deadline() {
        let refreshes = Arc::new(CloudDirectoryRefreshes::default());
        let root = Uuid::new_v4();
        assert!(refreshes.queue(root));
        let output = Arc::new(Mutex::new(Vec::new()));
        let worker = {
            let refreshes = refreshes.clone();
            let output = output.clone();
            tokio::spawn(async move {
                refreshes
                    .flush_pending(|roots| {
                        output.lock().unwrap().push(roots);
                        std::future::ready(())
                    })
                    .await;
            })
        };
        tokio::task::yield_now().await;
        for step in 1..=9 {
            assert!(!refreshes.queue(root));
            tokio::time::advance(Duration::from_millis(50)).await;
            tokio::task::yield_now().await;
            assert_eq!(output.lock().unwrap().len(), step / 3);
        }
        assert_eq!(*output.lock().unwrap(), vec![vec![root]; 3]);
        worker.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn independent_roots_and_updates_during_flush_are_preserved() {
        let refreshes = Arc::new(CloudDirectoryRefreshes::default());
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        assert!(refreshes.queue(first));
        assert!(!refreshes.queue(second));
        let mut batches = Vec::new();
        refreshes
            .flush_pending(|roots| {
                batches.push(roots);
                if batches.len() == 1 {
                    assert!(!refreshes.queue(first));
                }
                std::future::ready(())
            })
            .await;
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), 2);
        assert!(batches[0].contains(&first));
        assert!(batches[0].contains(&second));
        assert_eq!(batches[1], vec![first]);
        assert!(refreshes.queue(second));
    }
}
