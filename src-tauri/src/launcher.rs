use serde::Serialize;
use std::sync::atomic::{AtomicUsize, Ordering};

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WebviewWindow,
};

pub const HIDE_MANAGER: &str = "launcher.hide_manager";
pub const NEW_DOWNLOAD: &str = "launcher.new_download";
pub const START_ALL: &str = "launcher.start_all";
pub const PAUSE_ALL: &str = "launcher.pause_all";
pub const OPEN_SETTINGS: &str = "launcher.settings";
pub const QUIT: &str = "launcher.quit";
pub const TRAY_LAUNCHER_ALWAYS: &str = "tray.launcher.always";
pub const TRAY_LAUNCHER_DOWNLOADING: &str = "tray.launcher.downloading";
pub const TRAY_LAUNCHER_HIDDEN: &str = "tray.launcher.hidden";
const TRAY_ID: &str = "manager-tray";
const LAUNCHER_HEXAGON_SIZE: f64 = 64.0;
const LAUNCHER_LIVE_SPEED_LEFT: f64 = 54.0;
const LAUNCHER_LIVE_SPEED_TOP: f64 = 20.0;
const LAUNCHER_LIVE_SPEED_HEIGHT: f64 = 25.0;
// WS_OVERLAPPEDWINDOW: caption, system menu, thick frame and min/max boxes.
// Leaving any of these bits on a transparent HWND lets Windows draw a caption or pale frame.
const LAUNCHER_NATIVE_DECORATION_STYLE_BITS: i32 = 0x00CF_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LauncherRegion {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LauncherLiveRegions {
    pub hexagon: LauncherRegion,
    pub speed: LauncherRegion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LauncherPoint {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LauncherNativeChromePolicy {
    pub disable_non_client_rendering: bool,
    pub disable_transitions: bool,
    pub border_color: u32,
    pub corner_preference: i32,
}

pub const fn launcher_native_chrome_policy() -> LauncherNativeChromePolicy {
    LauncherNativeChromePolicy {
        disable_non_client_rendering: true,
        disable_transitions: true,
        border_color: 0xffff_fffe,
        corner_preference: 1,
    }
}

pub fn launcher_window_style_without_frame(style: i32) -> i32 {
    style & !LAUNCHER_NATIVE_DECORATION_STYLE_BITS
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LauncherInteractionSnapshot {
    pub drag_requests: usize,
    pub menu_requests: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerWindowSnapshot {
    pub exists: bool,
    pub visible: bool,
    pub minimized: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitWindowSnapshot {
    pub manager_visible: bool,
    pub launcher_visible: bool,
}

pub struct LauncherInteractionState {
    background_test: bool,
    drag_requests: AtomicUsize,
    menu_requests: AtomicUsize,
}

pub struct TrayMenuState {
    manager_item: MenuItem<tauri::Wry>,
}

impl LauncherInteractionState {
    pub fn new(background_test: bool) -> Self {
        Self {
            background_test,
            drag_requests: AtomicUsize::new(0),
            menu_requests: AtomicUsize::new(0),
        }
    }

    pub fn is_background_test(&self) -> bool {
        self.background_test
    }

    pub fn record_drag(&self) {
        self.drag_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_menu(&self) {
        self.menu_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> LauncherInteractionSnapshot {
        LauncherInteractionSnapshot {
            drag_requests: self.drag_requests.load(Ordering::Relaxed),
            menu_requests: self.menu_requests.load(Ordering::Relaxed),
        }
    }
}

pub fn launcher_menu_entries() -> [(&'static str, &'static str); 6] {
    [
        (HIDE_MANAGER, "隐藏主界面"),
        (NEW_DOWNLOAD, "新建下载"),
        (START_ALL, "开始全部任务"),
        (PAUSE_ALL, "暂停全部任务"),
        (OPEN_SETTINGS, "悬浮球显示"),
        (QUIT, "退出万能下载"),
    ]
}

pub fn tray_menu_entries(manager_visible: bool) -> [(&'static str, &'static str); 8] {
    [
        (HIDE_MANAGER, tray_manager_item_label(manager_visible)),
        (NEW_DOWNLOAD, "新建下载"),
        (START_ALL, "开始全部任务"),
        (PAUSE_ALL, "暂停全部任务"),
        (TRAY_LAUNCHER_ALWAYS, "总是显示"),
        (TRAY_LAUNCHER_DOWNLOADING, "仅下载时显示"),
        (TRAY_LAUNCHER_HIDDEN, "关闭显示"),
        (QUIT, "退出万能下载"),
    ]
}

fn tray_manager_item_label(manager_visible: bool) -> &'static str {
    if manager_visible {
        "隐藏主界面"
    } else {
        "显示主界面"
    }
}

pub fn setup_manager_tray(app: &AppHandle) -> Result<(), String> {
    let manager_visible = app
        .get_webview_window("main")
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false);
    let entries = tray_menu_entries(manager_visible);
    let manager_item = MenuItem::with_id(app, entries[0].0, entries[0].1, true, None::<&str>)
        .map_err(|error| error.to_string())?;
    let new_download = MenuItem::with_id(app, entries[1].0, entries[1].1, true, None::<&str>)
        .map_err(|error| error.to_string())?;
    let start_all = MenuItem::with_id(app, entries[2].0, entries[2].1, true, None::<&str>)
        .map_err(|error| error.to_string())?;
    let pause_all = MenuItem::with_id(app, entries[3].0, entries[3].1, true, None::<&str>)
        .map_err(|error| error.to_string())?;
    let always = MenuItem::with_id(app, entries[4].0, entries[4].1, true, None::<&str>)
        .map_err(|error| error.to_string())?;
    let downloading = MenuItem::with_id(app, entries[5].0, entries[5].1, true, None::<&str>)
        .map_err(|error| error.to_string())?;
    let hidden = MenuItem::with_id(app, entries[6].0, entries[6].1, true, None::<&str>)
        .map_err(|error| error.to_string())?;
    let quit = MenuItem::with_id(app, entries[7].0, entries[7].1, true, None::<&str>)
        .map_err(|error| error.to_string())?;
    let first_separator = PredefinedMenuItem::separator(app).map_err(|error| error.to_string())?;
    let second_separator = PredefinedMenuItem::separator(app).map_err(|error| error.to_string())?;
    let visibility_menu = Submenu::with_id_and_items(
        app,
        "tray.launcher.visibility",
        "悬浮球显示",
        true,
        &[&always, &downloading, &hidden],
    )
    .map_err(|error| error.to_string())?;
    let menu = Menu::with_items(
        app,
        &[
            &manager_item,
            &new_download,
            &start_all,
            &pause_all,
            &first_separator,
            &visibility_menu,
            &second_separator,
            &quit,
        ],
    )
    .map_err(|error| error.to_string())?;
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| "应用图标不可用".to_string())?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("万能下载")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                reveal_manager(tray.app_handle(), None);
            }
        })
        .build(app)
        .map_err(|error| error.to_string())?;
    app.manage(TrayMenuState { manager_item });
    Ok(())
}

pub fn set_tray_manager_visibility(app: &AppHandle, manager_visible: bool) {
    if let Some(state) = app.try_state::<TrayMenuState>() {
        let _ = state
            .manager_item
            .set_text(tray_manager_item_label(manager_visible));
    }
}

fn background_ui_test_enabled() -> bool {
    std::env::var("ALLDOWNLOADER_BACKGROUND_UI_TEST").as_deref() == Ok("1")
}

fn require_background_ui_test() -> Result<(), String> {
    if background_ui_test_enabled() {
        Ok(())
    } else {
        Err("窗口观测仅在后台测试模式可用".to_string())
    }
}

pub fn launcher_hexagon_region(
    client_origin: (i32, i32),
    inner: (u32, u32),
    scale_factor: f64,
) -> LauncherRegion {
    let diameter = (LAUNCHER_HEXAGON_SIZE * scale_factor).round() as i32;
    let (client_x, client_y) = client_origin;
    LauncherRegion {
        x: client_x + inner.0 as i32 - diameter,
        y: client_y + inner.1 as i32 - diameter,
        width: diameter,
        height: diameter,
    }
}

pub fn launcher_hexagon_points(region: LauncherRegion) -> [LauncherPoint; 6] {
    static HEXAGON: std::sync::LazyLock<[[f64; 2]; 6]> = std::sync::LazyLock::new(|| {
        serde_json::from_str(include_str!("../../src/assets/launcher/hexagon.json"))
            .expect("bundled launcher hexagon must contain six percentage coordinates")
    });
    // Match the SVG shell exactly so removing the native clip for click menus
    // does not make the launcher appear to grow.
    let scale = |coordinate: f64, size: i32| (coordinate * f64::from(size) / 100.0).round() as i32;
    HEXAGON.map(|[x, y]| LauncherPoint {
        x: region.x + scale(x, region.width),
        y: region.y + scale(y, region.height),
    })
}

pub fn launcher_live_regions(
    client_origin: (i32, i32),
    inner: (u32, u32),
    scale_factor: f64,
) -> LauncherLiveRegions {
    let diameter = (LAUNCHER_HEXAGON_SIZE * scale_factor).round() as i32;
    let speed_left = (LAUNCHER_LIVE_SPEED_LEFT * scale_factor).round() as i32;
    let speed_top = (LAUNCHER_LIVE_SPEED_TOP * scale_factor).round() as i32;
    let speed_height = (LAUNCHER_LIVE_SPEED_HEIGHT * scale_factor).round() as i32;
    let (client_x, client_y) = client_origin;
    LauncherLiveRegions {
        hexagon: LauncherRegion {
            x: client_x,
            y: client_y,
            width: diameter,
            height: diameter,
        },
        speed: LauncherRegion {
            x: client_x + speed_left,
            y: client_y + speed_top,
            width: inner.0 as i32 - speed_left,
            height: speed_height,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_a_64px_compact_hexagon_region() {
        let region = launcher_hexagon_region((0, 0), (64, 64), 1.0);

        assert_eq!(
            region,
            LauncherRegion {
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            }
        );
    }
}

#[tauri::command]
// Compatibility only until the fixed-canvas React state machine lands; Task 7 removes this path.
pub fn set_launcher_compact_shape(
    window: WebviewWindow,
    compact: bool,
) -> Result<Option<LauncherRegion>, String> {
    #[cfg(windows)]
    {
        return apply_launcher_shape(&window, compact, false, true);
    }

    #[cfg(not(windows))]
    {
        let _ = (window, compact);
        Ok(None)
    }
}

#[cfg(windows)]
fn disable_launcher_dwm_chrome(hwnd: windows::Win32::Foundation::HWND) -> Result<(), String> {
    use std::ffi::c_void;
    use windows::{
        core::BOOL,
        Win32::Graphics::Dwm::{
            DwmSetWindowAttribute, DWMNCRP_DISABLED, DWMWA_BORDER_COLOR, DWMWA_NCRENDERING_POLICY,
            DWMWA_TRANSITIONS_FORCEDISABLED, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
            DWMWINDOWATTRIBUTE,
        },
    };

    let policy = launcher_native_chrome_policy();
    let non_client_policy = DWMNCRP_DISABLED;
    let transitions_disabled = BOOL::from(policy.disable_transitions);
    let border_color = policy.border_color;
    let corner_preference = DWMWCP_DONOTROUND;
    let set_attribute = |attribute: DWMWINDOWATTRIBUTE, value: *const c_void, size: usize| unsafe {
        DwmSetWindowAttribute(hwnd, attribute, value, size as u32)
    };

    if policy.disable_non_client_rendering {
        set_attribute(
            DWMWA_NCRENDERING_POLICY,
            std::ptr::from_ref(&non_client_policy).cast::<c_void>(),
            std::mem::size_of_val(&non_client_policy),
        )
        .map_err(|error| format!("无法关闭悬浮窗非客户区绘制: {error}"))?;
    }
    set_attribute(
        DWMWA_TRANSITIONS_FORCEDISABLED,
        std::ptr::from_ref(&transitions_disabled).cast::<c_void>(),
        std::mem::size_of_val(&transitions_disabled),
    )
    .map_err(|error| format!("无法关闭悬浮窗过渡动画: {error}"))?;
    let _ = set_attribute(
        DWMWA_BORDER_COLOR,
        std::ptr::from_ref(&border_color).cast::<c_void>(),
        std::mem::size_of_val(&border_color),
    );
    let _ = set_attribute(
        DWMWA_WINDOW_CORNER_PREFERENCE,
        std::ptr::from_ref(&corner_preference).cast::<c_void>(),
        std::mem::size_of_val(&corner_preference),
    );
    Ok(())
}

#[cfg(windows)]
fn remove_launcher_native_frame(hwnd: windows::Win32::Foundation::HWND) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongW, SetWindowLongW, SetWindowPos, GWL_STYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE,
        SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
    };

    disable_launcher_dwm_chrome(hwnd)?;
    let current_style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) };
    let frameless_style = launcher_window_style_without_frame(current_style);
    if frameless_style == current_style {
        return Ok(());
    }

    unsafe {
        SetWindowLongW(hwnd, GWL_STYLE, frameless_style);
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED
                | SWP_NOACTIVATE
                | SWP_NOMOVE
                | SWP_NOOWNERZORDER
                | SWP_NOSIZE
                | SWP_NOZORDER,
        )
    }
    .map_err(|error| format!("无法移除悬浮窗原生边框: {error}"))?;
    Ok(())
}

#[cfg(windows)]
fn apply_launcher_shape(
    window: &WebviewWindow,
    compact: bool,
    live: bool,
    redraw: bool,
) -> Result<Option<LauncherRegion>, String> {
    use windows::Win32::{
        Foundation::{POINT, RECT},
        Graphics::Gdi::{
            ClientToScreen, CombineRgn, CreatePolygonRgn, CreateRoundRectRgn, DeleteObject,
            SetWindowRgn, ALTERNATE, HGDIOBJ, RGN_ERROR, RGN_OR,
        },
        UI::WindowsAndMessaging::{GetClientRect, GetWindowRect},
    };

    let hwnd = window.hwnd().map_err(|error| error.to_string())?;
    remove_launcher_native_frame(hwnd)?;
    if !compact && !live {
        let result = unsafe { SetWindowRgn(hwnd, None, redraw) };
        return if result == 0 {
            Err("无法恢复悬浮窗矩形区域".to_string())
        } else {
            Ok(None)
        };
    }

    // SetWindowRgn uses window coordinates; the renderer uses client coordinates.
    // Read the actual offset after frame removal rather than assuming symmetric borders.
    let mut outer = RECT::default();
    let mut inner = RECT::default();
    let mut client_screen = POINT::default();
    unsafe { GetWindowRect(hwnd, &mut outer) }.map_err(|error| error.to_string())?;
    unsafe { GetClientRect(hwnd, &mut inner) }.map_err(|error| error.to_string())?;
    if !unsafe { ClientToScreen(hwnd, &mut client_screen) }.as_bool() {
        return Err("无法定位悬浮窗客户区".to_string());
    }
    let client_origin = (client_screen.x - outer.left, client_screen.y - outer.top);
    let client_size = (
        (inner.right - inner.left) as u32,
        (inner.bottom - inner.top) as u32,
    );
    let scale_factor = window.scale_factor().map_err(|error| error.to_string())?;
    let live_regions = launcher_live_regions(client_origin, client_size, scale_factor);
    let region = if live {
        live_regions.hexagon
    } else {
        launcher_hexagon_region(client_origin, client_size, scale_factor)
    };
    let points = launcher_hexagon_points(region).map(|point| POINT {
        x: point.x,
        y: point.y,
    });
    let native_region = unsafe { CreatePolygonRgn(&points, ALTERNATE) };
    if native_region.0.is_null() {
        return Err("无法创建悬浮球六边形区域".to_string());
    }
    if live {
        let speed = live_regions.speed;
        let speed_region = unsafe {
            CreateRoundRectRgn(
                speed.x,
                speed.y,
                speed.x + speed.width,
                speed.y + speed.height,
                speed.height,
                speed.height,
            )
        };
        if speed_region.0.is_null() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(native_region.0));
            }
            return Err("无法创建悬浮窗速度条区域".to_string());
        }
        let combine_result = unsafe {
            CombineRgn(
                Some(native_region),
                Some(native_region),
                Some(speed_region),
                RGN_OR,
            )
        };
        unsafe {
            let _ = DeleteObject(HGDIOBJ(speed_region.0));
        }
        if combine_result == RGN_ERROR {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(native_region.0));
            }
            return Err("无法合并悬浮窗图形区域".to_string());
        }
    }
    if unsafe { SetWindowRgn(hwnd, Some(native_region), redraw) } == 0 {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(native_region.0));
        }
        return Err("无法应用悬浮球六边形区域".to_string());
    }
    Ok(Some(region))
}

#[tauri::command]
// Compatibility only until the fixed-canvas React state machine lands; Task 7 removes this path.
pub fn set_launcher_bounds(
    window: WebviewWindow,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    compact: bool,
    live: bool,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER,
        };

        apply_launcher_shape(&window, false, false, false)?;
        let hwnd = window.hwnd().map_err(|error| error.to_string())?;
        unsafe {
            SetWindowPos(
                hwnd,
                None,
                x,
                y,
                width as i32,
                height as i32,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_NOZORDER,
            )
        }
        .map_err(|error| format!("无法原子更新悬浮窗位置和尺寸: {error}"))?;
        apply_launcher_shape(&window, compact, live, true)?;
        return Ok(());
    }

    #[cfg(not(windows))]
    {
        use tauri::{PhysicalPosition, PhysicalSize};

        if !compact {
            set_launcher_compact_shape(window.clone(), false)?;
        }
        window
            .set_size(PhysicalSize::new(width, height))
            .map_err(|error| error.to_string())?;
        window
            .set_position(PhysicalPosition::new(x, y))
            .map_err(|error| error.to_string())?;
        if compact {
            set_launcher_compact_shape(window, true)?;
        }
        Ok(())
    }
}

pub fn should_apply_launcher_window_visibility(
    requested_visible: bool,
    application_exiting: bool,
) -> bool {
    !requested_visible || !application_exiting
}

/// Shows or hides the floating launcher from its own renderer.  Keeping this
/// native-side is important for the isolated background smoke process: that
/// process must be able to exercise the launcher without ever surfacing a
/// window on the user's desktop.
#[tauri::command]
pub fn set_launcher_window_visible(
    window: WebviewWindow,
    visible: bool,
    interaction: State<'_, LauncherInteractionState>,
) -> Result<(), String> {
    if window.label() != "launcher" {
        return Err("只能设置悬浮球窗口的可见性".to_string());
    }
    let application_exiting = window
        .app_handle()
        .try_state::<std::sync::Arc<crate::ApplicationLifecycle>>()
        .is_some_and(|lifecycle| lifecycle.is_exiting());
    if !should_apply_launcher_window_visibility(visible, application_exiting) {
        return Ok(());
    }
    if interaction.is_background_test() {
        return Ok(());
    }
    if visible {
        window.show().map_err(|error| error.to_string())
    } else {
        window.hide().map_err(|error| error.to_string())
    }
}

#[tauri::command]
pub fn start_launcher_dragging(
    window: WebviewWindow,
    interaction: State<'_, LauncherInteractionState>,
) -> Result<(), String> {
    interaction.record_drag();
    if interaction.is_background_test() {
        return Ok(());
    }
    window.start_dragging().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn show_launcher_menu(
    window: WebviewWindow,
    interaction: State<'_, LauncherInteractionState>,
) -> Result<(), String> {
    if window.label() != "launcher" {
        return Err("右键菜单只能从悬浮窗打开".to_string());
    }
    interaction.record_menu();
    Ok(())
}

#[tauri::command]
pub fn run_launcher_menu_action(
    app: AppHandle,
    window: WebviewWindow,
    action: String,
) -> Result<(), String> {
    if window.label() != "launcher" {
        return Err("悬浮球菜单动作只能从悬浮窗执行".to_string());
    }
    if !launcher_menu_entries().iter().any(|(id, _)| *id == action) {
        return Err("未知的悬浮球菜单动作".to_string());
    }
    handle_launcher_menu_action(&app, &action);
    Ok(())
}

#[tauri::command]
pub fn launcher_manager_visible(app: AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|manager| manager.is_visible().ok())
        .unwrap_or(false)
}

#[tauri::command]
pub fn launcher_interaction_snapshot(
    interaction: State<'_, LauncherInteractionState>,
) -> Result<LauncherInteractionSnapshot, String> {
    if !interaction.is_background_test() {
        return Err("交互观测仅在后台测试模式可用".to_string());
    }
    Ok(interaction.snapshot())
}

#[tauri::command]
pub fn hide_manager_for_test(app: AppHandle) -> Result<(), String> {
    require_background_ui_test()?;
    let manager = app
        .get_webview_window("main")
        .ok_or_else(|| "下载管理器窗口不存在".to_string())?;
    manager.hide().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn manager_window_snapshot(app: AppHandle) -> Result<ManagerWindowSnapshot, String> {
    require_background_ui_test()?;
    let manager = app
        .get_webview_window("main")
        .ok_or_else(|| "下载管理器窗口不存在".to_string())?;
    Ok(ManagerWindowSnapshot {
        exists: true,
        visible: manager.is_visible().map_err(|error| error.to_string())?,
        minimized: manager.is_minimized().map_err(|error| error.to_string())?,
    })
}

#[tauri::command]
pub fn activate_launcher_menu_for_test(
    app: AppHandle,
    action: String,
) -> Result<Option<ExitWindowSnapshot>, String> {
    require_background_ui_test()?;
    if !launcher_menu_entries().iter().any(|(id, _)| *id == action) {
        return Err("未知的悬浮窗菜单动作".to_string());
    }
    handle_launcher_menu_action(&app, &action);
    (action == QUIT)
        .then(|| exit_window_snapshot(&app))
        .transpose()
}

pub fn handle_launcher_menu_action(app: &AppHandle, id: &str) {
    match id {
        HIDE_MANAGER => toggle_manager_visibility(app),
        NEW_DOWNLOAD => {
            reveal_manager(app, Some("downloads"));
            let _ = app.emit_to("main", "launcher-command", "new-download");
        }
        START_ALL => {
            let _ = app.emit_to("main", "launcher-command", "resume-all");
        }
        PAUSE_ALL => {
            let _ = app.emit_to("main", "launcher-command", "pause-all");
        }
        OPEN_SETTINGS => reveal_manager(app, Some("settings")),
        TRAY_LAUNCHER_ALWAYS => {
            let _ = crate::commands::update_launcher_visibility_from_native_menu(
                app,
                crate::LauncherVisibility::Always,
            );
        }
        TRAY_LAUNCHER_DOWNLOADING => {
            let _ = crate::commands::update_launcher_visibility_from_native_menu(
                app,
                crate::LauncherVisibility::Downloading,
            );
        }
        TRAY_LAUNCHER_HIDDEN => {
            let _ = crate::commands::update_launcher_visibility_from_native_menu(
                app,
                crate::LauncherVisibility::Hidden,
            );
        }
        QUIT => begin_application_exit(app),
        _ => {}
    }
}

fn toggle_manager_visibility(app: &AppHandle) {
    let Some(manager) = app.get_webview_window("main") else {
        return;
    };
    if manager.is_visible().unwrap_or(false) {
        let _ = manager.hide();
        set_tray_manager_visibility(app, false);
    } else {
        reveal_manager(app, None);
    }
}

fn reveal_manager(app: &AppHandle, destination: Option<&str>) {
    let Some(manager) = app.get_webview_window("main") else {
        return;
    };
    if let Some(destination) = destination {
        let _ = app.emit_to("main", "launcher-navigate", destination);
    }
    if background_ui_test_enabled() {
        let _ = crate::window_lifecycle::show_manager_without_activation(&manager);
    } else {
        let _ = manager.unminimize();
        let _ = manager.show();
        let _ = manager.set_focus();
    }
    set_tray_manager_visibility(app, true);
}

fn begin_application_exit(app: &AppHandle) {
    if let Some(lifecycle) = app.try_state::<std::sync::Arc<crate::ApplicationLifecycle>>() {
        lifecycle.begin_exit();
    }
    hide_application_windows(app);
    let Some(state) = app.try_state::<crate::commands::AppState>() else {
        app.exit(0);
        return;
    };
    let runtime = std::sync::Arc::clone(&state.runtime);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _report = runtime.shutdown(std::time::Duration::from_secs(2)).await;
        app.exit(0);
    });
}

fn hide_application_windows(app: &AppHandle) {
    for label in ["main", "launcher"] {
        if let Some(window) = app.get_webview_window(label) {
            let _ = crate::window_lifecycle::hide_window_immediately(&window);
        }
    }
}

fn exit_window_snapshot(app: &AppHandle) -> Result<ExitWindowSnapshot, String> {
    let manager = app
        .get_webview_window("main")
        .ok_or_else(|| "下载管理器窗口不存在".to_string())?;
    let launcher = app
        .get_webview_window("launcher")
        .ok_or_else(|| "悬浮窗不存在".to_string())?;
    Ok(ExitWindowSnapshot {
        manager_visible: manager.is_visible().map_err(|error| error.to_string())?,
        launcher_visible: launcher.is_visible().map_err(|error| error.to_string())?,
    })
}
