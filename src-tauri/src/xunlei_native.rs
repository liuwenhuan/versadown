//! Windows-only bridge to the native task API shipped with an installed
//! Thunder client. It loads the official proxy DLL, whose isolated SDK server
//! owns P2SP tasks; neither `Thunder.exe` nor an extra Node host is started.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

#[cfg(windows)]
use hmac::{Hmac, Mac};
#[cfg(windows)]
use sha2::Sha256;
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

use crate::{
    cloud::commands::ResolvedCloudDownload, DownloadAction, DownloadTask, TaskStatus, TaskStore,
    UriSubmissionOptions,
};

/// An in-memory-only marker used to hand a drive response's opaque credential
/// to this backend. `ResolvedCloudDownload::into_uri_submission_options`
/// removes it before an aria2 request is constructed.
pub(crate) const ACCELERATION_TOKEN_MARKER_HEADER: &str = "X-Downloader-Xunlei-Acceleration-Token";
pub(crate) const ACCESS_TOKEN_MARKER_HEADER: &str = "X-Downloader-Xunlei-Access-Token";
pub(crate) const USER_ID_MARKER_HEADER: &str = "X-Downloader-Xunlei-User-Id";
pub(crate) const PEER_ID_MARKER_HEADER: &str = "X-Downloader-Xunlei-Peer-Id";
pub(crate) const CONTENT_LENGTH_MARKER_HEADER: &str = "X-Downloader-Xunlei-Content-Length";
pub(crate) const CID_MARKER_HEADER: &str = "X-Downloader-Xunlei-Cid";
pub(crate) const GCID_MARKER_HEADER: &str = "X-Downloader-Xunlei-Gcid";
const NATIVE_GID_PREFIX: &str = "xunlei-native:";
pub(crate) const PLUGIN_UPDATE_MESSAGE: &str = "迅雷云盘插件需要更新";

pub(crate) fn is_native_gid(gid: Option<&str>) -> bool {
    gid.is_some_and(|value| value.starts_with(NATIVE_GID_PREFIX))
}

pub struct XunleiNativeBackend {
    #[cfg(windows)]
    api: std::sync::Mutex<Option<WindowsApi>>,
    #[cfg(windows)]
    equity_tokens: std::sync::Mutex<BTreeMap<u32, String>>,
    #[cfg(windows)]
    plugin_config_path: Option<PathBuf>,
}

impl Default for XunleiNativeBackend {
    fn default() -> Self {
        Self {
            #[cfg(windows)]
            api: std::sync::Mutex::new(None),
            #[cfg(windows)]
            equity_tokens: std::sync::Mutex::new(BTreeMap::new()),
            #[cfg(windows)]
            plugin_config_path: None,
        }
    }
}

impl XunleiNativeBackend {
    pub(crate) fn with_plugin_config(plugin_config_path: PathBuf) -> Self {
        Self {
            #[cfg(windows)]
            api: std::sync::Mutex::new(None),
            #[cfg(windows)]
            equity_tokens: std::sync::Mutex::new(BTreeMap::new()),
            #[cfg(windows)]
            plugin_config_path: Some(plugin_config_path),
        }
    }

    /// Discovers the installed SDK and persists its path without loading the
    /// native module. Download submission later uses this saved, validated
    /// location directly.
    pub(crate) fn initialize_plugin_config(&self) {
        #[cfg(windows)]
        let _ = configured_or_installed_plugin_config(self.plugin_config_path.as_deref());
    }
}

#[cfg(windows)]
struct NativeEquityRequest<'a> {
    access_token: &'a str,
    user_id: &'a str,
    peer_id: &'a str,
    client_version: &'a str,
    trace_id: &'a str,
    resource_url: &'a str,
}

#[cfg(windows)]
fn request_equity_token(
    api: &mut WindowsApi,
    request: NativeEquityRequest<'_>,
) -> Result<String, NativeSubmitError> {
    let (encrypted, security_key) = request_equity_rule(request)?;
    api.decrypt_equity_rule(&encrypted, &security_key)
}

#[cfg(windows)]
fn request_equity_rule(
    request: NativeEquityRequest<'_>,
) -> Result<(String, String), NativeSubmitError> {
    use crate::cloud::{CloudHttpRequest, CloudHttpTransport, ReqwestTransport};

    const CLIENT_ID: &str = "XW-G4v1H72tgfJym";
    let rt = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| NativeSubmitError::Incompatible)?
        .as_secs()
        .to_string();
    let nonce = Uuid::new_v4().to_string();
    let resource_value: String =
        url::form_urlencoded::byte_serialize(request.resource_url.as_bytes()).collect();
    let security_seed = format!(
        "{CLIENT_ID}{}{}{}",
        request.client_version, request.user_id, rt
    );
    let security_key = format!("{:X}", md5::compute(security_seed.as_bytes()))[..16].to_owned();
    let signature_text = format!(
        "nonce={nonce}&resource_type=url&resource_value={resource_value}&rt={rt}&scene=download&trial_cost=0&trial_id="
    );
    let mut signer = Hmac::<Sha256>::new_from_slice(security_key.as_bytes())
        .map_err(|_| NativeSubmitError::Incompatible)?;
    signer.update(signature_text.as_bytes());
    let signature: String = signer
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let query = format!(
        "trace_id={}&resource_type=url&resource_value={resource_value}&scene=download&trial_id=&trial_cost=0&rt={rt}&nonce={nonce}&sign={signature}&user_id={}",
        url::form_urlencoded::byte_serialize(request.trace_id.as_bytes()).collect::<String>(),
        url::form_urlencoded::byte_serialize(request.user_id.as_bytes()).collect::<String>()
    );
    let headers = BTreeMap::from([
        ("Content-Type".into(), "application/json".into()),
        ("X-Guid".into(), request.peer_id.into()),
        ("X-Peer-Id".into(), request.peer_id.into()),
        ("X-Client-Id".into(), CLIENT_ID.into()),
        ("X-Client-Version".into(), request.client_version.into()),
        (
            "X-Client-Version-Code".into(),
            request.client_version.into(),
        ),
        ("X-Signature".into(), signature),
        ("X-Signature-Time".into(), rt),
        ("X-Signature-Nonce".into(), nonce),
        (
            "Authorization".into(),
            format!("Bearer {}", request.access_token),
        ),
    ]);
    let response = ReqwestTransport::new()
        .and_then(|http| {
            http.send(CloudHttpRequest {
                method: "GET",
                url: format!("https://api-gateway-pan.xunlei.com/speed-center/v2/rule?{query}"),
                headers,
                body: None,
            })
        })
        .map_err(|_| NativeSubmitError::Failed("迅雷会员加速通道网络请求失败".into()))?;
    if !(200..300).contains(&response.status) {
        return Err(NativeSubmitError::Failed("迅雷会员加速通道请求失败".into()));
    }
    let envelope: serde_json::Value =
        serde_json::from_str(&response.body).map_err(|_| NativeSubmitError::Incompatible)?;
    let response_code = envelope
        .get("error_code")
        .or_else(|| envelope.get("code"))
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        });
    if response_code != Some(0) {
        let code = envelope
            .get("error_code")
            .or_else(|| envelope.get("code"))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(-1);
        let message = envelope
            .get("error")
            .or_else(|| envelope.get("error_description"))
            .or_else(|| envelope.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        return Err(NativeSubmitError::Failed(format!(
            "迅雷会员加速权限不可用（代码 {code}: {message}）"
        )));
    }
    Ok((
        envelope
            .get("data")
            .and_then(serde_json::Value::as_str)
            .ok_or(NativeSubmitError::Incompatible)?
            .to_owned(),
        security_key,
    ))
}

#[derive(Debug)]
pub enum NativeSubmitError {
    Incompatible,
    Failed(String),
}

impl NativeSubmitError {
    pub fn user_message(&self) -> String {
        match self {
            Self::Incompatible => PLUGIN_UPDATE_MESSAGE.into(),
            Self::Failed(message) => message.clone(),
        }
    }
}

impl XunleiNativeBackend {
    /// Returns `Ok(None)` only when Thunder is not installed or this resolved
    /// file has no acceleration credential. Both cases deliberately preserve
    /// the existing aria2 behavior.
    pub fn submit(
        self: &Arc<Self>,
        app: &AppHandle,
        store: &Arc<TaskStore>,
        task_id: Uuid,
        resolved: &ResolvedCloudDownload,
        options: &UriSubmissionOptions,
    ) -> Result<Option<DownloadTask>, NativeSubmitError> {
        let Some(access_token) = marker_header(resolved, ACCESS_TOKEN_MARKER_HEADER) else {
            return Ok(None);
        };
        let Some(user_id) = marker_header(resolved, USER_ID_MARKER_HEADER) else {
            return Ok(None);
        };
        let Some(peer_id) = marker_header(resolved, PEER_ID_MARKER_HEADER) else {
            return Ok(None);
        };

        #[cfg(not(windows))]
        {
            let _ = (
                app,
                store,
                task_id,
                resolved,
                options,
                access_token,
                user_id,
                peer_id,
            );
            return Ok(None);
        }

        #[cfg(windows)]
        {
            let output_directory = options
                .directory
                .clone()
                .ok_or_else(|| NativeSubmitError::Failed("迅雷原生下载缺少保存目录".into()))?;
            let output_name = options
                .output_name
                .clone()
                .unwrap_or_else(|| resolved.name.clone());
            if output_name.is_empty() || output_name.contains(['/', '\\']) {
                return Err(NativeSubmitError::Failed("迅雷原生下载文件名无效".into()));
            }
            std::fs::create_dir_all(&output_directory)
                .map_err(|_| NativeSubmitError::Failed("无法创建迅雷下载目录".into()))?;
            let output_path = output_directory.join(&output_name);
            let referer = resolved
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("referer"))
                .map(|(_, value)| value.as_str())
                .unwrap_or("");
            let user_agent = resolved
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
                .map(|(_, value)| value.as_str())
                .unwrap_or("Mozilla/5.0");
            let cid = marker_header(resolved, CID_MARKER_HEADER).unwrap_or("");
            let gcid = marker_header(resolved, GCID_MARKER_HEADER).unwrap_or("");

            let (native_id, client_version) = {
                let mut api = self
                    .api
                    .lock()
                    .map_err(|_| NativeSubmitError::Incompatible)?;
                if api.is_none() {
                    *api = WindowsApi::load(
                        self.plugin_config_path.as_deref(),
                        Some((user_id, peer_id)),
                    )?;
                }
                let Some(api) = api.as_mut() else {
                    return Ok(None);
                };
                // Native checkpoint files are meaningful only after the
                // official backend has actually been selected. Without the
                // installed SDK, return `None` above and let aria2 apply its
                // own `.aria2` continuation and overwrite rules.
                prepare_native_output(
                    &output_path,
                    options.allow_overwrite,
                    options.continue_download,
                )?;
                api.set_identity(user_id, peer_id)?;
                let size = resolved.size_hint().unwrap_or(0);
                if let Ok(existing) = store.find(task_id) {
                    if let Some(old_id) = native_id(&existing) {
                        if api.stop(old_id).is_ok() {
                            std::thread::sleep(std::time::Duration::from_millis(300));
                        }
                    }
                }
                let native_id = api.create_p2sp_task(
                    &resolved.url,
                    &output_path,
                    &output_name,
                    &output_directory,
                    referer,
                    size,
                    user_agent,
                    cid,
                    gcid,
                )?;
                api.start(native_id)?;
                (native_id, api.client_version.clone())
            };

            let mut task = store
                .find(task_id)
                .map_err(|_| NativeSubmitError::Failed("下载任务已不存在".into()))?;
            task.aria_gid = Some(format!("{NATIVE_GID_PREFIX}{native_id}"));
            task.followed_by_gid = None;
            task.status = TaskStatus::Downloading;
            task.error = None;
            task.status_message = Some("正在启用迅雷加速".into());
            task.failure_code = None;
            task.aria_error_code = None;
            task.total_length = resolved.size_hint();
            task.download_speed = 0;
            task.connections = 0;
            task = store
                .apply_transfer_snapshot_with_files(
                    task,
                    &[(
                        0,
                        output_path.to_string_lossy().into_owned(),
                        resolved.size_hint().unwrap_or(0),
                        0,
                        true,
                    )],
                )
                .map_err(|_| NativeSubmitError::Failed("无法保存迅雷原生任务".into()))?;
            emit_task(app, store, &task);
            self.spawn_progress_monitor(app.clone(), Arc::clone(store), task_id, output_path);
            self.spawn_vip_activation(
                app.clone(),
                Arc::clone(store),
                task_id,
                native_id,
                access_token.to_owned(),
                user_id.to_owned(),
                peer_id.to_owned(),
                client_version,
                resolved.url.clone(),
            );
            Ok(Some(task))
        }
    }

    #[cfg(windows)]
    #[allow(clippy::too_many_arguments)]
    fn spawn_vip_activation(
        self: &Arc<Self>,
        app: AppHandle,
        store: Arc<TaskStore>,
        task_id: Uuid,
        task_native_id: u32,
        access_token: String,
        user_id: String,
        peer_id: String,
        client_version: String,
        resource_url: String,
    ) {
        let backend = Arc::clone(self);
        tauri::async_runtime::spawn_blocking(move || {
            let result = (|| -> Result<(), NativeSubmitError> {
                let trace_id = {
                    let mut api = backend
                        .api
                        .lock()
                        .map_err(|_| NativeSubmitError::Incompatible)?;
                    api.as_mut()
                        .ok_or(NativeSubmitError::Incompatible)?
                        .wait_for_trace_id(task_native_id)?
                };
                let (encrypted, security_key) = request_equity_rule(NativeEquityRequest {
                    access_token: &access_token,
                    user_id: &user_id,
                    peer_id: &peer_id,
                    client_version: &client_version,
                    trace_id: &trace_id,
                    resource_url: &resource_url,
                })?;
                if !store.find(task_id).is_ok_and(|task| {
                    task.status == TaskStatus::Downloading
                        && native_id(&task) == Some(task_native_id)
                }) {
                    return Err(NativeSubmitError::Failed("迅雷原生任务已暂停".into()));
                }
                let mut api = backend
                    .api
                    .lock()
                    .map_err(|_| NativeSubmitError::Incompatible)?;
                let api = api.as_mut().ok_or(NativeSubmitError::Incompatible)?;
                let token = api.decrypt_equity_rule(&encrypted, &security_key)?;
                api.enable_vip(task_native_id, &token)?;
                backend
                    .equity_tokens
                    .lock()
                    .map_err(|_| NativeSubmitError::Incompatible)?
                    .insert(task_native_id, token);
                Ok(())
            })();

            let Ok(mut task) = store.find(task_id) else {
                return;
            };
            if native_id(&task) != Some(task_native_id) || task.status != TaskStatus::Downloading {
                return;
            }
            task.status_message = match result {
                Ok(()) if task.completed_length > 0 || task.download_speed > 0 => {
                    Some("正在使用迅雷svip加速".into())
                }
                Ok(()) => None,
                Err(NativeSubmitError::Incompatible) => Some(PLUGIN_UPDATE_MESSAGE.into()),
                Err(NativeSubmitError::Failed(message)) => {
                    Some(format!("迅雷加速启用失败，正在使用普通通道：{message}"))
                }
            };
            if let Ok(task) = store.apply_transfer_snapshot(task) {
                emit_task(&app, &store, &task);
            }
        });
    }

    pub fn control(
        self: &Arc<Self>,
        app: &AppHandle,
        store: &Arc<TaskStore>,
        task: DownloadTask,
        action: DownloadAction,
    ) -> Result<DownloadTask, String> {
        let native_id = native_id(&task).ok_or_else(|| "迅雷原生任务 ID 无效".to_string())?;
        #[cfg(not(windows))]
        {
            let _ = (app, store, action, native_id);
            return Err(PLUGIN_UPDATE_MESSAGE.into());
        }
        #[cfg(windows)]
        {
            {
                let mut api = self
                    .api
                    .lock()
                    .map_err(|_| PLUGIN_UPDATE_MESSAGE.to_string())?;
                if api.is_none() {
                    *api = WindowsApi::load(self.plugin_config_path.as_deref(), None)
                        .map_err(|error| error.user_message())?;
                }
                let api = api
                    .as_mut()
                    .ok_or_else(|| "迅雷原生下载组件未安装".to_string())?;
                match action {
                    DownloadAction::Pause => api.stop(native_id),
                    DownloadAction::Resume => (|| -> Result<(), NativeSubmitError> {
                        api.start(native_id)?;
                        if let Some(token) = self
                            .equity_tokens
                            .lock()
                            .map_err(|_| NativeSubmitError::Incompatible)?
                            .get(&native_id)
                            .cloned()
                        {
                            api.enable_vip(native_id, &token)?;
                        }
                        Ok(())
                    })(),
                    DownloadAction::Cancel => api.delete(native_id),
                }
                .map_err(|error| error.user_message())?;
            }

            if action == DownloadAction::Cancel {
                if let Ok(mut tokens) = self.equity_tokens.lock() {
                    tokens.remove(&native_id);
                }
            }

            let mut updated = task;
            match action {
                DownloadAction::Pause => {
                    updated.status = TaskStatus::Paused;
                    updated.status_message = None;
                }
                DownloadAction::Resume => {
                    updated.status = TaskStatus::Downloading;
                    updated.status_message = self
                        .equity_tokens
                        .lock()
                        .is_ok_and(|tokens| tokens.contains_key(&native_id))
                        .then(|| "正在启用迅雷加速".into());
                }
                DownloadAction::Cancel => {
                    updated.status = TaskStatus::Cancelled;
                    updated.status_message = None;
                }
            }
            updated.download_speed = 0;
            updated.error = None;
            updated.aria_error_code = None;
            let updated = store
                .apply_transfer_snapshot(updated)
                .map_err(|error| error.to_string())?;
            emit_task(app, store, &updated);
            if action == DownloadAction::Cancel {
                release_cloud_slot(app, store, &updated);
            }
            if action == DownloadAction::Resume {
                if let Some(path) = store
                    .files(updated.id)
                    .ok()
                    .and_then(|files| files.into_iter().next())
                    .map(|file| PathBuf::from(file.path))
                {
                    self.spawn_progress_monitor(app.clone(), Arc::clone(store), updated.id, path);
                }
            }
            Ok(updated)
        }
    }

    fn spawn_progress_monitor(
        self: &Arc<Self>,
        app: AppHandle,
        store: Arc<TaskStore>,
        id: Uuid,
        output_path: PathBuf,
    ) {
        let backend = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let mut previous = store
                .find(id)
                .map(|task| task.completed_length)
                .unwrap_or(0);
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let Ok(mut task) = store.find(id) else { return };
                if !is_native_gid(task.aria_gid.as_deref())
                    || task.status != TaskStatus::Downloading
                {
                    return;
                }
                let Some(native_task_id) = native_id(&task) else {
                    return;
                };
                #[cfg(windows)]
                let native_progress = backend.api.lock().ok().and_then(|api| {
                    api.as_ref()
                        .and_then(|api| api.progress(native_task_id).ok())
                });
                #[cfg(not(windows))]
                let native_progress: Option<NativeProgress> = None;
                let Some(native_progress) = native_progress else {
                    continue;
                };
                let completed = native_progress.completed;
                let native_total = native_progress.total;
                let native_status = native_progress.status;
                let native_error_code = native_progress.error_code;
                let total = (native_total > 0)
                    .then_some(native_total)
                    .or(task.total_length);
                task.total_length = total;
                let delta = completed.saturating_sub(previous);
                previous = completed;
                task.completed_length = completed;
                task.download_speed = native_progress.speed.max(delta);
                #[cfg(windows)]
                if completed > 0
                    && task.status_message.as_deref() != Some("正在使用迅雷svip加速")
                    && backend
                        .equity_tokens
                        .lock()
                        .is_ok_and(|tokens| tokens.contains_key(&native_task_id))
                {
                    task.status_message = Some("正在使用迅雷svip加速".into());
                }
                task.progress = total
                    .filter(|total| *total > 0)
                    .map(|total| ((completed.saturating_mul(100) / total).min(100)) as u8)
                    .unwrap_or(0);
                if native_status == 4 || total.is_some_and(|total| completed >= total) {
                    task.status = TaskStatus::Completed;
                    task.progress = 100;
                    task.download_speed = 0;
                    task.error = None;
                    task.status_message = None;
                    let _ = backend.stop_completed_task(&task);
                } else if native_status == 1 {
                    task.status = TaskStatus::Failed;
                    task.download_speed = 0;
                    task.error = Some(native_failure_message(native_error_code));
                    task.status_message = None;
                }
                let Ok(updated) = store.apply_transfer_snapshot_with_files(
                    task.clone(),
                    &[(
                        0,
                        output_path.to_string_lossy().into_owned(),
                        total.unwrap_or(0),
                        completed,
                        true,
                    )],
                ) else {
                    return;
                };
                emit_task(&app, &store, &updated);
                if matches!(updated.status, TaskStatus::Completed | TaskStatus::Failed) {
                    release_cloud_slot(&app, &store, &updated);
                    return;
                }
            }
        });
    }

    fn stop_completed_task(&self, task: &DownloadTask) -> Result<(), NativeSubmitError> {
        #[cfg(windows)]
        if let Some(id) = native_id(task) {
            if let Ok(mut api) = self.api.lock() {
                if let Some(api) = api.as_mut() {
                    let _ = api.stop(id);
                }
            }
        }
        Ok(())
    }
}

impl ResolvedCloudDownload {
    fn size_hint(&self) -> Option<u64> {
        self.headers.iter().find_map(|(name, value)| {
            name.eq_ignore_ascii_case(CONTENT_LENGTH_MARKER_HEADER)
                .then(|| value.parse().ok())
                .flatten()
        })
    }
}

#[cfg(windows)]
fn native_checkpoint_path(output: &std::path::Path, suffix: &str) -> PathBuf {
    let mut path = output.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

#[cfg(windows)]
fn prepare_native_output(
    output: &std::path::Path,
    allow_overwrite: bool,
    continue_download: bool,
) -> Result<(), NativeSubmitError> {
    let checkpoint = native_checkpoint_path(output, ".xltd");
    let checkpoint_config = native_checkpoint_path(output, ".xltd.cfg");
    if allow_overwrite {
        for path in [output, checkpoint.as_path(), checkpoint_config.as_path()] {
            if path.exists() {
                std::fs::remove_file(path)
                    .map_err(|_| NativeSubmitError::Failed("无法覆盖已有的迅雷下载文件".into()))?;
            }
        }
        return Ok(());
    }
    if output.exists() {
        return Err(NativeSubmitError::Failed(
            "目标文件已存在，请选择覆盖或更换下载位置".into(),
        ));
    }
    if !continue_download && (checkpoint.exists() || checkpoint_config.exists()) {
        return Err(NativeSubmitError::Failed(
            "目标位置存在未完成的迅雷文件，请继续原任务或选择覆盖".into(),
        ));
    }
    Ok(())
}

fn marker_header<'a>(resolved: &'a ResolvedCloudDownload, marker: &str) -> Option<&'a str> {
    resolved
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(marker))
        .map(|(_, value)| value.as_str())
}

fn native_id(task: &DownloadTask) -> Option<u32> {
    task.aria_gid
        .as_deref()?
        .strip_prefix(NATIVE_GID_PREFIX)?
        .parse()
        .ok()
}

fn emit_task(app: &AppHandle, store: &TaskStore, task: &DownloadTask) {
    let state = app.state::<crate::commands::AppState>();
    if crate::commands::should_emit_task_update(&state, task.is_private) {
        if let Ok(payload) = crate::commands::task_update_payload(store, None, task.clone()) {
            let _ = app.emit("download-task-updated", payload);
        }
    }
}

fn release_cloud_slot(app: &AppHandle, store: &TaskStore, task: &DownloadTask) {
    let state = app.state::<crate::commands::AppState>();
    release_cloud_admission(&state.cloud_download_dispatch, store, task);
}

fn release_cloud_admission(
    dispatch: &crate::cloud::download_dispatch::CloudDownloadDispatch,
    store: &TaskStore,
    task: &DownloadTask,
) {
    if task.kind != "cloud" {
        return;
    }
    let directory_finished = store
        .cloud_directory_group_for_task(task.id)
        .ok()
        .flatten()
        .is_some_and(|group| {
            crate::commands::cloud_directory_group_queue_task(store, group.id).is_ok_and(|root| {
                matches!(
                    root.status,
                    TaskStatus::Completed
                        | TaskStatus::Failed
                        | TaskStatus::Cancelled
                        | TaskStatus::Recycled
                )
            })
        });
    dispatch.release_terminal(task.id, directory_finished);
}

#[cfg(windows)]
struct WindowsApi {
    // The native module owns worker threads for the lifetime of the process.
    // Keep the module loaded instead of calling FreeLibrary while they may run.
    _module: usize,
    client_version: String,
    trace_ids: BTreeMap<u32, String>,
    create_p2sp_task: XlCreateP2spTask,
    delete_task: XlTaskAction,
    query_task_info: XlQueryTaskInfo,
    set_app_guid: XlSetString,
    set_global_ext_info: XlSetGlobalExtInfo,
    set_origin_connect_count: XlTaskInteger,
    set_task_equity_token: XlTaskStringKind,
    set_task_trace_id: XlTaskStringKind,
    set_task_user_agent: XlTaskString,
    set_token_mode: XlSetInteger,
    set_user_info: XlSetUserInfo,
    start_task: XlTaskAction,
    stop_task: XlTaskAction,
}

#[cfg(windows)]
type XlInit = unsafe extern "system" fn(*const std::ffi::c_char, *const u8) -> i32;
#[cfg(windows)]
type XlCreateP2spTask = unsafe extern "system" fn(
    *const u16,
    *const u16,
    *const u16,
    *const u16,
    *const u16,
    *mut u32,
) -> i32;
#[cfg(windows)]
type XlTaskAction = unsafe extern "system" fn(u32) -> i32;
#[cfg(windows)]
type XlQueryTaskInfo = unsafe extern "system" fn(u32, *mut u8) -> i32;
#[cfg(windows)]
type XlSetString = unsafe extern "system" fn(*const std::ffi::c_char) -> i32;
#[cfg(windows)]
type XlSetInteger = unsafe extern "system" fn(i32) -> i32;
#[cfg(windows)]
type XlSetUserInfo =
    unsafe extern "system" fn(*const std::ffi::c_char, *const std::ffi::c_char) -> i32;
#[cfg(windows)]
type XlSetGlobalExtInfo = unsafe extern "system" fn(*const std::ffi::c_char, u8) -> i32;
#[cfg(windows)]
type XlTaskInteger = unsafe extern "system" fn(u32, i32) -> i32;
#[cfg(windows)]
type XlTaskString = unsafe extern "system" fn(u32, *const std::ffi::c_char) -> i32;
#[cfg(windows)]
type XlTaskStringKind = unsafe extern "system" fn(u32, i32, *const std::ffi::c_char) -> i32;

#[derive(Clone, Copy, Debug)]
struct NativeProgress {
    completed: u64,
    total: u64,
    status: i32,
    error_code: i32,
    speed: u64,
}

#[cfg(windows)]
fn c_string(value: &str) -> Result<std::ffi::CString, NativeSubmitError> {
    std::ffi::CString::new(value).map_err(|_| NativeSubmitError::Incompatible)
}

#[cfg(windows)]
fn wide_string(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

#[cfg(windows)]
fn wide_os(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain([0]).collect()
}

#[cfg(windows)]
fn native_call(code: i32, message: &'static str) -> Result<(), NativeSubmitError> {
    match code {
        0 => Ok(()),
        0x4E20 => Err(NativeSubmitError::Incompatible),
        _ => Err(NativeSubmitError::Failed(format!(
            "{message}（错误码 {code}）"
        ))),
    }
}

#[cfg(windows)]
fn native_cancel_call(code: i32) -> Result<(), NativeSubmitError> {
    match code {
        // Cancellation is idempotent. After an application restart the new
        // SDK server does not know task IDs owned by the previous process.
        0 | 0x4E21 => Ok(()),
        _ => native_call(code, "迅雷原生任务取消失败"),
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or_default())
}

fn read_i32(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or_default())
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap_or_default())
}

#[cfg(windows)]
fn decrypt_equity_payload(
    encrypted: &str,
    security_key: &str,
) -> Result<String, NativeSubmitError> {
    use base64::Engine as _;
    use windows::{
        core::PCWSTR,
        Win32::Security::Cryptography::{
            BCryptCloseAlgorithmProvider, BCryptDecrypt, BCryptDestroyKey,
            BCryptGenerateSymmetricKey, BCryptOpenAlgorithmProvider, BCryptSetProperty,
            BCRYPT_AES_ALGORITHM, BCRYPT_ALG_HANDLE, BCRYPT_BLOCK_PADDING, BCRYPT_CHAINING_MODE,
            BCRYPT_HANDLE, BCRYPT_KEY_HANDLE,
        },
    };
    use zeroize::Zeroize;

    let mut ciphertext = base64::engine::general_purpose::STANDARD
        .decode(encrypted)
        .map_err(|_| NativeSubmitError::Incompatible)?;
    let mut key_bytes = security_key.as_bytes().to_vec();
    if key_bytes.len() != 16 {
        ciphertext.zeroize();
        key_bytes.zeroize();
        return Err(NativeSubmitError::Incompatible);
    }

    let mut algorithm = BCRYPT_ALG_HANDLE::default();
    let open_status = unsafe {
        BCryptOpenAlgorithmProvider(
            &mut algorithm,
            BCRYPT_AES_ALGORITHM,
            PCWSTR::null(),
            Default::default(),
        )
    };
    if open_status.0 < 0 {
        ciphertext.zeroize();
        key_bytes.zeroize();
        return Err(NativeSubmitError::Incompatible);
    }

    let mode_units: Vec<u16> = "ChainingModeCBC".encode_utf16().chain([0]).collect();
    let mode_bytes = unsafe {
        std::slice::from_raw_parts(mode_units.as_ptr().cast::<u8>(), mode_units.len() * 2)
    };
    let set_status = unsafe {
        BCryptSetProperty(
            BCRYPT_HANDLE(algorithm.0),
            BCRYPT_CHAINING_MODE,
            mode_bytes,
            0,
        )
    };
    let mut key = BCRYPT_KEY_HANDLE::default();
    let key_status = if set_status.0 >= 0 {
        unsafe { BCryptGenerateSymmetricKey(algorithm, &mut key, None, &key_bytes, 0) }
    } else {
        set_status
    };
    let result = if key_status.0 >= 0 {
        let mut iv = key_bytes.clone();
        let mut plaintext = vec![0u8; ciphertext.len()];
        let mut plaintext_len = 0u32;
        let status = unsafe {
            BCryptDecrypt(
                key,
                Some(&ciphertext),
                None,
                Some(&mut iv),
                Some(&mut plaintext),
                &mut plaintext_len,
                BCRYPT_BLOCK_PADDING,
            )
        };
        iv.zeroize();
        if status.0 >= 0 {
            plaintext.truncate(plaintext_len as usize);
            let payload: Result<serde_json::Value, _> = serde_json::from_slice(&plaintext);
            plaintext.zeroize();
            payload
                .ok()
                .and_then(|payload| {
                    payload
                        .pointer("/rule/0/id")
                        .and_then(serde_json::Value::as_str)
                        .filter(|token| !token.is_empty())
                        .map(str::to_owned)
                })
                .ok_or(NativeSubmitError::Incompatible)
        } else {
            plaintext.zeroize();
            Err(NativeSubmitError::Incompatible)
        }
    } else {
        Err(NativeSubmitError::Incompatible)
    };
    if !key.0.is_null() {
        unsafe {
            let _ = BCryptDestroyKey(key);
        }
    }
    unsafe {
        let _ = BCryptCloseAlgorithmProvider(algorithm, 0);
    }
    ciphertext.zeroize();
    key_bytes.zeroize();
    result
}

#[cfg(windows)]
impl WindowsApi {
    fn load(
        plugin_config_path: Option<&std::path::Path>,
        identity: Option<(&str, &str)>,
    ) -> Result<Option<Self>, NativeSubmitError> {
        use windows::{
            core::{PCSTR, PCWSTR},
            Win32::System::LibraryLoader::{
                GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
            },
        };

        let Some(plugin) = configured_or_installed_plugin_config(plugin_config_path) else {
            return Ok(None);
        };
        let wide_path = wide_os(plugin.sdk_path.as_os_str());
        let module = unsafe {
            LoadLibraryExW(
                PCWSTR(wide_path.as_ptr()),
                None,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
            )
        }
        .map_err(|_| NativeSubmitError::Incompatible)?;

        unsafe fn symbol<T: Copy>(
            module: windows::Win32::Foundation::HMODULE,
            name: &'static [u8],
        ) -> Result<T, NativeSubmitError> {
            let address = GetProcAddress(module, PCSTR(name.as_ptr()))
                .ok_or(NativeSubmitError::Incompatible)?;
            Ok(std::mem::transmute_copy(&address))
        }

        let init: XlInit = unsafe { symbol(module, b"XL_Init\0")? };
        let mut init_data = [0u8; 40];
        init_data[..4].copy_from_slice(&40u32.to_le_bytes());
        init_data[4..8].copy_from_slice(&(-1i32).to_le_bytes());
        let version = plugin.client_version.as_bytes();
        if version.len() > 29 {
            return Err(NativeSubmitError::Incompatible);
        }
        init_data[10..10 + version.len()].copy_from_slice(version);
        let app_key = c_string("xzcGMudGh1bmRlclg7MA^^SDK==edee53fd0b15e8d65dbfe7824f5f^a23")?;
        if unsafe { init(app_key.as_ptr(), init_data.as_ptr()) } != 0 {
            return Err(NativeSubmitError::Incompatible);
        }

        let mut api = Self {
            _module: module.0 as usize,
            client_version: plugin.client_version,
            trace_ids: BTreeMap::new(),
            create_p2sp_task: unsafe { symbol(module, b"XL_CreateP2spTask\0")? },
            delete_task: unsafe { symbol(module, b"XL_DeleteTask\0")? },
            query_task_info: unsafe { symbol(module, b"XL_QueryTaskInfo\0")? },
            set_app_guid: unsafe { symbol(module, b"XL_SetAppGuid\0")? },
            set_global_ext_info: unsafe { symbol(module, b"XL_SetGlobalExtInfo\0")? },
            set_origin_connect_count: unsafe { symbol(module, b"XL_SetOriginConnectCount\0")? },
            set_task_equity_token: unsafe { symbol(module, b"XL_SetTaskEquityToken\0")? },
            set_task_trace_id: unsafe { symbol(module, b"XL_SetTaskTraceID\0")? },
            set_task_user_agent: unsafe { symbol(module, b"XL_SetTaskUserAgent\0")? },
            set_token_mode: unsafe { symbol(module, b"XL_SetTokenMode\0")? },
            set_user_info: unsafe { symbol(module, b"XL_SetUserInfo\0")? },
            start_task: unsafe { symbol(module, b"XL_StartTask\0")? },
            stop_task: unsafe { symbol(module, b"XL_StopTask\0")? },
        };
        if let Some((user_id, peer_id)) = identity {
            api.set_identity(user_id, peer_id)?;
        }
        Ok(Some(api))
    }

    fn create_p2sp_task(
        &mut self,
        url: &str,
        _full_path: &std::path::Path,
        display_name: &str,
        save_path: &std::path::Path,
        ref_url: &str,
        file_size: u64,
        user_agent: &str,
        cid: &str,
        gcid: &str,
    ) -> Result<u32, NativeSubmitError> {
        let _ = (file_size, cid, gcid);
        let url = wide_string(url);
        let referer = wide_string(ref_url);
        let cookie = wide_string("");
        let save_path = wide_os(save_path.as_os_str());
        let name = wide_string(display_name);
        let mut task_id = 0u32;
        let result = unsafe {
            (self.create_p2sp_task)(
                url.as_ptr(),
                referer.as_ptr(),
                cookie.as_ptr(),
                save_path.as_ptr(),
                name.as_ptr(),
                &mut task_id,
            )
        };
        if result != 0 || task_id == 0 {
            return Err(NativeSubmitError::Failed(format!(
                "迅雷原生任务创建失败（错误码 {result}）"
            )));
        }
        let user_agent = c_string(user_agent)?;
        native_call(
            unsafe { (self.set_task_user_agent)(task_id, user_agent.as_ptr()) },
            "迅雷原生任务 User-Agent 设置失败",
        )?;
        native_call(
            unsafe { (self.set_origin_connect_count)(task_id, 2) },
            "迅雷原生任务连接数设置失败",
        )?;
        let trace_id = Uuid::new_v4().to_string();
        let trace = c_string(&trace_id)?;
        native_call(
            unsafe { (self.set_task_trace_id)(task_id, 0, trace.as_ptr()) },
            "迅雷原生任务加速标识设置失败",
        )?;
        self.trace_ids.insert(task_id, trace_id);
        Ok(task_id)
    }

    fn enable_vip(&mut self, id: u32, token: &str) -> Result<(), NativeSubmitError> {
        let token = c_string(token)?;
        native_call(
            unsafe { (self.set_task_equity_token)(id, -1, token.as_ptr()) },
            "迅雷会员加速令牌注入失败",
        )
    }
    fn set_identity(&mut self, user_id: &str, peer_id: &str) -> Result<(), NativeSubmitError> {
        let user_id = c_string(user_id)?;
        let empty = c_string("")?;
        let peer_id = c_string(peer_id)?;
        let ext = c_string(&format!(
            "isvip=1,viptype=super,viplevel=,userchannel=,client_peerid={}",
            peer_id.to_string_lossy()
        ))?;
        native_call(
            unsafe { (self.set_app_guid)(peer_id.as_ptr()) },
            "迅雷原生设备标识设置失败",
        )?;
        native_call(
            unsafe { (self.set_user_info)(user_id.as_ptr(), empty.as_ptr()) },
            "迅雷原生账号设置失败",
        )?;
        native_call(
            unsafe { (self.set_global_ext_info)(ext.as_ptr(), 0) },
            "迅雷会员身份设置失败",
        )?;
        native_call(
            unsafe { (self.set_token_mode)(2) },
            "迅雷会员令牌模式设置失败",
        )
    }
    fn decrypt_equity_rule(
        &mut self,
        encrypted: &str,
        security_key: &str,
    ) -> Result<String, NativeSubmitError> {
        decrypt_equity_payload(encrypted, security_key)
    }
    fn wait_for_trace_id(&mut self, id: u32) -> Result<String, NativeSubmitError> {
        self.trace_ids
            .get(&id)
            .cloned()
            .ok_or_else(|| NativeSubmitError::Failed("迅雷原生任务未生成加速标识".into()))
    }
    fn start(&mut self, id: u32) -> Result<(), NativeSubmitError> {
        native_call(unsafe { (self.start_task)(id) }, "迅雷原生任务启动失败")
    }
    fn stop(&mut self, id: u32) -> Result<(), NativeSubmitError> {
        native_call(unsafe { (self.stop_task)(id) }, "迅雷原生任务暂停失败")
    }
    fn delete(&mut self, id: u32) -> Result<(), NativeSubmitError> {
        // The official engine expects an active task to be stopped before it
        // is deleted. Ignore the stop result here: delete below is the
        // authoritative operation, and both calls may report task-not-found
        // for a stale ID restored from an earlier application process.
        let _ = unsafe { (self.stop_task)(id) };
        let result = unsafe { (self.delete_task)(id) };
        self.trace_ids.remove(&id);
        native_cancel_call(result)
    }

    fn progress(&self, id: u32) -> Result<NativeProgress, NativeSubmitError> {
        let mut info = [0u8; 924];
        info[..4].copy_from_slice(&924u32.to_le_bytes());
        native_call(
            unsafe { (self.query_task_info)(id, info.as_mut_ptr()) },
            "迅雷原生任务进度暂不可用",
        )?;
        Ok(NativeProgress {
            status: read_i32(&info, 4),
            error_code: read_i32(&info, 8),
            total: read_u64(&info, 12),
            completed: read_u64(&info, 20),
            speed: read_u32(&info, 44) as u64,
        })
    }
}

#[cfg(windows)]
fn native_failure_message(error_code: i32) -> String {
    if error_code == 0 {
        "迅雷原生任务失败，请重试".into()
    } else {
        format!("迅雷原生任务失败（错误码 {error_code}），请重试")
    }
}

#[cfg(all(windows, test))]
fn configured_or_installed_sdk_path(
    plugin_config_path: Option<&std::path::Path>,
) -> Option<PathBuf> {
    configured_or_installed_plugin_config(plugin_config_path).map(|config| config.sdk_path)
}

#[cfg(windows)]
fn configured_or_installed_plugin_config(
    plugin_config_path: Option<&std::path::Path>,
) -> Option<XunleiPluginConfig> {
    let configured = plugin_config_path
        .and_then(read_plugin_config)
        .filter(|config| {
            is_download_sdk(&config.sdk_path) && valid_client_version(&config.client_version)
        });
    if let Some(config) = configured {
        if let Some(path) = plugin_config_path {
            let _ = write_plugin_config(path, &config);
        }
        return Some(config);
    }

    let discovered = discover_installed_plugin_config()?;
    if let Some(plugin_config_path) = plugin_config_path {
        if let Err(error) = write_plugin_config(plugin_config_path, &discovered) {
            eprintln!("无法保存迅雷云盘插件配置（{plugin_config_path:?}）: {error}");
        }
    }
    Some(discovered)
}

#[cfg(windows)]
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct XunleiPluginConfig {
    sdk_path: PathBuf,
    #[serde(default)]
    client_version: String,
}

#[cfg(windows)]
fn read_plugin_config(path: &std::path::Path) -> Option<XunleiPluginConfig> {
    std::fs::read(path)
        .ok()
        .and_then(|json| serde_json::from_slice(&json).ok())
}

#[cfg(windows)]
fn write_plugin_config(path: &std::path::Path, config: &XunleiPluginConfig) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(config).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

#[cfg(windows)]
fn is_download_sdk(path: &std::path::Path) -> bool {
    path.is_file()
        && path
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("DownloadSDKProxy.dll"))
        && path.with_file_name("DownloadSDK.dll").is_file()
        && path.with_file_name("DownloadSDKServer.exe").is_file()
}

#[cfg(windows)]
fn valid_client_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 29
        && version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
}

#[cfg(windows)]
fn discover_installed_plugin_config() -> Option<XunleiPluginConfig> {
    use winreg::{
        enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE},
        RegKey,
    };

    let uninstall_roots = [
        (
            HKEY_LOCAL_MACHINE,
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
        ),
        (
            HKEY_LOCAL_MACHINE,
            "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
        ),
        (
            HKEY_CURRENT_USER,
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
        ),
    ];
    uninstall_roots.into_iter().find_map(|(hive, subkey)| {
        let root = RegKey::predef(hive).open_subkey(subkey).ok()?;
        root.enum_keys().filter_map(Result::ok).find_map(|name| {
            let key = root.open_subkey(name).ok()?;
            let display_name: String = key.get_value("DisplayName").ok()?;
            if !is_thunder_installation(&display_name) {
                return None;
            }
            let install_location: String = key.get_value("InstallLocation").ok()?;
            let client_version: String = key.get_value("DisplayVersion").ok()?;
            if !valid_client_version(&client_version) {
                return None;
            }
            sdk_path_from_install_location(std::path::Path::new(install_location.trim())).map(
                |sdk_path| XunleiPluginConfig {
                    sdk_path,
                    client_version,
                },
            )
        })
    })
}

#[cfg(windows)]
fn is_thunder_installation(display_name: &str) -> bool {
    let name = display_name.trim().to_ascii_lowercase();
    name == "迅雷" || name.contains("thunder") || name.contains("xunlei")
}

#[cfg(windows)]
fn sdk_path_from_install_location(install_location: &std::path::Path) -> Option<PathBuf> {
    let path = install_location
        .join("Program")
        .join("SDK")
        .join("DownloadSDKProxy.dll");
    is_download_sdk(&path).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_gid_is_not_an_aria_gid() {
        assert!(is_native_gid(Some("xunlei-native:42")));
        assert!(!is_native_gid(Some("0123456789abcdef")));
    }

    #[test]
    fn native_credentials_never_reach_aria2_options() {
        let resolved = ResolvedCloudDownload {
            url: "https://cdn.example/file".into(),
            name: "file.zip".into(),
            headers: vec![
                (ACCELERATION_TOKEN_MARKER_HEADER.into(), "secret".into()),
                (ACCESS_TOKEN_MARKER_HEADER.into(), "access-secret".into()),
                (USER_ID_MARKER_HEADER.into(), "user-id".into()),
                (PEER_ID_MARKER_HEADER.into(), "peer-id".into()),
                (CONTENT_LENGTH_MARKER_HEADER.into(), "42".into()),
                (CID_MARKER_HEADER.into(), "cid".into()),
                (GCID_MARKER_HEADER.into(), "gcid".into()),
            ],
        };
        let options = resolved.into_uri_submission_options();
        assert!(options.headers.is_empty());
    }

    #[test]
    fn native_progress_layout_matches_the_current_proxy_abi() {
        let mut info = [0u8; 924];
        info[4..8].copy_from_slice(&3i32.to_le_bytes());
        info[8..12].copy_from_slice(&(-7i32).to_le_bytes());
        info[12..20].copy_from_slice(&122_600_000u64.to_le_bytes());
        info[20..28].copy_from_slice(&2_500_000u64.to_le_bytes());
        info[44..48].copy_from_slice(&461_400u32.to_le_bytes());
        assert_eq!(read_i32(&info, 4), 3);
        assert_eq!(read_i32(&info, 8), -7);
        assert_eq!(read_u64(&info, 12), 122_600_000);
        assert_eq!(read_u64(&info, 20), 2_500_000);
        assert_eq!(read_u32(&info, 44), 461_400);
    }

    #[tokio::test]
    async fn cancelling_native_cloud_task_releases_its_provider_admission() {
        use crate::cloud::{
            download_dispatch::CloudDownloadDispatch, download_scheduler::AdmissionOutcome,
            CloudProvider,
        };
        use std::time::Duration;

        let store = TaskStore::open_in_memory().unwrap();
        let tasks = (0..5)
            .map(|index| {
                store
                    .create(crate::NewTask {
                        source: format!("cloud://xunlei/file-{index}"),
                        name: format!("file-{index}.zip"),
                        kind: "cloud".into(),
                        is_private: false,
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let dispatch = CloudDownloadDispatch::default();
        for task in tasks.iter().take(4) {
            assert_eq!(
                dispatch
                    .scheduler
                    .enqueue(CloudProvider::Xunlei, task.id)
                    .await,
                AdmissionOutcome::Granted
            );
        }

        let waiting_dispatch = dispatch.clone();
        let waiting_id = tasks[4].id;
        let waiting = tokio::spawn(async move {
            waiting_dispatch
                .scheduler
                .enqueue(CloudProvider::Xunlei, waiting_id)
                .await
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        let mut cancelled = tasks[0].clone();
        cancelled.status = TaskStatus::Cancelled;
        let cancelled = store.apply_transfer_snapshot(cancelled).unwrap();
        release_cloud_admission(&dispatch, &store, &cancelled);

        assert_eq!(
            tokio::time::timeout(Duration::from_millis(250), waiting)
                .await
                .expect("the queued Xunlei task should be admitted after cancellation")
                .unwrap(),
            AdmissionOutcome::Granted
        );
    }

    #[cfg(windows)]
    #[test]
    fn native_cancel_is_idempotent_for_a_stale_sdk_task_id() {
        assert!(native_cancel_call(0).is_ok());
        assert!(native_cancel_call(0x4E21).is_ok());
        assert_eq!(
            native_cancel_call(0x4E23).unwrap_err().user_message(),
            "迅雷原生任务取消失败（错误码 20003）"
        );
    }

    #[cfg(windows)]
    #[test]
    fn native_output_conflicts_require_an_explicit_resume_or_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("file.zip");
        let checkpoint = native_checkpoint_path(&output, ".xltd");
        let checkpoint_config = native_checkpoint_path(&output, ".xltd.cfg");

        std::fs::write(&output, b"complete").unwrap();
        let error = prepare_native_output(&output, false, false).unwrap_err();
        assert_eq!(
            error.user_message(),
            "目标文件已存在，请选择覆盖或更换下载位置"
        );

        std::fs::remove_file(&output).unwrap();
        std::fs::write(&checkpoint, b"partial").unwrap();
        std::fs::write(&checkpoint_config, b"checkpoint").unwrap();
        assert!(prepare_native_output(&output, false, true).is_ok());
        assert_eq!(
            prepare_native_output(&output, false, false)
                .unwrap_err()
                .user_message(),
            "目标位置存在未完成的迅雷文件，请继续原任务或选择覆盖"
        );

        prepare_native_output(&output, true, false).unwrap();
        assert!(!checkpoint.exists());
        assert!(!checkpoint_config.exists());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires an installed Thunder client"]
    fn installed_download_sdk_loads_official_proxy() {
        let root = tempfile::tempdir().unwrap();
        let plugin_config_path = root.path().join("plugins").join("xunlei-native.json");
        let api = WindowsApi::load(Some(&plugin_config_path), None)
            .expect("installed proxy should be compatible")
            .expect("Thunder should be installed");
        let config = read_plugin_config(&plugin_config_path)
            .expect("the discovered SDK path should be persisted for later calls");
        assert!(is_download_sdk(&config.sdk_path));
        drop(api);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "read-only cloud source plus an isolated native download and resume probe"]
    fn live_xunlei_native_vip_download_and_resume() {
        use crate::cloud::CredentialStore;
        use std::io::Read as _;

        assert_eq!(
            std::env::var("ALLDOWNLOADER_LIVE_CLOUD").as_deref(),
            Ok("1")
        );
        let app_data = std::env::var_os("ALLDOWNLOADER_XUNLEI_SESSION_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("APPDATA").unwrap())
                    .join("com.alldownloader.desktop")
            });
        let session_root = app_data.join("cloud-sessions");
        let load_session = || {
            if let Ok(json) = std::env::var("ALLDOWNLOADER_XUNLEI_SESSION_JSON") {
                serde_json::from_str(&json).expect("in-memory Xunlei session should be valid")
            } else {
                let credentials =
                    crate::cloud::DpapiCredentialStore::new(session_root.clone()).unwrap();
                serde_json::from_slice(
                    &credentials
                        .load("xunlei")
                        .expect("saved Xunlei session should be readable")
                        .expect("saved Xunlei session should exist"),
                )
                .unwrap()
            }
        };
        let http = crate::cloud::ReqwestTransport::new().unwrap();
        let mut client = crate::cloud::xunlei::XunleiClient::new(load_session(), &http);
        let root_directory = match client.directory("0") {
            Ok(directory) => directory,
            Err(crate::cloud::QuarkError::AuthorizationExpired)
            | Err(crate::cloud::QuarkError::NotAuthorized)
            | Err(crate::cloud::QuarkError::WebAuthorizationRequired) => {
                let credentials =
                    crate::cloud::DpapiCredentialStore::new(session_root.clone()).unwrap();
                let credential = crate::cloud::load_password_credential(
                    &credentials,
                    crate::cloud::CloudProvider::Xunlei,
                )
                .unwrap()
                .expect("expired live session should have a saved Xunlei password");
                crate::cloud::commands::recover_provider_password_session(
                    crate::cloud::CloudProvider::Xunlei,
                    &credentials,
                    &http,
                    &credential,
                )
                .expect("saved Xunlei password should silently recover the live session");
                client = crate::cloud::xunlei::XunleiClient::new(load_session(), &http);
                client
                    .directory("0")
                    .expect("recovered Xunlei account root should be readable")
            }
            Err(error) => panic!("current Xunlei account root should be readable: {error:?}"),
        };
        let refreshed_credentials =
            crate::cloud::DpapiCredentialStore::new(session_root.clone()).unwrap();
        refreshed_credentials
            .save("xunlei", &serde_json::to_vec(&client.session()).unwrap())
            .expect("the refreshed live session should be persisted for the download probe");
        let file = root_directory
            .items
            .iter()
            .filter(|file| matches!(file.kind, crate::cloud::CloudFileKind::File))
            .find(|file| file.name == "midysj.zip")
            .or_else(|| {
                root_directory
                    .items
                    .iter()
                    .filter(|file| {
                        matches!(file.kind, crate::cloud::CloudFileKind::File)
                            && file.size.unwrap_or(0) >= 1024 * 1024
                    })
                    .max_by_key(|file| file.size.unwrap_or(0))
            })
            .expect("current Xunlei account should contain a downloadable test file");
        let download = client
            .download_with_acceleration(&file.id)
            .expect("Xunlei should return a fresh cloud download contract");
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(".tools")
            .join(format!("xunlei-native-live-{}", uuid::Uuid::new_v4()));
        let output_directory = root.join("downloads");
        std::fs::create_dir_all(&output_directory).unwrap();
        let plugin_config_path = root.join("plugins").join("xunlei-native.json");
        let session = client.session();
        let peer_id = if session.web_context.peer_id.is_empty() {
            if session.web_context.device_id.is_empty() {
                &session.profile.device_id
            } else {
                &session.web_context.device_id
            }
        } else {
            &session.web_context.peer_id
        };
        let mut api =
            WindowsApi::load(Some(&plugin_config_path), Some((&session.user_id, peer_id)))
                .expect("official Xunlei proxy should load")
                .expect("Thunder should be installed");
        api.set_identity(&session.user_id, peer_id)
            .expect("native proxy should accept the current Xunlei identity");
        let output_name = "midysj-native-live.zip";
        let output_path = output_directory.join(output_name);
        eprintln!(
            "native live contract: size={}, cid_len={}, gcid_len={}",
            download.size.unwrap_or(0),
            download.cid.as_deref().map(str::len).unwrap_or(0),
            download.gcid.as_deref().map(str::len).unwrap_or(0)
        );
        let task_id = api
            .create_p2sp_task(
                &download.url,
                &output_path,
                output_name,
                &output_directory,
                "https://pan.xunlei.com/",
                download.size.unwrap_or(0),
                &client.session().profile.user_agent,
                download.cid.as_deref().unwrap_or(""),
                download.gcid.as_deref().unwrap_or(""),
            )
            .expect("official P2SP task should be created");
        eprintln!("native live task id: {task_id}");
        api.start(task_id).expect("native task should start");
        let trace_id = api
            .wait_for_trace_id(task_id)
            .expect("native task should expose its acceleration trace id");
        let client_version = api.client_version.clone();
        let token = request_equity_token(
            &mut api,
            NativeEquityRequest {
                access_token: &session.access_token,
                user_id: &session.user_id,
                peer_id,
                client_version: &client_version,
                trace_id: &trace_id,
                resource_url: &download.url,
            },
        )
        .expect("current SVIP account should receive an acceleration rule");
        api.enable_vip(task_id, &token)
            .expect("SVIP rule should be accepted");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        let paused_at = loop {
            let progress = api
                .progress(task_id)
                .expect("native progress should be readable");
            assert_ne!(progress.status, 1, "native task failed: {progress:?}");
            if progress.completed >= 512 * 1024 {
                eprintln!(
                    "native live progress before pause: {}/{}, status={}, speed={}",
                    progress.completed, progress.total, progress.status, progress.speed
                );
                api.stop(task_id).expect("native task should pause");
                break progress.completed;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "native download did not produce data"
            );
            std::thread::sleep(std::time::Duration::from_secs(1));
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        let resumed_task_id = api
            .create_p2sp_task(
                &download.url,
                &output_path,
                output_name,
                &output_directory,
                "https://pan.xunlei.com/",
                download.size.unwrap_or(0),
                &client.session().profile.user_agent,
                download.cid.as_deref().unwrap_or(""),
                download.gcid.as_deref().unwrap_or(""),
            )
            .expect("native task should reopen the existing breakpoint data");
        api.start(resumed_task_id)
            .expect("recreated native task should resume");
        let resumed_trace_id = api
            .wait_for_trace_id(resumed_task_id)
            .expect("resumed native task should expose an acceleration trace id");
        let resumed_token = request_equity_token(
            &mut api,
            NativeEquityRequest {
                access_token: &session.access_token,
                user_id: &session.user_id,
                peer_id,
                client_version: &client_version,
                trace_id: &resumed_trace_id,
                resource_url: &download.url,
            },
        )
        .expect("resumed SVIP task should receive a fresh acceleration rule");
        api.enable_vip(resumed_task_id, &resumed_token)
            .expect("SVIP rule should be restored after resume");
        let resume_deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        let resumed_at = loop {
            let progress = api
                .progress(resumed_task_id)
                .expect("resumed progress should be readable");
            assert_ne!(
                progress.status, 1,
                "resumed native task failed: {progress:?}"
            );
            if progress.completed >= paused_at + 512 * 1024 {
                eprintln!(
                    "native live progress after resume: {}/{}, status={}, speed={}",
                    progress.completed, progress.total, progress.status, progress.speed
                );
                break progress.completed;
            }
            assert!(
                std::time::Instant::now() < resume_deadline,
                "native resume made no progress"
            );
            std::thread::sleep(std::time::Duration::from_secs(1));
        };
        assert!(resumed_at > paused_at);
        api.stop(resumed_task_id).unwrap();

        let data_path = std::fs::read_dir(&output_directory)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.is_file())
            .expect("native task should create a data file");
        let mut prefix = vec![0_u8; 64 * 1024];
        let mut file = std::fs::File::open(data_path).unwrap();
        file.read_exact(&mut prefix).unwrap();
        assert!(
            prefix.iter().any(|byte| *byte != 0),
            "downloaded prefix must contain real data"
        );
        eprintln!(
            "native SVIP token, real download data and resume verified in {}",
            root.display()
        );
    }

    #[cfg(windows)]
    #[test]
    fn configured_proxy_path_is_used_without_registry_access() {
        let root = tempfile::tempdir().unwrap();
        let proxy_path = root.path().join("DownloadSDKProxy.dll");
        let engine_path = root.path().join("DownloadSDK.dll");
        let server_path = root.path().join("DownloadSDKServer.exe");
        std::fs::write(&proxy_path, b"proxy fixture").unwrap();
        std::fs::write(&engine_path, b"engine fixture").unwrap();
        std::fs::write(&server_path, b"server fixture").unwrap();
        let plugin_config_path = root.path().join("xunlei-native.json");
        std::fs::write(
            &plugin_config_path,
            serde_json::json!({
                "sdkPath": proxy_path,
                "clientVersion": "25.1.13.1634"
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(
            configured_or_installed_sdk_path(Some(&plugin_config_path)),
            Some(proxy_path)
        );
        assert!(!is_download_sdk(&engine_path));
    }
}
