use serde::Serialize;
use std::sync::atomic::{AtomicUsize, Ordering};

use tauri::{menu::MenuBuilder, AppHandle, Emitter, Manager, State, WebviewWindow};

pub const OPEN_MANAGER: &str = "launcher.open_manager";
pub const OPEN_SETTINGS: &str = "launcher.settings";
pub const QUIT: &str = "launcher.quit";

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
pub struct LauncherInteractionSnapshot {
    pub drag_requests: usize,
    pub menu_requests: usize,
}

pub struct LauncherInteractionState {
    background_test: bool,
    drag_requests: AtomicUsize,
    menu_requests: AtomicUsize,
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

pub fn launcher_menu_entries() -> [(&'static str, &'static str); 3] {
    [
        (OPEN_MANAGER, "打开下载管理器"),
        (OPEN_SETTINGS, "设置"),
        (QUIT, "退出"),
    ]
}

pub fn launcher_circle_region(
    outer: (u32, u32),
    inner: (u32, u32),
    scale_factor: f64,
) -> LauncherRegion {
    let diameter = (48.0 * scale_factor).round() as i32;
    let client_x = outer.0.saturating_sub(inner.0) as i32 / 2;
    let client_y = outer.1.saturating_sub(inner.1) as i32 / 2;
    LauncherRegion {
        x: client_x + inner.0 as i32 - diameter,
        y: client_y + inner.1 as i32 - diameter,
        width: diameter,
        height: diameter,
    }
}

#[tauri::command]
pub fn set_launcher_compact_shape(
    window: WebviewWindow,
    compact: bool,
) -> Result<Option<LauncherRegion>, String> {
    #[cfg(windows)]
    {
        use windows::Win32::Graphics::Gdi::{
            CreateEllipticRgn, DeleteObject, SetWindowRgn, HGDIOBJ,
        };

        let hwnd = window.hwnd().map_err(|error| error.to_string())?;
        if !compact {
            let result = unsafe { SetWindowRgn(hwnd, None, true) };
            return if result == 0 {
                Err("无法恢复悬浮窗矩形区域".to_string())
            } else {
                Ok(None)
            };
        }

        let outer = window.outer_size().map_err(|error| error.to_string())?;
        let inner = window.inner_size().map_err(|error| error.to_string())?;
        let scale_factor = window.scale_factor().map_err(|error| error.to_string())?;
        let region = launcher_circle_region(
            (outer.width, outer.height),
            (inner.width, inner.height),
            scale_factor,
        );
        let native_region = unsafe {
            CreateEllipticRgn(
                region.x,
                region.y,
                region.x + region.width,
                region.y + region.height,
            )
        };
        if native_region.0.is_null() {
            return Err("无法创建悬浮球圆形区域".to_string());
        }
        if unsafe { SetWindowRgn(hwnd, Some(native_region), true) } == 0 {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(native_region.0));
            }
            return Err("无法应用悬浮球圆形区域".to_string());
        }
        return Ok(Some(region));
    }

    #[cfg(not(windows))]
    {
        let _ = (window, compact);
        Ok(None)
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
    app: AppHandle,
    window: WebviewWindow,
    interaction: State<'_, LauncherInteractionState>,
) -> Result<(), String> {
    if window.label() != "launcher" {
        return Err("右键菜单只能从悬浮窗打开".to_string());
    }
    interaction.record_menu();
    if interaction.is_background_test() {
        return Ok(());
    }
    let mut builder = MenuBuilder::new(&app);
    for (id, label) in launcher_menu_entries() {
        builder = builder.text(id, label);
    }
    let menu = builder.build().map_err(|error| error.to_string())?;
    window.popup_menu(&menu).map_err(|error| error.to_string())
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

pub fn handle_launcher_menu_action(app: &AppHandle, id: &str) {
    match id {
        OPEN_MANAGER => reveal_manager(app, None),
        OPEN_SETTINGS => reveal_manager(app, Some("settings")),
        QUIT => app.exit(0),
        _ => {}
    }
}

fn reveal_manager(app: &AppHandle, destination: Option<&str>) {
    let Some(manager) = app.get_webview_window("main") else {
        return;
    };
    if let Some(destination) = destination {
        let _ = app.emit_to("main", "launcher-navigate", destination);
    }
    let _ = manager.unminimize();
    let _ = manager.show();
    let _ = manager.set_focus();
}
