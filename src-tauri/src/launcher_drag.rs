//! Prefer native movement where the platform can preserve launcher geometry;
//! otherwise the captured browser pointer loop owns positioning.
use tauri::WebviewWindow;

#[derive(Clone, Copy, serde::Deserialize)]
pub struct LauncherDragOffset {
    x: f64,
    y: f64,
}

#[derive(Clone, Copy, Debug)]
struct DragOrigin {
    window: (i32, i32),
    cursor: (i32, i32),
}

impl DragOrigin {
    fn position(self, cursor: (i32, i32)) -> (i32, i32) {
        (
            self.window
                .0
                .saturating_add(cursor.0.saturating_sub(self.cursor.0)),
            self.window
                .1
                .saturating_add(cursor.1.saturating_sub(self.cursor.1)),
        )
    }
}

#[tauri::command]
pub async fn drag_launcher_window(
    window: WebviewWindow,
    grab_offset: Option<LauncherDragOffset>,
) -> Result<bool, String> {
    if window.label() != "launcher" {
        return Err("只能拖动悬浮球窗口".into());
    }
    if grab_offset.is_some_and(|point| {
        !point.x.is_finite()
            || !point.y.is_finite()
            || !(0.0..=410.0).contains(&point.x)
            || !(0.0..=720.0).contains(&point.y)
    }) {
        return Err("悬浮窗拖动坐标无效".into());
    }
    #[cfg(windows)]
    {
        let target = window.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        window
            .run_on_main_thread(move || platform::begin(target, grab_offset, sender))
            .map_err(|error| error.to_string())?;
        receiver.await.map_err(|error| error.to_string())??;
        Ok(true)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // X11 temporarily expands the input shape while the button is held,
        // allowing the browser's captured pointer loop to move the fixed
        // canvas. Unlike GTK's window-manager drag, this does not clamp the
        // icon to its large transparent canvas at the screen edge.
        Ok(false)
    }
    #[cfg(any(target_os = "macos", not(unix)))]
    {
        Ok(false)
    }
}

#[cfg(windows)]
pub(crate) use platform::is_dragging;

#[cfg(windows)]
mod platform {
    use super::{DragOrigin, LauncherDragOffset};
    use std::{
        collections::HashMap,
        sync::{Mutex, OnceLock},
    };
    use tauri::WebviewWindow;
    use windows::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE, VK_LBUTTON},
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::{
                GetCursorPos, GetWindowRect, IsWindowVisible, KillTimer, SetTimer, SetWindowPos,
                SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, WM_NCDESTROY, WM_TIMER,
            },
        },
    };

    const DRAG_ID: usize = 0x414C_4452;
    type Completion = tokio::sync::oneshot::Sender<Result<(), String>>;
    struct Drag {
        origin: DragOrigin,
        last_position: (i32, i32),
        completion: Completion,
    }
    static DRAGS: OnceLock<Mutex<HashMap<isize, Drag>>> = OnceLock::new();
    fn drags() -> &'static Mutex<HashMap<isize, Drag>> {
        DRAGS.get_or_init(|| Mutex::new(HashMap::new()))
    }
    pub(crate) fn is_dragging(key: isize) -> bool {
        drags().lock().is_ok_and(|drags| drags.contains_key(&key))
    }

    pub(super) fn begin(
        window: WebviewWindow,
        grab_offset: Option<LauncherDragOffset>,
        completion: Completion,
    ) {
        let setup = (|| {
            let hwnd = window.hwnd().map_err(|error| error.to_string())?;
            if is_dragging(hwnd.0 as isize) {
                return Err("悬浮窗正在拖动".into());
            }
            let mut rect = RECT::default();
            let mut cursor = POINT::default();
            unsafe {
                GetWindowRect(hwnd, &mut rect).map_err(|error| error.to_string())?;
                GetCursorPos(&mut cursor).map_err(|error| error.to_string())?;
            }
            if let Some(offset) = grab_offset {
                let scale = window.scale_factor().map_err(|error| error.to_string())?;
                // Use the original grab point so IPC startup latency and the
                // drag threshold do not leave the icon trailing the cursor.
                cursor.x = rect.left.saturating_add((offset.x * scale).round() as i32);
                cursor.y = rect.top.saturating_add((offset.y * scale).round() as i32);
            }
            // A quick release can precede delivery of the start command.
            if unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } >= 0 {
                return Ok(None);
            }
            if !unsafe { SetWindowSubclass(hwnd, Some(drag_proc), DRAG_ID, 0) }.as_bool() {
                return Err("无法安装悬浮窗拖动处理".into());
            }
            if unsafe { SetTimer(Some(hwnd), DRAG_ID, 10, None) } == 0 {
                unsafe {
                    let _ = RemoveWindowSubclass(hwnd, Some(drag_proc), DRAG_ID);
                }
                return Err("无法启动悬浮窗拖动计时器".into());
            }
            Ok(Some((
                hwnd,
                DragOrigin {
                    window: (rect.left, rect.top),
                    cursor: (cursor.x, cursor.y),
                },
            )))
        })();
        match setup {
            Ok(Some((hwnd, origin))) => {
                drags().lock().unwrap().insert(
                    hwnd.0 as isize,
                    Drag {
                        origin,
                        last_position: origin.window,
                        completion,
                    },
                );
                if let Err(error) = window.set_ignore_cursor_events(false) {
                    finish(hwnd, Err(error.to_string()));
                }
            }
            Ok(None) => {
                let _ = completion.send(Ok(()));
            }
            Err(error) => {
                let _ = completion.send(Err(error));
            }
        }
    }

    fn finish(hwnd: HWND, result: Result<(), String>) {
        unsafe {
            let _ = KillTimer(Some(hwnd), DRAG_ID);
            let _ = RemoveWindowSubclass(hwnd, Some(drag_proc), DRAG_ID);
        }
        let drag = drags().lock().unwrap().remove(&(hwnd.0 as isize));
        if let Some(drag) = drag {
            let _ = drag.completion.send(result);
        }
    }

    fn tick(hwnd: HWND) {
        if !unsafe { IsWindowVisible(hwnd) }.as_bool()
            || unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) } < 0
        {
            finish(hwnd, Ok(()));
            return;
        }
        let mut cursor = POINT::default();
        if let Err(error) = unsafe { GetCursorPos(&mut cursor) } {
            finish(hwnd, Err(error.to_string()));
            return;
        }
        if let Err(error) = move_to_cursor(hwnd, (cursor.x, cursor.y)) {
            finish(hwnd, Err(error));
        } else if unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } >= 0 {
            finish(hwnd, Ok(()));
        }
    }

    fn move_to_cursor(hwnd: HWND, cursor: (i32, i32)) -> Result<(), String> {
        let position = {
            let mut drags = drags().lock().unwrap();
            let Some(drag) = drags.get_mut(&(hwnd.0 as isize)) else {
                return Ok(());
            };
            let position = drag.origin.position(cursor);
            if position == drag.last_position {
                return Ok(());
            }
            drag.last_position = position;
            position
        };
        // Release the map lock before SetWindowPos synchronously sends window
        // messages. No resize, activation, work-area clamp or WebView IPC.
        unsafe {
            SetWindowPos(
                hwnd,
                None,
                position.0,
                position.1,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
        }
        .map_err(|error| error.to_string())
    }

    unsafe extern "system" fn drag_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        if message == WM_TIMER && wparam.0 == DRAG_ID {
            tick(hwnd);
            return LRESULT(0);
        }
        if message == WM_NCDESTROY {
            finish(hwnd, Ok(()));
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows::{
            core::w,
            Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_POPUP,
            },
        };

        #[test]
        fn native_movement_preserves_size_tracks_latest_sample_and_releases_state() {
            // Hidden fixture: no desktop cursor movement or visible test UI.
            let hwnd = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!("launcher drag fixture"),
                    WS_POPUP,
                    -1000,
                    -1000,
                    410,
                    720,
                    None,
                    None,
                    None,
                    None,
                )
            }
            .unwrap();
            let (completion, mut receiver) = tokio::sync::oneshot::channel();
            drags().lock().unwrap().insert(
                hwnd.0 as isize,
                Drag {
                    origin: DragOrigin {
                        window: (-1000, -1000),
                        cursor: (-622, -640),
                    },
                    last_position: (-1000, -1000),
                    completion,
                },
            );
            assert!(is_dragging(hwnd.0 as isize));
            for cursor in [(-600, -600), (-400, -500), (-400, -500), (-800, -1100)] {
                move_to_cursor(hwnd, cursor).unwrap();
                let mut rect = RECT::default();
                unsafe { GetWindowRect(hwnd, &mut rect) }.unwrap();
                assert_eq!(
                    (rect.left, rect.top),
                    (-1000 + cursor.0 + 622, -1000 + cursor.1 + 640)
                );
                assert_eq!((rect.right - rect.left, rect.bottom - rect.top), (410, 720));
            }
            finish(hwnd, Ok(()));
            assert!(!is_dragging(hwnd.0 as isize));
            assert_eq!(receiver.try_recv().unwrap(), Ok(()));
            unsafe { DestroyWindow(hwnd) }.unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_samples_do_not_feed_back_window_movement() {
        let origin = DragOrigin {
            window: (100, 200),
            cursor: (478, 560),
        };
        assert_eq!(origin.position((498, 570)), (120, 210));
        assert_eq!(origin.position((498, 570)), (120, 210));
        assert_eq!(origin.position((528, 590)), (150, 230));
        assert_eq!(origin.position((478, 560)), (100, 200));
    }

    #[test]
    fn physical_coordinates_allow_negative_monitors_and_desktop_top() {
        let origin = DragOrigin {
            window: (-1200, 200),
            cursor: (-650, 740),
        };
        assert_eq!(origin.position((-750, 0)), (-1300, -540));
    }
}
