use tauri::{Emitter, Manager};
use crate::cloud::CredentialStore;

pub mod aria2;
mod ai;
pub mod cloud;
mod commands;
mod connectors;
mod downloader;
mod duplicates;
mod engine_runtime;
mod launcher;
mod models;
mod parser;
mod policy;
mod processing;
mod store;
mod window_lifecycle;

pub use aria2::*;
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
pub use launcher::{
    launcher_circle_region, launcher_menu_entries, should_apply_launcher_window_visibility,
    LauncherInteractionState, LauncherRegion,
};
pub use models::{
    may_transition, AiSettings, AiSettingsInput, AppSettings, DownloadTask, DuplicateDownloadState,
    LauncherVisibility, NewTask, ProxySettings, TaskStatus,
};
pub use parser::{classify_input, extract_candidates, Candidate, CandidateKind};
pub use policy::{validate_processing, PolicyError, ProcessAction, ProcessStep};
pub use processing::{plan_processing, ProcessingHint, Resource};
pub use store::{StoreError, TaskStore};
pub use window_lifecycle::{
    manager_close_disposition, ApplicationLifecycle, ManagerCloseDisposition,
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
}

impl EventSink for TauriDownloadEvents {
    fn task_updated(&self, task: &DownloadTask) {
        if let Ok(payload) = commands::task_payload(&self.store, task.clone()) {
            let _ = self.app.emit("download-task-updated", payload);
        }
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
            let ai_secrets = cloud::DpapiCredentialStore::new(data_dir.join("ai-secrets"))?;
            // Webviews can invoke cloud commands while setup is still creating the
            // download runtime. Register this state immediately so those early
            // calls cannot race a later `manage` call.
            app.manage(cloud);
            let manifest_sidecar = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("binaries")
                .join(aria2_binary_name());
            let bundled_sidecar = app
                .path()
                .resource_dir()?
                .join("binaries")
                .join(aria2_binary_name());
            let sidecar = if manifest_sidecar.exists() {
                manifest_sidecar
            } else {
                bundled_sidecar
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
            if settings.download_directory.trim().is_empty() {
                settings.download_directory = default_destination.to_string_lossy().into_owned();
            }
            let debug_reports = settings.debug_reports;
            let launcher_visibility = settings.launcher_visibility;
            let destination = std::path::PathBuf::from(&settings.download_directory);
            std::fs::create_dir_all(&destination)?;
            let launch_config = Aria2LaunchConfig {
                binary: sidecar,
                data_dir: data_dir.clone(),
                download_dir: destination,
                parent_pid: std::process::id(),
                peer_discovery: should_enable_peer_discovery(background_ui_test.as_deref()),
            };
            let events = std::sync::Arc::new(TauriDownloadEvents {
                app: app.handle().clone(),
                store: std::sync::Arc::clone(&store),
            });
            let runtime = std::sync::Arc::new(EngineRuntime::new(
                launch_config,
                std::sync::Arc::clone(&store),
                events.clone(),
                events,
            ));
            app.manage(commands::AppState {
                store,
                runtime: std::sync::Arc::clone(&runtime),
                debug_reports: std::sync::Mutex::new(debug_reports),
                settings: std::sync::Mutex::new(settings),
                settings_path,
                ai_secrets,
            });
            app.manage(launcher::LauncherInteractionState::new(
                background_ui_test.as_deref() == Some("1"),
            ));
            if should_show_windows(background_ui_test.as_deref()) {
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
            commands::parse_download,
            commands::confirm_download_command,
            commands::list_tasks,
            commands::task_action,
            commands::resolve_duplicate_download,
            commands::open_task_directory,
            commands::remove_task_record,
            commands::recycle_task,
            commands::purge_recycled_tasks,
            commands::select_files,
            commands::get_download_engine_state,
            commands::retry_download_engine,
            commands::get_download_global_stat,
            commands::get_settings,
            commands::update_debug_reports,
            commands::update_settings,
            commands::update_ai_settings,
            commands::list_ai_models,
            cloud::commands::cloud_provider_capabilities,
            cloud::commands::cloud_begin_authorization,
            cloud::commands::cloud_complete_authorization,
            cloud::commands::cloud_login_password,
            cloud::commands::cloud_account,
            cloud::commands::cloud_list_directory,
            cloud::commands::cloud_create_directory,
            cloud::commands::cloud_rename,
            cloud::commands::cloud_move,
            cloud::commands::cloud_copy,
            cloud::commands::cloud_trash,
            cloud::commands::cloud_delete,
            cloud::commands::cloud_download_url,
            cloud::commands::cloud_upload_files,
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
        .run(tauri::generate_context!())
        .expect("error while running AllDownloader");
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
