//! Windows-only bridge to the native task API shipped with an installed
//! Thunder client.  It intentionally does not start or automate that client.
//! The DLL is retained for the lifetime of the app because its P2SP task state
//! lives in the module, not in aria2.

use std::{path::PathBuf, sync::Arc};

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
pub(crate) const CONTENT_LENGTH_MARKER_HEADER: &str = "X-Downloader-Xunlei-Content-Length";
const NATIVE_GID_PREFIX: &str = "xunlei-native:";
pub(crate) const PLUGIN_UPDATE_MESSAGE: &str = "迅雷云盘插件需要更新";

pub(crate) fn is_native_gid(gid: Option<&str>) -> bool {
    gid.is_some_and(|value| value.starts_with(NATIVE_GID_PREFIX))
}

#[derive(Default)]
pub struct XunleiNativeBackend {
    #[cfg(windows)]
    api: std::sync::Mutex<Option<WindowsApi>>,
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
        let Some(token) = resolved
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(ACCELERATION_TOKEN_MARKER_HEADER))
            .map(|(_, value)| value.as_str())
        else {
            return Ok(None);
        };

        #[cfg(not(windows))]
        {
            let _ = (app, store, task_id, resolved, options, token);
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

            let native_id = {
                let mut api = self
                    .api
                    .lock()
                    .map_err(|_| NativeSubmitError::Incompatible)?;
                if api.is_none() {
                    *api = WindowsApi::load()?;
                }
                let Some(api) = api.as_mut() else {
                    return Ok(None);
                };
                let native_id = api.create_p2sp_task(
                    &resolved.url,
                    &output_path,
                    &output_name,
                    &output_directory,
                    referer,
                )?;
                api.enable_vip(native_id, token)?;
                api.start(native_id)?;
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
                    *api = WindowsApi::load().map_err(|error| error.user_message())?;
                }
                let api = api
                    .as_mut()
                    .ok_or_else(|| "迅雷原生下载组件未安装".to_string())?;
                match action {
                    DownloadAction::Pause => api.stop(native_id),
                    DownloadAction::Resume => api.start(native_id),
                    DownloadAction::Cancel => api.delete(native_id),
                }
                .map_err(|error| error.user_message())?;
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
                let completed = std::fs::metadata(&output_path)
                    .map(|meta| meta.len())
                    .unwrap_or(0);
                let total = task.total_length;
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
    module: windows::Win32::Foundation::HMODULE,
    create: unsafe extern "system" fn(*mut P2spTaskData) -> u32,
    enable_vip: unsafe extern "system" fn(u32, u32, *const std::ffi::c_char) -> i32,
    start: unsafe extern "system" fn(u32) -> i32,
    stop: unsafe extern "system" fn(u32) -> i32,
    delete: unsafe extern "system" fn(u32) -> i32,
}

// The enclosing mutex serializes every invocation and retains the module for
// the task lifetime. The raw HMODULE is never exposed beyond this wrapper.
#[cfg(windows)]
unsafe impl Send for WindowsApi {}

#[cfg(windows)]
#[repr(C)]
struct P2spTaskData {
    size: u32,
    reserved: u32,
    full_path: *const std::ffi::c_char,
    display_name: *const std::ffi::c_char,
    task_name: *const std::ffi::c_char,
    save_path: *const std::ffi::c_char,
    ref_url: *const std::ffi::c_char,
    task_id: u64,
}

#[cfg(windows)]
impl WindowsApi {
    fn load() -> Result<Option<Self>, NativeSubmitError> {
        use std::{ffi::OsStr, os::windows::ffi::OsStrExt};
        use windows::{
            core::{PCSTR, PCWSTR},
            Win32::System::LibraryLoader::{
                GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
            },
        };
        let path = thunder_sdk_path();
        let Some(path) = path else { return Ok(None) };
        let wide = OsStr::new(&path)
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let module = unsafe {
            LoadLibraryExW(
                PCWSTR(wide.as_ptr()),
                None,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
            )
        }
        .map_err(|_| NativeSubmitError::Incompatible)?;
        unsafe fn symbol<T>(
            module: windows::Win32::Foundation::HMODULE,
            name: &'static [u8],
        ) -> Result<T, NativeSubmitError> {
            let raw = GetProcAddress(module, PCSTR(name.as_ptr()))
                .ok_or(NativeSubmitError::Incompatible)?;
            Ok(std::mem::transmute_copy(&raw))
        }
        let result = unsafe {
            Ok(Self {
                module,
                create: symbol(module, b"XL_CreateP2spTask_V2\0")?,
                enable_vip: symbol(module, b"XL_EnableDcdnWithVipCert\0")?,
                start: symbol(module, b"XL_StartTask\0")?,
                stop: symbol(module, b"XL_StopTask\0")?,
                delete: symbol(module, b"XL_DeleteTask\0")?,
            })
        };
        if result.is_err() {
            unsafe {
                let _ = windows::Win32::Foundation::FreeLibrary(module);
            }
        }
        result.map(Some)
    }

    fn create_p2sp_task(
        &mut self,
        url: &str,
        full_path: &std::path::Path,
        display_name: &str,
        save_path: &std::path::Path,
        ref_url: &str,
    ) -> Result<u32, NativeSubmitError> {
        let full_path = c_string(&full_path.to_string_lossy())?;
        let display_name = c_string(display_name)?;
        let task_name = c_string(url)?;
        let save_path = c_string(&save_path.to_string_lossy())?;
        let ref_url = c_string(ref_url)?;
        let mut input = P2spTaskData {
            size: std::mem::size_of::<P2spTaskData>() as u32,
            reserved: 0,
            full_path: full_path.as_ptr(),
            display_name: display_name.as_ptr(),
            task_name: task_name.as_ptr(),
            save_path: save_path.as_ptr(),
            ref_url: ref_url.as_ptr(),
            task_id: 0,
        };
        let id = unsafe { (self.create)(&mut input) };
        let id = if id == 0 { input.task_id as u32 } else { id };
        (id != 0)
            .then_some(id)
            .ok_or_else(|| NativeSubmitError::Failed("迅雷原生任务创建失败".into()))
    }

    fn enable_vip(&mut self, id: u32, token: &str) -> Result<(), NativeSubmitError> {
        let token = c_string(token)?;
        let status = unsafe { (self.enable_vip)(id, u32::MAX, token.as_ptr()) };
        (status == 0)
            .then_some(())
            .ok_or_else(|| NativeSubmitError::Failed("迅雷 SVIP 加速授权失败".into()))
    }
    fn start(&mut self, id: u32) -> Result<(), NativeSubmitError> {
        self.result(unsafe { (self.start)(id) }, "迅雷原生任务启动失败")
    }
    fn stop(&mut self, id: u32) -> Result<(), NativeSubmitError> {
        self.result(unsafe { (self.stop)(id) }, "迅雷原生任务暂停失败")
    }
    fn delete(&mut self, id: u32) -> Result<(), NativeSubmitError> {
        self.result(unsafe { (self.delete)(id) }, "迅雷原生任务取消失败")
    }
    fn result(&self, status: i32, message: &'static str) -> Result<(), NativeSubmitError> {
        (status == 0)
            .then_some(())
            .ok_or_else(|| NativeSubmitError::Failed(message.into()))
    }
}

#[cfg(windows)]
impl Drop for WindowsApi {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::FreeLibrary(self.module);
        }
    }
}

#[cfg(windows)]
fn c_string(value: &str) -> Result<std::ffi::CString, NativeSubmitError> {
    std::ffi::CString::new(value)
        .map_err(|_| NativeSubmitError::Failed("迅雷原生接口不接受包含空字节的路径".into()))
}

#[cfg(windows)]
fn thunder_sdk_path() -> Option<PathBuf> {
    let roots = ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"];
    roots
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .map(|root| {
            root.join("Thunder Network")
                .join("Thunder")
                .join("program")
                .join("SDK")
                .join("DownloadSDK.dll")
        })
        .find(|path| path.is_file())
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
                (CONTENT_LENGTH_MARKER_HEADER.into(), "42".into()),
            ],
        };
        let options = resolved.into_uri_submission_options();
        assert!(options.headers.is_empty());
    }
}
