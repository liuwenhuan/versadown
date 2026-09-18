use serde::Deserialize;
use tauri::{State, WebviewWindow};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LauncherInputRegion {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PhysicalLauncherRegion {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

fn physical_coordinate(value: f64, scale_factor: f64) -> Result<i32, String> {
    let scaled = value * scale_factor;
    if !scaled.is_finite() {
        return Err("悬浮窗输入区域无效".to_string());
    }
    let rounded = scaled.round();
    if rounded < i32::MIN as f64 || rounded > i32::MAX as f64 {
        return Err("悬浮窗输入区域无效".to_string());
    }
    Ok(rounded as i32)
}

fn physical_regions(
    regions: &[LauncherInputRegion],
    scale_factor: f64,
) -> Result<Vec<PhysicalLauncherRegion>, String> {
    if !scale_factor.is_finite() || scale_factor <= 0.0 || regions.is_empty() {
        return Err("悬浮窗输入区域无效".to_string());
    }
    regions
        .iter()
        .map(|region| {
            if !region.x.is_finite()
                || !region.y.is_finite()
                || !region.width.is_finite()
                || !region.height.is_finite()
                || region.width <= 0.0
                || region.height <= 0.0
            {
                return Err("悬浮窗输入区域无效".to_string());
            }
            let left = physical_coordinate(region.x, scale_factor)?;
            let top = physical_coordinate(region.y, scale_factor)?;
            let right = physical_coordinate(region.x + region.width, scale_factor)?;
            let bottom = physical_coordinate(region.y + region.height, scale_factor)?;
            if right <= left || bottom <= top {
                return Err("悬浮窗输入区域无效".to_string());
            }
            Ok(PhysicalLauncherRegion {
                left,
                top,
                right,
                bottom,
            })
        })
        .collect()
}

fn point_hits_regions((x, y): (i32, i32), regions: &[PhysicalLauncherRegion]) -> bool {
    regions
        .iter()
        .any(|region| x >= region.left && x < region.right && y >= region.top && y < region.bottom)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LauncherHitResult {
    Client,
    Transparent,
}

fn launcher_hit_result(point: (i32, i32), regions: &[PhysicalLauncherRegion]) -> LauncherHitResult {
    if point_hits_regions(point, regions) {
        LauncherHitResult::Client
    } else {
        LauncherHitResult::Transparent
    }
}

#[tauri::command]
pub async fn set_launcher_input_regions(
    window: WebviewWindow,
    regions: Vec<LauncherInputRegion>,
    interaction: State<'_, crate::launcher::LauncherInteractionState>,
) -> Result<(), String> {
    if window.label() != "launcher" {
        return Err("只能设置悬浮球窗口的输入区域".to_string());
    }
    let scale_factor = window.scale_factor().map_err(|error| error.to_string())?;
    let physical = physical_regions(&regions, scale_factor)?;
    #[cfg(windows)]
    {
        // SetWindowSubclass must run on the thread that owns the HWND.
        let target = window.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        window
            .run_on_main_thread(move || {
                let result = target
                    .hwnd()
                    .map_err(|error| error.to_string())
                    .and_then(|hwnd| platform::apply_hwnd(hwnd, physical));
                let _ = sender.send(result);
            })
            .map_err(|error| error.to_string())?;
        receiver.await.map_err(|error| error.to_string())??;
        interaction.record_input_region_update();
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (window, physical, interaction);
        Err("当前平台尚未安装悬浮窗输入区域适配器".to_string())
    }
}

/// Background smoke diagnostic: sends native hit-test messages, never mouse input.
/// This does not establish OS click-through to windows owned by another process.
#[tauri::command]
pub async fn launcher_input_hit_test_for_test(
    window: WebviewWindow,
    points: Vec<(i32, i32)>,
) -> Result<serde_json::Value, String> {
    if window.label() != "launcher"
        || std::env::var("ALLDOWNLOADER_BACKGROUND_UI_TEST").as_deref() != Ok("1")
    {
        return Err("输入命中观测仅在悬浮窗后台测试模式可用".to_string());
    }
    #[cfg(windows)]
    {
        let scale = window.scale_factor().map_err(|error| error.to_string())?;
        let target = window.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        window
            .run_on_main_thread(move || {
                let result = target
                    .hwnd()
                    .map_err(|error| error.to_string())
                    .and_then(|hwnd| platform::probe(hwnd, &points, scale));
                let _ = sender.send(result);
            })
            .map_err(|error| error.to_string())?;
        receiver.await.map_err(|error| error.to_string())?
    }
    #[cfg(not(windows))]
    {
        let _ = points;
        Err("当前平台尚未安装悬浮窗输入区域适配器".to_string())
    }
}

#[cfg(windows)]
mod platform {
    use super::{launcher_hit_result, LauncherHitResult, PhysicalLauncherRegion};
    use std::{
        collections::HashMap,
        sync::{Mutex, OnceLock, RwLock},
    };
    use windows::{
        core::BOOL,
        Win32::{
            Foundation::{
                GetLastError, SetLastError, HWND, LPARAM, LRESULT, POINT, WIN32_ERROR, WPARAM,
            },
            Graphics::Gdi::ScreenToClient,
            UI::{
                Shell::{
                    DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass,
                },
                WindowsAndMessaging::{
                    CallWindowProcW, DefWindowProcW, EnumChildWindows, GetWindowLongPtrW,
                    GetWindowThreadProcessId, SetWindowLongPtrW, GWLP_WNDPROC, HTCLIENT,
                    HTTRANSPARENT, WM_NCDESTROY, WM_NCHITTEST, WNDPROC,
                },
            },
        },
    };

    const LAUNCHER_SUBCLASS_ID: usize = 0x414C_444C;
    static REGIONS: OnceLock<RwLock<HashMap<isize, Vec<PhysicalLauncherRegion>>>> = OnceLock::new();

    #[derive(Clone, Copy)]
    struct ForeignThreadSubclass {
        root_key: isize,
        original_proc: isize,
    }

    // WebView2 can create child HWNDs on a renderer-owned UI thread.  The
    // common-control subclass helper deliberately rejects those windows, so
    // keep their instance subclasses separately and forward every message to
    // the saved procedure with CallWindowProcW.
    static FOREIGN_THREAD_SUBCLASSES: OnceLock<Mutex<HashMap<isize, ForeignThreadSubclass>>> =
        OnceLock::new();

    fn regions() -> &'static RwLock<HashMap<isize, Vec<PhysicalLauncherRegion>>> {
        REGIONS.get_or_init(|| RwLock::new(HashMap::new()))
    }

    fn foreign_thread_subclasses() -> &'static Mutex<HashMap<isize, ForeignThreadSubclass>> {
        FOREIGN_THREAD_SUBCLASSES.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn hit_test_result(root_key: isize, lparam: LPARAM) -> Option<LRESULT> {
        // WM_NCHITTEST packs signed screen coordinates, including negative monitors.
        let packed = lparam.0 as u32;
        let mut point = POINT {
            x: (packed & 0xffff) as i16 as i32,
            y: ((packed >> 16) as u16) as i16 as i32,
        };
        let root = HWND(root_key as *mut _);
        // Regions always use launcher-client coordinates, also on offset children.
        if !unsafe { ScreenToClient(root, &mut point) }.as_bool() {
            return None;
        }
        let guard = regions().read().ok()?;
        let active = guard.get(&root_key)?;
        Some(match launcher_hit_result((point.x, point.y), active) {
            LauncherHitResult::Client => LRESULT(HTCLIENT as isize),
            LauncherHitResult::Transparent => LRESULT(HTTRANSPARENT as isize),
        })
    }

    unsafe extern "system" fn subclass_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _subclass_id: usize,
        root_key: usize,
    ) -> LRESULT {
        if message == WM_NCHITTEST {
            if let Some(result) = hit_test_result(root_key as isize, lparam) {
                return result;
            }
        }
        if message == WM_NCDESTROY {
            if hwnd.0 as usize == root_key {
                if let Ok(mut guard) = regions().write() {
                    guard.remove(&(root_key as isize));
                }
            }
            let _ = RemoveWindowSubclass(hwnd, Some(subclass_proc), LAUNCHER_SUBCLASS_ID);
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }

    unsafe extern "system" fn foreign_thread_subclass_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let subclass = foreign_thread_subclasses()
            .lock()
            .ok()
            .and_then(|mut subclasses| {
                let subclass = subclasses.get(&(hwnd.0 as isize)).copied();
                if message == WM_NCDESTROY {
                    subclasses.remove(&(hwnd.0 as isize));
                }
                subclass
            });

        let Some(subclass) = subclass else {
            return DefWindowProcW(hwnd, message, wparam, lparam);
        };
        if message == WM_NCHITTEST {
            if let Some(result) = hit_test_result(subclass.root_key, lparam) {
                return result;
            }
        }
        // The value returned by GetWindowLongPtrW for GWLP_WNDPROC can be an
        // internal thunk, so it must be called through CallWindowProcW rather
        // than invoked as a Rust function pointer directly.
        let original: WNDPROC = std::mem::transmute(subclass.original_proc);
        CallWindowProcW(original, hwnd, message, wparam, lparam)
    }

    unsafe extern "system" fn collect_child(hwnd: HWND, data: LPARAM) -> BOOL {
        let children = &mut *(data.0 as *mut Vec<HWND>);
        children.push(hwnd);
        BOOL(1)
    }

    pub(super) fn window_tree(root: HWND) -> Vec<HWND> {
        let mut windows = vec![root];
        // EnumChildWindows enumerates all descendants. Its return value is unused by Win32.
        unsafe {
            let _ = EnumChildWindows(
                Some(root),
                Some(collect_child),
                LPARAM(&mut windows as *mut _ as isize),
            );
        }
        windows
    }

    pub(super) fn is_installed(hwnd: HWND) -> bool {
        unsafe {
            GetWindowSubclass(hwnd, Some(subclass_proc), LAUNCHER_SUBCLASS_ID, None).as_bool()
        }
    }

    fn install_foreign_thread_subclass(root: HWND, hwnd: HWND) -> Result<bool, String> {
        let key = hwnd.0 as isize;
        {
            let subclasses = foreign_thread_subclasses()
                .lock()
                .map_err(|_| "悬浮窗输入区域状态不可用".to_string())?;
            if let Some(existing) = subclasses.get(&key) {
                if existing.root_key == root.0 as isize {
                    return Ok(false);
                }
                return Err("悬浮窗输入窗口已被其他实例占用".to_string());
            }
        }

        // Store the current procedure before installing ours.  This prevents
        // an unrelated WebView message racing the cross-thread replacement
        // from ever falling through to DefWindowProcW.
        let original = unsafe { GetWindowLongPtrW(hwnd, GWLP_WNDPROC) };
        if original == 0 {
            return Err("无法读取悬浮窗输入窗口过程".to_string());
        }
        foreign_thread_subclasses()
            .lock()
            .map_err(|_| "悬浮窗输入区域状态不可用".to_string())?
            .insert(
                key,
                ForeignThreadSubclass {
                    root_key: root.0 as isize,
                    original_proc: original,
                },
            );
        // `SetWindowLongPtrW` can legitimately return zero when the previous
        // procedure value is zero. Clear and inspect last-error as required by
        // Win32 instead of treating that successful result as a failure; doing
        // so previously left the launcher hidden during startup.
        unsafe { SetLastError(WIN32_ERROR(0)) };
        let previous = unsafe {
            SetWindowLongPtrW(
                hwnd,
                GWLP_WNDPROC,
                foreign_thread_subclass_proc as *const () as usize as isize,
            )
        };
        if previous == 0 && unsafe { GetLastError() }.0 != 0 {
            foreign_thread_subclasses()
                .lock()
                .map_err(|_| "悬浮窗输入区域状态不可用".to_string())?
                .remove(&key);
            return Err("无法安装悬浮窗输入窗口过程".to_string());
        }
        if previous != original {
            if let Ok(mut subclasses) = foreign_thread_subclasses().lock() {
                if let Some(subclass) = subclasses.get_mut(&key) {
                    subclass.original_proc = previous;
                }
            }
        }
        Ok(true)
    }

    fn remove_foreign_thread_subclass(hwnd: HWND) {
        let key = hwnd.0 as isize;
        let subclass = foreign_thread_subclasses()
            .lock()
            .ok()
            .and_then(|mut subclasses| subclasses.remove(&key));
        if let Some(subclass) = subclass {
            unsafe {
                let _ = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, subclass.original_proc);
            }
        }
    }

    fn install_foreign_thread_subclasses(
        root: HWND,
        targets: &[HWND],
    ) -> Result<Vec<HWND>, String> {
        let mut installed = Vec::new();
        for &hwnd in targets {
            match install_foreign_thread_subclass(root, hwnd) {
                Ok(true) => installed.push(hwnd),
                Ok(false) => {}
                Err(error) => {
                    for installed_hwnd in installed {
                        remove_foreign_thread_subclass(installed_hwnd);
                    }
                    return Err(error);
                }
            }
        }
        Ok(installed)
    }

    pub(super) fn apply_hwnd(
        root: HWND,
        active: Vec<PhysicalLauncherRegion>,
    ) -> Result<(), String> {
        let owner = unsafe { GetWindowThreadProcessId(root, None) };
        let windows = window_tree(root);
        let same_thread: Vec<_> = windows
            .iter()
            .copied()
            .filter(|hwnd| unsafe { GetWindowThreadProcessId(*hwnd, None) } == owner)
            .collect();
        let foreign_thread: Vec<_> = windows
            .into_iter()
            .filter(|hwnd| unsafe { GetWindowThreadProcessId(*hwnd, None) } != owner)
            .collect();

        // SetWindowSubclass is intentionally restricted to the owner thread.
        // WebView2's foreign UI-thread children use instance subclassing
        // instead, so their transparent pixels cannot block the system tray.
        let newly_hooked = install_foreign_thread_subclasses(root, &foreign_thread)?;
        if let Err(error) = install_and_replace(root, &same_thread, active) {
            for hwnd in newly_hooked {
                remove_foreign_thread_subclass(hwnd);
            }
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn install_and_replace(
        root: HWND,
        targets: &[HWND],
        active: Vec<PhysicalLauncherRegion>,
    ) -> Result<(), String> {
        // Check availability before modifying native state. Never erase the last valid set.
        drop(
            regions()
                .read()
                .map_err(|_| "悬浮窗输入区域状态不可用".to_string())?,
        );
        let mut newly_installed = Vec::new();
        let result = (|| {
            for &hwnd in targets {
                let existed = is_installed(hwnd);
                if !unsafe {
                    SetWindowSubclass(
                        hwnd,
                        Some(subclass_proc),
                        LAUNCHER_SUBCLASS_ID,
                        root.0 as usize,
                    )
                }
                .as_bool()
                {
                    return Err("无法安装悬浮窗命中测试".to_string());
                }
                if !existed {
                    newly_installed.push(hwnd);
                }
            }
            regions()
                .write()
                .map_err(|_| "悬浮窗输入区域状态不可用".to_string())?
                .insert(root.0 as isize, active);
            Ok(())
        })();
        if result.is_err() {
            for hwnd in newly_installed {
                unsafe {
                    let _ = RemoveWindowSubclass(hwnd, Some(subclass_proc), LAUNCHER_SUBCLASS_ID);
                }
            }
        }
        result
    }

    pub(super) fn probe(
        root: HWND,
        points: &[(i32, i32)],
        scale: f64,
    ) -> Result<serde_json::Value, String> {
        use windows::Win32::{
            Graphics::Gdi::ClientToScreen, UI::WindowsAndMessaging::SendMessageW,
        };
        let mut screen_points = Vec::new();
        for &(x, y) in points {
            let mut point = POINT {
                x: (x as f64 * scale).round() as i32,
                y: (y as f64 * scale).round() as i32,
            };
            if !unsafe { ClientToScreen(root, &mut point) }.as_bool()
                || i16::try_from(point.x).is_err()
                || i16::try_from(point.y).is_err()
            {
                return Err("无法转换命中测试屏幕坐标".to_string());
            }
            screen_points.push(LPARAM(
                ((point.x as u16 as u32) | ((point.y as u16 as u32) << 16)) as isize,
            ));
        }
        let owner = unsafe { GetWindowThreadProcessId(root, None) };
        let observations: Vec<_> = window_tree(root)
            .into_iter()
            .map(|hwnd| {
                let thread = unsafe { GetWindowThreadProcessId(hwnd, None) };
                let same_thread = thread == owner;
                // Do not send synchronous messages to foreign Chromium threads.
                let hits = same_thread.then(|| {
                    screen_points
                        .iter()
                        .map(|&point| unsafe {
                            SendMessageW(hwnd, WM_NCHITTEST, None, Some(point)).0
                        })
                        .collect::<Vec<_>>()
                });
                serde_json::json!({
                    "hwnd": hwnd.0 as usize,
                    "sameThread": same_thread,
                    "installed": same_thread && is_installed(hwnd),
                    "hits": hits,
                })
            })
            .collect();
        Ok(serde_json::json!(observations))
    }

    #[cfg(test)]
    pub(super) fn has_regions(key: isize) -> bool {
        regions().read().unwrap().contains_key(&key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_the_active_regions_is_transparent() {
        let regions = [PhysicalLauncherRegion {
            left: 519,
            top: 492,
            right: 615,
            bottom: 588,
        }];
        assert_eq!(
            launcher_hit_result((550, 520), &regions),
            LauncherHitResult::Client
        );
        assert_eq!(
            launcher_hit_result((100, 100), &regions),
            LauncherHitResult::Transparent
        );
    }

    #[test]
    fn converts_logical_regions_at_fractional_dpi() {
        let regions = physical_regions(
            &[LauncherInputRegion {
                x: 346.0,
                y: 328.0,
                width: 64.0,
                height: 64.0,
            }],
            1.5,
        )
        .unwrap();
        assert_eq!(
            regions,
            vec![PhysicalLauncherRegion {
                left: 519,
                top: 492,
                right: 615,
                bottom: 588,
            }]
        );
    }

    #[test]
    fn rejects_invalid_regions_without_replacing_the_last_valid_set() {
        let error = physical_regions(
            &[LauncherInputRegion {
                x: 0.0,
                y: 0.0,
                width: -1.0,
                height: 64.0,
            }],
            1.0,
        )
        .unwrap_err();
        assert_eq!(error, "悬浮窗输入区域无效");
    }

    #[test]
    fn rejects_regions_that_overflow_at_high_dpi() {
        let error = physical_regions(
            &[LauncherInputRegion {
                x: 2_000_000_000.0,
                y: 328.0,
                width: 64.0,
                height: 64.0,
            }],
            2.0,
        )
        .unwrap_err();
        assert_eq!(error, "悬浮窗输入区域无效");
    }

    #[test]
    fn hit_testing_includes_left_top_and_excludes_right_bottom_edges() {
        let region = PhysicalLauncherRegion {
            left: 10,
            top: 20,
            right: 30,
            bottom: 40,
        };
        assert!(point_hits_regions((10, 20), &[region]));
        assert!(point_hits_regions((29, 39), &[region]));
        assert!(!point_hits_regions((30, 40), &[region]));
        assert!(!point_hits_regions((9, 20), &[region]));
    }

    #[cfg(windows)]
    mod native {
        use super::*;
        use windows::{
            core::w,
            Win32::{
                Foundation::{HWND, LPARAM, POINT},
                Graphics::Gdi::ClientToScreen,
                UI::WindowsAndMessaging::{
                    CreateWindowExW, DestroyWindow, SendMessageW, HTCLIENT, HTTRANSPARENT,
                    WINDOW_EX_STYLE, WM_NCHITTEST, WS_CHILD, WS_POPUP,
                },
            },
        };

        struct TestWindow(HWND);

        impl TestWindow {
            fn new(parent: Option<HWND>) -> Self {
                Self(unsafe {
                    CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        w!("STATIC"),
                        w!("launcher hit-test fixture"),
                        if parent.is_some() { WS_CHILD } else { WS_POPUP },
                        if parent.is_some() { 30 } else { -1000 },
                        if parent.is_some() { 40 } else { -1000 },
                        600,
                        600,
                        parent,
                        None,
                        None,
                        None,
                    )
                    .unwrap()
                })
            }
        }

        impl Drop for TestWindow {
            fn drop(&mut self) {
                unsafe {
                    let _ = DestroyWindow(self.0);
                }
            }
        }

        fn hit(hwnd: HWND, root: HWND, x: i32, y: i32) -> isize {
            let mut point = POINT { x, y };
            unsafe {
                assert!(ClientToScreen(root, &mut point).as_bool());
                let packed = (point.x as u16 as u32) | ((point.y as u16 as u32) << 16);
                SendMessageW(hwnd, WM_NCHITTEST, None, Some(LPARAM(packed as isize))).0
            }
        }

        fn icon() -> Vec<PhysicalLauncherRegion> {
            vec![PhysicalLauncherRegion {
                left: 346,
                top: 328,
                right: 410,
                bottom: 392,
            }]
        }

        #[test]
        fn native_subclass_handles_parent_and_offset_child_and_replaces_regions() {
            let root = TestWindow::new(None);
            let child = TestWindow::new(Some(root.0));
            platform::apply_hwnd(root.0, icon()).unwrap();
            for hwnd in [root.0, child.0] {
                assert_eq!(hit(hwnd, root.0, 378, 360), HTCLIENT as isize);
                for (x, y) in [(100, 100), (500, 100), (100, 500), (500, 500)] {
                    assert_eq!(hit(hwnd, root.0, x, y), HTTRANSPARENT as isize);
                }
            }
            platform::apply_hwnd(
                root.0,
                vec![PhysicalLauncherRegion {
                    left: 90,
                    top: 90,
                    right: 110,
                    bottom: 110,
                }],
            )
            .unwrap();
            assert_eq!(hit(root.0, root.0, 378, 360), HTTRANSPARENT as isize);
            assert_eq!(hit(child.0, root.0, 100, 100), HTCLIENT as isize);
            let key = root.0 .0 as isize;
            drop(child);
            assert!(platform::has_regions(key));
            drop(root);
            assert!(!platform::has_regions(key));
        }

        #[test]
        fn failed_subclass_update_keeps_previous_regions_and_removes_partial_installation() {
            let root = TestWindow::new(None);
            platform::apply_hwnd(root.0, icon()).unwrap();
            let child = TestWindow::new(Some(root.0));
            let error = platform::install_and_replace(
                root.0,
                &[root.0, child.0, HWND::default()],
                vec![PhysicalLauncherRegion {
                    left: 0,
                    top: 0,
                    right: 20,
                    bottom: 20,
                }],
            );
            assert!(error.is_err());
            assert_eq!(hit(root.0, root.0, 378, 360), HTCLIENT as isize);
            assert_eq!(hit(root.0, root.0, 10, 10), HTTRANSPARENT as isize);
            assert!(!platform::is_installed(child.0));
        }
    }
}
