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
    /// A directory root occupies one provider task slot even while several of
    /// its file members are transferring. Ordinary cloud files use themselves
    /// as the owner, so their limit remains one member.
    active: HashMap<Uuid, ActiveOwner>,
    waiting: VecDeque<Waiter>,
}

struct ActiveOwner {
    members: HashSet<Uuid>,
    member_limit: usize,
}

struct Waiter {
    task_id: Uuid,
    owner_id: Uuid,
    member_limit: usize,
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
        self.enqueue_member(provider, task_id, task_id, 1).await
    }

    /// Admit one member of a directory. The directory itself consumes one
    /// provider task slot and may own up to `max_active_members` files at once.
    /// Each member still has its own aria2 GID, but never becomes an additional
    /// logical directory task.
    pub async fn enqueue_directory(
        &self,
        provider: CloudProvider,
        directory_id: Uuid,
        task_id: Uuid,
        max_active_members: usize,
    ) -> AdmissionOutcome {
        self.enqueue_member(provider, directory_id, task_id, max_active_members.max(1))
            .await
    }

    async fn enqueue_member(
        &self,
        provider: CloudProvider,
        owner_id: Uuid,
        task_id: Uuid,
        member_limit: usize,
    ) -> AdmissionOutcome {
        let token = Arc::new(());
        {
            let mut providers = self.inner.providers.lock().unwrap();
            if providers.values().any(|queue| {
                queue
                    .active
                    .values()
                    .any(|owner| owner.members.contains(&task_id))
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
                    owner_id,
                    member_limit,
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
                    Some(0) => {
                        let waiter = queue.waiting.front().expect("FIFO head exists");
                        let has_owner = queue.active.contains_key(&waiter.owner_id);
                        let has_provider_slot = queue.active.len() < limit;
                        let admitted = match queue.active.get_mut(&waiter.owner_id) {
                            Some(owner) if owner.members.len() < waiter.member_limit => {
                                owner.member_limit = waiter.member_limit;
                                owner.members.insert(waiter.task_id);
                                true
                            }
                            Some(_) => false,
                            None if !has_owner && has_provider_slot => {
                                queue.active.insert(
                                    waiter.owner_id,
                                    ActiveOwner {
                                        members: HashSet::from([waiter.task_id]),
                                        member_limit: waiter.member_limit,
                                    },
                                );
                                true
                            }
                            None => false,
                        };
                        admitted.then(|| {
                            queue.waiting.pop_front();
                            AdmissionOutcome::Granted
                        })
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
            .any(|queue| {
                let owner_id = queue.active.iter().find_map(|(owner_id, owner)| {
                    owner.members.contains(&task_id).then_some(*owner_id)
                });
                owner_id.is_some_and(|owner_id| queue.active.remove(&owner_id).is_some())
            });
        if released {
            self.inner.changed.notify_waiters();
        }
        released
    }

    /// Marks one directory member as terminal. The directory's provider task
    /// slot is retained until its aggregate queue root becomes terminal.
    pub fn release_directory_member(
        &self,
        directory_id: Uuid,
        task_id: Uuid,
        directory_finished: bool,
    ) -> bool {
        let released = self
            .inner
            .providers
            .lock()
            .unwrap()
            .values_mut()
            .any(|queue| {
                let Some(owner) = queue.active.get_mut(&directory_id) else {
                    return false;
                };
                let removed = owner.members.remove(&task_id);
                let owner_released =
                    directory_finished && queue.active.remove(&directory_id).is_some();
                removed || owner_released
            });
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
