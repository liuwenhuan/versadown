# P2P 完成状态与分享限速 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让已下载完的 P2P 任务立即完成，同时在默认分享率 `1.5` 达成前继续受控分享，并提供下载/P2P 分享上传的网络设置与自动测速限速。

**Architecture:** `completed` 仍是唯一的用户完成状态；持久化的 `p2p_sharing` 补充标记表示 aria2 仍在分享。后端 P2P 控制器掌管 aria2 运行时限速、测速闸门和网盘上传让行，前端只负责使用 Cloudflare 官方测速组件测出上行并报告结果。网盘上传注册表仅提供活跃信号，不能反向接受 P2P 的暂停控制。

**Tech Stack:** Rust/Tauri、rusqlite、aria2 JSON-RPC、React 18、TypeScript、Vitest、`@cloudflare/speedtest`。

**Spec:** `docs/superpowers/specs/2026-09-14-p2p-completion-network-limits-design.md`

## Global Constraints

- BT 分享率默认值固定为 `1.5`。
- P2P 数据完整后立即进入完成标签页；不得新增顶层“分享中”状态。
- 仅在实际分享时显示 `P2P 分享中`；自动或手动暂停时不显示替代文案。
- 下载速度默认不限速，仅控制 aria2 下载；网盘传输不受其控制。
- `P2P 分享上传速度限制` 默认自动，仅控制 BT、磁链、ED2K 分享上传；不控制网盘上传或传统 FTP。
- 自动模式启动及每小时测速，60 秒硬超时，成功上限为上传测速结果的 50%；失败使用上一次结果，首次失败使用 `256 KiB/s`。
- 网盘上传活跃时暂停 P2P 分享；只恢复自动策略暂停的任务。
- Vitest 必须排除 `.worktrees/**`；不默认跑全量测试。

---

## File structure

- `src-tauri/src/models.rs` — 设置模式、测速结果和 `p2p_sharing` 模型。
- `src-tauri/src/store.rs` — SQLite 迁移、读写与 aria2 同步候选查询。
- `src-tauri/src/aria2/types.rs` / `rpc.rs` — aria2 全局选项 RPC 契约。
- `src-tauri/src/aria2/p2p_share.rs` (new) — 分享闸门、限速决策、自动暂停来源。
- `src-tauri/src/aria2/service.rs` / `sync.rs` — P2P 字节完成映射及持续同步。
- `src-tauri/src/engine_runtime.rs` / `commands.rs` / `lib.rs` — 控制器生命周期和测速报告命令。
- `src-tauri/src/cloud/upload/control.rs` — 活跃网盘上传计数。
- `src/lib/types.ts` / `src/lib/api.ts` / `src/lib/p2pSpeedProbe.ts` (new) — UI 契约、Tauri 调用和 60 秒测速封装。
- `src/App.tsx` / `src/components/SettingsPanel.tsx` / `src/components/DownloadQueue.tsx` — 测速调度、设置和完成页显示。

## Task 1: Durable models and storage

**Files:**
- Modify: `src-tauri/src/models.rs:31-83,132-166,273-318,380-405`
- Modify: `src-tauri/src/store.rs:300-330,758-764,1151-1181,1386-1468,1594-1621`
- Modify: `src-tauri/src/lib.rs:49-53`
- Modify: `src/lib/types.ts:1-120`, `src/lib/api.ts:220-241`
- Test: model and store test modules

**Interfaces:**
- Produces `P2pShareUploadMode::{Auto, Unlimited, Custom}`.
- Produces `P2pShareUploadSettings { mode, custom_limit_bytes_per_second, effective_limit_bytes_per_second, measured_at }`.
- Produces `NetworkSpeedSettings { download_limit_bytes_per_second, p2p_share_upload }` and `DownloadTask::p2p_sharing: bool`.
- Produces `TaskStore::list_for_aria2_sync()`: normal active tasks plus completed records where `p2p_sharing=1`.

- [ ] **Step 1: Write failing model and migration tests**

```rust
#[test]
fn default_network_speed_settings_are_unlimited_download_and_auto_p2p_upload() {
    let settings = AppSettings::default();
    assert_eq!(settings.network.download_limit_bytes_per_second, None);
    assert_eq!(settings.network.p2p_share_upload.mode, P2pShareUploadMode::Auto);
    assert_eq!(settings.aria2.bt_seed_ratio, 1.5);
}

#[test]
fn migration_defaults_p2p_sharing_to_false() {
    let store = open_pre_p2p_schema_fixture();
    assert!(!store.find(existing_completed_id()).unwrap().p2p_sharing);
}
```

- [ ] **Step 2: Run the tests to verify failure**

Run: `cargo test -p alldownloader default_network_speed_settings_are_unlimited_download_and_auto_p2p_upload migration_defaults_p2p_sharing_to_false`

Expected: compile failure because the new network settings and `p2p_sharing` field do not exist.

- [ ] **Step 3: Add the minimal durable contract**

```rust
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum P2pShareUploadMode { #[default] Auto, Unlimited, Custom }

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NetworkSpeedSettings {
    pub download_limit_bytes_per_second: Option<u64>,
    pub p2p_share_upload: P2pShareUploadSettings,
}
```

Validate custom settings as positive bounded bytes/s. Add `network` to `AppSettings`; add `p2p_sharing INTEGER NOT NULL DEFAULT 0` through a forward-only migration; extend task inserts, selects, snapshot updates, queue payloads and TypeScript defaults. Add `list_for_aria2_sync` with terminal filtering overridden only by `p2p_sharing=1`.

- [ ] **Step 4: Run targeted Rust tests**

Run: `cargo test -p alldownloader models::tests store::tests`

Expected: PASS, including defaults, validation, migration and task row round-trips.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/models.rs src-tauri/src/store.rs src-tauri/src/lib.rs src/lib/types.ts src/lib/api.ts
git commit -m "feat: persist p2p sharing and network speed settings"
```

## Task 2: Finish P2P at data completion and retain its synchronizer

**Files:**
- Modify: `src-tauri/src/aria2/service.rs:900-940,1147-1265`
- Modify: `src-tauri/src/aria2/sync.rs:24-45`
- Test: `src-tauri/tests/aria2_bt.rs` and service tests

**Interfaces:**
- Consumes Task 1’s `p2p_sharing` and `list_for_aria2_sync`.
- Produces active P2P snapshot with full data → `Completed + p2p_sharing=true`.
- Produces terminal aria2 completion → `Completed + p2p_sharing=false`.

- [ ] **Step 1: Write failing snapshot tests**

```rust
#[tokio::test]
async fn full_active_torrent_is_completed_but_remains_marked_as_p2p_sharing() {
    let updated = persist(fixture_task("torrent"), active_snapshot("100", "100"));
    assert_eq!(updated.status, TaskStatus::Completed);
    assert!(updated.p2p_sharing);
    assert!(updated.completed_at.is_some());
}

#[tokio::test]
async fn terminal_torrent_completion_clears_p2p_sharing() {
    let updated = persist(sharing_completed_task(), complete_snapshot("100", "100"));
    assert_eq!(updated.status, TaskStatus::Completed);
    assert!(!updated.p2p_sharing);
}
```

- [ ] **Step 2: Run the tests to verify failure**

Run: `cargo test -p alldownloader full_active_torrent_is_completed_but_remains_marked_as_p2p_sharing terminal_torrent_completion_clears_p2p_sharing`

Expected: FAIL because active aria2 snapshots are currently persisted as `Downloading`.

- [ ] **Step 3: Implement completion classification and polling**

```rust
fn is_p2p_kind(kind: &str) -> bool {
    matches!(kind, "magnet" | "torrent" | "ed2k")
}
let data_complete = snapshot.total_length.is_some_and(|total| total > 0 && snapshot.completed_length >= total);
if is_p2p_kind(&task.kind) && snapshot.status == TaskStatus::Downloading && data_complete {
    task.status = TaskStatus::Completed;
    task.p2p_sharing = true;
}
```

Clear the flag for aria2 terminal/error/removed and user pause paths. Reconcile through `list_for_aria2_sync`; count a sharing-completed task as poll-active. Preserve observed BT peers while it remains sharing.

- [ ] **Step 4: Run targeted aria2 tests**

Run: `cargo test -p alldownloader --test aria2_bt -- full_active_torrent_is_completed_but_remains_marked_as_p2p_sharing terminal_torrent_completion_clears_p2p_sharing`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/aria2/service.rs src-tauri/src/aria2/sync.rs src-tauri/tests/aria2_bt.rs
git commit -m "feat: finish p2p tasks before seeding ends"
```

## Task 3: P2P share controller, measurements and cloud-upload yielding

**Files:**
- Create: `src-tauri/src/aria2/p2p_share.rs`
- Modify: `src-tauri/src/aria2/mod.rs`, `src-tauri/src/aria2/rpc.rs:34-69,240-289`, `src-tauri/src/aria2/process.rs`
- Modify: `src-tauri/src/engine_runtime.rs`, `src-tauri/src/commands.rs:3019-3058`, `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/cloud/upload/control.rs:15-80`
- Test: `p2p_share.rs`, `rpc.rs`, `upload/control.rs`

**Interfaces:**
- Produces `Aria2Api::change_global_option(options)` → `aria2.changeGlobalOption`.
- Produces `P2pShareController::reconcile(api, tasks, cloud_upload_active)`.
- Produces Tauri `record_p2p_upload_speed_measurement(bytes_per_second: u64)` which applies and persists the effective setting.
- Produces `UploadRegistry::has_active_uploads() -> bool`.

- [ ] **Step 1: Write failing controller tests**

```rust
#[tokio::test]
async fn successful_measurement_sets_half_as_the_global_p2p_cap() {
    let mut controller = auto_controller_without_history();
    controller.record_measurement(8 * 1024 * 1024, 100).unwrap();
    assert_eq!(controller.effective_limit(), Some(4 * 1024 * 1024));
}

#[tokio::test]
async fn cloud_upload_pauses_only_sharing_p2p_gids() {
    let actions = reconcile_with_cloud_upload(&[sharing_torrent("a"), ftp_download("b")]);
    assert_eq!(actions.paused_gids, vec!["a"]);
}
```

- [ ] **Step 2: Run the tests to verify failure**

Run: `cargo test -p alldownloader successful_measurement_sets_half_as_the_global_p2p_cap cloud_upload_pauses_only_sharing_p2p_gids`

Expected: compile failure because the controller and global RPC method do not exist.

- [ ] **Step 3: Implement controller and runtime wiring**

```rust
async fn change_global_option(&self, options: Aria2Options) -> Result<String, Aria2Error> {
    self.call("aria2.changeGlobalOption", vec![json!(options)]).await
}
```

Keep one runtime-owned mutex with settings, latest confirmed cap, measurement gate and a `HashSet<String>` of policy-paused GIDs. Auto mode starts gated, applies `measurement / 2` on success, uses `256K` on the first failure, and retains history on later failure. Apply the download setting with `max-overall-download-limit`; apply the P2P share cap with `max-overall-upload-limit`. In auto mode, every hourly probe closes the gate until it resolves. In custom/unlimited modes, do not start probes.

On every reconcile, read `UploadRegistry::has_active_uploads()`. A true result force-pauses only complete P2P sharing GIDs and records them; when false, only those recorded GIDs are resumed, and only after the measurement gate opens. Use pause rather than aria2’s `0` upload limit for zero-share behavior. RPC failures retain last confirmed values and never alter completion status.

- [ ] **Step 4: Run focused Rust verification**

Run: `cargo test -p alldownloader p2p_share rpc::tests upload_control_tests`

Expected: PASS for successful/failed/history measurements, startup/hourly gating, custom/unlimited modes, exact RPC wire method, cloud yielding, and user-pause protection.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/aria2/p2p_share.rs src-tauri/src/aria2/mod.rs src-tauri/src/aria2/rpc.rs src-tauri/src/aria2/process.rs src-tauri/src/engine_runtime.rs src-tauri/src/commands.rs src-tauri/src/lib.rs src-tauri/src/cloud/upload/control.rs
git commit -m "feat: control p2p sharing bandwidth and cloud upload yielding"
```

## Task 4: Settings UI, automatic probe and completion display

**Files:**
- Modify: `package.json`, lockfile, `src/App.tsx`, `src/components/SettingsPanel.tsx`, `src/components/DownloadQueue.tsx`
- Create: `src/lib/p2pSpeedProbe.ts`, `src/lib/p2pSpeedProbe.test.ts`
- Test: `src/components/SettingsPanel.test.tsx`, `src/components/DownloadQueue.test.tsx`, `src/App.test.tsx`

**Interfaces:**
- Produces `measureP2pUploadSpeed(signal): Promise<number>`, resolving bytes/s or rejecting at 60 seconds.
- Consumes Task 3 measurement command.
- Produces one startup probe plus cleanup-safe one-hour probe only for automatic mode.

- [ ] **Step 1: Write failing UI tests**

```tsx
it('defaults download to unlimited and P2P sharing upload to automatic', async () => {
  render(<SettingsPanel initial={defaultSettings} onChange={vi.fn()} />);
  await userEvent.setup().click(screen.getByRole('tab', { name: '网络' }));
  expect(screen.getByLabelText('下载速度限制')).toHaveValue('unlimited');
  expect(screen.getByLabelText('P2P 分享上传速度限制')).toHaveValue('auto');
});

it('shows sharing text only for a completed sharing task', () => {
  renderQueue({ status: 'completed', p2p_sharing: true, completed_at: 1_789_095_720 });
  expect(screen.getByText('P2P 分享中')).toBeInTheDocument();
});
```

- [ ] **Step 2: Run tests to verify failure**

Run: `pnpm vitest run src/components/SettingsPanel.test.tsx src/components/DownloadQueue.test.tsx --exclude '.worktrees/**'`

Expected: FAIL because the controls and conditional sharing text do not exist.

- [ ] **Step 3: Implement the minimal user-facing behavior**

Add `@cloudflare/speedtest` as a production dependency. Wrap it in `p2pSpeedProbe.ts`, abort at 60 seconds, and mock only that wrapper in tests so no test hits the network. Add download `不限速/自定义` and P2P `自动/不限制/自定义` controls; render custom number input only in custom mode. Show `自动（当前 …）` when a verified effective cap exists and disclose the Cloudflare measurement/60-second limit.

In `App`, schedule probes after settings load and every hour only in auto mode; abort on unmount/mode change and report only positive bytes/s results. In the completed summary append `P2P 分享中` only for `task.p2p_sharing === true`; render no waiting, pause, cloud-upload, ratio, or error substitute text.

- [ ] **Step 4: Run targeted front-end tests and build**

Run: `pnpm vitest run src/lib/p2pSpeedProbe.test.ts src/components/SettingsPanel.test.tsx src/components/DownloadQueue.test.tsx src/App.test.tsx --exclude '.worktrees/**'`

Expected: PASS.

Run: `pnpm run build`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add package.json pnpm-lock.yaml src/lib/p2pSpeedProbe.ts src/lib/p2pSpeedProbe.test.ts src/App.tsx src/components/SettingsPanel.tsx src/components/SettingsPanel.test.tsx src/components/DownloadQueue.tsx src/components/DownloadQueue.test.tsx
git commit -m "feat: show p2p sharing and configure speed limits"
```

## Task 5: Review, release build and merge

**Files:**
- Modify: only files repaired by focused verification

- [ ] **Step 1: Run final focused Rust verification**

Run: `cargo test -p alldownloader --test aria2_bt && cargo test -p alldownloader p2p_share && cargo test -p alldownloader upload_control_tests`

Expected: PASS; no unrelated Rust suite runs.

- [ ] **Step 2: Run final focused front-end verification**

Run: `pnpm vitest run src/lib/p2pSpeedProbe.test.ts src/components/SettingsPanel.test.tsx src/components/DownloadQueue.test.tsx src/App.test.tsx --exclude '.worktrees/**' && pnpm run build`

Expected: PASS.

- [ ] **Step 3: Build the Windows release artifact**

Run: `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows.ps1`

Expected: PASS and updates `artifacts/alldownloader.exe`.

- [ ] **Step 4: Review and commit repairs**

```bash
git diff --check
git status --short
git add src-tauri/src/models.rs src-tauri/src/store.rs src-tauri/src/aria2/p2p_share.rs src-tauri/src/aria2/service.rs src-tauri/src/aria2/sync.rs src-tauri/src/aria2/rpc.rs src-tauri/src/engine_runtime.rs src-tauri/src/commands.rs src/App.tsx src/components/SettingsPanel.tsx src/components/DownloadQueue.tsx
git commit -m "fix: verify p2p sharing speed controls"
```

- [ ] **Step 5: Request final code review and merge the isolated branch**

```bash
git checkout master
git merge --no-ff codex/p2p-completion-network-limits
git status --short
```

Expected: merge contains only the feature commits and preserves unrelated dirty root changes.
