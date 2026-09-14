use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
};

use tokio::sync::Notify;
use uuid::Uuid;

use super::{CloudProvider, ProviderRegistry};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionOutcome {
    Granted,
    Cancelled,
    AlreadyTracked,
}

/// In-memory provider slots. Granted tasks hold their slot until `release`.
/// Only task IDs are retained; resolution credentials and URLs stay with callers.
#[derive(Clone, Default)]
pub struct CloudDownloadScheduler {
    inner: Arc<SchedulerState>,
}

#[derive(Default)]
struct SchedulerState {
    providers: Mutex<HashMap<CloudProvider, ProviderQueue>>,
    changed: Notify,
}

#[derive(Default)]
struct ProviderQueue {
    active: HashSet<Uuid>,
    waiting: VecDeque<Waiter>,
}

struct Waiter {
    task_id: Uuid,
    // Distinguishes a cancelled enqueue from a later retry using the same ID.
    token: Arc<()>,
}

struct WaitingRegistration {
    scheduler: CloudDownloadScheduler,
    provider: CloudProvider,
    token: Arc<()>,
}

impl Drop for WaitingRegistration {
    fn drop(&mut self) {
        let removed = {
            let mut providers = self.scheduler.inner.providers.lock().unwrap();
            let queue = providers.get_mut(&self.provider).unwrap();
            let before = queue.waiting.len();
            queue
                .waiting
                .retain(|waiter| !Arc::ptr_eq(&waiter.token, &self.token));
            queue.waiting.len() != before
        };
        if removed {
            self.scheduler.inner.changed.notify_waiters();
        }
    }
}

impl CloudDownloadScheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wait for the FIFO head and an available slot for this provider.
    /// Duplicate IDs return immediately, including IDs tracked by another provider.
    /// Dropping this future removes its waiting entry, but never a granted slot.
    pub async fn enqueue(&self, provider: CloudProvider, task_id: Uuid) -> AdmissionOutcome {
        let token = Arc::new(());
        {
            let mut providers = self.inner.providers.lock().unwrap();
            if providers.values().any(|queue| {
                queue.active.contains(&task_id)
                    || queue.waiting.iter().any(|waiter| waiter.task_id == task_id)
            }) {
                return AdmissionOutcome::AlreadyTracked;
            }
            providers
                .entry(provider)
                .or_default()
                .waiting
                .push_back(Waiter {
                    task_id,
                    token: Arc::clone(&token),
                });
        }
        let _registration = WaitingRegistration {
            scheduler: self.clone(),
            provider,
            token: Arc::clone(&token),
        };
        let limit = usize::from(
            ProviderRegistry::global()
                .get(provider)
                .download_concurrency
                .max_concurrent_tasks,
        );
        loop {
            // Register before checking the condition so release/cancel cannot lose a wakeup.
            let notified = self.inner.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let outcome = {
                let mut providers = self.inner.providers.lock().unwrap();
                let queue = providers.get_mut(&provider).unwrap();
                let position = queue
                    .waiting
                    .iter()
                    .position(|waiter| Arc::ptr_eq(&waiter.token, &token));
                match position {
                    None => Some(AdmissionOutcome::Cancelled),
                    Some(0) if queue.active.len() < limit => {
                        queue.waiting.pop_front();
                        queue.active.insert(task_id);
                        Some(AdmissionOutcome::Granted)
                    }
                    Some(_) => None,
                }
            };
            if let Some(outcome) = outcome {
                // More than one slot may be available, so allow the next FIFO head to check.
                self.inner.changed.notify_waiters();
                return outcome;
            }
            notified.await;
        }
    }

    /// Release an admitted task. Repeated releases are harmless.
    pub fn release(&self, task_id: Uuid) -> bool {
        let released = self
            .inner
            .providers
            .lock()
            .unwrap()
            .values_mut()
            .any(|queue| queue.active.remove(&task_id));
        if released {
            self.inner.changed.notify_waiters();
        }
        released
    }

    /// Cancel a waiter. Active work must be stopped by its caller, then released.
    pub fn cancel(&self, task_id: Uuid) -> bool {
        let cancelled = self
            .inner
            .providers
            .lock()
            .unwrap()
            .values_mut()
            .any(|queue| {
                let before = queue.waiting.len();
                queue.waiting.retain(|waiter| waiter.task_id != task_id);
                queue.waiting.len() != before
            });
        if cancelled {
            self.inner.changed.notify_waiters();
        }
        cancelled
    }

    pub fn pending(&self, provider: CloudProvider) -> Vec<Uuid> {
        self.inner
            .providers
            .lock()
            .unwrap()
            .get(&provider)
            .map(|queue| queue.waiting.iter().map(|waiter| waiter.task_id).collect())
            .unwrap_or_default()
    }
}
