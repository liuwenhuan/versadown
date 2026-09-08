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

        if let Some(service) = slot.service.clone() {
            if service.engine_running().await.ok().flatten() == Some(true) {
                return match service.recover().await {
                    Ok(_) => {
                        let client = slot.client.clone();
                        Self::start_synchronizer_if_needed(&mut slot, service, client);
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
        match service.recover().await {
            Ok(_) => {
                Self::start_synchronizer_if_needed(&mut slot, service, Some(client));
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
                let service = runtime.slot.lock().await.service.clone();
                let child_exited = match service {
                    Some(service) => service.engine_running().await.ok().flatten() == Some(false),
                    None => true,
                };
                let recovery_pending = !runtime.status().available;
                if (child_exited || recovery_pending)
                    && !runtime.automatic_retry_used.swap(true, Ordering::AcqRel)
                {
                    let _ = runtime.retry().await;
                }
            }
        });
    }

    pub async fn shutdown(&self, timeout: Duration) -> crate::aria2::recovery::ShutdownReport {
        self.exiting.store(true, Ordering::Release);
        let mut slot = self.slot.lock().await;
        if let Some(cancel) = slot.sync_cancel.take() {
            let _ = cancel.send(true);
        }
        slot.client.take();
        let report = if let Some(service) = slot.service.take() {
            service.shutdown_report(timeout).await
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
