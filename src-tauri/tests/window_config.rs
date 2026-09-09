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

#[test]
fn main_window_opens_with_the_compact_desktop_dimensions() {
    let window = main_window();

    assert_eq!(window["width"], 900);
    assert_eq!(window["height"], 575);
    assert_eq!(window["minWidth"], 720);
    assert_eq!(window["minHeight"], 500);
}
