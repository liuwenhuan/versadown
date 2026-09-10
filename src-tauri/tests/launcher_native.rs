use alldownloader_lib::{
    launcher_hexagon_points, launcher_hexagon_region, launcher_live_regions, launcher_menu_entries,
    launcher_native_chrome_policy, launcher_window_style_without_frame,
    should_apply_launcher_window_visibility, tray_menu_entries, AppSettings,
    LauncherInteractionState, LauncherLiveRegions, LauncherPoint, LauncherRegion,
    LauncherVisibility,
};

#[test]
fn launcher_disables_windows_non_client_rendering_transitions_border_and_rounding() {
    let policy = launcher_native_chrome_policy();

    assert!(policy.disable_non_client_rendering);
    assert!(policy.disable_transitions);
    assert_eq!(policy.border_color, 0xffff_fffe);
    assert_eq!(policy.corner_preference, 1);
}

#[test]
fn compact_launcher_style_removes_all_native_window_decorations() {
    // Caption, system menu and resize-frame bits all make a transparent HWND render a native shell.
    assert_eq!(
        launcher_window_style_without_frame(0x14CB_0000),
        0x1400_0000
    );
}

#[test]
fn compact_region_follows_the_icon_inside_a_larger_windows_webview() {
    let region = launcher_hexagon_region((365, 202), (343, 182), 1.75);

    assert_eq!(
        region,
        LauncherRegion {
            x: 242,
            y: 80,
            width: 112,
            height: 112
        }
    );
}

#[test]
fn compact_region_matches_the_icon_hexagon_edge() {
    let region = LauncherRegion {
        x: 141,
        y: 8,
        width: 84,
        height: 84,
    };

    assert_eq!(
        launcher_hexagon_points(region),
        [
            LauncherPoint { x: 183, y: 15 },
            LauncherPoint { x: 215, y: 34 },
            LauncherPoint { x: 215, y: 66 },
            LauncherPoint { x: 183, y: 85 },
            LauncherPoint { x: 152, y: 66 },
            LauncherPoint { x: 152, y: 34 },
        ]
    );
}

#[test]
fn compact_region_keeps_the_launcher_icon_antialiasing_inside_the_native_shape() {
    let region = LauncherRegion {
        x: 0,
        y: 0,
        width: 128,
        height: 128,
    };

    assert_eq!(
        launcher_hexagon_points(region),
        [
            LauncherPoint { x: 64, y: 10 },
            LauncherPoint { x: 112, y: 39 },
            LauncherPoint { x: 112, y: 89 },
            LauncherPoint { x: 64, y: 118 },
            LauncherPoint { x: 16, y: 89 },
            LauncherPoint { x: 16, y: 39 },
        ]
    );
}

#[test]
fn live_region_contains_only_the_hexagon_and_short_speed_strip() {
    assert_eq!(
        launcher_live_regions((146, 64), (146, 64), 1.0),
        LauncherLiveRegions {
            hexagon: LauncherRegion {
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            },
            speed: LauncherRegion {
                x: 54,
                y: 20,
                width: 92,
                height: 25,
            },
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
fn tray_context_menu_matches_the_launcher_actions_when_manager_is_hidden() {
    assert_eq!(
        tray_menu_entries(false),
        [
            ("launcher.hide_manager", "显示主界面"),
            ("launcher.new_download", "新建下载"),
            ("launcher.start_all", "开始全部任务"),
            ("launcher.pause_all", "暂停全部任务"),
            ("tray.launcher.always", "总是显示"),
            ("tray.launcher.downloading", "仅下载时显示"),
            ("tray.launcher.hidden", "关闭显示"),
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
