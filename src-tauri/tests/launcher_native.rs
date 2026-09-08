use alldownloader_lib::{
    launcher_circle_region, launcher_menu_entries, LauncherInteractionState, LauncherRegion,
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
            ("launcher.open_manager", "打开下载管理器"),
            ("launcher.settings", "设置"),
            ("launcher.quit", "退出"),
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
