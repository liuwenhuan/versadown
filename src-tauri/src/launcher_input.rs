use serde::Deserialize;
use tauri::WebviewWindow;

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
            let left = (region.x * scale_factor).round() as i32;
            let top = (region.y * scale_factor).round() as i32;
            let right = ((region.x + region.width) * scale_factor).round() as i32;
            let bottom = ((region.y + region.height) * scale_factor).round() as i32;
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

#[tauri::command]
pub fn set_launcher_input_regions(
    _window: WebviewWindow,
    _regions: Vec<LauncherInputRegion>,
) -> Result<(), String> {
    Err("当前平台尚未安装悬浮窗输入区域适配器".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
