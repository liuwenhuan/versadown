# Main Window Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Windows system title bar with an VersaDown title bar and make every close request hide, rather than destroy, the download manager.

**Architecture:** React owns title-bar presentation and invokes a small window-controller module for minimize, maximize/restore, and hide. Rust intercepts native close requests such as Alt+F4 so the `main` WebviewWindow remains registered; the launcher reuses its existing reveal path.

**Tech Stack:** Tauri 2.11, React 18, TypeScript, Vitest/Testing Library, Rust integration tests, hidden WebView2/CDP smoke tests.

**Spec:** `docs/superpowers/specs/2026-09-08-native-titlebar-aria2-rpc-design.md`

## Global Constraints

- Windows-first; automated UI tests must not activate or manipulate the user's foreground applications.
- Closing the manager hides it; only launcher action `launcher.quit` exits VersaDown.
- Preserve the launcher menu, circular region, position, and background-test mode.
- The main window must remain reopenable after both the custom close button and a native close request.
- Do not change download-engine behavior in this plan.

---

### Task 1: Define manager-window lifecycle policy

**Files:**
- Create: `src-tauri/src/window_lifecycle.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: `src-tauri/tests/window_lifecycle.rs`

**Interfaces:**
- Produces: `pub enum ManagerCloseDisposition { Hide, Exit }`.
- Produces: `pub fn manager_close_disposition(window_label: &str, application_exiting: bool) -> ManagerCloseDisposition`.
- Produces: `pub struct ApplicationLifecycle` with `begin_exit()` and `is_exiting()`.
- Produces: `pub fn install_manager_close_handler(window: &tauri::WebviewWindow, lifecycle: Arc<ApplicationLifecycle>)`.

- [ ] **Step 1: Write the failing lifecycle policy test**

```rust
use versadown_lib::{manager_close_disposition, ManagerCloseDisposition};

#[test]
fn main_close_hides_until_application_exit_begins() {
    assert_eq!(manager_close_disposition("main", false), ManagerCloseDisposition::Hide);
    assert_eq!(manager_close_disposition("main", true), ManagerCloseDisposition::Exit);
    assert_eq!(manager_close_disposition("launcher", false), ManagerCloseDisposition::Exit);
}
```

- [ ] **Step 2: Run the test and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test window_lifecycle`

Expected: compilation fails because the lifecycle types do not exist.

- [ ] **Step 3: Implement the policy and native close interception**

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagerCloseDisposition { Hide, Exit }

pub fn manager_close_disposition(label: &str, exiting: bool) -> ManagerCloseDisposition {
    if label == "main" && !exiting { ManagerCloseDisposition::Hide } else { ManagerCloseDisposition::Exit }
}
```

`install_manager_close_handler` must match `tauri::WindowEvent::CloseRequested`, call `api.prevent_close()`, and hide a cloned main window. The exit flag prevents interception during intentional application shutdown.

- [ ] **Step 4: Run the focused lifecycle and launcher tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test window_lifecycle --test launcher_native`

Expected: all tests pass.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/window_lifecycle.rs src-tauri/src/lib.rs src-tauri/tests/window_lifecycle.rs
git commit -m "fix: keep manager window alive when closed"
```

### Task 2: Add a tested main-window controller

**Files:**
- Create: `src/lib/mainWindow.ts`
- Create: `src/lib/mainWindow.test.ts`
- Modify: `src-tauri/capabilities/default.json`

**Interfaces:**
- Produces: `export type MainWindowAction = 'minimize' | 'toggle-maximize' | 'hide'`.
- Produces: `export async function runMainWindowAction(action: MainWindowAction, operations?: WindowOperations): Promise<void>`.
- Produces: `export async function mainWindowIsMaximized(): Promise<boolean>`.

- [ ] **Step 1: Write failing action-routing tests**

```ts
it('maps close to hide instead of close', async () => {
  const calls: string[] = [];
  await runMainWindowAction('hide', {
    minimize: async () => { calls.push('minimize'); },
    toggleMaximize: async () => { calls.push('toggle'); },
    hide: async () => { calls.push('hide'); },
    isMaximized: async () => false
  });
  expect(calls).toEqual(['hide']);
});
```

Add distinct assertions for minimize and toggle-maximize. `WindowOperations` is injected so tests exercise routing without mocking Tauri modules.

- [ ] **Step 2: Run and verify RED**

Run: `npm run test:run -- src/lib/mainWindow.test.ts`

Expected: import resolution fails because `mainWindow.ts` does not exist.

- [ ] **Step 3: Implement the controller**

```ts
export async function runMainWindowAction(action: MainWindowAction, operations = tauriWindowOperations) {
  if (action === 'minimize') return operations.minimize();
  if (action === 'toggle-maximize') return operations.toggleMaximize();
  return operations.hide();
}
```

Use `getCurrentWindow()` for production operations. Grant only `allow-minimize`, `allow-toggle-maximize`, `allow-hide`, `allow-is-maximized`, and existing required permissions.

- [ ] **Step 4: Run test and TypeScript build**

Run: `npm run test:run -- src/lib/mainWindow.test.ts; npm run build`

Expected: test and build pass.

- [ ] **Step 5: Commit**

```powershell
git add src/lib/mainWindow.ts src/lib/mainWindow.test.ts src-tauri/capabilities/default.json
git commit -m "feat: add main window controls"
```

### Task 3: Build the self-drawn title bar

**Files:**
- Create: `src/components/WindowTitleBar.tsx`
- Create: `src/components/WindowTitleBar.test.tsx`
- Modify: `src/App.tsx`
- Modify: `src/styles.css`
- Modify: `src-tauri/tauri.conf.json`

**Interfaces:**
- Consumes: `runMainWindowAction` and `mainWindowIsMaximized` from Task 2.
- Produces: `WindowTitleBar({ title, maximized, onAction })`.

- [ ] **Step 1: Write failing component tests**

```tsx
it('renders a drag region and hides through the close button', async () => {
  render(<WindowTitleBar title="下载管理" maximized={false} onAction={onAction} />);
  expect(screen.getByTestId('titlebar-drag-region')).toHaveAttribute('data-tauri-drag-region');
  await user.click(screen.getByRole('button', { name: '关闭下载管理器' }));
  expect(onAction).toHaveBeenCalledWith('hide');
});
```

Add separate assertions for minimize, maximize/restore accessible names, and that buttons are outside the drag region.

- [ ] **Step 2: Run and verify RED**

Run: `npm run test:run -- src/components/WindowTitleBar.test.tsx`

Expected: component import fails.

- [ ] **Step 3: Implement title-bar component and styling**

Set `data-tauri-drag-region` only on the noninteractive title area. Buttons stop pointer propagation. Subscribe to Tauri resize events in `App` and refresh `isMaximized`. Set main window `decorations: false` and `shadow: true`. Reserve a 46px title-bar row so content never overlaps it.

- [ ] **Step 4: Run component, app, and build tests**

Run: `npm run test:run -- src/components/WindowTitleBar.test.tsx src/App.test.tsx; npm run build`

Expected: tests and production build pass.

- [ ] **Step 5: Commit**

```powershell
git add src/components/WindowTitleBar.tsx src/components/WindowTitleBar.test.tsx src/App.tsx src/styles.css src-tauri/tauri.conf.json
git commit -m "feat: add self-drawn manager title bar"
```

### Task 4: Verify hide and launcher reopen end to end

**Files:**
- Modify: `src-tauri/src/launcher.rs`
- Modify: `scripts/smoke-launcher.cjs`
- Test: `src-tauri/tests/window_lifecycle.rs`

**Interfaces:**
- Consumes: persistent main window from Task 1 and production `handle_launcher_menu_action`.
- Produces background-only commands `hide_manager_for_test`, `activate_launcher_menu_for_test`, and `manager_window_snapshot`.

- [ ] **Step 1: Extend hidden smoke test before observability exists**

```js
await evaluate(manager, `window.__TAURI_INTERNALS__.invoke('hide_manager_for_test')`);
const hidden = await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('manager_window_snapshot')`);
assert(hidden.exists && !hidden.visible, 'main was destroyed or remained visible');
await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('activate_launcher_menu_for_test', { action: 'launcher.open_manager' })`);
const shown = await evaluate(launcher, `window.__TAURI_INTERNALS__.invoke('manager_window_snapshot')`);
assert(shown.exists && shown.visible, 'launcher could not reopen manager');
```

- [ ] **Step 2: Run smoke and verify RED**

Run: `npm run test:launcher`

Expected: command-not-found failure for background-only commands.

- [ ] **Step 3: Implement gated observability using production routing**

Reject these commands unless `VERSADOWN_BACKGROUND_UI_TEST=1`. `activate_launcher_menu_for_test` calls the same handler as native menu events. The snapshot returns `{ exists, visible, minimized }` without focusing any window.

- [ ] **Step 4: Rebuild and run hidden integration**

Run: `powershell -ExecutionPolicy Bypass -File scripts/build-windows.ps1; npm run test:launcher`

Expected: title-bar DOM, hide, reopen, settings navigation, launcher behavior, and zero foreground activation pass.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/launcher.rs src-tauri/tests/window_lifecycle.rs scripts/smoke-launcher.cjs
git commit -m "test: cover manager hide and reopen lifecycle"
```

### Task 5: Verify and package the window release

**Files:**
- Modify only files from Tasks 1–4 if verification exposes a defect.

**Interfaces:**
- Produces: `src-tauri/target/release/versadown.exe` plus MSI and NSIS bundles.

- [ ] **Step 1: Run full frontend and Rust suites**

Run: `npm run test:run; cargo test --manifest-path src-tauri/Cargo.toml`

Expected: zero failures.

- [ ] **Step 2: Build and run hidden smoke tests**

Run: `powershell -ExecutionPolicy Bypass -File scripts/build-windows.ps1; npm run test:launcher; powershell -ExecutionPolicy Bypass -File scripts/smoke-download.ps1`

Expected: EXE/MSI/NSIS exist, hidden smoke passes, and no VersaDown process remains.

- [ ] **Step 3: Inspect the final repository state**

Run: `git diff --check; git status --short; git log -4 --oneline`

Expected: no whitespace errors, no uncommitted implementation files, and the four task commits are visible. Do not create an empty verification commit.
