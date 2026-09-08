use tauri::Manager;

mod aria2_rpc;
pub mod cloud;
mod commands;
mod connectors;
mod downloader;
mod launcher;
mod models;
mod parser;
mod policy;
mod processing;
mod store;

pub use aria2_rpc::{Aria2Client, Aria2Engine, Aria2RpcError, Aria2Status};
pub use connectors::{
    CloudConnector, CloudDirectory, CloudItem, ConnectorError, MockConnector, Provider,
};
pub use downloader::{
    aria2_binary_name, confirm_download, prepare_download, DownloadError, DuplicateDownloadState,
    ParseOutcome,
};
pub use launcher::{
    launcher_circle_region, launcher_menu_entries, LauncherInteractionState, LauncherRegion,
};
pub use models::{
    may_transition, AppSettings, DownloadFileState, DownloadTask, NewTask, ProxySettings,
    StoredDownloadFile, TaskStatus,
};
pub use parser::{classify_input, extract_candidates, Candidate, CandidateKind};
pub use policy::{validate_processing, PolicyError, ProcessAction, ProcessStep};
pub use processing::{plan_processing, ProcessingHint, Resource};
pub use store::{StoreError, TaskStore};

pub fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

pub fn should_show_windows(background_ui_test: Option<&str>) -> bool {
    background_ui_test != Some("1")
}

pub fn should_hide_on_close(window_label: &str) -> bool {
    window_label == "main"
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
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if should_hide_on_close(window.label()) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let store = std::sync::Arc::new(TaskStore::open_path(&data_dir.join("tasks.sqlite"))?);
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
            let destination = std::env::var_os("USERPROFILE")
                .map(std::path::PathBuf::from)
                .map(|path| path.join("Downloads"))
                .unwrap_or_else(|| data_dir.join("downloads"));
            let settings_path = data_dir.join("settings.json");
            let mut settings: AppSettings = std::fs::read(&settings_path)
                .ok()
                .and_then(|json| serde_json::from_slice(&json).ok())
                .unwrap_or_default();
            if settings.download_directory.trim().is_empty() {
                settings.download_directory = destination.to_string_lossy().into_owned();
            }
            let engine = std::sync::Arc::new(Aria2Engine::start(sidecar, &data_dir)?);
            let cloud = std::sync::Arc::new(cloud::commands::CloudState::new(
                data_dir.join("cloud-sessions"),
            )?);
            let state = commands::AppState {
                store,
                engine,
                settings: std::sync::Mutex::new(settings),
                settings_path,
                cloud: cloud.clone(),
                recovering: Default::default(),
                cloud_refresh_attempts: Default::default(),
            };
            let background_ui_test = std::env::var("ALLDOWNLOADER_BACKGROUND_UI_TEST").ok();
            // Register every old GID before the UI can poll; resolve cloud URLs off the UI thread.
            let recovery_tasks = if background_ui_test.as_deref() != Some("1") {
                commands::prepare_recovery(&state).map_err(std::io::Error::other)?
            } else {
                Vec::new()
            };
            app.manage(state);
            app.manage(cloud);
            app.manage(launcher::LauncherInteractionState::new(
                background_ui_test.as_deref() == Some("1"),
            ));
            if !recovery_tasks.is_empty() {
                let recovery_app = app.handle().clone();
                tauri::async_runtime::spawn_blocking(move || {
                    commands::recover_downloads(
                        &recovery_app.state::<commands::AppState>(),
                        recovery_tasks,
                    );
                });
            }
            if should_show_windows(background_ui_test.as_deref()) {
                if let Some(main) = app.get_webview_window("main") {
                    main.show()?;
                }
                if let Some(launcher) = app.get_webview_window("launcher") {
                    launcher::set_launcher_compact_shape(launcher.clone(), true)
                        .map_err(std::io::Error::other)?;
                    launcher.show()?;
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_version_command,
            commands::parse_download,
            commands::confirm_download_command,
            commands::resolve_duplicate_download,
            commands::list_tasks,
            commands::task_action,
            commands::open_task_directory,
            commands::remove_task_record,
            commands::get_settings,
            commands::update_settings,
            launcher::set_launcher_compact_shape,
            launcher::start_launcher_dragging,
            launcher::show_launcher_menu,
            launcher::launcher_interaction_snapshot,
            cloud::commands::cloud_begin_authorization,
            cloud::commands::cloud_complete_authorization,
            cloud::commands::cloud_login_password,
            cloud::commands::cloud_account,
            cloud::commands::cloud_list_directory,
            cloud::commands::cloud_create_directory,
            cloud::commands::cloud_rename,
            cloud::commands::cloud_move,
            cloud::commands::cloud_trash,
            cloud::commands::cloud_download_url,
            cloud::commands::cloud_logout,
            cloud::commands::cloud_upload_files
        ])
        .run(tauri::generate_context!())
        .expect("error while running AllDownloader");
}

#[cfg(test)]
mod tests {
    use super::{should_hide_on_close, should_show_windows};

    #[test]
    fn main_window_close_is_hidden_but_launcher_close_is_not_intercepted() {
        assert!(should_hide_on_close("main"));
        assert!(!should_hide_on_close("launcher"));
    }

    #[test]
    fn background_ui_test_suppresses_initial_windows() {
        assert!(!should_show_windows(Some("1")));
        assert!(should_show_windows(None));
    }
}
