use serde_json::Value;

fn main_window() -> Value {
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tauri.conf.json"))
        .expect("tauri configuration should be readable");
    let config: Value =
        serde_json::from_str(&raw).expect("tauri configuration should be valid JSON");
    config["app"]["windows"]
        .as_array()
        .and_then(|windows| windows.iter().find(|window| window["label"] == "main"))
        .cloned()
        .expect("main window configuration should exist")
}

fn launcher_window() -> Value {
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tauri.conf.json"))
        .expect("tauri configuration should be readable");
    let config: Value =
        serde_json::from_str(&raw).expect("tauri configuration should be valid JSON");
    config["app"]["windows"]
        .as_array()
        .and_then(|windows| windows.iter().find(|window| window["label"] == "launcher"))
        .cloned()
        .expect("launcher window configuration should exist")
}

#[test]
fn main_window_uses_the_requested_960_by_640_minimum() {
    let window = main_window();

    assert_eq!(window["width"], 960);
    assert_eq!(window["height"], 640);
    assert_eq!(window["minWidth"], 960);
    assert_eq!(window["minHeight"], 640);
}

#[test]
fn launcher_window_leaves_room_for_the_context_menu_and_its_shadow() {
    let window = launcher_window();

    assert!(window["maxWidth"].as_u64().unwrap_or_default() >= 304);
    assert!(window["maxHeight"].as_u64().unwrap_or_default() >= 392);
    assert_eq!(window["title"], "");
    assert_eq!(window["decorations"], false);
    assert_eq!(window["shadow"], false);
}

fn config_windows(file: &str) -> Vec<Value> {
    let raw = std::fs::read_to_string(format!("{}/{file}", env!("CARGO_MANIFEST_DIR")))
        .expect("tauri configuration should be readable");
    let config: Value =
        serde_json::from_str(&raw).expect("tauri configuration should be valid JSON");
    config["app"]["windows"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn linux_creates_the_launcher_in_code_from_the_shared_window_config() {
    // Tauri replaces the whole `windows` array when merging platform configs,
    // so the Linux copy must stay identical apart from deferring the launcher
    // (it is built at runtime to share the manager's web process).
    let base = config_windows("tauri.conf.json");
    let linux = config_windows("tauri.linux.conf.json");
    let expected: Vec<Value> = base
        .into_iter()
        .map(|mut window| {
            if window["label"] == "launcher" {
                window["create"] = Value::Bool(false);
            }
            window
        })
        .collect();

    assert_eq!(linux, expected);
}

#[test]
fn launcher_canvas_leaves_a_popup_slot_on_both_sides_of_the_icon() {
    // Mirrors `launcherCanvas` in src/lib/launcherWindow.ts: a 346px popup
    // slot, the 64px icon, then another 346px slot.
    let window = launcher_window();

    assert_eq!(window["width"], 346 * 2 + 64);
    assert_eq!(window["height"], 720);
}
