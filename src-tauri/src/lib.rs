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
