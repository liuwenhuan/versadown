use crate::cloud::CredentialStore;
use tauri::{Emitter, Manager};

mod ai;
pub mod aria2;
mod bandwidth;
pub mod cloud;
mod cloud_directory_events;
mod commands;
mod connectors;
mod downloader;
mod duplicates;
mod engine_runtime;
mod ftp;
mod launcher;
mod local_upload_picker;
mod magnet_metadata;
mod models;
mod parser;
mod policy;
mod privacy;
mod processing;
mod store;
mod update;
#[cfg(windows)]
mod window_interaction;
mod window_lifecycle;

pub use aria2::*;
pub use commands::list_queue_tasks_for_scope;
pub use connectors::{
    CloudConnector, CloudDirectory, CloudItem, ConnectorError, MockConnector, Provider,
};
pub use downloader::{
    aria2_binary_name, confirm_download, prepare_download, DownloadError, ParseOutcome,
    ParseResource,
};
pub use duplicates::{duplicate_state, retry_duplicate_state};
pub use engine_runtime::{
    EngineAvailability, EngineRuntime, EngineStateSink, EngineUnavailableError,
};
pub use ftp::{parse_ftp_url, FtpDownloadEngine, FtpDownloadError, FtpDownloadLimits, FtpTarget};
pub use launcher::{
    launcher_hexagon_points, launcher_hexagon_region, launcher_live_regions, launcher_menu_entries,
    launcher_native_chrome_policy, launcher_window_style_without_frame,
    should_apply_launcher_window_visibility, tray_menu_entries, LauncherInteractionState,
    LauncherLiveRegions, LauncherNativeChromePolicy, LauncherPoint, LauncherRegion,
};
pub use magnet_metadata::{
    resolve_magnet_metadata, resolve_magnet_torrent, MagnetMetadataError, ResolvedMagnetFile,
    ResolvedMagnetMetadata, ResolvedMagnetTorrent,
};
pub use models::{
    may_transition, AiSettings, AiSettingsInput, AppSettings, Aria2Settings, DownloadTask,
    DuplicateDownloadState, FtpSegment, LauncherVisibility, MagnetMetadataSettings, NewTask,
    ProxySettings, TaskStatus,
};
pub use parser::{
    classify_input, extract_candidates, extract_cloud_share_candidate, scan_download_inputs,
    validate_download_input, validate_task_source, Candidate, CandidateKind, CloudShareCandidate,
    InputValidationError, ScannedInput, ValidatedCandidate,
};
pub use policy::{validate_processing, PolicyError, ProcessAction, ProcessStep};
pub use processing::{plan_processing, ProcessingHint, Resource};
pub use store::{IncompatibleTaskPurgeReport, StoreError, TaskStore};
pub use window_lifecycle::{
    manager_close_disposition, should_prevent_implicit_exit, ApplicationLifecycle,
    ManagerCloseDisposition,
};

pub fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

pub fn should_show_windows(background_ui_test: Option<&str>) -> bool {
    background_ui_test != Some("1")
}

pub fn should_enable_peer_discovery(background_ui_test: Option<&str>) -> bool {
    background_ui_test != Some("1")
}

struct TauriDownloadEvents {
    app: tauri::AppHandle,
    store: std::sync::Arc<TaskStore>,
    cloud: std::sync::Arc<cloud::commands::CloudState>,
    directory_refreshes: std::sync::Arc<cloud_directory_events::CloudDirectoryRefreshes>,
    cloud_download_dispatch: cloud::download_dispatch::CloudDownloadDispatch,
}

impl TauriDownloadEvents {
    fn schedule_directory_refresh(&self, root: uuid::Uuid) {
        if !self.directory_refreshes.queue(root) {
            return;
        }
        let refreshes = std::sync::Arc::clone(&self.directory_refreshes);
        let app = self.app.clone();
        let store = std::sync::Arc::clone(&self.store);
        tauri::async_runtime::spawn(async move {
            refreshes
                .flush_pending(|roots| {
                    let app = app.clone();
                    let store = std::sync::Arc::clone(&store);
                    async move {
                        let _ = tauri::async_runtime::spawn_blocking(move || {
                            for root in roots {
                                if let Ok(payload) =
                                    commands::cloud_directory_group_queue_task(&store, root)
                                {
                                    // Recheck privacy after the delayed projection:
                                    // the user may have locked the session meanwhile.
                                    let visible = app
                                        .try_state::<commands::AppState>()
                                        .map(|state| {
                                            commands::should_emit_task_update(
                                                &state,
                                                payload.is_private,
                                            )
                                        })
                                        .unwrap_or(!payload.is_private);
                                    if visible {
                                        let _ = app.emit(
                                            "download-task-updated",
                                            commands::TaskUpdatePayload::Queue(payload),
                                        );
                                    }
                                }
                            }
                        })
                        .await;
                    }
                })
                .await;
        });
    }
}

impl EventSink for TauriDownloadEvents {
    fn task_updated(&self, task: &DownloadTask) {
        if matches!(
            task.status,
            TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Recycled
        ) && self
            .store
            .find(task.id)
            .is_ok_and(|current| current.status == task.status && current.aria_gid == task.aria_gid)
        {
            self.cloud_download_dispatch.scheduler.release(task.id);
            self.cloud_download_dispatch.scheduler.cancel(task.id);
        }
        if let Ok(Some(group)) = self.store.cloud_directory_group_for_task(task.id) {
            self.schedule_directory_refresh(group.id);
        } else if let Ok(payload) =
            commands::task_update_payload(&self.store, Some(&self.cloud.share_plans), task.clone())
        {
            let visible = self
                .app
                .try_state::<commands::AppState>()
                .map(|state| commands::should_emit_task_update(&state, task.is_private))
                .unwrap_or(!task.is_private);
            if visible {
                let _ = self.app.emit("download-task-updated", payload);
            }
        }
        commands::schedule_cloud_download_refresh(self.app.clone(), task);
        cloud::commands::schedule_cloud_share_cleanup(self.app.clone(), task);
    }

    fn global_stat_updated(&self, stat: &Aria2GlobalStat) {
        let _ = self.app.emit("download-global-stat-updated", stat);
    }
}

impl EngineStateSink for TauriDownloadEvents {
    fn engine_state_updated(&self, state: &EngineAvailability) {
        let _ = self.app.emit("download-engine-state-updated", state);
    }
}

#[tauri::command]
fn app_version_command() -> String {
    app_version()
}

#[tauri::command]
async fn check_for_app_update() -> Result<Option<update::AppUpdate>, String> {
    let current_version = app_version();
    tauri::async_runtime::spawn_blocking(move || update::fetch_app_update(&current_version))
        .await
        .map_err(|error| format!("版本检测任务失败：{error}"))?
}

#[tauri::command]
async fn main_window_interaction_state(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    #[cfg(windows)]
    {
        window_interaction::snapshot(app).await
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        Ok(serde_json::json!({ "active": false, "nativeDrag": false }))
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .on_menu_event(|app, event| launcher::handle_launcher_menu_action(app, event.id().as_ref()))
        .on_window_event(|window, event| {
            if let Some(lifecycle) = window
                .app_handle()
                .try_state::<std::sync::Arc<ApplicationLifecycle>>()
            {
                window_lifecycle::handle_manager_window_event(
                    window,
                    event,
                    lifecycle.inner().as_ref(),
                );
            }
        })
        .setup(|app| {
            let lifecycle = std::sync::Arc::new(ApplicationLifecycle::new());
            app.manage(std::sync::Arc::clone(&lifecycle));
            let background_ui_test = std::env::var("ALLDOWNLOADER_BACKGROUND_UI_TEST").ok();
            let data_dir = if background_ui_test.as_deref() == Some("1") {
                std::env::var_os("ALLDOWNLOADER_TEST_DATA_DIR")
                    .map(std::path::PathBuf::from)
                    .ok_or("后台界面测试必须提供隔离的数据目录")?
            } else {
                app.path().app_data_dir()?
            };
            std::fs::create_dir_all(&data_dir)?;
            let store = std::sync::Arc::new(TaskStore::open_path(&data_dir.join("tasks.sqlite"))?);
            let cloud = std::sync::Arc::new(cloud::commands::CloudState::new(
                data_dir.join("cloud-sessions"),
            )?);
            commands::purge_tianyi_local_state(&store, &cloud).map_err(std::io::Error::other)?;
            store.recover_interrupted_cloud_directories()?;
            let ai_secrets = cloud::DpapiCredentialStore::new(data_dir.join("ai-secrets"))?;
            // Webviews can invoke cloud commands while setup is still creating the
            // download runtime. Register this state immediately so those early
            // calls cannot race a later `manage` call.
            app.manage(std::sync::Arc::clone(&cloud));
            cloud.start_authorization_maintenance();
            let manifest_sidecar = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("binaries")
                .join(aria2_binary_name());
            let bundled_sidecar = app
                .path()
                .resource_dir()?
                .join("binaries")
                .join(aria2_binary_name());
            let sidecar = if bundled_sidecar.exists() {
                bundled_sidecar
            } else {
                manifest_sidecar
            };
            let default_destination = if background_ui_test.as_deref() == Some("1") {
                std::env::var_os("ALLDOWNLOADER_TEST_DOWNLOAD_DIR")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| data_dir.join("downloads"))
            } else {
                std::env::var_os("USERPROFILE")
                    .map(std::path::PathBuf::from)
                    .map(|path| path.join("Downloads"))
                    .unwrap_or_else(|| data_dir.join("downloads"))
            };
            let settings_path = data_dir.join("settings.json");
            let mut settings: AppSettings = std::fs::read(&settings_path)
                .ok()
                .and_then(|json| serde_json::from_slice(&json).ok())
                .unwrap_or_default();
            settings.ai.api_key_configured = ai_secrets
                .load(ai::API_KEY_SECRET_NAME)
                .map_err(|_| "无法读取本机保存的 AI API Key")?
                .is_some();
            settings.privacy_password_configured =
                privacy::password_is_configured(&ai_secrets).map_err(|_| "无法读取隐私模式密码")?;
            if settings.download_directory.trim().is_empty() {
                settings.download_directory = default_destination.to_string_lossy().into_owned();
            }
            settings.aria2.ensure_rpc_token();
            settings.aria2.validate().map_err(std::io::Error::other)?;
            std::fs::write(&settings_path, serde_json::to_vec_pretty(&settings)?)?;
            let launcher_visibility = settings.launcher_visibility;
            let cloud_upload_limit = settings.cloud_upload_limit;
            let destination = std::path::PathBuf::from(&settings.download_directory);
            std::fs::create_dir_all(&destination)?;
            let launch_config = Aria2LaunchConfig {
                binary: sidecar,
                data_dir: data_dir.clone(),
                download_dir: destination,
                parent_pid: std::process::id(),
                peer_discovery: should_enable_peer_discovery(background_ui_test.as_deref()),
                aria2: settings.aria2.clone(),
            };
            let cloud_download_dispatch =
                cloud::download_dispatch::CloudDownloadDispatch::default();
            let events = std::sync::Arc::new(TauriDownloadEvents {
                app: app.handle().clone(),
                store: std::sync::Arc::clone(&store),
                cloud,
                directory_refreshes: Default::default(),
                cloud_download_dispatch: cloud_download_dispatch.clone(),
            });
            let runtime = std::sync::Arc::new(EngineRuntime::new(
                launch_config,
                std::sync::Arc::clone(&store),
                events.clone(),
                events,
            ));
            runtime.set_cloud_upload_limit(cloud_upload_limit);
            app.manage(commands::AppState {
                store,
                runtime: std::sync::Arc::clone(&runtime),
                settings: std::sync::Mutex::new(settings),
                settings_path,
                ai_secrets,
                privacy_session: Default::default(),
                cloud_download_sources: Default::default(),
                cloud_download_dispatch,
            });
            app.manage(launcher::LauncherInteractionState::new(
                background_ui_test.as_deref() == Some("1"),
            ));
            #[cfg(windows)]
            if let Some(main) = app.get_webview_window("main") {
                window_interaction::install(&main).map_err(std::io::Error::other)?;
            }
            if should_show_windows(background_ui_test.as_deref()) {
                launcher::setup_manager_tray(app.handle()).map_err(std::io::Error::other)?;
                if let Some(main) = app.get_webview_window("main") {
                    main.show()?;
                }
                if launcher_visibility == LauncherVisibility::Always {
                    if let Some(launcher) = app.get_webview_window("launcher") {
                        launcher::set_launcher_compact_shape(launcher.clone(), true)
                            .map_err(std::io::Error::other)?;
                        launcher.show()?;
                    }
                }
            }
            tauri::async_runtime::spawn(async move {
                let _ = runtime.initialize().await;
                runtime.start_monitor(std::time::Duration::from_secs(2));
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_version_command,
            check_for_app_update,
            local_upload_picker::local_upload_entries,
            main_window_interaction_state,
            commands::scan_download_inputs,
            commands::parse_download,
            commands::discard_prepared_download,
            commands::confirm_cloud_share_download,
            commands::confirm_download_command,
            commands::set_task_private,
            commands::list_tasks,
            commands::task_action,
            commands::resolve_duplicate_download,
            commands::open_task_directory,
            commands::open_task_file,
            commands::play_task_videos,
            commands::remove_task_record,
            commands::recycle_task,
            commands::purge_recycled_tasks,
            commands::select_files,
            commands::cancel_download_file,
            commands::get_download_engine_state,
            commands::retry_download_engine,
            commands::get_download_global_stat,
            commands::get_settings,
            commands::get_privacy_task_visibility,
            commands::configure_privacy_download,
            commands::unlock_privacy_tasks,
            commands::lock_privacy_tasks,
            commands::update_settings,
            commands::update_ai_settings,
            commands::list_ai_models,
            cloud::commands::cloud_provider_capabilities,
            cloud::commands::cloud_provider_catalog,
            cloud::commands::cloud_begin_authorization,
            cloud::commands::cloud_complete_authorization,
            cloud::commands::cloud_open_operation_verification,
            cloud::commands::cloud_completed_authorization,
            cloud::commands::resume_reauthorized_provider_tasks,
            cloud::commands::cloud_reauthorize_failed_task,
            cloud::commands::cloud_login_password,
            cloud::commands::cloud_saved_password_credentials,
            cloud::commands::cloud_tianyi_session_shape_for_test,
            cloud::commands::cloud_tianyi_resolve_download_for_test,
            cloud::commands::cloud_account,
            cloud::commands::cloud_account_capacity,
            cloud::commands::cloud_list_directory,
            cloud::commands::cloud_create_share,
            cloud::commands::cloud_list_shares,
            cloud::commands::cloud_cancel_shares,
            cloud::commands::cloud_create_directory,
            cloud::commands::cloud_rename,
            cloud::commands::cloud_move,
            cloud::commands::cloud_copy,
            cloud::commands::cloud_trash,
            cloud::commands::cloud_delete,
            cloud::commands::cloud_restore,
            cloud::commands::cloud_download_url,
            cloud::commands::cloud_download_directory,
            cloud::commands::cloud_upload_files,
            cloud::commands::cloud_upload_directory,
            cloud::commands::cloud_control_upload,
            cloud::commands::cloud_logout,
            launcher::set_launcher_compact_shape,
            launcher::set_launcher_bounds,
            launcher::set_launcher_window_visible,
            launcher::start_launcher_dragging,
            launcher::show_launcher_menu,
            launcher::run_launcher_menu_action,
            launcher::launcher_manager_visible,
            launcher::launcher_interaction_snapshot,
            launcher::hide_manager_for_test,
            launcher::manager_window_snapshot,
            launcher::activate_launcher_menu_for_test,
            window_lifecycle::request_native_manager_close_for_test,
            window_lifecycle::request_native_launcher_close_for_test,
            window_lifecycle::launcher_visible_for_test,
            window_lifecycle::foreground_window_handle_for_test,
            window_lifecycle::manager_close_request_count_for_test
        ])
        .build(tauri::generate_context!())
        .expect("error while building AllDownloader")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { code, api, .. } = event {
                let application_exiting = app
                    .try_state::<std::sync::Arc<ApplicationLifecycle>>()
                    .is_some_and(|lifecycle| lifecycle.is_exiting());
                if should_prevent_implicit_exit(application_exiting, code) {
                    api.prevent_exit();
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::{should_enable_peer_discovery, should_show_windows};

    #[test]
    fn background_ui_test_is_hidden_and_network_isolated() {
        assert!(!should_show_windows(Some("1")));
        assert!(!should_enable_peer_discovery(Some("1")));
    }

    #[test]
    fn production_keeps_windows_and_peer_discovery_enabled() {
        assert!(should_show_windows(None));
        assert!(should_enable_peer_discovery(None));
    }
}
