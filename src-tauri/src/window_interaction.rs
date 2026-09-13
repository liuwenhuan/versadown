//! Native move/resize lifecycle. No per-frame JavaScript notifications.

const ENTER_SIZE_MOVE: u32 = 0x0231;
const EXIT_SIZE_MOVE: u32 = 0x0232;
const NC_DESTROY: u32 = 0x0082;

#[derive(Default)]
struct InteractionState {
    active: bool,
}

impl InteractionState {
    fn update(&mut self, message: u32) -> Option<bool> {
        let next = match message {
            ENTER_SIZE_MOVE => true,
            EXIT_SIZE_MOVE | NC_DESTROY => false,
            _ => return None,
        };
        if next == self.active {
            return None;
        }
        self.active = next;
        Some(next)
    }
}

use std::ffi::c_void;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::{Emitter, Manager, WebviewWindow};
use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings9;
use windows::core::Interface;

#[derive(Default)]
struct NativeInteraction(AtomicBool);

pub async fn snapshot(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let window = app.get_webview_window("main").ok_or("主窗口不存在")?;
    let state = app
        .try_state::<Arc<NativeInteraction>>()
        .ok_or("窗口交互尚未初始化")?;
    let state = Arc::clone(state.inner());
    let (send, receive) = tokio::sync::oneshot::channel();
    window
        .with_webview(move |view| {
            let native_drag = unsafe {
                view.controller()
                    .CoreWebView2()
                    .and_then(|webview| webview.Settings())
                    .and_then(|settings| settings.cast::<ICoreWebView2Settings9>())
                    .and_then(|settings| {
                        let mut enabled = windows::core::BOOL(0);
                        settings.IsNonClientRegionSupportEnabled(&mut enabled)?;
                        Ok(enabled.as_bool())
                    })
                    .unwrap_or(false)
            };
            let _ = send.send(serde_json::json!({
                "active": state.0.load(Ordering::Acquire), "nativeDrag": native_drag,
            }));
        })
        .map_err(|error| error.to_string())?;
    receive.await.map_err(|error| error.to_string())
}

type SubclassProc =
    unsafe extern "system" fn(*mut c_void, u32, usize, isize, usize, usize) -> isize;

#[link(name = "comctl32")]
extern "system" {
    fn SetWindowSubclass(hwnd: *mut c_void, callback: SubclassProc, id: usize, data: usize) -> i32;
    fn RemoveWindowSubclass(hwnd: *mut c_void, callback: SubclassProc, id: usize) -> i32;
    fn DefSubclassProc(hwnd: *mut c_void, message: u32, wparam: usize, lparam: isize) -> isize;
}

struct Hook {
    window: WebviewWindow,
    state: InteractionState,
    shared: Arc<NativeInteraction>,
}

/// Called once on the window thread during setup, before showing the manager.
pub fn install(window: &WebviewWindow) -> Result<(), String> {
    let hwnd = window.hwnd().map_err(|error| error.to_string())?;
    let shared = Arc::new(NativeInteraction::default());
    window.app_handle().manage(Arc::clone(&shared));
    let hook = Box::into_raw(Box::new(Hook {
        window: window.clone(),
        state: InteractionState::default(),
        shared,
    }));
    // The callback address + id identifies this subclass independently of
    // Tao/Wry's hooks. Ownership transfers only when registration succeeds.
    if unsafe { SetWindowSubclass(hwnd.0, window_proc, 1, hook as usize) } == 0 {
        unsafe {
            drop(Box::from_raw(hook));
        }
        return Err("无法安装窗口交互监听".to_string());
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: *mut c_void,
    message: u32,
    wparam: usize,
    lparam: isize,
    id: usize,
    data: usize,
) -> isize {
    let hook = data as *mut Hook;
    // End the mutable borrow before emitting or forwarding: either operation
    // can re-enter the message loop. Never intercept sizing/hit-test messages.
    let changed = (*hook).state.update(message);
    if let Some(active) = changed {
        (&(*hook).shared).0.store(active, Ordering::Release);
        let window = (*hook).window.clone();
        let _ = window.emit("main-window-interaction", active);
    }
    if message == NC_DESTROY {
        RemoveWindowSubclass(hwnd, window_proc, id);
        drop(Box::from_raw(hook));
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_loop_emits_only_entry_and_exit_not_each_resize() {
        let mut state = InteractionState::default();
        assert_eq!(state.update(ENTER_SIZE_MOVE), Some(true));
        assert_eq!(state.update(ENTER_SIZE_MOVE), None);
        for message in [0x0005, 0x0214, 0x0216, 0x0007] {
            assert_eq!(state.update(message), None);
        }
        assert_eq!(state.update(EXIT_SIZE_MOVE), Some(false));
        assert_eq!(state.update(EXIT_SIZE_MOVE), None);
    }

    #[test]
    fn destruction_ends_an_active_interaction() {
        let mut state = InteractionState::default();
        assert_eq!(state.update(NC_DESTROY), None);
        state.update(ENTER_SIZE_MOVE);
        assert_eq!(state.update(NC_DESTROY), Some(false));
    }
}
