use super::Aria2GlobalStat;
use super::Aria2RpcClient;
use super::{DownloadService, DownloadServiceError};
use crate::DownloadTask;
use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio::sync::{watch, Notify};

/// Implement with app.emit("download-task-updated", task) and
/// app.emit("download-global-stat-updated", stat) in the desktop composition root.
pub trait EventSink: Send + Sync {
    fn task_updated(&self, task: &DownloadTask);
    fn global_stat_updated(&self, stat: &Aria2GlobalStat);
}
pub struct NoEvents;
impl EventSink for NoEvents {
    fn task_updated(&self, _: &DownloadTask) {}
    fn global_stat_updated(&self, _: &Aria2GlobalStat) {}
}

pub struct TransferSynchronizer {
    service: Arc<DownloadService>,
    wake: Arc<Notify>,
    websocket: watch::Sender<Option<Aria2RpcClient>>,
    restart_config: Option<super::Aria2LaunchConfig>,
}
impl TransferSynchronizer {
    pub fn new(service: Arc<DownloadService>) -> Self {
        Self {
            service,
            wake: Arc::new(Notify::new()),
            websocket: watch::channel(None).0,
            restart_config: None,
        }
    }
    pub fn with_websocket(self, client: Aria2RpcClient) -> Self {
        self.websocket.send_replace(Some(client));
        self
    }
    pub fn with_restart_config(mut self, config: super::Aria2LaunchConfig) -> Self {
        self.restart_config = Some(config);
        self
    }
    pub fn poll_interval(&self) -> Result<Duration, DownloadServiceError> {
        let active = self.service.store().list()?.iter().any(|task| {
            task.aria_gid.is_some()
                && !crate::xunlei_native::is_native_gid(task.aria_gid.as_deref())
                && (matches!(
                    task.status,
                    crate::TaskStatus::Downloading | crate::TaskStatus::AwaitingUser
                ) || task.p2p_sharing)
        });
        Ok(if active {
            Duration::from_millis(500)
        } else {
            Duration::from_secs(2)
        })
    }
    pub async fn run(self, cancel: watch::Receiver<bool>) {
        tokio::select! {
            _ = self.poll_loop(cancel.clone()) => {},
            _ = self.notification_loop(cancel) => {},
        }
    }
    async fn poll_loop(&self, mut cancel: watch::Receiver<bool>) {
        loop {
            if *cancel.borrow() || !self.service.is_accepting() {
                break;
            }
            if let Some(config) = self.restart_config.as_ref() {
                // Only an observed owned-child exit spends the automatic restart budget.
                // HTTP/WS outages alone never launch a second sidecar.
                if let Ok(Some(report)) = self.service.restart_if_exited(config.clone()).await {
                    // Task recovery can fail after process replacement. The WS endpoint
                    // and secret must still advance to the newly running child.
                    self.websocket.send_replace(Some(report.client));
                }
            }
            tokio::select! {
                _ = cancel.changed() => break,
                _ = self.service.reconcile_now() => {}
            }
            let interval = self.poll_interval().unwrap_or(Duration::from_secs(2));
            tokio::select! {
                _ = cancel.changed() => break,
                _ = self.wake.notified() => {},
                _ = tokio::time::sleep(interval) => {},
            }
        }
    }

    async fn notification_loop(&self, mut cancel: watch::Receiver<bool>) {
        let mut clients = self.websocket.subscribe();
        let mut backoff = Duration::from_millis(100);
        loop {
            if *cancel.borrow() {
                return;
            }
            let client = clients.borrow_and_update().clone();
            let Some(client) = client else {
                tokio::select! { _ = cancel.changed() => return, _ = clients.changed() => continue }
            };
            tokio::select! {
                _ = cancel.changed() => return,
                _ = clients.changed() => { backoff = Duration::from_millis(100); continue; },
                _ = self.notification_connection(&client) => {},
            }
            tokio::select! {
                _ = cancel.changed() => return,
                _ = clients.changed() => { backoff = Duration::from_millis(100); continue; },
                _ = tokio::time::sleep(backoff) => {},
            }
            backoff = (backoff * 2).min(Duration::from_secs(5));
        }
    }

    async fn notification_connection(&self, client: &Aria2RpcClient) {
        use tokio_tungstenite::tungstenite::Message;
        let connection = tokio::time::timeout(
            Duration::from_secs(3),
            tokio_tungstenite::connect_async(client.websocket_endpoint().as_str()),
        )
        .await;
        let Ok(Ok((mut socket, _))) = connection else {
            return;
        };
        let request = serde_json::json!({"jsonrpc":"2.0","id":"sync-auth","method":"aria2.getVersion","params":[client.authorization_param()]});
        if socket
            .send(Message::Text(request.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
        let authenticated = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(Ok(message)) = socket.next().await {
                if let Ok(text) = message.to_text() {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
                        if value.get("id").and_then(|v| v.as_str()) == Some("sync-auth") {
                            return value.get("error").is_none()
                                && value["result"]["version"].is_string()
                                && value["jsonrpc"] == "2.0";
                        }
                    }
                }
            }
            false
        })
        .await;
        if !matches!(authenticated, Ok(true)) {
            return;
        }
        while let Some(Ok(message)) = socket.next().await {
            if message.is_close() {
                return;
            }
            if let Ok(text) = message.to_text() {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
                    if let Some(gid) = notification_gid(&value) {
                        if matches!(self.service.store().find_by_gid(gid), Ok(Some(_))) {
                            self.wake.notify_one();
                        }
                    }
                }
            }
        }
    }
}

pub fn notification_gid(value: &serde_json::Value) -> Option<&str> {
    if value.get("jsonrpc")?.as_str()? != "2.0" || value.get("id").is_some() {
        return None;
    }
    match value.get("method")?.as_str()? {
        "aria2.onDownloadStart"
        | "aria2.onDownloadPause"
        | "aria2.onDownloadStop"
        | "aria2.onDownloadComplete"
        | "aria2.onDownloadError"
        | "aria2.onBtDownloadComplete" => {}
        _ => return None,
    }
    let gid = value
        .get("params")?
        .as_array()?
        .first()?
        .get("gid")?
        .as_str()?;
    (!gid.is_empty()).then_some(gid)
}
