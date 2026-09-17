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
        sync::{OnceLock, RwLock},
    };
    use windows::{
        core::BOOL,
        Win32::{
            Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
            Graphics::Gdi::ScreenToClient,
            UI::{
                Shell::{
                    DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass,
                },
                WindowsAndMessaging::{
                    EnumChildWindows, GetWindowThreadProcessId, HTCLIENT, HTTRANSPARENT,
                    WM_NCDESTROY, WM_NCHITTEST,
                },
            },
        },
    };

    const LAUNCHER_SUBCLASS_ID: usize = 0x414C_444C;
    static REGIONS: OnceLock<RwLock<HashMap<isize, Vec<PhysicalLauncherRegion>>>> = OnceLock::new();

    fn regions() -> &'static RwLock<HashMap<isize, Vec<PhysicalLauncherRegion>>> {
        REGIONS.get_or_init(|| RwLock::new(HashMap::new()))
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
            // WM_NCHITTEST packs signed screen coordinates, including negative monitors.
            let packed = lparam.0 as u32;
            let mut point = POINT {
                x: (packed & 0xffff) as i16 as i32,
                y: ((packed >> 16) & 0xffff) as i16 as i32,
            };
            let root = HWND(root_key as *mut _);
            // Regions always use launcher-client coordinates, also on offset children.
            if ScreenToClient(root, &mut point).as_bool() {
                if let Ok(guard) = regions().read() {
                    if let Some(active) = guard.get(&(root_key as isize)) {
                        return match launcher_hit_result((point.x, point.y), active) {
                            LauncherHitResult::Client => LRESULT(HTCLIENT as isize),
                            LauncherHitResult::Transparent => LRESULT(HTTRANSPARENT as isize),
                        };
                    }
                }
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

    pub(super) fn apply_hwnd(
        root: HWND,
        active: Vec<PhysicalLauncherRegion>,
    ) -> Result<(), String> {
        let owner = unsafe { GetWindowThreadProcessId(root, None) };
        let targets: Vec<_> = window_tree(root)
            .into_iter()
            // Foreign-thread Chromium HWNDs cannot legally use SetWindowSubclass.
            // The smoke diagnostic reports them; OS routing still needs visible validation.
            .filter(|hwnd| unsafe { GetWindowThreadProcessId(*hwnd, None) } == owner)
            .collect();
        install_and_replace(root, &targets, active)
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
