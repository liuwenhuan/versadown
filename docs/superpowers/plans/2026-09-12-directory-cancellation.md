# 目录任务取消修复计划

**Goal:** 未入队目录能够取消；取消覆盖全部关联活动任务，保留下载文件，并阻止目录提交在取消之后继续运行。

**Architecture:** 沿用目录表的 status 字段持久化取消意图。目录提交、重试和取消使用同一按目录串行锁；先记录取消意图，再等待正在执行的单文件提交结束，逐一取消已有活动任务。投影保留仍然活动的真实状态，所有活动任务停止后才显示 cancelled。

**Tech Stack:** Rust、Tauri、SQLite、Tokio；不改变前端数据契约。

**Spec:** 本任务用户确认的修复要求：未入队文件及已入队任务都可取消，不删除文件或清空数据库。

## 范围与验证

- 保留现有工作区改动；不修改用户运行中的任务数据库，不提交或合并。
- 使用真实内存/临时 SQLite，模拟外部下载提交与取消边界。
- `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_directory_download`
- `cargo test --manifest-path src-tauri/Cargo.toml --lib commands::tests -- --test-threads=1`
- `scripts/build-windows.ps1`；不运行全量测试。

## 执行

- [x] 在 cloud_directory_download.rs 补回归：全未入队目录、混合已完成/活动任务、单个取消失败不饿死后续兄弟任务、并发提交与取消、取消持久化和已下载文件保留。
- [x] 增加 store.cancel_cloud_directory_group，保存目录取消状态，不触碰任务文件；在 cloud/commands.rs 提供 cancel_cloud_directory_group_with(store, id, cancel) 测试/生产共用边界。
- [x] 目录提交每个文件之前获取按目录锁并重读状态；取消先标记状态再获取同一锁。重试拒绝已取消/回收目录，避免重新授权回调复活任务。
- [x] commands.rs 的 cancel 分支调用目录取消边界；虚拟文件遵循目录取消状态，汇总不得掩盖真实活动子任务；响应 task 和 queueTask 使用一致投影。
- [x] 运行上述定向测试、发布构建及 diff 检查；提供独立测试二进制并明确人工测试边界。

## 验证记录

- 初始回归复现未入队目录取消报错、混合目录仍排队、首个取消错误阻断兄弟任务；回收站回归分别复现无关联文件无法删除记录及虚拟叶仍显示排队。修正测试夹具中的计划遍历顺序和重试状态后重新验证。
- `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_directory_download -- --test-threads=1`：18 passed。
- `cargo test --manifest-path src-tauri/Cargo.toml --lib commands::tests -- --test-threads=1`：113 passed（过滤同时包含 commands 和 cloud::commands，非全量）。
- `vitest run src/components/DownloadQueue.test.tsx src/App.live.test.tsx -t 'directory|folder|cancel|recycl' --exclude '.worktrees/**' --exclude '.pnpm-store/**'`：17 passed，81 skipped。
- `git diff --check`（本次四个 Rust 文件）：通过，仅 LF/CRLF 提示。
- 独立复审发现并修正关联失败成员的授权恢复/链接刷新绕过取消保护，以及已回收虚拟叶状态不一致；按目录锁覆盖显式回收与批量删除，组锁先于来源锁。
- 所有数据库/文件验证均使用隔离测试数据；未修改用户真实任务数据库或取消用户当前任务。
- `scripts/build-windows.ps1`：release 构建成功（前端未修改，复用 dist）；独立测试目录包含主程序及配套下载引擎。用户需退出旧版本后验证截图中的任务取消操作。
