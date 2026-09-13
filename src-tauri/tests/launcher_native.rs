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
    // The actual client origin need not be half the outer/inner size difference.
    let region = launcher_hexagon_region((7, 13), (343, 182), 1.75);

    assert_eq!(
        region,
        LauncherRegion {
            x: 238,
            y: 83,
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
            LauncherPoint { x: 183, y: 17 },
            LauncherPoint { x: 212, y: 36 },
            LauncherPoint { x: 212, y: 64 },
            LauncherPoint { x: 183, y: 83 },
            LauncherPoint { x: 154, y: 64 },
            LauncherPoint { x: 154, y: 36 },
        ]
    );
}

#[test]
fn compact_region_does_not_expose_pixels_outside_the_css_clip() {
    let region = LauncherRegion {
        x: 0,
        y: 0,
        width: 128,
        height: 128,
    };

    assert_eq!(
        launcher_hexagon_points(region),
        [
            LauncherPoint { x: 64, y: 13 },
            LauncherPoint { x: 109, y: 41 },
            LauncherPoint { x: 109, y: 87 },
            LauncherPoint { x: 64, y: 115 },
            LauncherPoint { x: 19, y: 87 },
            LauncherPoint { x: 19, y: 41 },
        ]
    );
}

#[test]
fn live_region_contains_only_the_hexagon_and_short_speed_strip() {
    assert_eq!(
        launcher_live_regions((0, 0), (146, 64), 1.0),
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
fn launcher_clip_stays_inside_the_visible_polygon_at_common_dpi_scales() {
    // Independently specified CSS geometry; native points must never expand it.
    let polygon = [
        (50.0, 9.0),
        (86.0, 31.0),
        (86.0, 69.0),
        (50.0, 91.0),
        (14.0, 69.0),
        (14.0, 31.0),
    ];
    for scale in [1.0, 1.01, 1.1, 1.25, 1.3, 1.5, 1.55, 1.75, 2.0, 2.5, 3.0] {
        let region = launcher_hexagon_region((3, 7), (640, 480), scale);
        for point in launcher_hexagon_points(region) {
            // CSS retains the fractional 64 * scale size and anchors bottom/right.
            let diameter = 64.0 * scale;
            let left = f64::from(region.x + region.width) - diameter;
            let top = f64::from(region.y + region.height) - diameter;
            let x = (f64::from(point.x) - left) * 100.0 / diameter;
            let y = (f64::from(point.y) - top) * 100.0 / diameter;
            for edge in 0..6 {
                let (ax, ay) = polygon[edge];
                let (bx, by) = polygon[(edge + 1) % 6];
                assert!(
                    (bx - ax) * (y - ay) - (by - ay) * (x - ax) >= -0.000001,
                    "native point {point:?} leaks past clip at scale {scale}"
                );
            }
        }
    }
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
