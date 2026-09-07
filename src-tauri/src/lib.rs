mod connectors;
mod models;
mod parser;
mod policy;
mod processing;
mod store;

pub use connectors::{CloudConnector, CloudDirectory, CloudItem, ConnectorError, MockConnector, Provider};
pub use models::{may_transition, DownloadTask, NewTask, TaskStatus};
pub use parser::{classify_input, extract_candidates, Candidate, CandidateKind};
pub use policy::{validate_processing, PolicyError, ProcessAction, ProcessStep};
pub use processing::{plan_processing, ProcessingHint, Resource};
pub use store::{StoreError, TaskStore};

pub fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
fn app_version_command() -> String {
    app_version()
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![app_version_command])
        .run(tauri::generate_context!())
        .expect("error while running AllDownloader");
}
