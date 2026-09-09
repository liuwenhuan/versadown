use alldownloader_lib::{
    launcher_circle_region, launcher_menu_entries, should_apply_launcher_window_visibility,
    AppSettings, LauncherInteractionState, LauncherRegion, LauncherVisibility,
};

#[test]
fn compact_region_follows_the_ball_inside_a_larger_windows_webview() {
    let region = launcher_circle_region((236, 98), (214, 86), 1.75);

    assert_eq!(
        region,
        LauncherRegion {
            x: 141,
            y: 8,
            width: 84,
            height: 84
        }
    );
}

#[test]
fn launcher_context_menu_contains_the_required_actions() {
    assert_eq!(
        launcher_menu_entries(),
        [
            ("launcher.hide_manager", "隐藏主界面"),
            ("launcher.new_download", "新建下载"),
            ("launcher.start_all", "开始全部任务"),
            ("launcher.pause_all", "暂停全部任务"),
            ("launcher.settings", "悬浮球显示"),
            ("launcher.quit", "退出万能下载"),
        ]
    );
}

#[test]
fn background_interaction_state_records_drag_and_menu_without_native_ui() {
    let state = LauncherInteractionState::new(true);
    state.record_drag();
    state.record_menu();

    let snapshot = state.snapshot();
    assert!(state.is_background_test());
    assert_eq!(snapshot.drag_requests, 1);
    assert_eq!(snapshot.menu_requests, 1);
}

#[test]
fn legacy_settings_default_to_always_showing_the_launcher() {
    let settings: AppSettings = serde_json::from_str(
        r#"{
            "debugReports": false,
            "downloadDirectory": "C:\\Downloads",
            "proxy": {
                "enabled": false,
                "protocol": "http",
                "host": "",
                "port": 7890,
                "username": "",
                "password": ""
            }
        }"#,
    )
    .expect("legacy settings should remain readable");

    assert_eq!(settings.launcher_visibility, LauncherVisibility::Always);
}

#[test]
fn application_exit_rejects_late_launcher_show_requests() {
    assert!(should_apply_launcher_window_visibility(false, true));
    assert!(should_apply_launcher_window_visibility(true, false));
    assert!(!should_apply_launcher_window_visibility(true, true));
}
