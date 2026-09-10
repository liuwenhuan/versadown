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

pub(crate) fn cleanup_completed_share(
    plans: &crate::cloud::share::CloudSharePlanStore,
    tasks: &TaskStore,
    task: &crate::DownloadTask,
    enabled: bool,
    retry: bool,
    trash: impl FnOnce(crate::cloud::CloudProvider, &[String]) -> Result<(), crate::cloud::QuarkError>,
) -> Result<Option<crate::DownloadTask>, crate::cloud::QuarkError> {
    use crate::cloud::{share::parse_stable_cloud_share_source, QuarkError};
    if !enabled
        || task.status != TaskStatus::Completed
        || !task.source.starts_with("cloud-share://")
    {
        return Ok(None);
    }
    let (provider, _, _) = parse_stable_cloud_share_source(&task.source)?;
    let current = tasks.find(task.id).map_err(|_| QuarkError::Storage)?;
    if current.status != TaskStatus::Completed || current.source != task.source {
        return Ok(None);
    }
    let attempt = (|| {
        let ids = plans.mark_local_completion(&task.source, retry, |id, source| {
            tasks
                .find(id)
                .is_ok_and(|local| local.status == TaskStatus::Completed && local.source == source)
        })?;
        if ids.is_empty() {
            // aria2 may publish another completion without the cleanup message.
            // Re-project the journal outcome without replaying a provider call.
            use crate::cloud::share::CloudShareCleanupState;
            let plan = plans.resolve_entry(&task.source)?.plan;
            let pending = plan.transfers.iter().any(|transfer| {
                plan.linked_local_tasks
                    .contains_key(&transfer.entry_fingerprint)
                    && matches!(
                        transfer.cleanup_state,
                        CloudShareCleanupState::Failed | CloudShareCleanupState::Attempted
                    )
            });
            if pending
                && !current
                    .error
                    .as_deref()
                    .is_some_and(|message| message.starts_with("cleanup_pending："))
            {
                return Ok(Some(false));
            }
            if !pending
                && current
                    .error
                    .as_deref()
                    .is_some_and(|message| message.starts_with("cleanup_pending："))
                && plan
                    .transfers
                    .iter()
                    .any(|transfer| transfer.cleanup_state == CloudShareCleanupState::Succeeded)
            {
                return Ok(Some(true));
            }
            return Ok(None);
        }
        let succeeded = trash(provider, &ids).is_ok();
        plans.finish_cleanup(&task.source, &ids, succeeded)?;
        Ok::<_, QuarkError>(Some(succeeded))
    })();
    let pending = match attempt {
        Ok(None) => return Ok(None),
        Ok(Some(succeeded)) => !succeeded,
        Err(_) => true,
    };
    tasks
        .set_completed_cleanup_message(task.id, pending)
        .map(Some)
        .map_err(|_| QuarkError::Storage)
}

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

    fn set_status(tasks: &TaskStore, id: uuid::Uuid, status: TaskStatus) -> crate::DownloadTask {
        let mut task = tasks.find(id).unwrap();
        task.status = status;
        tasks.apply_transfer_snapshot(task).unwrap()
    }

    fn cleanup_fixture() -> (
        tempfile::TempDir,
        crate::cloud::share::CloudSharePlanStore,
        TaskStore,
        Vec<crate::DownloadTask>,
    ) {
        use crate::cloud::{share::*, CloudProvider, DpapiCredentialStore};
        let root = tempfile::tempdir().unwrap();
        let plans =
            CloudSharePlanStore::new(DpapiCredentialStore::new(root.path().into()).unwrap());
        let plan = CloudSharePlan::new(
            CloudProvider::Baidu,
            "share".into(),
            None,
            ["first", "second"]
                .into_iter()
                .map(|id| CloudShareEntry::new("share", id.into(), "file".into(), vec![], Some(1)))
                .collect(),
            serde_json::json!({}),
        );
        plans.upsert(&plan).unwrap();
        let sources = selected_share_sources(
            &plans,
            &plan.source_for("first"),
            &plan
                .entries
                .iter()
                .map(|e| e.entry_fingerprint.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        plans.record_transfer(&sources[0], "created-id").unwrap();
        let tasks = TaskStore::open_in_memory().unwrap();
        let locals = sources
            .into_iter()
            .map(|source| {
                let task = tasks
                    .create(crate::NewTask {
                        source,
                        name: "file".into(),
                        kind: "cloud".into(),
                    })
                    .unwrap();
                plans.link_local_task(&task.source, task.id).unwrap();
                tasks.transition(task.id, TaskStatus::Queued).unwrap();
                tasks.transition(task.id, TaskStatus::Downloading).unwrap()
            })
            .collect();
        (root, plans, tasks, locals)
    }

    #[test]
    fn last_completed_local_task_trashes_only_recorded_transfer_when_enabled() {
        let (_root, plans, tasks, locals) = cleanup_fixture();
        let first = set_status(&tasks, locals[0].id, TaskStatus::Completed);
        cleanup_completed_share(&plans, &tasks, &first, true, false, |_, _| {
            panic!("sibling incomplete")
        })
        .unwrap();
        let second = set_status(&tasks, locals[1].id, TaskStatus::Completed);
        let mut actions = vec![];
        cleanup_completed_share(&plans, &tasks, &second, true, false, |provider, ids| {
            actions.push((provider, ids.to_vec()));
            // The claim must already survive a reload before the network call.
            assert!(plans
                .mark_cleanup_attempted(&first.source)
                .unwrap()
                .is_empty());
            Ok(())
        })
        .unwrap();
        assert_eq!(
            actions,
            vec![(
                crate::cloud::CloudProvider::Baidu,
                vec!["created-id".to_string()]
            )]
        );
        cleanup_completed_share(&plans, &tasks, &second, true, false, |_, _| {
            panic!("duplicate cleanup")
        })
        .unwrap();
    }

    #[test]
    fn disabled_cleanup_or_failed_sibling_keeps_transfer() {
        for sibling_status in [
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
        ] {
            let (_root, plans, tasks, locals) = cleanup_fixture();
            set_status(&tasks, locals[0].id, sibling_status);
            let last = set_status(&tasks, locals[1].id, TaskStatus::Completed);
            cleanup_completed_share(
                &plans,
                &tasks,
                &last,
                sibling_status != TaskStatus::Completed,
                false,
                |_, _| panic!("cleanup blocked"),
            )
            .unwrap();
            assert_eq!(
                plans
                    .resolve_entry(&locals[0].source)
                    .unwrap()
                    .plan
                    .transfers[0]
                    .cleanup_state,
                crate::cloud::share::CloudShareCleanupState::Pending
            );
        }
    }

    #[test]
    fn cleanup_failure_keeps_completed_and_manual_retry_only_retries_cleanup() {
        let (_root, plans, tasks, locals) = cleanup_fixture();
        let first = set_status(&tasks, locals[0].id, TaskStatus::Completed);
        set_status(&tasks, locals[1].id, TaskStatus::Completed);
        let result = cleanup_completed_share(&plans, &tasks, &first, true, false, |_, _| {
            Err(crate::cloud::QuarkError::Provider("secret-token".into()))
        })
        .unwrap()
        .unwrap();
        assert_eq!(result.status, TaskStatus::Completed);
        assert!(result.error.as_ref().unwrap().contains("cleanup_pending"));
        assert!(!result.error.as_ref().unwrap().contains("secret-token"));
        cleanup_completed_share(&plans, &tasks, &result, true, false, |_, _| {
            panic!("automatic retry")
        })
        .unwrap();
        let retry =
            cleanup_completed_share(&plans, &tasks, &result, true, true, |provider, ids| {
                assert_eq!(provider, crate::cloud::CloudProvider::Baidu);
                assert_eq!(ids, &["created-id"]);
                Ok(())
            })
            .unwrap()
            .unwrap();
        assert_eq!(retry.status, TaskStatus::Completed);
        assert!(retry.error.is_none());
    }

    #[test]
    fn cleanup_rechecks_current_status_and_ignores_non_share_sources() {
        let (_root, plans, tasks, locals) = cleanup_fixture();
        let stale = set_status(&tasks, locals[0].id, TaskStatus::Completed);
        set_status(&tasks, locals[1].id, TaskStatus::Completed);
        let mut restarted = stale.clone();
        restarted.status = TaskStatus::Downloading;
        tasks.apply_transfer_snapshot(restarted).unwrap();
        cleanup_completed_share(&plans, &tasks, &stale, true, true, |_, _| {
            panic!("stale completion")
        })
        .unwrap();
        for source in [
            "cloud://baidu/existing-personal-id",
            "https://example.com/file",
            "cloud-share://baidu/invalid/invalid",
        ] {
            let mut task = stale.clone();
            task.source = source.into();
            let _ = cleanup_completed_share(&plans, &tasks, &task, true, true, |_, _| {
                panic!("untracked source")
            });
        }
    }

    #[test]
    fn cleanup_result_cannot_restore_a_task_changed_during_network_work() {
        let (_root, plans, tasks, locals) = cleanup_fixture();
        let first = set_status(&tasks, locals[0].id, TaskStatus::Completed);
        set_status(&tasks, locals[1].id, TaskStatus::Completed);
        let result = cleanup_completed_share(&plans, &tasks, &first, true, false, |_, _| {
            set_status(&tasks, first.id, TaskStatus::Cancelled);
            Err(crate::cloud::QuarkError::Transport(
                "private-provider-error".into(),
            ))
        })
        .unwrap()
        .unwrap();
        assert_eq!(result.status, TaskStatus::Cancelled);
        assert!(result.error.is_none());
    }

    #[test]
    fn repeated_completion_restores_cleanup_pending_without_repeating_network_action() {
        let (_root, plans, tasks, locals) = cleanup_fixture();
        let first = set_status(&tasks, locals[0].id, TaskStatus::Completed);
        set_status(&tasks, locals[1].id, TaskStatus::Completed);
        cleanup_completed_share(&plans, &tasks, &first, true, false, |_, _| {
            Err(crate::cloud::QuarkError::Storage)
        })
        .unwrap();
        // A repeated aria2 completion snapshot carries no provider cleanup error.
        tasks.apply_transfer_snapshot(first.clone()).unwrap();
        cleanup_completed_share(&plans, &tasks, &first, true, false, |_, _| {
            panic!("automatic replay")
        })
        .unwrap();
        assert!(tasks
            .find(first.id)
            .unwrap()
            .error
            .unwrap()
            .contains("cleanup_pending"));
    }

    #[tokio::test]
    async fn cleanup_waiting_on_resubmission_gate_rechecks_restarted_sibling() {
        let (_root, plans, tasks, locals) = cleanup_fixture();
        let first = set_status(&tasks, locals[0].id, TaskStatus::Completed);
        set_status(&tasks, locals[1].id, TaskStatus::Completed);
        let gates = crate::cloud::commands::CloudDownloadSourceGate::default();
        let retry_guard = gates.acquire(&first.source).await.unwrap();
        let cleanup_gate = gates.acquire(&first.source);
        tokio::pin!(cleanup_gate);
        assert!(futures_util::poll!(&mut cleanup_gate).is_pending());
        set_status(&tasks, first.id, TaskStatus::Downloading);
        drop(retry_guard);
        let _cleanup_guard = cleanup_gate.await.unwrap();
        cleanup_completed_share(&plans, &tasks, &first, true, false, |_, _| {
            panic!("restarted sibling was deleted")
        })
        .unwrap();
    }

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
