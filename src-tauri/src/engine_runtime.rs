use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};

use serde::Serialize;
use thiserror::Error;
use tokio::sync::{watch, Mutex};

use crate::{
    aria2::{
        Aria2GlobalStat, Aria2LaunchConfig, Aria2Process, Aria2RpcClient, DownloadService,
        EventSink, TransferSynchronizer,
    },
    TaskStatus, TaskStore,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineAvailability {
    pub available: bool,
    pub retrying: bool,
    pub error: Option<String>,
}

impl EngineAvailability {
    fn starting() -> Self {
        Self {
            available: false,
            retrying: true,
            error: None,
        }
    }

    fn available() -> Self {
        Self {
            available: true,
            retrying: false,
            error: None,
        }
    }

    fn unavailable(error: impl Into<String>) -> Self {
        Self {
            available: false,
            retrying: false,
            error: Some(format!("下载引擎不可用：{}", error.into())),
        }
    }
}

pub trait EngineStateSink: Send + Sync {
    fn engine_state_updated(&self, state: &EngineAvailability);
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct EngineUnavailableError {
    message: String,
}

struct EngineSlot {
    service: Option<Arc<DownloadService>>,
    client: Option<Aria2RpcClient>,
    sync_cancel: Option<watch::Sender<bool>>,
}

pub struct EngineRuntime {
    launch_config: Aria2LaunchConfig,
    store: Arc<TaskStore>,
    download_events: Arc<dyn EventSink>,
    state_events: Arc<dyn EngineStateSink>,
    state: RwLock<EngineAvailability>,
    slot: Mutex<EngineSlot>,
    exiting: AtomicBool,
    monitor_started: AtomicBool,
    automatic_retry_used: AtomicBool,
}

impl EngineRuntime {
    pub fn new(
        launch_config: Aria2LaunchConfig,
        store: Arc<TaskStore>,
        download_events: Arc<dyn EventSink>,
        state_events: Arc<dyn EngineStateSink>,
    ) -> Self {
        Self {
            launch_config,
            store,
            download_events,
            state_events,
            state: RwLock::new(EngineAvailability::starting()),
            slot: Mutex::new(EngineSlot {
                service: None,
                client: None,
                sync_cancel: None,
            }),
            exiting: AtomicBool::new(false),
            monitor_started: AtomicBool::new(false),
            automatic_retry_used: AtomicBool::new(false),
        }
    }

    pub fn status(&self) -> EngineAvailability {
        self.state
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub async fn initialize(&self) -> EngineAvailability {
        self.retry().await
    }

    pub async fn retry(&self) -> EngineAvailability {
        if self.exiting.load(Ordering::Acquire) {
            return self.set_state(EngineAvailability::unavailable("应用正在退出"));
        }
        self.set_state(EngineAvailability::starting());
        let mut slot = self.slot.lock().await;
        if self.exiting.load(Ordering::Acquire) {
            self.stop_slot_for_exit(&mut slot).await;
            return self.set_state(EngineAvailability::unavailable("应用正在退出"));
        }
        self.retry_locked(&mut slot).await
    }

    async fn retry_locked(&self, slot: &mut EngineSlot) -> EngineAvailability {
        if let Some(service) = slot.service.clone() {
            if service.engine_running().await.ok().flatten() == Some(true) {
                let recovered = service.recover().await;
                if self.exiting.load(Ordering::Acquire) {
                    self.stop_slot_for_exit(slot).await;
                    return self.set_state(EngineAvailability::unavailable("应用正在退出"));
                }
                return match recovered {
                    Ok(_) => {
                        let client = slot.client.clone();
                        Self::start_synchronizer_if_needed(slot, service, client);
                        self.set_state(EngineAvailability::available())
                    }
                    Err(error) => {
                        self.mark_tasks_unavailable(&error.to_string());
                        self.set_state(EngineAvailability::unavailable(error.to_string()))
                    }
                };
            }
        }

        if let Some(cancel) = slot.sync_cancel.take() {
            let _ = cancel.send(true);
        }
        slot.service.take();
        slot.client.take();
        let (process, client) = match Aria2Process::start(self.launch_config.clone()).await {
            Ok(engine) => engine,
            Err(error) => {
                self.mark_tasks_unavailable(&error.to_string());
                return self.set_state(EngineAvailability::unavailable(error.to_string()));
            }
        };
        if self.exiting.load(Ordering::Acquire) {
            drop(process);
            return self.set_state(EngineAvailability::unavailable("应用正在退出"));
        }
        let service = Arc::new(
            DownloadService::with_process(
                Arc::new(client.clone()),
                Arc::clone(&self.store),
                process,
            )
            .with_events(Arc::clone(&self.download_events)),
        );
        slot.service = Some(Arc::clone(&service));
        slot.client = Some(client.clone());
        let recovered = service.recover().await;
        if self.exiting.load(Ordering::Acquire) {
            self.stop_slot_for_exit(slot).await;
            return self.set_state(EngineAvailability::unavailable("应用正在退出"));
        }
        match recovered {
            Ok(_) => {
                Self::start_synchronizer_if_needed(slot, service, Some(client));
                self.set_state(EngineAvailability::available())
            }
            Err(error) => {
                self.mark_tasks_unavailable(&error.to_string());
                self.set_state(EngineAvailability::unavailable(error.to_string()))
            }
        }
    }

    pub async fn service(&self) -> Result<Arc<DownloadService>, EngineUnavailableError> {
        let state = self.status();
        if !state.available {
            return Err(EngineUnavailableError {
                message: state
                    .error
                    .unwrap_or_else(|| "下载引擎不可用，请稍后重试".into()),
            });
        }
        self.slot
            .lock()
            .await
            .service
            .clone()
            .ok_or_else(|| EngineUnavailableError {
                message: "下载引擎不可用，请稍后重试".into(),
            })
    }

    pub async fn global_stat(&self) -> Result<Aria2GlobalStat, EngineUnavailableError> {
        let service = self.service().await?;
        service
            .api()
            .get_global_stat()
            .await
            .map_err(|error| EngineUnavailableError {
                message: format!("下载引擎不可用：{error}"),
            })
    }

    pub fn start_monitor(self: &Arc<Self>, interval: Duration) {
        if self.monitor_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let runtime = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                if runtime.exiting.load(Ordering::Acquire) {
                    return;
                }
                let mut slot = runtime.slot.lock().await;
                if runtime.exiting.load(Ordering::Acquire) {
                    return;
                }
                // A public retry publishes this state before waiting for the slot.
                // Recheck it under the same mutex used to start a process so the
                // monitor cannot race ahead and spend its one automatic attempt.
                if runtime.status().retrying {
                    continue;
                }
                let service = slot.service.clone();
                let child_exited = match service {
                    Some(service) => service.engine_running().await.ok().flatten() == Some(false),
                    None => true,
                };
                let recovery_pending = !runtime.status().available;
                if (child_exited || recovery_pending)
                    && runtime
                        .automatic_retry_used
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    runtime.set_state(EngineAvailability::starting());
                    let _ = runtime.retry_locked(&mut slot).await;
                }
            }
        });
    }

    pub async fn shutdown(&self, timeout: Duration) -> crate::aria2::recovery::ShutdownReport {
        let deadline = tokio::time::Instant::now() + timeout;
        self.exiting.store(true, Ordering::Release);
        let mut slot = match tokio::time::timeout_at(deadline, self.slot.lock()).await {
            Ok(slot) => slot,
            Err(_) => {
                self.set_state(EngineAvailability::unavailable("应用正在退出"));
                return crate::aria2::recovery::ShutdownReport {
                    session_saved: false,
                    shutdown_requested: false,
                    force_killed: false,
                    child_stopped: false,
                };
            }
        };
        if let Some(cancel) = slot.sync_cancel.take() {
            let _ = cancel.send(true);
        }
        slot.client.take();
        let report = if let Some(service) = slot.service.take() {
            service
                .shutdown_report(deadline.saturating_duration_since(tokio::time::Instant::now()))
                .await
        } else {
            crate::aria2::recovery::ShutdownReport {
                session_saved: false,
                shutdown_requested: false,
                force_killed: false,
                child_stopped: true,
            }
        };
        self.set_state(EngineAvailability::unavailable("应用正在退出"));
        report
    }

    async fn stop_slot_for_exit(&self, slot: &mut EngineSlot) {
        if let Some(cancel) = slot.sync_cancel.take() {
            let _ = cancel.send(true);
        }
        slot.client.take();
        if let Some(service) = slot.service.take() {
            let _ = service.shutdown_report(Duration::ZERO).await;
        }
    }

    fn start_synchronizer_if_needed(
        slot: &mut EngineSlot,
        service: Arc<DownloadService>,
        client: Option<Aria2RpcClient>,
    ) {
        if slot.sync_cancel.is_some() {
            return;
        }
        let (cancel, cancel_rx) = watch::channel(false);
        slot.sync_cancel = Some(cancel);
        let synchronizer = match client {
            Some(client) => TransferSynchronizer::new(service).with_websocket(client),
            None => TransferSynchronizer::new(service),
        };
        tauri::async_runtime::spawn(async move {
            synchronizer.run(cancel_rx).await;
        });
    }

    fn mark_tasks_unavailable(&self, cause: &str) {
        let Ok(tasks) = self.store.list() else {
            return;
        };
        for mut task in tasks {
            if !matches!(
                task.status,
                TaskStatus::Downloading | TaskStatus::Queued | TaskStatus::Parsing
            ) {
                continue;
            }
            let can_resume = task.aria_gid.is_some();
            task.status = if can_resume {
                TaskStatus::Paused
            } else {
                TaskStatus::Failed
            };
            task.download_speed = 0;
            task.upload_speed = 0;
            task.eta_seconds = None;
            task.error = Some(if can_resume {
                format!("下载引擎不可用：{cause}")
            } else {
                format!("下载引擎不可用，解析已中断，请重新添加任务：{cause}")
            });
            if let Ok(task) = self.store.apply_transfer_snapshot(task) {
                self.download_events.task_updated(&task);
            }
        }
    }

    fn set_state(&self, next: EngineAvailability) -> EngineAvailability {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if *state != next {
            *state = next.clone();
            self.state_events.engine_state_updated(&next);
        }
        next
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    struct NoEngineEvents;
    impl EngineStateSink for NoEngineEvents {
        fn engine_state_updated(&self, _: &EngineAvailability) {}
    }

    fn runtime(root: &std::path::Path) -> Arc<EngineRuntime> {
        Arc::new(EngineRuntime::new(
            Aria2LaunchConfig {
                binary: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("binaries/aria2c-x86_64-pc-windows-msvc.exe"),
                data_dir: root.join("data"),
                download_dir: root.join("downloads"),
                parent_pid: std::process::id(),
                peer_discovery: false,
            },
            Arc::new(TaskStore::open_in_memory().unwrap()),
            Arc::new(crate::aria2::sync::NoEvents),
            Arc::new(NoEngineEvents),
        ))
    }

    #[tokio::test]
    async fn shutdown_deadline_includes_waiting_for_a_retry_that_owns_the_slot() {
        let root = tempfile::tempdir().unwrap();
        let runtime = runtime(root.path());
        let slot = runtime.slot.lock().await;
        let retry_runtime = Arc::clone(&runtime);
        let retry = tokio::spawn(async move { retry_runtime.retry().await });
        tokio::task::yield_now().await;

        let began = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_millis(150),
            runtime.shutdown(Duration::from_millis(40)),
        )
        .await;

        assert!(result.is_ok(), "shutdown waited indefinitely for the slot");
        assert!(began.elapsed() < Duration::from_millis(150));
        drop(slot);
        assert!(!retry.await.unwrap().available);
        assert!(runtime.slot.lock().await.service.is_none());
    }

    #[tokio::test]
    async fn monitor_does_not_spend_restart_budget_while_manual_retry_is_queued() {
        let root = tempfile::tempdir().unwrap();
        let runtime = runtime(root.path());
        let slot = runtime.slot.lock().await;
        runtime.start_monitor(Duration::from_millis(10));
        tokio::time::sleep(Duration::from_millis(30)).await;
        let retry_runtime = Arc::clone(&runtime);
        let manual = tokio::spawn(async move { retry_runtime.retry().await });
        tokio::task::yield_now().await;
        drop(slot);

        assert!(manual.await.unwrap().available);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !runtime.automatic_retry_used.load(Ordering::Acquire),
            "monitor spent its only budget while a manual retry was already pending"
        );

        let old = runtime.service().await.unwrap();
        old.api().shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(7), async {
            loop {
                if let Ok(service) = runtime.service().await {
                    if !Arc::ptr_eq(&old, &service)
                        && service.engine_running().await.unwrap() == Some(true)
                    {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("real child exit did not receive the automatic restart budget");
        assert!(runtime.shutdown(Duration::from_secs(5)).await.child_stopped);
    }
}
