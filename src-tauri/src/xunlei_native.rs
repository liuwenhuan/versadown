//! Windows-only bridge to the native task API shipped with an installed
//! Thunder client.  It intentionally does not start or automate that client.
//! The DLL is retained for the lifetime of the app because its P2SP task state
//! lives in the module, not in aria2.

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::Arc,
};

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
    #[cfg(windows)]
    helper_path: Option<PathBuf>,
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
            #[cfg(windows)]
            helper_path: None,
        }
    }
}

impl XunleiNativeBackend {
    pub(crate) fn with_plugin_paths(plugin_config_path: PathBuf, helper_path: PathBuf) -> Self {
        Self {
            #[cfg(windows)]
            api: std::sync::Mutex::new(None),
            #[cfg(windows)]
            equity_tokens: std::sync::Mutex::new(BTreeMap::new()),
            #[cfg(windows)]
            plugin_config_path: Some(plugin_config_path),
            #[cfg(windows)]
            helper_path: Some(helper_path),
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
    api.decrypt_equity_rule(
        envelope
            .get("data")
            .and_then(serde_json::Value::as_str)
            .ok_or(NativeSubmitError::Incompatible)?,
        &security_key,
    )
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

            let native_id = {
                let mut api = self
                    .api
                    .lock()
                    .map_err(|_| NativeSubmitError::Incompatible)?;
                if api.is_none() {
                    *api = WindowsApi::load(
                        self.plugin_config_path.as_deref(),
                        self.helper_path.as_deref(),
                        Some((user_id, peer_id)),
                    )?;
                }
                let Some(api) = api.as_mut() else {
                    return Ok(None);
                };
                let size = resolved.size_hint().unwrap_or(0);
                let reusable = api.reusable_task(&output_directory, &output_name, size)?;
                let native_id = if let Some((native_id, status)) = reusable {
                    if status == 5 {
                        api.stop(native_id)?;
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    }
                    native_id
                } else {
                    let failed =
                        api.failed_zero_data_tasks(&output_directory, &output_name, size)?;
                    for failed_id in &failed {
                        api.delete(*failed_id)?;
                    }
                    if !failed.is_empty() {
                        std::thread::sleep(std::time::Duration::from_millis(300));
                        remove_failed_artifacts(&output_path, size)?;
                    }
                    api.create_p2sp_task(
                        &resolved.url,
                        &output_path,
                        &output_name,
                        &output_directory,
                        referer,
                        size,
                        user_agent,
                        cid,
                        gcid,
                    )?
                };
                api.start(native_id)?;
                let trace_id = api.wait_for_trace_id(native_id)?;
                let client_version = api.client_version.clone();
                let token = request_equity_token(
                    api,
                    NativeEquityRequest {
                        access_token,
                        user_id,
                        peer_id,
                        client_version: &client_version,
                        trace_id: &trace_id,
                        resource_url: &resolved.url,
                    },
                )?;
                api.enable_vip(native_id, &token)?;
                self.equity_tokens
                    .lock()
                    .map_err(|_| NativeSubmitError::Incompatible)?
                    .insert(native_id, token);
                native_id
            };

            let mut task = store
                .find(task_id)
                .map_err(|_| NativeSubmitError::Failed("下载任务已不存在".into()))?;
            task.aria_gid = Some(format!("{NATIVE_GID_PREFIX}{native_id}"));
            task.followed_by_gid = None;
            task.status = TaskStatus::Downloading;
            task.error = None;
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
            Ok(Some(task))
        }
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
                    *api = WindowsApi::load(
                        self.plugin_config_path.as_deref(),
                        self.helper_path.as_deref(),
                        None,
                    )
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
                DownloadAction::Pause => updated.status = TaskStatus::Paused,
                DownloadAction::Resume => updated.status = TaskStatus::Downloading,
                DownloadAction::Cancel => updated.status = TaskStatus::Cancelled,
            }
            updated.download_speed = 0;
            updated.error = None;
            updated.aria_error_code = None;
            let updated = store
                .apply_transfer_snapshot(updated)
                .map_err(|error| error.to_string())?;
            emit_task(app, store, &updated);
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
            let mut previous = 0_u64;
            let mut unchanged = 0_u8;
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
                let native_progress = backend
                    .api
                    .lock()
                    .ok()
                    .and_then(|api| api.as_ref()?.progress(native_task_id).ok());
                #[cfg(not(windows))]
                let native_progress: Option<(u64, u64, i32)> = None;
                let Some((completed, native_total, _native_status)) = native_progress else {
                    continue;
                };
                let total = (native_total > 0)
                    .then_some(native_total)
                    .or(task.total_length);
                task.total_length = total;
                let delta = completed.saturating_sub(previous);
                previous = completed;
                unchanged = if delta == 0 {
                    unchanged.saturating_add(1)
                } else {
                    0
                };
                task.completed_length = completed;
                task.download_speed = delta;
                task.progress = total
                    .filter(|total| *total > 0)
                    .map(|total| ((completed.saturating_mul(100) / total).min(100)) as u8)
                    .unwrap_or(0);
                if total.is_some_and(|total| completed >= total) {
                    task.status = TaskStatus::Completed;
                    task.progress = 100;
                    task.download_speed = 0;
                    task.error = None;
                    let _ = backend.stop_completed_task(&task);
                } else if unchanged >= 120 {
                    task.status = TaskStatus::Failed;
                    task.download_speed = 0;
                    task.error = Some("迅雷原生任务长时间未返回数据，请重试".into());
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
    if task.kind != "cloud" {
        return;
    }
    let state = app.state::<crate::commands::AppState>();
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
    state
        .cloud_download_dispatch
        .release_terminal(task.id, directory_finished);
}

#[cfg(windows)]
struct WindowsApi {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_request_id: u64,
    state_directory: PathBuf,
    client_version: String,
}

#[cfg(windows)]
impl WindowsApi {
    fn load(
        plugin_config_path: Option<&std::path::Path>,
        helper_path: Option<&std::path::Path>,
        identity: Option<(&str, &str)>,
    ) -> Result<Option<Self>, NativeSubmitError> {
        let Some(plugin) = configured_or_installed_plugin_config(plugin_config_path) else {
            return Ok(None);
        };
        let sdk_path = plugin.sdk_path;
        let Some(program_directory) = sdk_path.parent().and_then(std::path::Path::parent) else {
            return Err(NativeSubmitError::Incompatible);
        };
        let addon_path = program_directory.join("dk_addon.node");
        let thunder_path = program_directory.join("Thunder.exe");
        if !addon_path.is_file() || !thunder_path.is_file() {
            return Err(NativeSubmitError::Incompatible);
        }

        let config_directory = plugin_config_path
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&config_directory).map_err(|_| NativeSubmitError::Incompatible)?;
        let helper_path = helper_path
            .filter(|path| path.is_file())
            .ok_or(NativeSubmitError::Incompatible)?;
        let state_directory = config_directory.join("xunlei-native-state");

        use std::os::windows::process::CommandExt;
        let mut command = Command::new(&thunder_path);
        command
            .arg(helper_path)
            .current_dir(program_directory)
            .env("ELECTRON_RUN_AS_NODE", "1")
            .env("XUNLEI_DK_ADDON", &addon_path)
            .env("XUNLEI_NATIVE_STATE_DIR", &state_directory)
            .env("XUNLEI_CLIENT_VERSION", &plugin.client_version)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000);
        if let Some((user_id, peer_id)) = identity {
            command
                .env("XUNLEI_NATIVE_USER_ID", user_id)
                .env("XUNLEI_NATIVE_PEER_ID", peer_id);
        }
        let mut child = command
            .spawn()
            .map_err(|_| NativeSubmitError::Incompatible)?;
        let input = child.stdin.take().ok_or(NativeSubmitError::Incompatible)?;
        let output = child.stdout.take().ok_or(NativeSubmitError::Incompatible)?;
        let mut api = Self {
            child,
            input,
            output: BufReader::new(output),
            next_request_id: 1,
            state_directory,
            client_version: plugin.client_version,
        };
        let ready = api.read_message()?;
        if ready.get("ready").and_then(serde_json::Value::as_bool) != Some(true) {
            eprintln!(
                "迅雷原生 helper 初始化失败：{}",
                ready
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("incompatible")
            );
            return Err(NativeSubmitError::Incompatible);
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
        let response = self.request(serde_json::json!({
            "action": "create",
            "url": url,
            "savePath": save_path,
            "name": display_name,
            "referer": ref_url,
            "fileSize": file_size,
            "userAgent": user_agent,
            "cid": cid,
            "gcid": gcid,
        }))?;
        response
            .as_u64()
            .and_then(|id| u32::try_from(id).ok())
            .filter(|id| *id != 0)
            .ok_or_else(|| NativeSubmitError::Failed("迅雷原生任务创建失败".into()))
    }

    fn reusable_task(
        &self,
        save_path: &std::path::Path,
        display_name: &str,
        file_size: u64,
    ) -> Result<Option<(u32, i32)>, NativeSubmitError> {
        let database = self.state_directory.join("TaskDb.dat");
        reusable_task_in_database(&database, save_path, display_name, file_size)
    }

    fn failed_zero_data_tasks(
        &self,
        save_path: &std::path::Path,
        display_name: &str,
        file_size: u64,
    ) -> Result<Vec<u32>, NativeSubmitError> {
        let database = self.state_directory.join("TaskDb.dat");
        failed_zero_data_tasks_in_database(&database, save_path, display_name, file_size)
    }

    fn enable_vip(&mut self, id: u32, token: &str) -> Result<(), NativeSubmitError> {
        self.request(serde_json::json!({
            "action": "enableVip",
            "taskId": id,
            "token": token,
        }))?;
        Ok(())
    }
    fn decrypt_equity_rule(
        &mut self,
        encrypted: &str,
        security_key: &str,
    ) -> Result<String, NativeSubmitError> {
        self.request(serde_json::json!({
            "action": "decryptRule",
            "data": encrypted,
            "key": security_key,
        }))?
        .as_str()
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .ok_or(NativeSubmitError::Incompatible)
    }
    fn wait_for_trace_id(&mut self, id: u32) -> Result<String, NativeSubmitError> {
        for _ in 0..100 {
            let value = self.request(serde_json::json!({
                "action": "taskInfo",
                "taskId": id,
            }))?;
            if let Some(trace_id) = value
                .get("traceId")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
            {
                return Ok(trace_id.to_owned());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Err(NativeSubmitError::Failed(
            "迅雷原生任务未生成加速标识".into(),
        ))
    }
    fn start(&mut self, id: u32) -> Result<(), NativeSubmitError> {
        self.task_action("start", id, "迅雷原生任务启动失败")
    }
    fn stop(&mut self, id: u32) -> Result<(), NativeSubmitError> {
        self.task_action("stop", id, "迅雷原生任务暂停失败")
    }
    fn delete(&mut self, id: u32) -> Result<(), NativeSubmitError> {
        self.task_action("delete", id, "迅雷原生任务取消失败")
    }

    fn progress(&self, id: u32) -> Result<(u64, u64, i32), NativeSubmitError> {
        let database = self.state_directory.join("TaskDb.dat");
        let connection = rusqlite::Connection::open_with_flags(
            database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| NativeSubmitError::Failed("迅雷原生任务进度暂不可用".into()))?;
        connection
            .query_row(
                "SELECT TotalReceiveSize, ResourceSize, Status FROM TaskBase WHERE TaskId=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|_| NativeSubmitError::Failed("迅雷原生任务进度暂不可用".into()))
    }

    #[cfg(test)]
    fn accelerated_bytes(&self, id: u32) -> Result<(u64, u64), NativeSubmitError> {
        let database = self.state_directory.join("TaskDb.dat");
        let connection = rusqlite::Connection::open_with_flags(
            database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| NativeSubmitError::Failed("迅雷原生任务进度暂不可用".into()))?;
        connection
            .query_row(
                "SELECT VipReceiveSize, FreeDcdnReceiveSize FROM TaskBase WHERE TaskId=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| NativeSubmitError::Failed("迅雷原生任务进度暂不可用".into()))
    }

    #[cfg(test)]
    fn vip_speed(&mut self, id: u32) -> Result<u64, NativeSubmitError> {
        self.request(serde_json::json!({
            "action": "vipSpeed",
            "taskId": id,
        }))?
        .as_u64()
        .ok_or(NativeSubmitError::Incompatible)
    }

    fn task_action(
        &mut self,
        action: &'static str,
        id: u32,
        message: &'static str,
    ) -> Result<(), NativeSubmitError> {
        self.request(serde_json::json!({"action": action, "taskId": id}))
            .map(|_| ())
            .map_err(|error| match error {
                NativeSubmitError::Incompatible => error,
                NativeSubmitError::Failed(_) => NativeSubmitError::Failed(message.into()),
            })
    }

    fn request(
        &mut self,
        mut request: serde_json::Value,
    ) -> Result<serde_json::Value, NativeSubmitError> {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        request["id"] = request_id.into();
        serde_json::to_writer(&mut self.input, &request)
            .and_then(|_| self.input.write_all(b"\n").map_err(serde_json::Error::io))
            .and_then(|_| self.input.flush().map_err(serde_json::Error::io))
            .map_err(|_| NativeSubmitError::Incompatible)?;
        let response = self.read_message()?;
        if response.get("id").and_then(serde_json::Value::as_u64) != Some(request_id) {
            return Err(NativeSubmitError::Incompatible);
        }
        if response.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            return Err(NativeSubmitError::Failed("迅雷原生下载调用失败".into()));
        }
        Ok(response
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }

    fn read_message(&mut self) -> Result<serde_json::Value, NativeSubmitError> {
        let mut line = String::new();
        if self
            .output
            .read_line(&mut line)
            .map_err(|_| NativeSubmitError::Incompatible)?
            == 0
        {
            return Err(NativeSubmitError::Incompatible);
        }
        serde_json::from_str(&line).map_err(|_| NativeSubmitError::Incompatible)
    }
}

#[cfg(windows)]
fn reusable_task_in_database(
    database: &std::path::Path,
    save_path: &std::path::Path,
    display_name: &str,
    file_size: u64,
) -> Result<Option<(u32, i32)>, NativeSubmitError> {
    use rusqlite::OptionalExtension;

    if !database.is_file() {
        return Ok(None);
    }
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| NativeSubmitError::Failed("迅雷原生任务状态暂不可用".into()))?;
    connection
        .query_row(
            "SELECT TaskId, Status FROM TaskBase
             WHERE SavePath=?1 AND Name=?2 AND (?3=0 OR ResourceSize=?3)
               AND (Status IN (5, 7) OR (Status=9 AND TotalReceiveSize>0))
             ORDER BY TotalReceiveSize DESC, TaskId DESC LIMIT 1",
            rusqlite::params![save_path.to_string_lossy(), display_name, file_size],
            |row| Ok((row.get::<_, u32>(0)?, row.get::<_, i32>(1)?)),
        )
        .optional()
        .map_err(|_| NativeSubmitError::Failed("迅雷原生任务状态暂不可用".into()))
}

#[cfg(windows)]
fn failed_zero_data_tasks_in_database(
    database: &std::path::Path,
    save_path: &std::path::Path,
    display_name: &str,
    file_size: u64,
) -> Result<Vec<u32>, NativeSubmitError> {
    if !database.is_file() {
        return Ok(Vec::new());
    }
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| NativeSubmitError::Failed("迅雷原生任务状态暂不可用".into()))?;
    let mut statement = connection
        .prepare(
            "SELECT TaskId FROM TaskBase
             WHERE SavePath=?1 AND Name=?2 AND (?3=0 OR ResourceSize=?3)
               AND Status=9 AND TotalReceiveSize=0
             ORDER BY TaskId",
        )
        .map_err(|_| NativeSubmitError::Failed("迅雷原生任务状态暂不可用".into()))?;
    let rows = statement
        .query_map(
            rusqlite::params![save_path.to_string_lossy(), display_name, file_size],
            |row| row.get::<_, u32>(0),
        )
        .map_err(|_| NativeSubmitError::Failed("迅雷原生任务状态暂不可用".into()))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|_| NativeSubmitError::Failed("迅雷原生任务状态暂不可用".into()))
}

#[cfg(windows)]
fn remove_failed_artifacts(
    output_path: &std::path::Path,
    expected_size: u64,
) -> Result<(), NativeSubmitError> {
    if let Ok(metadata) = std::fs::metadata(output_path) {
        if !metadata.is_file()
            || (metadata.len() != 0 && (expected_size == 0 || metadata.len() != expected_size))
        {
            return Err(NativeSubmitError::Failed(
                "目标文件已存在，无法清理迅雷失败任务的占位文件".into(),
            ));
        }
        std::fs::remove_file(output_path)
            .map_err(|_| NativeSubmitError::Failed("无法清理迅雷失败任务的占位文件".into()))?;
    }

    for suffix in [".xltd", ".xltd.cfg"] {
        let mut temporary = output_path.as_os_str().to_owned();
        temporary.push(suffix);
        let temporary = PathBuf::from(temporary);
        if temporary.is_file() {
            std::fs::remove_file(temporary)
                .map_err(|_| NativeSubmitError::Failed("无法清理迅雷失败任务的临时文件".into()))?;
        }
    }
    Ok(())
}

#[cfg(windows)]
impl Drop for WindowsApi {
    fn drop(&mut self) {
        let _ = self.request(serde_json::json!({"action": "shutdown"}));
        let _ = self.child.kill();
        let _ = self.child.wait();
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
    if configured.is_some() {
        return configured;
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
            .is_some_and(|name| name.eq_ignore_ascii_case("DownloadSDK.dll"))
}

#[cfg(windows)]
fn valid_client_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 32
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
        .join("DownloadSDK.dll");
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

    #[cfg(windows)]
    #[test]
    fn retry_reuses_the_most_progressed_native_task_for_the_same_destination() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("TaskDb.dat");
        let save_path = root.path().join("下载目录");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE TaskBase (
                    TaskId INTEGER PRIMARY KEY,
                    Status INTEGER NOT NULL,
                    SavePath TEXT NOT NULL,
                    Name TEXT NOT NULL,
                    ResourceSize INTEGER NOT NULL,
                    TotalReceiveSize INTEGER NOT NULL
                );",
            )
            .unwrap();
        for (id, status, received) in [(10, 5, 1024), (11, 7, 4096), (12, 9, 0), (13, 9, 8192)] {
            connection
                .execute(
                    "INSERT INTO TaskBase VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![
                        id,
                        status,
                        save_path.to_string_lossy(),
                        "文件.zip",
                        65_536_u64,
                        received
                    ],
                )
                .unwrap();
        }
        drop(connection);

        assert_eq!(
            reusable_task_in_database(&database, &save_path, "文件.zip", 65_536).unwrap(),
            Some((13, 9))
        );
        assert_eq!(
            failed_zero_data_tasks_in_database(&database, &save_path, "文件.zip", 65_536).unwrap(),
            vec![12]
        );
        assert_eq!(
            reusable_task_in_database(&database, &save_path, "其他.zip", 65_536).unwrap(),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn failed_artifact_cleanup_removes_native_temporary_files_but_preserves_real_data() {
        let root = tempfile::tempdir().unwrap();
        let placeholder = root.path().join("placeholder.bin");
        let file = std::fs::File::create(&placeholder).unwrap();
        file.set_len(65_536).unwrap();
        drop(file);
        std::fs::write(
            root.path().join("placeholder.bin.xltd"),
            b"sparse placeholder",
        )
        .unwrap();
        std::fs::write(root.path().join("placeholder.bin.xltd.cfg"), b"metadata").unwrap();

        remove_failed_artifacts(&placeholder, 65_536).unwrap();
        assert!(!placeholder.exists());
        assert!(!root.path().join("placeholder.bin.xltd").exists());
        assert!(!root.path().join("placeholder.bin.xltd.cfg").exists());

        std::fs::write(root.path().join("placeholder.bin.xltd"), b"native-only").unwrap();
        std::fs::write(root.path().join("placeholder.bin.xltd.cfg"), b"metadata").unwrap();
        remove_failed_artifacts(&placeholder, 65_536).unwrap();
        assert!(!root.path().join("placeholder.bin.xltd").exists());
        assert!(!root.path().join("placeholder.bin.xltd.cfg").exists());

        std::fs::write(&placeholder, b"real data").unwrap();
        assert!(remove_failed_artifacts(&placeholder, 65_536).is_err());
        assert_eq!(std::fs::read(&placeholder).unwrap(), b"real data");
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires an installed Thunder client"]
    fn installed_download_sdk_starts_in_isolated_helper() {
        let root = tempfile::tempdir().unwrap();
        let plugin_config_path = root.path().join("plugins").join("xunlei-native.json");
        let helper_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join("xunlei-native-helper.cjs");
        let api = WindowsApi::load(Some(&plugin_config_path), Some(&helper_path), None)
            .expect("installed helper should be compatible")
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
        let session: crate::cloud::xunlei::XunleiSession = if let Ok(json) =
            std::env::var("ALLDOWNLOADER_XUNLEI_SESSION_JSON")
        {
            serde_json::from_str(&json).expect("in-memory Xunlei session should be valid")
        } else {
            let credentials =
                crate::cloud::DpapiCredentialStore::new(app_data.join("cloud-sessions")).unwrap();
            serde_json::from_slice(
                &credentials
                    .load("xunlei")
                    .expect("saved Xunlei session should be readable")
                    .expect("saved Xunlei session should exist"),
            )
            .unwrap()
        };
        let http = crate::cloud::ReqwestTransport::new().unwrap();
        let client = crate::cloud::xunlei::XunleiClient::new(session, &http);
        let root_directory = client
            .directory("0")
            .expect("current Xunlei account root should be readable");
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
        let helper_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join("xunlei-native-helper.cjs");
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
        let mut api = WindowsApi::load(
            Some(&plugin_config_path),
            Some(&helper_path),
            Some((&session.user_id, peer_id)),
        )
        .expect("official Xunlei helper should start")
        .expect("Thunder should be installed");
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
        let reusable = api
            .reusable_task(&output_directory, output_name, download.size.unwrap_or(0))
            .expect("native task database should be readable")
            .expect("the running native destination should be reusable");
        assert_eq!(reusable.0, task_id);
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
            let (received, total, status) = api.progress(task_id).unwrap_or((0, 0, 0));
            if received >= 512 * 1024 {
                eprintln!("native live progress before pause: {received}/{total}, status={status}");
                api.stop(task_id).expect("native task should pause");
                break received;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "native download did not produce data"
            );
            std::thread::sleep(std::time::Duration::from_secs(1));
        };
        std::thread::sleep(std::time::Duration::from_secs(3));
        api.start(task_id).expect("native task should resume");
        api.enable_vip(task_id, &token)
            .expect("SVIP rule should be restored after resume");
        let resumed_at = loop {
            let (received, total, status) = api.progress(task_id).unwrap_or((0, 0, 0));
            if received >= paused_at + 512 * 1024 {
                eprintln!("native live progress after resume: {received}/{total}, status={status}");
                break received;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "native resume made no progress"
            );
            std::thread::sleep(std::time::Duration::from_secs(1));
        };
        assert!(resumed_at > paused_at);

        let acceleration_deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        let (vip_bytes, dcdn_bytes, vip_speed) = loop {
            let counters = api.accelerated_bytes(task_id).unwrap_or((0, 0));
            let vip_speed = api.vip_speed(task_id).unwrap_or(0);
            if counters.0 > 0 || counters.1 > 0 || vip_speed > 0 {
                break (counters.0, counters.1, vip_speed);
            }
            assert!(
                std::time::Instant::now() < acceleration_deadline,
                "official native task downloaded data but never used the VIP/DCDN channel"
            );
            std::thread::sleep(std::time::Duration::from_secs(1));
        };
        eprintln!(
            "native acceleration verified: vip={vip_bytes}, dcdn={dcdn_bytes}, vip_speed={vip_speed}"
        );
        api.stop(task_id).unwrap();

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
        eprintln!("native live download verified in {}", root.display());
    }

    #[cfg(windows)]
    #[test]
    fn configured_sdk_path_is_used_without_registry_access() {
        let root = tempfile::tempdir().unwrap();
        let sdk_path = root.path().join("DownloadSDK.dll");
        std::fs::write(&sdk_path, b"fixture").unwrap();
        let plugin_config_path = root.path().join("xunlei-native.json");
        write_plugin_config(
            &plugin_config_path,
            &XunleiPluginConfig {
                sdk_path: sdk_path.clone(),
                client_version: "25.1.13.1634".into(),
            },
        )
        .unwrap();

        assert_eq!(
            configured_or_installed_sdk_path(Some(&plugin_config_path)),
            Some(sdk_path)
        );
    }
}
