# Single-WebView Flicker-Free Launcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep the existing single WebView2 launcher while making left- and right-click popup transitions instant and flicker-free, without moving, resizing, hiding, clipping, or recreating the native launcher window.

**Architecture:** The launcher becomes one fixed 410 × 720 logical-pixel transparent canvas. Its 64 × 64 hexagon stays at one permanent internal coordinate, while the quick-add panel and context menu occupy preallocated slots above or below it. React owns a three-state surface machine (`compact`, `quick-add`, `context-menu`); Rust owns DPI conversion and platform input hit testing so inactive transparent pixels pass mouse input to windows behind the launcher.

**Tech Stack:** React 18, TypeScript, Vitest/Testing Library, Tauri 2, Rust, Windows `WM_NCHITTEST`/window subclassing, WebView2.

**Spec:** `docs/superpowers/specs/2026-09-17-single-webview-launcher-no-flicker-design.md`

## Global Constraints

- Keep exactly one launcher WebView; do not introduce Slint, Flutter, Qt, a second WebView, or an auxiliary popup window.
- Opening, closing, or switching a popup must not change the native launcher's position, size, visibility, or visual clipping region.
- The hexagon DOM node must remain mounted and at the same canvas coordinate for every surface state.
- Transparent pixels outside the active launcher and popup rectangles must pass mouse input to the application behind them.
- If an input-region update fails, retain the last valid region; never fall back to making the entire 410 × 720 canvas interactive.
- Windows is the first implementation target. Non-Windows builds must keep compiling through a platform adapter whose unsupported implementation returns a clear error without leaking Windows APIs into React.
- Preserve existing menu commands, themes, quick-add behavior, dragging, visibility settings, and manager-window actions.
- Never stage the unrelated existing changes in `src/App.tsx` or `src/App.live.test.tsx` while implementing this plan.
- Follow `AGENTS.md`: run only the focused launcher Vitest/Rust tests plus the normal frontend and release builds; do not run either full test suite unless a focused test exposes shared-infrastructure risk.

## File Map

- `src/lib/launcherWindow.ts`: fixed-canvas constants, popup placement, canvas anchoring, input-region command wrapper, and launcher native helpers.
- `src/lib/launcherWindow.test.ts`: pure geometry, DPI-independent frontend contract, and command payload tests.
- `src/components/LauncherWindow.tsx`: single surface state machine and serialized last-intent-wins transitions.
- `src/components/LauncherWindow.test.tsx`: DOM stability, transition ordering, no-resize assertions, Escape/blur/drag behavior, and rapid switching.
- `src/components/FloatingLauncher.tsx`: persistent hexagon node and controlled quick-add panel rendering.
- `src/components/LauncherContextMenu.tsx`: menu container ref/region reporting without changing menu actions.
- `src/styles.css`: fixed-canvas coordinates, popup slots, visibility, and pointer-event rules.
- `src-tauri/src/launcher_input.rs`: logical-to-physical input regions, per-window registry, and Windows `WM_NCHITTEST` adapter.
- `src-tauri/src/launcher.rs`: fixed-canvas placement and launcher commands; remove popup-time resizing/clipping paths.
- `src-tauri/src/lib.rs`: module registration, startup initialization, and Tauri command registration.
- `src-tauri/Cargo.toml`: enable the Windows Controls API used by `SetWindowSubclass`.
- `src-tauri/tauri.conf.json`: fixed launcher dimensions.
- `scripts/smoke-launcher.cjs`: regression assertions for stable native bounds and left/right interaction.

---

### Task 1: Lock down the fixed-canvas geometry contract

**Files:**
- Modify: `src/lib/launcherWindow.test.ts`
- Modify: `src/lib/launcherWindow.ts`

**Interfaces:**
- Produces: `launcherCanvas`, `launcherCanvasPosition(workArea, scaleFactor)`, `launcherIconScreenRect(windowPosition, scaleFactor)`, and `launcherPopupPlacementFromIcon(iconRect, popupHeight, workArea)`.
- Consumes: Tauri physical monitor work-area coordinates and logical CSS pixel sizes.

- [ ] **Step 1: Replace resize-oriented geometry tests with fixed-canvas tests**

Add these focused cases to `src/lib/launcherWindow.test.ts` and remove assertions that expect compact/menu/expanded native sizes:

```ts
it('uses one fixed canvas with an invariant hexagon anchor', () => {
  expect(launcherWindow.launcherCanvas).toEqual({
    width: 410,
    height: 720,
    popupSlotHeight: 328,
    iconSize: 64,
    iconTop: 328,
    margin: 24,
  });
});

it('anchors the invariant hexagon to the work-area bottom right', () => {
  expect(launcherWindow.launcherCanvasPosition(
    { x: 100, y: 50, width: 1920, height: 1080 },
    1.5,
  )).toEqual({ x: 1369, y: 506 });
});

it('chooses the lower slot without moving the hexagon when upper space is insufficient', () => {
  const icon = { x: 850, y: 20, width: 64, height: 64 };
  expect(launcherWindow.launcherPopupPlacementFromIcon(
    icon,
    328,
    { x: 0, y: 0, width: 1000, height: 800 },
  )).toBe('below');
});

it('prefers the upper slot when the complete popup fits', () => {
  const icon = { x: 850, y: 500, width: 64, height: 64 };
  expect(launcherWindow.launcherPopupPlacementFromIcon(
    icon,
    328,
    { x: 0, y: 0, width: 1000, height: 800 },
  )).toBe('above');
});
```

The canvas-position expectation follows this physical-pixel formula:

```ts
const width = Math.round(410 * 1.5);          // 615
const iconBottom = Math.round((328 + 64) * 1.5); // 588
const margin = Math.round(24 * 1.5);          // 36
// x = 100 + 1920 - 615 - 36 = 1369
// y = 50 + 1080 - 588 - 36 = 506
```

- [ ] **Step 2: Run the geometry tests and confirm the old implementation fails**

Run:

```powershell
pnpm vitest run src/lib/launcherWindow.test.ts --exclude '.worktrees/**'
```

Expected: FAIL because `launcherCanvas`, `launcherCanvasPosition`, and `launcherPopupPlacementFromIcon` do not exist and old resize assertions no longer match.

- [ ] **Step 3: Implement the fixed geometry helpers**

In `src/lib/launcherWindow.ts`, define the public contract exactly as follows:

```ts
export const launcherCanvas = {
  width: 410,
  height: 720,
  popupSlotHeight: 328,
  iconSize: 64,
  iconTop: 328,
  margin: 24,
} as const;

type WorkArea = { x: number; y: number; width: number; height: number };
type Rect = { x: number; y: number; width: number; height: number };
export type LauncherPopupPlacement = 'above' | 'below';

export function launcherCanvasPosition(workArea: WorkArea, scaleFactor: number) {
  const canvasWidth = Math.round(launcherCanvas.width * scaleFactor);
  const iconBottom = Math.round(
    (launcherCanvas.iconTop + launcherCanvas.iconSize) * scaleFactor,
  );
  const margin = Math.round(launcherCanvas.margin * scaleFactor);
  return {
    x: workArea.x + workArea.width - canvasWidth - margin,
    y: workArea.y + workArea.height - iconBottom - margin,
  };
}

export function launcherIconScreenRect(
  windowPosition: { x: number; y: number },
  scaleFactor: number,
): Rect {
  return {
    x: windowPosition.x + Math.round((launcherCanvas.width - launcherCanvas.iconSize) * scaleFactor),
    y: windowPosition.y + Math.round(launcherCanvas.iconTop * scaleFactor),
    width: Math.round(launcherCanvas.iconSize * scaleFactor),
    height: Math.round(launcherCanvas.iconSize * scaleFactor),
  };
}

export function launcherPopupPlacementFromIcon(
  icon: Rect,
  popupHeight: number,
  workArea: WorkArea,
): LauncherPopupPlacement {
  return icon.y - popupHeight < workArea.y ? 'below' : 'above';
}
```

Keep `launcherDimensions`, `launcherCompactSize`, `launcherResizedPosition`, and `launcherPopupPlacement` temporarily so this commit remains runnable with the old React transition path. Task 7 deletes them only after the fixed-canvas state machine is active.

- [ ] **Step 4: Run the focused geometry tests**

Run:

```powershell
pnpm vitest run src/lib/launcherWindow.test.ts --exclude '.worktrees/**'
```

Expected: PASS.

- [ ] **Step 5: Commit only the geometry files**

```powershell
git add src/lib/launcherWindow.ts src/lib/launcherWindow.test.ts
git commit -m "refactor: define fixed launcher canvas geometry"
```

---

### Task 2: Add a tested platform-neutral input-region model

**Files:**
- Create: `src-tauri/src/launcher_input.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces: `LauncherInputRegion`, `PhysicalLauncherRegion`, `physical_regions(regions, scale_factor)`, `point_hits_regions(point, regions)`, and `set_launcher_input_regions`.
- Consumes: logical CSS-pixel rectangles supplied by React and the launcher window scale factor.

- [ ] **Step 1: Write pure Rust tests before native window code**

Create `src-tauri/src/launcher_input.rs` with the serializable input type, private physical type, and tests first:

```rust
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
    unimplemented!()
}

fn point_hits_regions(
    point: (i32, i32),
    regions: &[PhysicalLauncherRegion],
) -> bool {
    unimplemented!()
}

#[tauri::command]
pub fn set_launcher_input_regions(
    window: WebviewWindow,
    regions: Vec<LauncherInputRegion>,
) -> Result<(), String> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_logical_regions_at_fractional_dpi() {
        let regions = physical_regions(
            &[LauncherInputRegion { x: 346.0, y: 328.0, width: 64.0, height: 64.0 }],
            1.5,
        ).unwrap();
        assert_eq!(regions, vec![PhysicalLauncherRegion {
            left: 519,
            top: 492,
            right: 615,
            bottom: 588,
        }]);
    }

    #[test]
    fn rejects_invalid_regions_without_replacing_the_last_valid_set() {
        let error = physical_regions(
            &[LauncherInputRegion { x: 0.0, y: 0.0, width: -1.0, height: 64.0 }],
            1.0,
        ).unwrap_err();
        assert_eq!(error, "悬浮窗输入区域无效");
    }

    #[test]
    fn hit_testing_includes_left_top_and_excludes_right_bottom_edges() {
        let region = PhysicalLauncherRegion { left: 10, top: 20, right: 30, bottom: 40 };
        assert!(point_hits_regions((10, 20), &[region]));
        assert!(point_hits_regions((29, 39), &[region]));
        assert!(!point_hits_regions((30, 40), &[region]));
        assert!(!point_hits_regions((9, 20), &[region]));
    }
}
```

Add `mod launcher_input;` beside the other top-level modules in `src-tauri/src/lib.rs`.

- [ ] **Step 2: Run the focused Rust tests and confirm failure**

Run:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml launcher_input::tests --lib
```

Expected: FAIL because the two pure helpers are unimplemented.

- [ ] **Step 3: Implement validation and DPI conversion**

Replace the two pure helper bodies with:

```rust
fn physical_regions(
    regions: &[LauncherInputRegion],
    scale_factor: f64,
) -> Result<Vec<PhysicalLauncherRegion>, String> {
    if !scale_factor.is_finite() || scale_factor <= 0.0 || regions.is_empty() {
        return Err("悬浮窗输入区域无效".to_string());
    }
    regions.iter().map(|region| {
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
        Ok(PhysicalLauncherRegion { left, top, right, bottom })
    }).collect()
}

fn point_hits_regions(
    (x, y): (i32, i32),
    regions: &[PhysicalLauncherRegion],
) -> bool {
    regions.iter().any(|region| {
        x >= region.left && x < region.right && y >= region.top && y < region.bottom
    })
}
```

Keep `set_launcher_input_regions` temporarily returning the explicit unsupported error below; Task 3 replaces it with the native adapter:

```rust
#[tauri::command]
pub fn set_launcher_input_regions(
    _window: WebviewWindow,
    _regions: Vec<LauncherInputRegion>,
) -> Result<(), String> {
    Err("当前平台尚未安装悬浮窗输入区域适配器".to_string())
}
```

- [ ] **Step 4: Run the focused Rust tests**

Run:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml launcher_input::tests --lib
```

Expected: PASS with 3 tests.

- [ ] **Step 5: Commit the region model**

```powershell
git add src-tauri/src/launcher_input.rs src-tauri/src/lib.rs
git commit -m "feat: model launcher input regions"
```

---

### Task 3: Prove Windows transparent hit testing before migrating React

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/src/launcher_input.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `scripts/smoke-launcher.cjs`

**Interfaces:**
- Produces: an installed Windows HWND subclass that returns `HTCLIENT` inside the last valid active regions and `HTTRANSPARENT` elsewhere.
- Consumes: `set_launcher_input_regions(window, regions)` from Task 2.

- [ ] **Step 1: Add a deterministic hit-result test**

Add this platform-neutral helper and test to `launcher_input.rs`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LauncherHitResult { Client, Transparent }

fn launcher_hit_result(
    point: (i32, i32),
    regions: &[PhysicalLauncherRegion],
) -> LauncherHitResult {
    if point_hits_regions(point, regions) {
        LauncherHitResult::Client
    } else {
        LauncherHitResult::Transparent
    }
}

#[test]
fn outside_the_active_regions_is_transparent() {
    let regions = [PhysicalLauncherRegion { left: 519, top: 492, right: 615, bottom: 588 }];
    assert_eq!(launcher_hit_result((550, 520), &regions), LauncherHitResult::Client);
    assert_eq!(launcher_hit_result((100, 100), &regions), LauncherHitResult::Transparent);
}
```

- [ ] **Step 2: Enable the Windows subclass API**

Append `"Win32_UI_Controls"` to the existing `windows` feature list in `src-tauri/Cargo.toml` without changing the crate version.

- [ ] **Step 3: Implement the Windows registry and subclass callback**

Under `#[cfg(windows)]`, add a registry keyed by HWND value and install exactly one subclass per launcher window:

```rust
#[cfg(windows)]
mod platform {
    use super::{launcher_hit_result, LauncherHitResult, PhysicalLauncherRegion};
    use std::{collections::HashMap, sync::{OnceLock, RwLock}};
    use tauri::WebviewWindow;
    use windows::{
        core::BOOL,
        Win32::{
            Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
            UI::{
                Controls::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
                WindowsAndMessaging::{ScreenToClient, HTCLIENT, HTTRANSPARENT, WM_NCHITTEST, WM_NCDESTROY},
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
        _ref_data: usize,
    ) -> LRESULT {
        if message == WM_NCHITTEST {
            let packed = lparam.0 as u32;
            let mut point = POINT {
                x: (packed & 0xffff) as i16 as i32,
                y: ((packed >> 16) & 0xffff) as i16 as i32,
            };
            let _ = ScreenToClient(hwnd, &mut point);
            if let Ok(guard) = regions().read() {
                if let Some(active) = guard.get(&(hwnd.0 as isize)) {
                    return match launcher_hit_result((point.x, point.y), active) {
                        LauncherHitResult::Client => LRESULT(HTCLIENT as isize),
                        LauncherHitResult::Transparent => LRESULT(HTTRANSPARENT as isize),
                    };
                }
            }
        }
        if message == WM_NCDESTROY {
            if let Ok(mut guard) = regions().write() {
                guard.remove(&(hwnd.0 as isize));
            }
            let _ = RemoveWindowSubclass(hwnd, Some(subclass_proc), LAUNCHER_SUBCLASS_ID);
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }

    pub fn apply(
        window: &WebviewWindow,
        active: Vec<PhysicalLauncherRegion>,
    ) -> Result<(), String> {
        if window.label() != "launcher" {
            return Err("只能设置悬浮球窗口的输入区域".to_string());
        }
        let hwnd = window.hwnd().map_err(|error| error.to_string())?;
        unsafe {
            SetWindowSubclass(hwnd, Some(subclass_proc), LAUNCHER_SUBCLASS_ID, 0)
                .ok()
                .map_err(|error| format!("无法安装悬浮窗命中测试: {error}"))?;
        }
        regions().write()
            .map_err(|_| "悬浮窗输入区域状态不可用".to_string())?
            .insert(hwnd.0 as isize, active);
        Ok(())
    }
}
```

If the `windows` 0.61 signatures differ, follow the compiler's exact generated signature while preserving this ownership rule: validate and convert first, install the subclass second, replace the registry entry last. This guarantees a failed update cannot erase the previous valid region.

Implement the command as:

```rust
#[tauri::command]
pub fn set_launcher_input_regions(
    window: WebviewWindow,
    regions: Vec<LauncherInputRegion>,
) -> Result<(), String> {
    let scale_factor = window.scale_factor().map_err(|error| error.to_string())?;
    let physical = physical_regions(&regions, scale_factor)?;
    #[cfg(windows)]
    return platform::apply(&window, physical);
    #[cfg(not(windows))]
    {
        let _ = (window, physical);
        Err("当前平台尚未安装悬浮窗输入区域适配器".to_string())
    }
}
```

Register `launcher_input::set_launcher_input_regions` in `tauri::generate_handler!` in `src-tauri/src/lib.rs`.

- [ ] **Step 4: Run the focused Rust tests and compile the Windows target**

Run:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml launcher_input::tests --lib
cargo check --manifest-path src-tauri/Cargo.toml --features custom-protocol
```

Expected: both commands PASS. Compiler signature errors in the Win32 bindings must be resolved before proceeding.

- [ ] **Step 5: Add a smoke-only icon-region initialization path**

Extend `scripts/smoke-launcher.cjs` so its launcher setup invokes:

```js
await evaluate(
  launcher,
  `window.__TAURI_INTERNALS__.invoke('set_launcher_input_regions', {
    regions: [{ x: 346, y: 328, width: 64, height: 64 }]
  })`,
);
```

The existing smoke script drives DOM events through the WebView2 DevTools endpoint, so it proves the command path and region registry but does not prove operating-system mouse routing. Keep that distinction explicit in the output.

- [ ] **Step 6: Run the Windows smoke proof**

Run:

```powershell
pnpm run test:launcher
```

Expected: PASS; the input-region command is accepted, the icon path remains responsive, and native bounds stay unchanged. The visible operating-system routing check below remains mandatory because DevTools-dispatched events cannot prove click-through.

After the automated command-path smoke passes, run the app visibly with `pnpm tauri dev`. Put a click-observable native window (Notepad is sufficient) behind each quadrant of the launcher canvas, then click one point outside the 64 × 64 icon in every quadrant. Each click must activate/place the caret in the background window, while a click at the icon center must reach the launcher. If any transparent click is consumed, use `EnumChildWindows` to locate the WebView2 child HWND and install the same subclass on that child as well as the top-level launcher HWND. Repeat this manual proof before Task 4; this is the hard checkpoint for the child-HWND risk.

- [ ] **Step 7: Commit the native input adapter**

```powershell
git add src-tauri/Cargo.toml src-tauri/src/launcher_input.rs src-tauri/src/lib.rs scripts/smoke-launcher.cjs
git commit -m "feat: add launcher transparent input hit testing"
```

---

### Task 4: Create the fixed native launcher canvas

> **Atomic migration batch:** Tasks 4–6 change one cross-process contract. Do not commit or launch the partially migrated app after Task 4 or Task 5; complete Task 6, run its focused tests/build, and commit the entire fixed-canvas migration together. This avoids a release point where React expects 410 × 720 while Tauri still resizes to 64 × 64, or vice versa.

**Files:**
- Modify: `src-tauri/tauri.conf.json`
- Modify: `src-tauri/src/launcher.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src/lib/launcherWindow.ts`
- Modify: `src/lib/launcherWindow.test.ts`

**Interfaces:**
- Produces: a launcher window whose physical bounds are initialized once and whose icon-only input region is installed before first show.
- Consumes: `launcherCanvasPosition` and `set_launcher_input_regions` from Tasks 1–3.

- [ ] **Step 1: Add frontend command-contract tests**

Add to `src/lib/launcherWindow.test.ts`:

```ts
it('installs the compact icon input region without changing native bounds', async () => {
  await launcherWindow.setLauncherInputRegions([
    { x: 346, y: 328, width: 64, height: 64 },
  ]);
  expect(native.invoke).toHaveBeenCalledWith('set_launcher_input_regions', {
    regions: [{ x: 346, y: 328, width: 64, height: 64 }],
  });
  expect(native.invoke).not.toHaveBeenCalledWith('set_launcher_bounds', expect.anything());
});
```

- [ ] **Step 2: Implement the frontend region wrapper and fixed anchor**

Add to `src/lib/launcherWindow.ts`:

```ts
export type LauncherInputRegion = {
  x: number;
  y: number;
  width: number;
  height: number;
};

export async function setLauncherInputRegions(regions: LauncherInputRegion[]) {
  if (!isTauriRuntime()) return;
  await invoke('set_launcher_input_regions', { regions });
}

export const launcherIconRegion: LauncherInputRegion = {
  x: launcherCanvas.width - launcherCanvas.iconSize,
  y: launcherCanvas.iconTop,
  width: launcherCanvas.iconSize,
  height: launcherCanvas.iconSize,
};
```

Rewrite `anchorLauncherToWorkArea()` to set only the fixed canvas's startup/drag-end position:

```ts
export async function anchorLauncherToWorkArea() {
  if (!isTauriRuntime()) return;
  const launcher = getCurrentWindow();
  const monitor = await currentMonitor();
  if (!monitor) return;
  const position = launcherCanvasPosition(
    {
      x: monitor.workArea.position.x,
      y: monitor.workArea.position.y,
      width: monitor.workArea.size.width,
      height: monitor.workArea.size.height,
    },
    monitor.scaleFactor,
  );
  await launcher.setPosition(new PhysicalPosition(position.x, position.y));
}
```

Also replace the old size-dependent placement query with the fixed-anchor query below:

```ts
export async function getLauncherPopupPlacement(): Promise<LauncherPopupPlacement> {
  if (!isTauriRuntime()) return 'above';
  const launcher = getCurrentWindow();
  const [position, monitor] = await Promise.all([
    launcher.outerPosition(),
    currentMonitor(),
  ]);
  if (!monitor) return 'above';
  const icon = launcherIconScreenRect(position, monitor.scaleFactor);
  return launcherPopupPlacementFromIcon(
    icon,
    Math.round(launcherCanvas.popupSlotHeight * monitor.scaleFactor),
    {
      x: monitor.workArea.position.x,
      y: monitor.workArea.position.y,
      width: monitor.workArea.size.width,
      height: monitor.workArea.size.height,
    },
  );
}
```

- [ ] **Step 3: Make the Tauri launcher dimensions fixed**

Change only the launcher window entry in `src-tauri/tauri.conf.json` to:

```json
{
  "label": "launcher",
  "title": "",
  "width": 410,
  "height": 720,
  "minWidth": 410,
  "minHeight": 720,
  "maxWidth": 410,
  "maxHeight": 720,
  "resizable": false,
  "decorations": false,
  "transparent": true,
  "shadow": false,
  "alwaysOnTop": true,
  "skipTaskbar": true,
  "visible": false
}
```

- [ ] **Step 4: Remove startup visual clipping and initialize safe input before show**

In `src-tauri/src/lib.rs`, replace the startup call to `set_launcher_compact_shape` with:

```rust
launcher_input::set_launcher_input_regions(
    launcher.clone(),
    vec![launcher_input::LauncherInputRegion {
        x: 346.0,
        y: 328.0,
        width: 64.0,
        height: 64.0,
    }],
).map_err(std::io::Error::other)?;
launcher.show()?;
```

Leave visibility-policy `show`/`hide` commands intact; they are allowed when the user's launcher visibility setting changes, but they must not be called for popup transitions.

- [ ] **Step 5: Keep the old popup commands registered only for the migration window**

Do not delete `set_launcher_bounds` or `set_launcher_compact_shape` in this task because the production React entry point still imports them until Task 5. Add a comment above each command stating `// Compatibility only until the fixed-canvas React state machine lands; Task 7 removes this path.` Task 5 removes all runtime callers, and Task 7 deletes the commands and clipping code.

- [ ] **Step 6: Run focused tests and builds**

Run:

```powershell
pnpm vitest run src/lib/launcherWindow.test.ts --exclude '.worktrees/**'
cargo test --manifest-path src-tauri/Cargo.toml launcher_input::tests --lib
cargo check --manifest-path src-tauri/Cargo.toml --features custom-protocol
```

Expected: all PASS.

- [ ] **Step 7: Continue directly to the React migration**

Do not commit or run the desktop app at this intermediate point. Keep the focused unit-test results, then continue directly to Task 5.

---

### Task 5: Replace resize transitions with one last-intent-wins React state machine

**Files:**
- Modify: `src/components/LauncherWindow.test.tsx`
- Modify: `src/components/LauncherWindow.tsx`
- Modify: `src/components/FloatingLauncher.tsx`
- Modify: `src/components/LauncherContextMenu.tsx`
- Modify: `src/lib/launcherWindow.ts`

**Interfaces:**
- Produces: `LauncherSurfaceMode = 'compact' | 'quick-add' | 'context-menu'` and a serialized `requestMode(nextMode)` transition.
- Consumes: `setLauncherInputRegions`, `launcherIconRegion`, and `getLauncherPopupPlacement()` from prior tasks.

- [ ] **Step 1: Rewrite the component mocks around input regions**

In `src/components/LauncherWindow.test.tsx`, remove mocks/imports for `setLauncherCompact`, `setLauncherContextMenuVisible`, `setLauncherExpanded`, and `launcherDimensions`. Add:

```ts
const setLauncherInputRegions = vi.fn().mockResolvedValue(undefined);

vi.mock('../lib/launcherWindow', () => ({
  launcherCanvas: { width: 410, height: 720, popupSlotHeight: 328, iconSize: 64, iconTop: 328, margin: 24 },
  launcherIconRegion: { x: 346, y: 328, width: 64, height: 64 },
  setLauncherInputRegions,
  getLauncherPopupPlacement: vi.fn().mockResolvedValue('above'),
  anchorLauncherToWorkArea: vi.fn().mockResolvedValue(undefined),
  setLauncherWindowVisible: vi.fn().mockResolvedValue(undefined),
  runLauncherMenuAction: vi.fn().mockResolvedValue(undefined),
  getLauncherManagerVisible: state.getManagerVisible,
  showLauncherMenu: vi.fn().mockResolvedValue(undefined),
  startLauncherDragging: vi.fn().mockResolvedValue(undefined),
  openLauncherManager: vi.fn().mockResolvedValue(undefined),
  sendLauncherInput: vi.fn().mockResolvedValue(undefined),
}));
```

- [ ] **Step 2: Add failing state-machine behavior tests**

Add the following tests using `userEvent` and `waitFor`:

```tsx
it('keeps the same hexagon node while switching between both popup types', async () => {
  const user = userEvent.setup();
  render(<LauncherWindow />);
  const trigger = await screen.findByRole('button', { name: '打开下载输入' });
  const mark = trigger.querySelector('.launcher-mark');
  await user.click(trigger);
  expect(screen.getByRole('dialog', { name: '新建下载' })).toBeVisible();
  fireEvent.contextMenu(trigger);
  expect(await screen.findByRole('menu', { name: '悬浮球菜单' })).toBeVisible();
  expect(trigger.querySelector('.launcher-mark')).toBe(mark);
});

it('never resizes, clips, hides, or shows the native window for popup transitions', async () => {
  const user = userEvent.setup();
  render(<LauncherWindow />);
  const trigger = await screen.findByRole('button', { name: '打开下载输入' });
  await user.click(trigger);
  fireEvent.contextMenu(trigger);
  await user.keyboard('{Escape}');
  expect(setLauncherInputRegions).toHaveBeenCalled();
  const callsAfterInitialSync = vi.mocked(setLauncherWindowVisible).mock.calls.slice(1);
  expect(callsAfterInitialSync).toEqual([]);
});

it('uses the last input when left and right clicks alternate rapidly', async () => {
  render(<LauncherWindow />);
  const trigger = await screen.findByRole('button', { name: '打开下载输入' });
  fireEvent.click(trigger);
  fireEvent.contextMenu(trigger);
  fireEvent.click(trigger);
  await waitFor(() => expect(screen.getByRole('dialog', { name: '新建下载' })).toBeVisible());
  expect(screen.queryByRole('menu')).not.toBeInTheDocument();
});

it.each(['Escape', 'blur', 'drag'] as const)('restores icon-only input for %s', async (reason) => {
  render(<LauncherWindow />);
  const trigger = await screen.findByRole('button', { name: '打开下载输入' });
  fireEvent.contextMenu(trigger);
  await screen.findByRole('menu');
  if (reason === 'Escape') fireEvent.keyDown(window, { key: 'Escape' });
  if (reason === 'blur') window.dispatchEvent(new Event('blur'));
  if (reason === 'drag') {
    const target = trigger.closest('.launcher-interaction-target')!;
    fireEvent.mouseDown(target, { button: 0, clientX: 10, clientY: 10 });
    fireEvent.mouseMove(target, { buttons: 1, clientX: 30, clientY: 30 });
  }
  await waitFor(() => expect(setLauncherInputRegions).toHaveBeenLastCalledWith([
    { x: 346, y: 328, width: 64, height: 64 },
  ]));
});
```

- [ ] **Step 3: Run the focused component tests and confirm failure**

Run:

```powershell
pnpm vitest run src/components/LauncherWindow.test.tsx --exclude '.worktrees/**'
```

Expected: FAIL because current transitions resize the native viewport and the state uses independent booleans.

- [ ] **Step 4: Implement one surface mode and last-intent-wins serialization**

In `LauncherWindow.tsx`, replace the four popup booleans with:

```ts
export type LauncherSurfaceMode = 'compact' | 'quick-add' | 'context-menu';

const [mode, setMode] = useState<LauncherSurfaceMode>('compact');
const [popupPlacement, setPopupPlacement] = useState<LauncherPopupPlacement>('above');
const requestedMode = useRef<LauncherSurfaceMode>('compact');
const transitionQueue = useRef(Promise.resolve());
```

Use a measured-region callback from each popup and this transition function:

```ts
const iconRegion = launcherIconRegion;
const quickAddRegion = useRef<LauncherInputRegion | null>(null);
const menuRegion = useRef<LauncherInputRegion | null>(null);

function requestMode(next: LauncherSurfaceMode) {
  requestedMode.current = next;
  transitionQueue.current = transitionQueue.current.then(async () => {
    const target = requestedMode.current;
    const placement = target === 'compact'
      ? popupPlacement
      : await getLauncherPopupPlacement();
    if (target !== requestedMode.current) return;
    setPopupPlacement(placement);
    setMode(target);
  }).catch(() => undefined);
}
```

After React commits a mode, measure the visible popup with `getBoundingClientRect()` and update native input. Use `useLayoutEffect`, not an animation frame:

```ts
useLayoutEffect(() => {
  const popup = mode === 'quick-add' ? quickAddRegion.current : mode === 'context-menu' ? menuRegion.current : null;
  const regions = popup ? [iconRegion, popup] : [iconRegion];
  void setLauncherInputRegions(regions).catch(() => undefined);
}, [mode, popupPlacement]);
```

The actual ref values must be assigned from element bounds in logical canvas coordinates:

```ts
function captureRegion(element: HTMLElement | null, target: React.MutableRefObject<LauncherInputRegion | null>) {
  if (!element) { target.current = null; return; }
  const rect = element.getBoundingClientRect();
  target.current = { x: rect.x, y: rect.y, width: rect.width, height: rect.height };
}
```

If layout-effect ordering shows that the ref callback has not yet measured the newly visible popup, use a child `useLayoutEffect` to report its rectangle before the parent's region update. Do not reintroduce `requestAnimationFrame`, `setTimeout`, native resize calls, or transition flags.

- [ ] **Step 5: Keep the hexagon mounted while controlling the quick-add panel**

Change `FloatingLauncher` props to:

```ts
interface FloatingLauncherProps {
  mode: LauncherSurfaceMode;
  placement: LauncherPopupPlacement;
  onModeChange(mode: LauncherSurfaceMode): void;
  onQuickAddElement(element: HTMLElement | null): void;
  // retain onParse/onScan/onOpenTaskList/onDragStart/onContextMenu
}
```

Render the interaction target and its SVG unconditionally. Render the launcher-placement `NewDownloadDialog` only for `mode === 'quick-add'`, wrapping it in a ref-bearing element if `NewDownloadDialog` cannot forward a ref. Left click requests `quick-add` from compact and `compact` from quick-add. Right click always requests `context-menu`, including when quick-add is open.

Before `onDragStart`, request `compact`; await the icon-only region update in `LauncherWindow` before invoking `startLauncherDragging`. After native dragging returns, call `getLauncherPopupPlacement()` once so the next click uses the new monitor/work-area direction.

- [ ] **Step 6: Make the context menu report its stable container**

Add `onElement(element: HTMLElement | null): void` to `LauncherContextMenuProps`, assign it to the `.launcher-context-menu` ref, and retain the existing Escape/blur behavior. The outer backdrop remains inside the fixed canvas, but it is not added to the native active regions; only the icon and actual menu rectangle receive input.

- [ ] **Step 7: Run focused React tests**

Run:

```powershell
pnpm vitest run src/components/LauncherWindow.test.tsx src/components/NewDownloadDialog.test.tsx --exclude '.worktrees/**'
```

Expected: PASS. The test must prove the SVG element reference remains identical across surface changes.

- [ ] **Step 8: Continue directly to fixed-canvas CSS**

Do not commit or run the desktop app with the old launcher CSS. Continue directly to Task 6 so the native size, React state, and fixed coordinates land together.

---

### Task 6: Lay out the fixed canvas without moving the icon

**Files:**
- Modify: `src/styles.css`
- Modify: `src/components/LauncherWindow.test.tsx`

**Interfaces:**
- Produces: fixed CSS coordinates for the icon and the above/below popup slots.
- Consumes: `launcher-popup-below`, `launcher-expanded`, and `launcher-menu-open` state classes.

- [ ] **Step 1: Add CSS contract assertions**

Extend the existing stylesheet assertions in `LauncherWindow.test.tsx`:

```ts
it('keeps the hexagon at the same fixed-canvas coordinate for both popup placements', () => {
  const styles = readFileSync(resolve('src/styles.css'), 'utf8');
  expect(styles).toContain('.launcher-shell .launcher-interaction-target{position:absolute;right:0;top:328px;');
  expect(styles).not.toContain('.launcher-popup-below .launcher-interaction-target');
  expect(styles).not.toContain('launcher-viewport-transitioning');
});
```

- [ ] **Step 2: Run the focused test and confirm failure**

Run:

```powershell
pnpm vitest run src/components/LauncherWindow.test.tsx --exclude '.worktrees/**'
```

Expected: FAIL because the icon currently uses `bottom: 0` and the old transition class remains.

- [ ] **Step 3: Replace launcher-specific positioning CSS**

Replace the current launcher-shell positioning rules with this logical layout:

```css
.launcher-shell{position:relative;width:410px;height:720px;overflow:visible;background:transparent}
.launcher-shell .launcher-interaction-target{position:absolute;right:0;top:328px;z-index:4;width:64px;height:64px;cursor:grab}
.launcher-shell .launcher-trigger{position:absolute;inset:0}
.launcher-shell .launcher-panel{position:absolute;right:0;bottom:392px;z-index:5;width:410px;max-height:328px}
.launcher-shell.launcher-popup-below .launcher-panel{top:392px;bottom:auto}
.launcher-shell .launcher-menu-backdrop{position:absolute;inset:0;z-index:5;background:transparent;pointer-events:none}
.launcher-shell .launcher-context-menu{position:absolute;right:0;bottom:392px;pointer-events:auto}
.launcher-shell.launcher-popup-below .launcher-context-menu{top:392px;bottom:auto}
```

Adjust the menu's existing width/height margins so its full shadow fits within the 410 × 328 slot. Keep panel/menu opacity transitions only if they do not affect `.launcher-interaction-target` or `.launcher-mark`. Delete `.launcher-viewport-transitioning` entirely.

- [ ] **Step 4: Run component tests and the frontend build**

Run:

```powershell
pnpm vitest run src/components/LauncherWindow.test.tsx src/components/NewDownloadDialog.test.tsx --exclude '.worktrees/**'
pnpm run build
```

Expected: PASS with no TypeScript or CSS-build errors.

- [ ] **Step 5: Commit the complete fixed-canvas migration**

```powershell
git add src-tauri/tauri.conf.json src-tauri/src/launcher.rs src-tauri/src/lib.rs src/lib/launcherWindow.ts src/lib/launcherWindow.test.ts src/components/LauncherWindow.tsx src/components/LauncherWindow.test.tsx src/components/FloatingLauncher.tsx src/components/LauncherContextMenu.tsx src/styles.css
git commit -m "refactor: switch launcher popups on a fixed canvas"
```

---

### Task 7: Remove obsolete redraw and clipping paths

**Files:**
- Modify: `src/lib/launcherWindow.ts`
- Modify: `src/lib/launcherWindow.test.ts`
- Modify: `src-tauri/src/launcher.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/Cargo.toml` if GDI is no longer used elsewhere.

**Interfaces:**
- Produces: a popup path with no `set_launcher_bounds`, `set_launcher_compact_shape`, `SetWindowPos`, or `SetWindowRgn` dependency.
- Consumes: fixed-canvas startup and input-region code from Tasks 3–6.

- [ ] **Step 1: Add source-level regression guards**

Add a test to `src/lib/launcherWindow.test.ts` that reads the component source and checks forbidden popup APIs are absent:

```ts
it('does not expose native resize or visual clipping helpers to popup code', () => {
  const source = readFileSync(resolve('src/lib/launcherWindow.ts'), 'utf8');
  expect(source).not.toContain('set_launcher_bounds');
  expect(source).not.toContain('set_launcher_compact_shape');
  expect(source).not.toContain('resizeLauncher');
});
```

Add a Rust source guard in `launcher.rs` tests:

```rust
#[test]
fn popup_commands_do_not_reintroduce_window_region_redraws() {
    let source = include_str!("launcher.rs");
    assert!(!source.contains("pub fn set_launcher_bounds"));
    assert!(!source.contains("pub fn set_launcher_compact_shape"));
    assert!(!source.contains("SetWindowRgn"));
}
```

- [ ] **Step 2: Delete obsolete frontend resize helpers**

Remove `launcherResizeQueue`, `resizeLauncherNow`, `resizeLauncher`, `setLauncherExpanded`, `setLauncherCompact`, `setLauncherContextMenuVisible`, and `setLauncherCompactShape` from `src/lib/launcherWindow.ts`. Remove their old tests and imports.

- [ ] **Step 3: Delete obsolete native visual-shape code**

Remove `set_launcher_bounds`, `set_launcher_compact_shape`, `apply_launcher_shape`, polygon-region helpers used only for visual clipping, and their registrations. Retain pure hexagon geometry only if it is still used by another tested feature. Remove GDI imports/features only when `rg "Win32::Graphics::Gdi|CreatePolygonRgn|SetWindowRgn" src-tauri/src` confirms they have no remaining user.

- [ ] **Step 4: Run focused frontend/Rust tests**

Run:

```powershell
pnpm vitest run src/lib/launcherWindow.test.ts src/components/LauncherWindow.test.tsx --exclude '.worktrees/**'
cargo test --manifest-path src-tauri/Cargo.toml launcher_ --lib
```

Expected: PASS; the source guards confirm no popup resize/clipping path remains.

- [ ] **Step 5: Commit cleanup**

```powershell
git add src/lib/launcherWindow.ts src/lib/launcherWindow.test.ts src-tauri/src/launcher.rs src-tauri/src/lib.rs src-tauri/Cargo.toml
git commit -m "cleanup: remove launcher resize and clipping transitions"
```

---

### Task 8: Harden drag, monitor, DPI, visibility, and failure behavior

**Files:**
- Modify: `src/components/LauncherWindow.tsx`
- Modify: `src/components/LauncherWindow.test.tsx`
- Modify: `src/lib/launcherWindow.ts`
- Modify: `src/lib/launcherWindow.test.ts`
- Modify: `src-tauri/src/launcher_input.rs`

**Interfaces:**
- Produces: safe region updates after drag/DPI/layout changes, stable last-valid input regions, and visibility changes isolated from popup changes.
- Consumes: state machine and Windows adapter from earlier tasks.

- [ ] **Step 1: Add failure-retention and visibility-isolation tests**

Add component tests covering these exact behaviors:

```tsx
it('keeps the previous native input region when a later update fails', async () => {
  setLauncherInputRegions
    .mockResolvedValueOnce(undefined)
    .mockRejectedValueOnce(new Error('native failure'));
  render(<LauncherWindow />);
  const trigger = await screen.findByRole('button', { name: '打开下载输入' });
  fireEvent.contextMenu(trigger);
  await waitFor(() => expect(setLauncherInputRegions).toHaveBeenCalledTimes(2));
  expect(setLauncherInputRegions).not.toHaveBeenCalledWith([
    { x: 0, y: 0, width: 410, height: 720 },
  ]);
});

it('changes native visibility only for the visibility policy, not popup state', async () => {
  render(<LauncherWindow />);
  await screen.findByRole('button', { name: '打开下载输入' });
  vi.mocked(setLauncherWindowVisible).mockClear();
  fireEvent.click(screen.getByRole('button', { name: '打开下载输入' }));
  fireEvent.contextMenu(screen.getByRole('button', { name: '打开下载输入' }));
  expect(setLauncherWindowVisible).not.toHaveBeenCalled();
});
```

- [ ] **Step 2: Serialize region updates and remember the last valid set**

In `LauncherWindow.tsx`, keep a queue separate from mode intent:

```ts
const regionQueue = useRef(Promise.resolve());
const lastValidRegions = useRef<LauncherInputRegion[]>([launcherIconRegion]);

function applyRegions(regions: LauncherInputRegion[]) {
  regionQueue.current = regionQueue.current.then(async () => {
    await setLauncherInputRegions(regions);
    lastValidRegions.current = regions;
  }).catch(() => undefined);
  return regionQueue.current;
}
```

Never enqueue a full-canvas fallback. On application teardown, set a `disposed` ref and ignore new mode requests.

- [ ] **Step 3: Recompute placement only at allowed lifecycle points**

Recompute placement on:

- launcher initialization after anchoring;
- native drag completion;
- Tauri scale-factor change;
- Tauri moved/monitor change.

Do not reposition the window after user dragging. `anchorLauncherToWorkArea` is startup-only. Event listeners update only `popupPlacement` and the DPI-derived native region set.

Use the Tauri window-event listener returned by `getCurrentWindow().onMoved`/`onScaleChanged` (matching the installed Tauri API signatures) and dispose each unlisten function in the effect cleanup.

- [ ] **Step 4: Make dragging compact before the native drag starts**

Implement the drag sequence as:

```ts
async function beginLauncherDrag() {
  requestedMode.current = 'compact';
  setMode('compact');
  await applyRegions([launcherIconRegion]);
  await startLauncherDragging();
  setPopupPlacement(await getLauncherPopupPlacement().catch(() => popupPlacement));
}
```

Do not invoke native drag if the compact input-region update failed; this prevents the invisible popup rectangle from remaining interactive during drag.

- [ ] **Step 5: Run focused regression tests**

Run:

```powershell
pnpm vitest run src/components/LauncherWindow.test.tsx src/lib/launcherWindow.test.ts --exclude '.worktrees/**'
cargo test --manifest-path src-tauri/Cargo.toml launcher_input::tests --lib
```

Expected: PASS.

- [ ] **Step 6: Commit lifecycle hardening**

```powershell
git add src/components/LauncherWindow.tsx src/components/LauncherWindow.test.tsx src/lib/launcherWindow.ts src/lib/launcherWindow.test.ts src-tauri/src/launcher_input.rs
git commit -m "fix: harden launcher input-region lifecycle"
```

---

### Task 9: Extend smoke instrumentation and run the acceptance loop

**Files:**
- Modify: `src-tauri/src/launcher.rs`
- Modify: `scripts/smoke-launcher.cjs`

**Interfaces:**
- Produces: an automated 50-cycle check that native bounds never change and both input paths remain responsive.
- Consumes: `launcher_interaction_snapshot` and existing smoke-control commands.

- [ ] **Step 1: Extend the launcher interaction snapshot**

Add immutable diagnostics to `LauncherInteractionSnapshot`:

```rust
pub struct LauncherInteractionSnapshot {
    pub drag_requests: usize,
    pub menu_requests: usize,
    pub bounds_update_requests: usize,
    pub input_region_update_requests: usize,
}

pub struct LauncherInteractionState {
    background_test: bool,
    drag_requests: AtomicUsize,
    menu_requests: AtomicUsize,
    bounds_update_requests: AtomicUsize,
    input_region_update_requests: AtomicUsize,
}
```

Initialize both new atomics to zero in `LauncherInteractionState::new`, expose this method, and include both values in `snapshot()`:

```rust
pub fn record_input_region_update(&self) {
    self.input_region_update_requests.fetch_add(1, Ordering::Relaxed);
}
```

Accept `State<'_, LauncherInteractionState>` in `set_launcher_input_regions` and call `record_input_region_update()` only after a valid region set is applied. Keep `bounds_update_requests` at zero now that the obsolete bounds command is deleted; it is a regression sentinel, not a compatibility counter.

- [ ] **Step 2: Add the alternating click loop to the smoke script**

First replace the old variable-size smoke assumptions:

- compact viewport size is always 410 × 720;
- the icon rect is `left: 346, top: 328, right: 410, bottom: 392`;
- only the icon, not the complete native canvas, must stay inside the current work area;
- menu/dialog close checks assert the popup is absent, not `innerWidth < 200`/`innerHeight < 100`;
- opening either popup asserts the native viewport is still 410 × 720;
- remove every `set_launcher_compact_shape` call and native visual-region assertion.

Capture the launcher's physical position and size before the loop. Alternate left and right clicks 50 times, closing each popup through Escape or its close action. Capture position and size again and assert:

```js
assert.deepEqual(after.position, before.position);
assert.deepEqual(after.size, before.size);
assert.equal(snapshot.boundsUpdateRequests, 0);
assert.ok(snapshot.menuRequests >= 25);
assert.ok(snapshot.inputRegionUpdateRequests >= 50);
```

Add this generic Chrome DevTools command helper beside `evaluate`:

```js
async function devtoolsCommand(target, method, params = {}) {
  return await new Promise((resolve, reject) => {
    const socket = new WebSocket(target.webSocketDebuggerUrl);
    socket.on('open', () => socket.send(JSON.stringify({ id: 1, method, params })));
    socket.on('error', reject);
    socket.on('message', (data) => {
      const message = JSON.parse(data.toString());
      if (message.id !== 1) return;
      socket.close();
      if (message.error) reject(new Error(JSON.stringify(message.error)));
      else resolve(message.result);
    });
  });
}

async function captureLauncherIcon(target) {
  const { data } = await devtoolsCommand(target, 'Page.captureScreenshot', {
    format: 'png',
    fromSurface: true,
    clip: { x: 346, y: 328, width: 64, height: 64, scale: 1 },
  });
  return Buffer.from(data, 'base64');
}
```

Capture the icon immediately before and after the 50-cycle loop, after waiting for all popups to close. Compare the PNG buffers with `Buffer.equals`; because the crop excludes speed text and popup slots, the 64 × 64 hexagon pixels must be identical.

- [ ] **Step 3: Run focused smoke and build verification**

Run in this order:

```powershell
pnpm vitest run src/components/LauncherWindow.test.tsx src/components/NewDownloadDialog.test.tsx src/lib/launcherWindow.test.ts --exclude '.worktrees/**'
cargo test --manifest-path src-tauri/Cargo.toml launcher_ --lib
pnpm run build
cargo build --manifest-path src-tauri/Cargo.toml --release --features custom-protocol
pnpm run test:launcher
```

Expected:

- Focused Vitest files PASS.
- Focused launcher Rust tests PASS.
- Frontend build PASS.
- Release Rust build PASS.
- Smoke test performs 50 alternating interactions with invariant position, size, and icon pixels.
- The earlier visible Windows hard-check confirms clicks in each transparent canvas quadrant reach the background window rather than the launcher.

Do not run full Vitest or full Rust test suites unless one of these focused commands reveals a shared-infrastructure regression.

- [ ] **Step 4: Perform the required manual Windows edge checks**

With the release app running, verify and record:

1. Drag the hexagon near the top edge; left and right popups open below it while the hexagon does not move.
2. Drag near the bottom edge; both popups open above it.
3. Repeat on monitors with different DPI scaling; transparent pixels remain click-through.
4. Type in the quick-add input, close with Escape, reopen, and confirm focus/input work.
5. Rapidly alternate left/right click; only the last requested popup is visible.
6. Change theme and download-speed updates; the icon does not disappear for a frame.
7. Hide/show according to launcher visibility settings; only this policy is allowed to call native show/hide.

- [ ] **Step 5: Commit final instrumentation**

```powershell
git add src-tauri/src/launcher.rs scripts/smoke-launcher.cjs
git commit -m "test: verify launcher stays stable across popup cycles"
```

---

## Final Review Checklist

- [ ] Compare every implementation commit with the design spec and confirm all stated frontend, Rust, and Windows acceptance requirements have a corresponding passing test or recorded manual check.
- [ ] Run `rg -n "set_launcher_bounds|set_launcher_compact_shape|launcher-viewport-transitioning|resizeLauncher" src src-tauri/src` and confirm there are no popup-time implementations or imports left.
- [ ] Run `git status --short` and confirm `src/App.tsx` and `src/App.live.test.tsx` remain unstaged and unchanged by this implementation.
- [ ] Review the final diff for accidental menu visual redesigns, business-command changes, or a second native/WebView window.
- [ ] Do not merge to `master` or generate the packaged Windows binary unless the user explicitly asks after implementation review; the release `cargo build` above is the required code-level build verification.
