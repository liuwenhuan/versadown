# 任务详情、悬浮弹窗与隐私转换 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让任务详情只滚动文件树、所有模态遵循主题、悬浮图标无闪烁弹出独立窗口，并允许已有任务即时切换隐私状态。

**Architecture:** 详情文件树从任务 children 或扁平 files 生成只读节点并保持摘要区固定。悬浮图标和 popup 使用两个 Tauri WebviewWindow，避免改变图标 HWND。隐私转换采用新的 Tauri 命令和事务性 store 更新，前端依据返回投影立即更新可见性。

**Tech Stack:** React 18、TypeScript、Vitest、Tauri 2、Rust、rusqlite。

**Spec:** `docs/superpowers/specs/2026-09-17-task-details-launcher-privacy-design.md`

## Global Constraints

- 详情窗口使用 `min(520px, calc(100vh - 36px))`，只有文件树可以纵向滚动。
- 所有新增视觉颜色使用主题变量或新增 `--modal-scrim` / `--modal-shadow`；不写固定主题色。
- launcher 图标窗口在任何 popup 操作前后均保持 64x64。
- 隐私转换必须先持久化，再返回更新投影；锁定时不发出私密任务事件。
- 前端 Vitest 命令均必须包含 `--exclude '.worktrees/**'`；不运行无关全量测试。
- 本轮不调用 `scripts/build-windows.ps1`，除非用户另行要求生成二进制。

---

### Task 1: 详情文件树、搜索禁用与弹窗主题

**Files:**
- Create: `src/lib/taskFileTree.ts`
- Create: `src/lib/taskFileTree.test.ts`
- Modify: `src/components/TaskDetails.tsx`
- Modify: `src/components/TaskDetails.test.tsx`
- Modify: `src/components/TaskDetailsDialog.test.tsx`
- Modify: `src/App.tsx`
- Modify: `src/App.test.tsx`
- Modify: `src/styles.css`

**Interfaces:**
- Produces `buildTaskFileTree(task: Pick<QueueTask, 'name' | 'children' | 'files'>): readonly TaskFileTreeNode[]`.
- `TaskFileTreeNode` is a folder with `children`, aggregate bytes and file count, or a file with its `DownloadFile`.
- `TaskDetails` owns `expandedDirectoryIds: Set<string>` and renders only root direct children initially.

- [ ] **Step 1: Write the failing test**

```ts
it('keeps nested task files collapsed until their directory is opened', async () => {
  render(<TaskDetails task={nestedTask} />);
  expect(screen.getByText('_crack')).toBeVisible();
  expect(screen.queryByText('Win64')).not.toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: '展开 _crack' }));
  expect(screen.getByText('Win64')).toBeVisible();
});
```

Add a page test that opens task details through the context menu, expects the global search textarea to be disabled, closes the dialog, and expects the textarea enabled.

- [ ] **Step 2: Run the test to verify it fails**

Run: ` .\node_modules\.bin\vitest.cmd run --dir src --exclude '.worktrees/**' lib/taskFileTree.test.ts components/TaskDetails.test.tsx components/TaskDetailsDialog.test.tsx App.test.tsx --reporter=dot`

Expected: FAIL because the tree builder, expandable folder controls and details-driven search disable state do not exist.

- [ ] **Step 3: Write the minimal implementation**

```ts
export function buildTaskFileTree(task: Pick<QueueTask, 'name' | 'children' | 'files'>): readonly TaskFileTreeNode[] {
  if (task.children?.length) return task.children.map(toTaskFileTreeNode);
  return buildTreeFromFiles(task.files ?? [], task.name);
}

const [expandedDirectoryIds, setExpandedDirectoryIds] = useState<Set<string>>(() => new Set());
```

Render folder buttons with `aria-label="展开 ${node.name}"` / `收起 ${node.name}` using the queue tree arrow/icon classes. Give the file-list child `overflow-y:auto`; make outer dialog content and `TaskDetails` non-scrolling grids. Change App to `disabled={newDownloadOpen || Boolean(detailsTask)}`. Add `--modal-scrim` / `--modal-shadow`, apply them to modal selectors, and replace the confirmation icon’s fixed blue gradient with accent variables.

- [ ] **Step 4: Run the test to verify it passes**

Run: ` .\node_modules\.bin\vitest.cmd run --dir src --exclude '.worktrees/**' lib/taskFileTree.test.ts components/TaskDetails.test.tsx components/TaskDetailsDialog.test.tsx App.test.tsx --reporter=dot`

Expected: PASS with root-only initial display, independent file scrolling, themed modal tokens and search-disable behavior.

- [ ] **Step 5: Record the verified scope without committing**

```bash
git add src/lib/taskFileTree.ts src/lib/taskFileTree.test.ts src/components/TaskDetails.tsx src/components/TaskDetails.test.tsx src/components/TaskDetailsDialog.test.tsx src/App.tsx src/App.test.tsx src/styles.css
git diff --cached --check
```

Do not create a commit unless the user explicitly requests it.

### Task 2: 独立 launcher popup 窗口

**Files:**
- Create: `src/components/LauncherPopupWindow.tsx`
- Create: `src/components/LauncherPopupWindow.test.tsx`
- Modify: `src/components/LauncherWindow.tsx`
- Modify: `src/components/LauncherWindow.test.tsx`
- Modify: `src/lib/launcherWindow.ts`
- Modify: `src/lib/launcherWindow.test.ts`
- Modify: `src/main.tsx`
- Modify: `src/styles.css`
- Modify: `src-tauri/tauri.conf.json`
- Modify: `src-tauri/src/launcher.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces `showLauncherPopup(mode: 'input' | 'menu'): Promise<void>` and `hideLauncherPopup(): Promise<void>`.
- Produces native commands `show_launcher_popup(mode)` and `hide_launcher_popup()`.
- `LauncherPopupWindow` consumes `launcher-popup-state` with popup mode and placement.

- [ ] **Step 1: Write the failing test**

```ts
it('opens the popup without changing the launcher window bounds', async () => {
  await showLauncherPopup('menu');
  expect(invoke).toHaveBeenCalledWith('show_launcher_popup', { mode: 'menu' });
  expect(invoke).not.toHaveBeenCalledWith('set_launcher_bounds', expect.anything());
});
```

```rust
#[test]
fn popup_below_is_selected_when_above_space_is_insufficient() {
    assert_eq!(launcher_popup_placement(20, 64, 392, 0), LauncherPopupPlacement::Below);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: ` .\node_modules\.bin\vitest.cmd run --dir src --exclude '.worktrees/**' components/LauncherWindow.test.tsx components/LauncherPopupWindow.test.tsx lib/launcherWindow.test.ts --reporter=dot`

Run: `cargo test launcher_popup --manifest-path src-tauri/Cargo.toml`

Expected: FAIL because popup commands, popup root and pure bounds logic do not exist.

- [ ] **Step 3: Write the minimal implementation**

```rust
#[tauri::command]
pub fn show_launcher_popup(app: AppHandle, mode: LauncherPopupMode) -> Result<(), String> {
    let launcher = app.get_webview_window("launcher").ok_or("悬浮图标不存在")?;
    let popup = app.get_webview_window("launcher-popup").ok_or("悬浮弹窗不存在")?;
    let bounds = launcher_popup_bounds(&launcher, &popup)?;
    popup.set_position(bounds.position)?;
    popup.show()?;
    app.emit_to("launcher-popup", "launcher-popup-state", bounds.event(mode))?;
    Ok(())
}
```

Add a hidden transparent always-on-top skip-taskbar `launcher-popup` window in `tauri.conf.json`. Bounds right-align and clamp to the monitor work area, selecting below placement when above would cross the top edge. `LauncherWindow` renders only the fixed icon and calls `showLauncherPopup`. `LauncherPopupWindow` renders the existing launcher input dialog or context menu for the received mode and hides only the popup on close. `main.tsx` routes the new window label to this root.

- [ ] **Step 4: Run the test to verify it passes**

Run: ` .\node_modules\.bin\vitest.cmd run --dir src --exclude '.worktrees/**' components/LauncherWindow.test.tsx components/LauncherPopupWindow.test.tsx lib/launcherWindow.test.ts --reporter=dot`

Run: `cargo test launcher_popup --manifest-path src-tauri/Cargo.toml`

Expected: PASS. Tests demonstrate no launcher resize operation and correct above/below bounds selection.

- [ ] **Step 5: Record the verified scope without committing**

```bash
git add src/components/LauncherPopupWindow.tsx src/components/LauncherPopupWindow.test.tsx src/components/LauncherWindow.tsx src/components/LauncherWindow.test.tsx src/lib/launcherWindow.ts src/lib/launcherWindow.test.ts src/main.tsx src/styles.css src-tauri/tauri.conf.json src-tauri/src/launcher.rs src-tauri/src/lib.rs
git diff --cached --check
```

Do not create a commit unless the user explicitly requests it.

### Task 3: 后端隐私状态转换

**Files:**
- Modify: `src-tauri/src/store.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src/lib/api.ts`
- Modify: `src/lib/api.test.ts`

**Interfaces:**
- Produces `Store::set_task_private(task_id: Uuid, is_private: bool) -> Result<DownloadTask, StoreError>`.
- Produces transactional `Store::set_cloud_directory_group_private(group_id: Uuid, is_private: bool)`.
- Produces `set_task_private(task_id, is_private)` Tauri command and `setTaskPrivacy(taskId, isPrivate)` API.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn changing_a_cloud_directory_group_privacy_updates_each_linked_task() {
    let group = seed_group_with_two_tasks(&store, false);
    store.set_cloud_directory_group_private(group.id, true).unwrap();
    assert!(store.find(task_a.id).unwrap().is_private);
    assert!(store.find(task_b.id).unwrap().is_private);
}
```

```ts
it('invokes the dedicated privacy command instead of an aria2 task action', async () => {
  await setTaskPrivacy('task-1', true);
  expect(invoke).toHaveBeenCalledWith('set_task_private', { taskId: 'task-1', isPrivate: true });
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test private --manifest-path src-tauri/Cargo.toml`

Run: ` .\node_modules\.bin\vitest.cmd run --dir src --exclude '.worktrees/**' lib/api.test.ts --reporter=dot`

Expected: FAIL because neither command/API nor group transaction exists.

- [ ] **Step 3: Write the minimal implementation**

```rust
let transaction = connection.unchecked_transaction()?;
transaction.execute("UPDATE cloud_directory_groups SET is_private = ?1, updated_at = ?2 WHERE id = ?3", params![is_private, current_timestamp(), group])?;
transaction.execute("UPDATE tasks SET is_private = ?1, updated_at = ?2 WHERE id IN (SELECT task_id FROM cloud_directory_group_items WHERE group_id = ?3 AND task_id IS NOT NULL)", params![is_private, current_timestamp(), group])?;
transaction.commit()?;
```

Ordinary tasks use the existing private setter. Ordinary-to-private requires privacy downloads enabled; private-to-ordinary requires private-task access. Return the updated task payload and emit it only through the existing private-event visibility guard. Register the new command in `invoke_handler`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test private --manifest-path src-tauri/Cargo.toml`

Run: ` .\node_modules\.bin\vitest.cmd run --dir src --exclude '.worktrees/**' lib/api.test.ts --reporter=dot`

Expected: PASS with group propagation, access rejection and exact IPC payload covered.

- [ ] **Step 5: Record the verified scope without committing**

```bash
git add src-tauri/src/store.rs src-tauri/src/commands.rs src-tauri/src/lib.rs src/lib/api.ts src/lib/api.test.ts
git diff --cached --check
```

Do not create a commit unless the user explicitly requests it.

### Task 4: 隐私右键菜单与即时列表更新

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/components/DownloadQueue.tsx`
- Modify: `src/components/DownloadQueue.test.tsx`
- Modify: `src/App.tsx`
- Modify: `src/App.live.test.tsx`

**Interfaces:**
- Adds `move_to_private | move_to_normal` to `TaskCommand` and `BatchTaskCommand`.
- Adds `privacyEnabled: boolean` to `DownloadQueueProps`.
- `App.taskCommand` and `App.batchTaskCommand` call `setTaskPrivacy` for matching task states.

- [ ] **Step 1: Write the failing test**

```ts
it.each(['downloading', 'completed', 'failed', 'recycled'] as const)(
  'shows move-to-private for a normal %s task only when privacy downloads are enabled',
  async (status) => { /* render a task, open its menu, assert the menuitem */ }
);
```

Add an App live test that makes `setTaskPrivacy` return `{ ...normalTask, is_private: true }`, invokes “移入隐私下载” while locked, and verifies the row, selection and task-details dialog disappear immediately.

- [ ] **Step 2: Run the test to verify it fails**

Run: ` .\node_modules\.bin\vitest.cmd run --dir src --exclude '.worktrees/**' components/DownloadQueue.test.tsx App.live.test.tsx --reporter=dot`

Expected: FAIL because the conversion commands and queue update dispatch do not exist.

- [ ] **Step 3: Write the minimal implementation**

```ts
if (privacyEnabled && !isPrivateTask(task)) commands.push({ command: 'move_to_private', label: '移入隐私下载', separated: true });
if (privacyEnabled && isPrivateTask(task)) commands.push({ command: 'move_to_normal', label: '移入正常下载', separated: true });

const changed = await setTaskPrivacy(id, command === 'move_to_private');
recordTaskEvent(changed);
setTasks(current => upsertTask(current, changed));
```

For multi-select, show each conversion if at least one selected task needs it, and only operate on matching-state tasks. Pass `privacyEnabled={settings.privacyDownloadEnabled === true}` to `DownloadQueue`. After locked ordinary-to-private conversion, clear selected/detail ids for that task; existing visibility memo removes it from every tab.

- [ ] **Step 4: Run the test to verify it passes**

Run: ` .\node_modules\.bin\vitest.cmd run --dir src --exclude '.worktrees/**' components/DownloadQueue.test.tsx App.live.test.tsx --reporter=dot`

Expected: PASS for every task tab state, both privacy states, mixed selection, and instant locked visibility.

- [ ] **Step 5: Run cross-boundary verification and commit**

Run: ` .\node_modules\.bin\vitest.cmd run --dir src --exclude '.worktrees/**' App.test.tsx App.live.test.tsx components/DownloadQueue.test.tsx components/TaskDetails.test.tsx components/TaskDetailsDialog.test.tsx components/LauncherWindow.test.tsx components/LauncherPopupWindow.test.tsx lib/taskFileTree.test.ts lib/launcherWindow.test.ts lib/api.test.ts --reporter=dot`

Run: `cargo test private launcher_popup --manifest-path src-tauri/Cargo.toml`

Run: ` .\node_modules\.bin\tsc.cmd -b; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }; .\node_modules\.bin\vite.cmd build`

Expected: PASS. Do not run full Vitest or full Rust suites unless a targeted failure demonstrates shared-infrastructure risk.

```bash
git add src/lib/types.ts src/components/DownloadQueue.tsx src/components/DownloadQueue.test.tsx src/App.tsx src/App.live.test.tsx
git diff --cached --check
```

Do not create a commit unless the user explicitly requests it.
