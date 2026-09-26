# 云盘目录下载任务组 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 使任意网盘目录下载在队列中始终显示为一个可持久化、可递归展开的任务。

**Architecture:** 文件仍由既有 `tasks` 表和 aria2 管理；新增目录组和组项目保存它们的稳定关联。命令层把组投影为合成 `folder` 根节点并隐藏关联叶子，React 递归渲染该树，但保留 BT 的文件选择行为。

**Tech Stack:** Rust、SQLite/rusqlite、Tauri、React、TypeScript、Vitest、Cargo。

**Spec:** `docs/superpowers/specs/2026-09-10-cloud-directory-group-design.md`

## Global Constraints

- 直接在 `master` 工作；绝不覆盖现有未提交的悬浮窗相关修改，也不使用临时构建目录。
- 不生成安装包；最终只构建并启动 `src-tauri/target/release/versadown.exe`。
- 目录组仅保存提供方、稳定远端 ID、安全相对路径和本地任务关联，不保存凭据或带签名直链。
- 仅由持久化组项目关联任务；禁止按文件名或本地路径猜测成员关系。
- 每项行为必须先写 RED 测试、确认失败后再写最小实现。
- 旧版本平铺任务不迁移、不隐藏；BT/磁力多文件选择保持原有行为。

---

### Task 1: 持久化目录组和成员关系

**Files:**
- Modify: `src-tauri/src/models.rs`
- Modify: `src-tauri/src/store.rs`
- Test: `src-tauri/src/store.rs`

**Interfaces:**
- Produces `CloudDirectoryGroup`, `NewCloudDirectoryGroup`, `CloudDirectoryGroupItem` 和 `NewCloudDirectoryGroupItem`。
- Produces `TaskStore::{create_cloud_directory_group, cloud_directory_group, cloud_directory_groups, cloud_directory_group_items, link_cloud_directory_group_item_task, fail_cloud_directory_group_item, cloud_directory_group_for_task}`。
- 每项拥有持久化 UUID；`task_id: Option<Uuid>` 允许直链生成失败的虚拟叶子稳定存在。

- [ ] **Step 1: Write the failing test**

在 `store.rs` 新增测试：创建根目录 `Season 1`、路径为 `Season 1/Extras` 的项目后关闭并重新打开文件型数据库，断言组/项目 UUID、提供方、远端 ID、相对目录和输出名完全保留。第二个测试把一个项目关联真实 `NewTask::direct(\"https://example.test/episode.mkv\")`，把另一项目标记失败，断言成员查询只返回真实子任务且失败项目无 `task_id`。

```rust
let group = store.create_cloud_directory_group(
    NewCloudDirectoryGroup::new(CloudProvider::Quark, "root-id", "Season 1", &base),
    &[NewCloudDirectoryGroupItem::new("remote-1", &["Season 1", "Extras"], "behind.mkv", None)],
)?;
assert_eq!(store.cloud_directory_group_items(group.id)?[0].relative_directory, vec!["Season 1", "Extras"]);
```

- [ ] **Step 2: Verify RED**

Run: `D:\\projects\\downloader\\.tools\\cargo\\bin\\cargo.exe test --manifest-path src-tauri\\Cargo.toml store::tests::cloud_directory_group -- --test-threads=1`

Expected: FAIL because schema and storage APIs do not exist.

- [ ] **Step 3: Implement the additive migration and transactional APIs**

在现有 schema 初始化中创建 `cloud_directory_groups` 和 `cloud_directory_group_items`，项目表使用组外键和可空 `task_id` 外键，建立唯一 `(group_id,item_index)` 与非空 `task_id` 索引。组和所有项目必须同一事务创建；拒绝 UUID 无效、控制字符与带分隔符的目录组件。

```rust
pub fn link_cloud_directory_group_item_task(
    &self, group_id: Uuid, item_id: Uuid, task_id: Uuid,
) -> Result<(), StoreError>;
```

- [ ] **Step 4: Verify GREEN**

Run: `D:\\projects\\downloader\\.tools\\cargo\\bin\\cargo.exe test --manifest-path src-tauri\\Cargo.toml store::tests::cloud_directory_group -- --test-threads=1`

Expected: PASS;重开数据库后成员关系和未入队失败项目都存在。

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/models.rs src-tauri/src/store.rs
git commit -m "feat(cloud): persist directory download groups"
```

### Task 2: 队列投影、汇总和组级操作

**Files:**
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/models.rs`
- Test: `src-tauri/src/commands.rs`

**Interfaces:**
- Produces `QueueTaskPayload { id: String, children: Vec<QueueTaskPayload>, ... }` for list responses.
- Produces `project_cloud_directory_groups(store, leaves, scope)` used by `list_tasks`。
- Extends `run_task_action` to route持久化组 UUID 与项目 UUID；目录树中间节点仅可展开，不接受命令。

- [ ] **Step 1: Write the failing projection tests**

构造一个组：一项在 `Season 1/Extras` 正在下载、一项完成、一项未入队失败。断言 active scope 只含一个 `kind == \"folder\"` 根任务，关联本地叶子不在顶层；根节点有递归 `Extras` 目录与三叶子。断言总长度、完成长度、速度、进度和状态来自实际叶子；最后活动叶子完成后，根只出现在 terminal scope。

```rust
assert_eq!(active.len(), 1);
assert_eq!(active[0].kind, "folder");
assert_eq!(active[0].children[0].name, "Extras");
assert!(active[0].children[0].children.iter().any(|child| child.status == TaskStatus::Failed));
```

- [ ] **Step 2: Verify RED**

Run: `D:\\projects\\downloader\\.tools\\cargo\\bin\\cargo.exe test --manifest-path src-tauri\\Cargo.toml commands::tests::cloud_directory_group_projection -- --test-threads=1`

Expected: FAIL because linked leaves are still flat tasks.

- [ ] **Step 3: Implement one queue-tree projection**

普通任务映射为 `QueueTaskPayload`；组根 ID 使用组 UUID，未入队失败叶子使用项目 UUID，中间目录使用稳定 `group:<group-uuid>:dir:<encoded-path>` ID。仅汇总已知长度；速度/连接数相加；任一活动叶子使根保持 active，全部成功才 completed，无活动且存在失败才 failed。组操作只作用可操作的真实叶子；失败项目的 retry 调用 Task 3 的重提交流程。

```rust
if let Some(group) = store.cloud_directory_group(id)? {
    return run_cloud_directory_group_action(&state, &group, action).await;
}
```

- [ ] **Step 4: Verify GREEN**

Run: `D:\\projects\\downloader\\.tools\\cargo\\bin\\cargo.exe test --manifest-path src-tauri\\Cargo.toml commands::tests::cloud_directory_group_projection -- --test-threads=1`

Run: `D:\\projects\\downloader\\.tools\\cargo\\bin\\cargo.exe test --manifest-path src-tauri\\Cargo.toml --lib commands -- --test-threads=1`

Expected: PASS;普通任务和旧平铺目录任务的列表不变。

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/commands.rs src-tauri/src/models.rs
git commit -m "feat(cloud): project directory groups in download queue"
```

### Task 3: 目录提交、失败项目持久化与重试

**Files:**
- Modify: `src-tauri/src/cloud/commands.rs`
- Modify: `src-tauri/src/commands.rs`
- Test: `src-tauri/src/cloud/commands.rs`
- Test: `src-tauri/tests/cloud_directory_download.rs`

**Interfaces:**
- Extends `PlannedCloudFile` with `size: Option<u64>`。
- Changes `CloudDirectoryDownloadOutcome` to `{ task: TaskActionOutcome, queued_count, failures }`，不再返回每个文件的顶层任务。
- Produces `retry_cloud_directory_group_item(app, group_id, item_id)` for Task 2.

- [ ] **Step 1: Write the failing command tests**

用嵌套两文件计划：第一文件成功入队、第二文件取直链失败。断言只创建一个组，第一项目链接一个任务，第二项目保存远端 ID、路径、失败码；返回的 `task` 是组父任务且 `queuedCount == 1`。第二测试让失败项目稍后返回直链，断言 retry 只创建并链接这一项的新任务。

```rust
assert_eq!(outcome.queued_count, 1);
assert!(items[0].task_id.is_some());
assert_eq!(items[1].failure_code.as_deref(), Some("provider_error"));
```

- [ ] **Step 2: Verify RED**

Run: `D:\\projects\\downloader\\.tools\\cargo\\bin\\cargo.exe test --manifest-path src-tauri\\Cargo.toml cloud_directory_download -- --test-threads=1`

Expected: FAIL because current command returns child outcomes and discards durable membership/failure data.

- [ ] **Step 3: Create the group before per-file submission**

计划成功后先在 TaskStore 建立包含全部项目的组。每个项目沿用现有安全目录/输出名验证；成功入队后写 `task_id`，重复源记录 `duplicate` 失败，其他错误经现有错误映射持久化。重试只读取该项目已保存的提供方、远端 ID 和安全目标，不扫描同名任务也不重传已关联项目。

```rust
match submit_cloud_file(app.clone(), state, provider, item.remote_file_id.clone(), options).await {
    Ok(CloudFileSubmission::Queued(outcome)) => store.link_cloud_directory_group_item_task(group_id, item.id, outcome.payload.task.id)?,
    Ok(CloudFileSubmission::Duplicate(_)) => store.fail_cloud_directory_group_item(group_id, item.id, "duplicate", "已存在下载任务，未重复入队")?,
    Err(error) => store.fail_cloud_directory_group_item(group_id, item.id, code, message)?,
}
```

- [ ] **Step 4: Verify GREEN**

Run: `D:\\projects\\downloader\\.tools\\cargo\\bin\\cargo.exe test --manifest-path src-tauri\\Cargo.toml cloud_directory_download -- --test-threads=1`

Run: `D:\\projects\\downloader\\.tools\\cargo\\bin\\cargo.exe test --manifest-path src-tauri\\Cargo.toml --lib cloud::commands::tests -- --test-threads=1`

Expected: PASS;计划错误不入队，提交阶段失败在父树中可见且可重试。

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/cloud/commands.rs src-tauri/src/commands.rs src-tauri/tests/cloud_directory_download.rs
git commit -m "feat(cloud): group recursive directory downloads"
```

### Task 4: 云盘 API 契约与递归队列 UI

**Files:**
- Modify: `src/lib/cloudTypes.ts`
- Modify: `src/lib/cloudApi.ts`
- Modify: `src/components/CloudManager.tsx`
- Modify: `src/components/CloudManager.test.tsx`
- Modify: `src/components/DownloadQueue.tsx`
- Modify: `src/components/DownloadQueue.test.tsx`
- Modify: `src/styles.css`

**Interfaces:**
- `CloudDirectoryDownloadOutcome` 改为一个 `task: TaskActionResult`、`queuedCount`、`failures`。
- `QueueTask.children` 保持递归，可含中间目录、真实叶子和失败虚拟叶子。
- 仅 BT/magnet 的 `files` 分支调用 `onFileSelection`；云盘树叶通过自身 ID 调用普通 `onCommand`。

- [ ] **Step 1: Write the failing frontend tests**

CloudManager 测试：选目录后结果只含一个组父任务，`onDownloadTask` 只被调用一次。DownloadQueue 测试使用 `Season 1 → Extras → behind.mkv` 和同级失败文件：折叠根时所有后代隐藏，先展开根再展开 Extras 才显示文件；现有 BT 多文件暂停/取消断言仍通过。

```tsx
await user.click(screen.getByRole('button', { name: '展开 Season 1' }));
await user.click(screen.getByRole('button', { name: '展开 Extras' }));
expect(screen.getByText('behind.mkv')).toBeVisible();
expect(screen.getByText('link generation failed')).toBeVisible();
```

- [ ] **Step 2: Verify RED**

Run: `pnpm exec vitest run src/components/CloudManager.test.tsx src/components/DownloadQueue.test.tsx --exclude '.worktrees/**'`

Expected: FAIL because CloudManager currently publishes each child and DownloadQueue only maps one children level.

- [ ] **Step 3: Implement contract mapping and recursive accessible rows**

云盘 API 映射并发布唯一父任务。用递归 `QueueTreeRows` 替换单层 `taskChildren.map`，保留 `展开 <name>` / `收起 <name>` 无障碍标签、层级缩进、叶子状态/错误/进度和原有 BT 文件转换分支；新增局部树形 CSS，不全局改写现有平铺行选择器。

```tsx
function QueueTreeRows({ nodes, depth, expanded, onToggle }: QueueTreeRowsProps) {
  return <>{nodes.map((node) => <QueueTreeRow key={node.id} node={node} depth={depth} expanded={expanded} onToggle={onToggle} />)}</>;
}
```

- [ ] **Step 4: Verify GREEN and production frontend build**

Run: `pnpm exec vitest run src/components/CloudManager.test.tsx src/components/DownloadQueue.test.tsx src/App.live.test.tsx --exclude '.worktrees/**'`

Run: `pnpm run build`

Expected: PASS;云盘目录一个顶层行、任意深度展开，BT 操作未回归。

- [ ] **Step 5: Commit**

```powershell
git add src/lib/cloudTypes.ts src/lib/cloudApi.ts src/components/CloudManager.tsx src/components/CloudManager.test.tsx src/components/DownloadQueue.tsx src/components/DownloadQueue.test.tsx src/styles.css
git commit -m "feat(ui): show cloud directory downloads as trees"
```

### Task 5: 集成回归与标准 release 二进制

**Files:**
- Modify: only a missing integration test or minimal integration correction
- Test: `src-tauri/tests/cloud_directory_download.rs`
- Test: `src/components/DownloadQueue.test.tsx`

**Interfaces:**
- Consumes completed Tasks 1–4 and must not introduce another grouping mechanism.

- [ ] **Step 1: Write/identify one command-list boundary test**

执行一次嵌套目录提交后调用 `list_tasks(\"active\")`，断言回复只有一个 folder 根和嵌套叶子、没有关联叶子处于顶层。若 Task 3 已覆盖相同命令/列表边界，记录该测试名而不重复测试。

```rust
assert_eq!(visible.iter().filter(|task| task.kind == "folder").count(), 1);
```

- [ ] **Step 2: Verify RED or record existing exact coverage**

Run: `D:\\projects\\downloader\\.tools\\cargo\\bin\\cargo.exe test --manifest-path src-tauri\\Cargo.toml cloud_directory_download -- --test-threads=1`

Expected: 新边界测试在修复前失败；若完全由 Task 3 覆盖则该套件已通过且报告精确测试名。

- [ ] **Step 3: Make the smallest missing correction**

只修复命令提交和列表投影之间缺失的关联；禁止调整 schema 或另建第二套分组逻辑。

- [ ] **Step 4: Run final relevant verification**

Run: `D:\\projects\\downloader\\.tools\\cargo\\bin\\cargo.exe test --manifest-path src-tauri\\Cargo.toml cloud_directory_download -- --test-threads=1`

Run: `D:\\projects\\downloader\\.tools\\cargo\\bin\\cargo.exe test --manifest-path src-tauri\\Cargo.toml --lib commands -- --test-threads=1`

Run: `pnpm exec vitest run src/components/CloudManager.test.tsx src/components/DownloadQueue.test.tsx src/App.live.test.tsx --exclude '.worktrees/**'`

Run: `pnpm run build`

Expected: all pass, no installer is produced.

- [ ] **Step 5: Build and start the normal release executable**

只在确认运行进程路径精确等于标准 release EXE 时停止旧进程；使用 MSVC 初始化环境构建，再启动同一 EXE。

```powershell
cmd.exe /d /s /c '"C:\\Program Files (x86)\\Microsoft Visual Studio\\2022\\BuildTools\\VC\\Auxiliary\\Build\\vcvars64.bat" >nul && set "CARGO_HOME=D:\\projects\\downloader\\.tools\\cargo" && set "RUSTUP_HOME=D:\\projects\\downloader\\.tools\\rustup" && "D:\\projects\\downloader\\.tools\\cargo\\bin\\cargo.exe" build --manifest-path "D:\\projects\\downloader\\src-tauri\\Cargo.toml" --release --features custom-protocol'
Start-Process 'D:\\projects\\downloader\\src-tauri\\target\\release\\versadown.exe'
```

- [ ] **Step 6: Commit only a required correction**

```powershell
git add <only-files-changed-by-the-integration-correction>
git commit -m "test(cloud): verify directory task group lifecycle"
```

