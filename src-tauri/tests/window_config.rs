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
fn main_window_opens_with_the_compact_desktop_dimensions() {
    let window = main_window();

    assert_eq!(window["width"], 900);
    assert_eq!(window["height"], 575);
    assert_eq!(window["minWidth"], 720);
    assert_eq!(window["minHeight"], 500);
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
