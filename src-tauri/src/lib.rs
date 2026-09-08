use tauri::{Emitter, Manager};

pub mod aria2;
mod commands;
mod connectors;
mod downloader;
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
pub use launcher::{
    launcher_circle_region, launcher_menu_entries, LauncherInteractionState, LauncherRegion,
};
pub use models::{may_transition, DownloadTask, NewTask, TaskStatus};
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

#[tauri::command]
fn app_version_command() -> String {
    app_version()
}

pub fn run() {
    tauri::Builder::default()
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
            let destination = if background_ui_test.as_deref() == Some("1") {
                std::env::var_os("ALLDOWNLOADER_TEST_DOWNLOAD_DIR")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| data_dir.join("downloads"))
            } else {
                std::env::var_os("USERPROFILE")
                    .map(std::path::PathBuf::from)
                    .map(|path| path.join("Downloads"))
                    .unwrap_or_else(|| data_dir.join("downloads"))
            };
            let launch_config = Aria2LaunchConfig {
                binary: sidecar,
                data_dir: data_dir.clone(),
                download_dir: destination,
                parent_pid: std::process::id(),
                peer_discovery: true,
            };
            let (process, client) =
                tauri::async_runtime::block_on(Aria2Process::start(launch_config.clone()))?;
            let events = std::sync::Arc::new(TauriDownloadEvents {
                app: app.handle().clone(),
                store: std::sync::Arc::clone(&store),
            });
            let service = std::sync::Arc::new(
                DownloadService::with_process(
                    std::sync::Arc::new(client.clone()),
                    std::sync::Arc::clone(&store),
                    process,
                )
                .with_events(events),
            );
            tauri::async_runtime::block_on(service.recover())?;
            let (sync_cancel, sync_cancel_rx) = tokio::sync::watch::channel(false);
            let synchronizer = TransferSynchronizer::new(std::sync::Arc::clone(&service))
                .with_websocket(client)
                .with_restart_config(launch_config);
            tauri::async_runtime::spawn(async move {
                synchronizer.run(sync_cancel_rx).await;
            });
            app.manage(commands::AppState {
                store,
                service,
                sync_cancel,
                debug_reports: std::sync::Mutex::new(false),
            });
            app.manage(launcher::LauncherInteractionState::new(
                background_ui_test.as_deref() == Some("1"),
            ));
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
            commands::list_tasks,
            commands::task_action,
            commands::select_files,
            commands::get_settings,
            commands::update_debug_reports,
            launcher::set_launcher_compact_shape,
            launcher::start_launcher_dragging,
            launcher::show_launcher_menu,
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
