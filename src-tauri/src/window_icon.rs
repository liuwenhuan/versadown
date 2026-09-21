//! Theme-aware native window icons.
//!
//! Tauri's public `WebviewWindow::set_icon` maps to Windows `ICON_SMALL`,
//! which updates the title-bar glyph only.  The taskbar reads `ICON_BIG`, so
//! Windows receives that handle explicitly below while other platforms retain
//! Tauri's standard window-icon update.

use std::sync::Mutex;

use tauri::{AppHandle, Manager};

const ICON_SIZE: u32 = 256;

#[derive(Default)]
pub struct TaskbarIconState {
    #[cfg(windows)]
    icon: Mutex<Option<isize>>,
}

#[cfg(windows)]
impl Drop for TaskbarIconState {
    fn drop(&mut self) {
        let Ok(icon) = self.icon.get_mut() else {
            return;
        };
        if let Some(icon) = icon.take() {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(
                    windows::Win32::UI::WindowsAndMessaging::HICON(icon as *mut std::ffi::c_void),
                );
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

impl Rgb {
    const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

#[derive(Clone, Copy)]
struct Palette {
    top_start: Rgb,
    top_end: Rgb,
    left_start: Rgb,
    left_end: Rgb,
    right_start: Rgb,
    right_end: Rgb,
    rim_start: Rgb,
    rim_end: Rgb,
    logo_left: Rgb,
    logo_right: Rgb,
}

fn latest_lighting_palette(
    surface: Rgb,
    accent: Rgb,
    logo_left: Rgb,
    logo_right: Rgb,
) -> Palette {
    // Match the latest artwork's lighting hierarchy: a softly lit top,
    // left face brightening into the light, and the right face falling into
    // shadow.  Non-mint themes retain their own accent through these same
    // relative stops.
    Palette {
        top_start: mix(surface, accent, 0.30),
        top_end: mix(surface, accent, 0.37),
        left_start: mix(surface, accent, 0.37),
        left_end: mix(surface, accent, 0.16),
        right_start: mix(surface, accent, 0.16),
        right_end: mix(surface, accent, 0.42),
        rim_start: mix(surface, accent, 0.18),
        rim_end: mix(surface, accent, 0.37),
        logo_left,
        logo_right,
    }
}

fn palette_for(theme: &str) -> Palette {
    match theme {
        "clear-blue" => latest_lighting_palette(
            Rgb::new(255, 255, 255),
            Rgb::new(79, 125, 243),
            Rgb::new(255, 255, 255),
            Rgb::new(244, 250, 246),
        ),
        "warm-gold" => latest_lighting_palette(
            Rgb::new(255, 254, 250),
            Rgb::new(181, 139, 0),
            Rgb::new(255, 255, 255),
            Rgb::new(244, 250, 246),
        ),
        "lavender" => latest_lighting_palette(
            Rgb::new(255, 255, 255),
            Rgb::new(138, 108, 188),
            Rgb::new(255, 255, 255),
            Rgb::new(244, 250, 246),
        ),
        "coral" => latest_lighting_palette(
            Rgb::new(255, 255, 255),
            Rgb::new(200, 100, 123),
            Rgb::new(255, 255, 255),
            Rgb::new(244, 250, 246),
        ),
        "deep-ocean" => latest_lighting_palette(
            Rgb::new(26, 36, 50),
            Rgb::new(101, 166, 255),
            Rgb::new(232, 244, 236),
            Rgb::new(210, 228, 218),
        ),
        "night-violet" => latest_lighting_palette(
            Rgb::new(36, 30, 49),
            Rgb::new(174, 145, 245),
            Rgb::new(238, 230, 255),
            Rgb::new(220, 208, 244),
        ),
        // The bundled mint SVG is used verbatim for the default theme so
        // launch, taskbar, tray and the main-window mark share its exact
        // source color stops.  An invalid persisted value follows that same
        // default, matching the CSS fallback.
        _ => Palette {
            top_start: Rgb::new(196, 224, 212),
            top_end: Rgb::new(182, 216, 202),
            left_start: Rgb::new(182, 216, 202),
            left_end: Rgb::new(224, 238, 230),
            right_start: Rgb::new(224, 238, 230),
            right_end: Rgb::new(168, 202, 190),
            rim_start: Rgb::new(220, 236, 228),
            rim_end: Rgb::new(182, 216, 202),
            logo_left: Rgb::new(255, 255, 255),
            logo_right: Rgb::new(244, 250, 246),
        },
    }
}

fn mix(base: Rgb, overlay: Rgb, amount: f32) -> Rgb {
    let blend = |from: u8, to: u8| (from as f32 + (to as f32 - from as f32) * amount).round() as u8;
    Rgb::new(
        blend(base.r, overlay.r),
        blend(base.g, overlay.g),
        blend(base.b, overlay.b),
    )
}

fn gradient_progress(x: f32, y: f32, start: (f32, f32), end: (f32, f32)) -> f32 {
    let direction = (end.0 - start.0, end.1 - start.1);
    let length_squared = direction.0 * direction.0 + direction.1 * direction.1;
    (((x - start.0) * direction.0 + (y - start.1) * direction.1) / length_squared).clamp(0.0, 1.0)
}

fn gradient_color(start: Rgb, end: Rgb, progress: f32) -> Rgb {
    mix(start, end, progress)
}

fn point_in_polygon(x: f32, y: f32, polygon: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let mut previous = polygon.len() - 1;
    for current in 0..polygon.len() {
        let (current_x, current_y) = polygon[current];
        let (previous_x, previous_y) = polygon[previous];
        let crosses = (current_y > y) != (previous_y > y)
            && x < (previous_x - current_x) * (y - current_y) / (previous_y - current_y)
                + current_x;
        if crosses {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

fn set_pixel(pixels: &mut [u8], x: u32, y: u32, color: Rgb) {
    let index = ((y * ICON_SIZE + x) * 4) as usize;
    pixels[index] = color.r;
    pixels[index + 1] = color.g;
    pixels[index + 2] = color.b;
    pixels[index + 3] = 255;
}

pub(crate) fn rasterize_icon(theme: &str) -> Vec<u8> {
    let palette = palette_for(theme);
    let mut pixels = vec![0; (ICON_SIZE * ICON_SIZE * 4) as usize];
    // The same cropped drawing bounds used by FloatingLauncher. Coordinates
    // remain in the source SVG's 1024px space so its facets stay aligned.
    const MIN_X: f32 = 175.0;
    const MIN_Y: f32 = 123.0;
    const WIDTH: f32 = 674.0;
    const HEIGHT: f32 = 778.0;
    let scale = ICON_SIZE as f32 / HEIGHT;
    let offset_x = (ICON_SIZE as f32 - WIDTH * scale) / 2.0;
    let to_source = |value: u32, offset: f32, min: f32| (value as f32 - offset) / scale + min;

    let rim = [
        (512.0, 122.88),
        (848.988, 317.44),
        (848.988, 706.56),
        (512.0, 901.12),
        (175.012, 706.56),
        (175.012, 317.44),
    ];
    let top = [
        (512.0, 140.39),
        (833.823, 326.195),
        (512.0, 512.0),
        (190.177, 326.195),
    ];
    let left = [
        (190.177, 326.195),
        (512.0, 512.0),
        (512.0, 883.61),
        (190.177, 697.805),
    ];
    let right = [
        (833.823, 326.195),
        (833.823, 697.805),
        (512.0, 883.61),
        (512.0, 512.0),
    ];
    let logo_top = [
        (333.396, 408.883),
        (512.0, 512.0),
        (690.604, 408.883),
        (690.604, 492.544),
        (512.0, 595.661),
        (333.396, 492.544),
    ];
    let logo_bottom = [
        (333.396, 560.64),
        (512.0, 663.757),
        (690.604, 560.64),
        (690.604, 644.301),
        (512.0, 747.418),
        (333.396, 644.301),
    ];

    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let source_x = to_source(x, offset_x, MIN_X);
            let source_y = to_source(y, 0.0, MIN_Y);
            if !point_in_polygon(source_x, source_y, &rim) {
                continue;
            }
            let top_progress = gradient_progress(source_x, source_y, (307.2, 0.0), (716.8, 1024.0));
            let left_progress = gradient_progress(source_x, source_y, (0.0, 102.4), (1024.0, 921.6));
            let right_progress = gradient_progress(source_x, source_y, (1024.0, 0.0), (0.0, 1024.0));
            let rim_progress = gradient_progress(source_x, source_y, (307.2, 0.0), (716.8, 1024.0));
            let mut color = gradient_color(palette.rim_start, palette.rim_end, rim_progress);
            if point_in_polygon(source_x, source_y, &top) {
                color = gradient_color(palette.top_start, palette.top_end, top_progress);
            } else if point_in_polygon(source_x, source_y, &left) {
                color = gradient_color(palette.left_start, palette.left_end, left_progress);
            } else if point_in_polygon(source_x, source_y, &right) {
                color = gradient_color(palette.right_start, palette.right_end, right_progress);
            }
            if point_in_polygon(source_x, source_y, &logo_top)
                || point_in_polygon(source_x, source_y, &logo_bottom)
            {
                color = if source_x <= 512.0 {
                    palette.logo_left
                } else {
                    palette.logo_right
                };
            }
            set_pixel(&mut pixels, x, y, color);
        }
    }
    pixels
}

pub(crate) fn icon_image(theme: &str) -> tauri::image::Image<'static> {
    tauri::image::Image::new_owned(rasterize_icon(theme), ICON_SIZE, ICON_SIZE)
}

fn sync_tray_icon(app: &AppHandle, theme: &str) {
    if let Some(tray) = app.tray_by_id(crate::launcher::TRAY_ID) {
        let _ = tray.set_icon(Some(icon_image(theme)));
    }
}

#[cfg(windows)]
fn set_windows_taskbar_icon(
    app: &AppHandle,
    window: &tauri::WebviewWindow,
    rgba: &[u8],
) -> Result<(), String> {
    use windows::Win32::{
        Foundation::{LPARAM, WPARAM},
        UI::WindowsAndMessaging::{
            CreateIcon, DestroyIcon, SendMessageW, HICON, ICON_BIG, WM_SETICON,
        },
    };

    let mut bgra = rgba.to_vec();
    let mut and_mask = Vec::with_capacity((ICON_SIZE * ICON_SIZE) as usize);
    for pixel in bgra.chunks_exact_mut(4) {
        and_mask.push(pixel[3].wrapping_sub(u8::MAX));
        pixel.swap(0, 2);
    }
    let icon = unsafe {
        CreateIcon(
            None,
            ICON_SIZE as i32,
            ICON_SIZE as i32,
            1,
            32,
            and_mask.as_ptr(),
            bgra.as_ptr(),
        )
    }
    .map_err(|error| error.to_string())?;
    let hwnd = window.hwnd().map_err(|error| error.to_string())?;
    unsafe {
        SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_BIG as usize)),
            Some(LPARAM(icon.0 as isize)),
        );
    }

    let state = app.state::<TaskbarIconState>();
    let mut previous = state
        .icon
        .lock()
        .map_err(|_| "任务栏图标状态不可用".to_string())?;
    if let Some(previous) = previous.replace(icon.0 as isize) {
        unsafe {
            let _ = DestroyIcon(HICON(previous as *mut std::ffi::c_void));
        }
    }
    Ok(())
}

#[tauri::command]
pub fn set_main_window_icon(app: AppHandle, theme: String) -> Result<(), String> {
    let window = app.get_webview_window("main").ok_or("主窗口不存在")?;
    let rgba = rasterize_icon(&theme);
    // This remains the correct platform-neutral path for Linux and the small
    // native title-bar glyph on Windows.
    window
        .set_icon(tauri::image::Image::new_owned(
            rgba.clone(),
            ICON_SIZE,
            ICON_SIZE,
        ))
        .map_err(|error| error.to_string())?;
    #[cfg(windows)]
    set_windows_taskbar_icon(&app, &window, &rgba)?;
    sync_tray_icon(&app, &theme);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_a_transparent_square_icon_with_colored_artwork() {
        let icon = rasterize_icon("night-violet");
        assert_eq!(icon.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
        assert_eq!(&icon[..4], &[0, 0, 0, 0]);
        assert!(icon.chunks_exact(4).any(|pixel| pixel[3] == 255));
    }

    #[test]
    fn tray_raster_keeps_the_visible_mark_nearly_full_height() {
        let icon = rasterize_icon("mint");
        let opaque = icon
            .chunks_exact(4)
            .enumerate()
            .filter_map(|(index, pixel)| {
                (pixel[3] > 0).then_some((index as u32 % ICON_SIZE, index as u32 / ICON_SIZE))
            })
            .collect::<Vec<_>>();
        let min_y = opaque.iter().map(|(_, y)| *y).min().unwrap();
        let max_y = opaque.iter().map(|(_, y)| *y).max().unwrap();
        let min_x = opaque.iter().map(|(x, _)| *x).min().unwrap();
        let max_x = opaque.iter().map(|(x, _)| *x).max().unwrap();
        assert!(
            max_y - min_y >= 250,
            "tray artwork must fill its icon height"
        );
        assert!(
            max_x - min_x >= 220,
            "tray artwork must not retain export-side padding"
        );
    }

    #[test]
    fn each_theme_has_a_distinct_accent() {
        assert_ne!(
            palette_for("mint").top_start,
            palette_for("clear-blue").top_start
        );
        assert_ne!(
            palette_for("deep-ocean").top_start,
            palette_for("night-violet").top_start
        );
    }

    #[test]
    fn mint_uses_the_supplied_left_lit_top_gradient() {
        let icon = rasterize_icon("mint");
        let pixel = |x: u32, y: u32| {
            let index = ((y * ICON_SIZE + x) * 4) as usize;
            [icon[index], icon[index + 1], icon[index + 2], icon[index + 3]]
        };

        // Source point (512, about 214): the updated mint artwork's top
        // facet is #C1DED2 there after its left-to-right light falloff.
        assert_eq!(pixel(128, 30), [193, 222, 210, 255]);
    }
}
