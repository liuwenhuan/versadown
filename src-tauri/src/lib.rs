use tauri::Manager;

mod connectors;
mod commands;
mod downloader;
mod models;
mod parser;
mod policy;
mod processing;
mod store;

pub use connectors::{CloudConnector, CloudDirectory, CloudItem, ConnectorError, MockConnector, Provider};
pub use downloader::{aria2_binary_name, confirm_download, prepare_download, start_aria2, DownloadError, ParseOutcome};
pub use models::{may_transition, DownloadTask, NewTask, TaskStatus};
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

#[tauri::command]
fn app_version_command() -> String {
    app_version()
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let store = std::sync::Arc::new(TaskStore::open_path(&data_dir.join("tasks.sqlite"))?);
            let manifest_sidecar = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries").join(aria2_binary_name());
            let bundled_sidecar = app.path().resource_dir()?.join("binaries").join(aria2_binary_name());
            let sidecar = if manifest_sidecar.exists() { manifest_sidecar } else { bundled_sidecar };
            let destination = std::env::var_os("USERPROFILE").map(std::path::PathBuf::from).map(|path| path.join("Downloads")).unwrap_or_else(|| data_dir.join("downloads"));
            app.manage(commands::AppState { store, sidecar, destination, debug_reports: std::sync::Mutex::new(false) });
            let background_ui_test = std::env::var("ALLDOWNLOADER_BACKGROUND_UI_TEST").ok();
            if should_show_windows(background_ui_test.as_deref()) {
                if let Some(main) = app.get_webview_window("main") { main.show()?; }
                if let Some(launcher) = app.get_webview_window("launcher") { launcher.show()?; }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![app_version_command, commands::parse_download, commands::confirm_download_command, commands::list_tasks, commands::task_action, commands::get_settings, commands::update_debug_reports])
        .run(tauri::generate_context!())
        .expect("error while running AllDownloader");
}
