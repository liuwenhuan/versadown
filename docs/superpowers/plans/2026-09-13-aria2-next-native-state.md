# Aria2 Next 原生状态统一 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 删除旧 aria2 状态依赖，使任务清单、断点恢复和媒体下载统一使用 Aria2 Next 原生机制。

**Architecture:** Aria2 Next 的 `aria2-next.tasks` 保存任务清单，`aria2-next-state` 保存协议级断点状态，`tasks.sqlite` 只保留应用任务投影和 GID 映射。恢复过程严格核对双方状态并永久清理不兼容任务，不再导入或迁移旧状态。

**Tech Stack:** Rust 2021、Tokio、rusqlite、Tauri 2、Aria2 Next 2.7.5 JSON-RPC、React/TypeScript。

**Spec:** `docs/superpowers/specs/2026-09-13-aria2-next-native-state-design.md`

## Global Constraints

- 在现有独立 worktree `cloud-provider-architecture-v2` 和分支 `codex/cloud-provider-architecture-v2` 内实现。
- 不读取、写入或迁移旧 `aria2.session`；不依据 `.aria2` 判断任务状态。
- 不兼容的未完成任务及其下载根目录内局部文件直接删除；已完成历史和完整文件保留。
- 删除文件必须验证规范化路径位于配置下载根目录内。
- 每项行为变更先获得 RED，再实现 GREEN；只运行相关定向测试和 Windows 发布构建。

---

### Task 1: 切换原生任务文件并删除旧迁移修补

**Files:**
- Modify: `src-tauri/src/aria2/process.rs`
- Modify: `src-tauri/tests/aria2_process.rs`
- Modify: `src-tauri/src/lib.rs`
- Delete: `src-tauri/src/engine_migration.rs`
- Delete: `src-tauri/tests/aria2_next_migration.rs`

**Interfaces:**
- Produces: `build_aria2_args` 只绑定 `aria2-next.tasks` 和 `aria2-next-state`。
- Removes: `run_aria2_next_migration`、`LegacyPurgeReport` 和启动重号数据库修改。

- [ ] 修改启动参数测试，断言任务文件为 `aria2-next.tasks`，且不存在 `aria2.session`。
- [ ] 运行 `cargo test --test aria2_process launch_arguments_disable_remote_rpc_and_enable_persistence`，确认因旧路径而 RED。
- [ ] 修改 `process.rs` 并删除旧迁移入口、模块和测试。
- [ ] 运行 `cargo test --test aria2_process`，确认 GREEN。
- [ ] 提交 `refactor(aria2): use native task and state files`。

### Task 2: 建立安全的不兼容任务清理原语

**Files:**
- Modify: `src-tauri/src/store.rs`
- Modify: `src-tauri/tests/aria2_recovery.rs`

**Interfaces:**
- Produces: TaskStore 方法按任务 ID 原子删除记录，并在下载根目录内删除记录的输出和遗留 `.aria2`。
- Consumes: 恢复层提供的不兼容任务 ID 集合。

- [ ] 写测试覆盖局部文件与遗留 `.aria2` 删除、已完成任务保留、越界文件拒绝和目录不删除。
- [ ] 运行对应 `cargo test --test aria2_recovery <case>`，确认缺少清理 API 而 RED。
- [ ] 将旧迁移专用 purge 重构为当前严格恢复使用的安全清理方法；不保留版本标记或迁移分支。
- [ ] 运行相关恢复测试，确认 GREEN。
- [ ] 提交 `refactor(store): add strict native recovery cleanup`。

### Task 3: 将恢复改为严格双边核对

**Files:**
- Modify: `src-tauri/src/aria2/recovery.rs`
- Modify: `src-tauri/src/aria2/service.rs`
- Modify: `src-tauri/src/engine_runtime.rs`
- Modify: `src-tauri/tests/aria2_recovery.rs`
- Modify: `src-tauri/tests/engine_runtime.rs`

**Interfaces:**
- Produces: `DownloadService::recover(download_root)` 严格同步匹配 GID，并清理应用孤儿和引擎孤儿。
- Removes: 未知会话导入、旧磁力父子猜测、`.aria2` HTTP 回退重提交。

- [ ] 写恢复测试：匹配 GID 正常同步；应用孤儿被删除；引擎孤儿被 remove/forceRemove；单个冲突不阻断其他任务；完成历史保留。
- [ ] 逐个运行测试并观察预期 RED，失败必须来自当前宽松导入/回退行为。
- [ ] 实现严格核对和逐项清理，恢复报告增加清理计数但不暴露敏感路径。
- [ ] 更新运行时启动、自动重启与设置重启调用，传入下载根目录。
- [ ] 运行 `cargo test --test aria2_recovery` 与 `cargo test --test engine_runtime`，确认 GREEN。
- [ ] 提交 `refactor(aria2): enforce strict native recovery ownership`。

### Task 4: 删除 `.aria2` 状态判断

**Files:**
- Modify: `src-tauri/src/duplicates.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/store.rs`
- Modify: `src-tauri/tests/aria2_recovery.rs`

**Interfaces:**
- Produces: 重复下载与续传判定只依据任务状态、GID 和 Aria2 Next RPC 状态。
- Retains: 永久删除文件时清扫同名遗留 `.aria2`。

- [ ] 修改重复任务测试，证明仅创建 `.aria2` 不再产生 `ResumeAvailable`，存在受管 GID 才能进入原生恢复路径。
- [ ] 运行 `cargo test duplicates::tests`，确认 RED。
- [ ] 删除完成/恢复分支中的 `.aria2` 检查及旧 HTTP 回退。
- [ ] 保留用户彻底删除和冲突清理中的遗留 sidecar 清扫。
- [ ] 运行重复任务、命令删除和恢复相关定向测试，确认 GREEN。
- [ ] 提交 `refactor(aria2): remove legacy control file semantics`。

### Task 5: HLS/DASH 切换到 Aria2 Next 原生媒体

**Files:**
- Modify: `src-tauri/src/engine_runtime.rs`
- Modify: `src-tauri/src/aria2/options.rs`
- Modify: `src-tauri/src/lib.rs`
- Delete: `src-tauri/src/media.rs`
- Modify: `src-tauri/tests/aria2_live.rs`
- Modify: `src-tauri/Cargo.toml` only if a dependency becomes unused.

**Interfaces:**
- Produces: `hls`/`dash` 调用普通 `DownloadService`，分别传递 `media=hls`/`media=dash`。
- Removes: `MediaDownloadEngine`、分片工作目录、单独 worker 生命周期和 `.aria2` 分片判断。

- [ ] 写测试断言 HLS/DASH 最终提交为单一 `aria2.addUri`，并包含正确 `media` 选项。
- [ ] 运行相关测试，确认当前路由到 `MediaDownloadEngine` 而 RED。
- [ ] 删除媒体拦截器，给提交选项增加原生媒体模式并统一控制/恢复/退出。
- [ ] 删除旧媒体模块与只验证旧解析器的测试。
- [ ] 运行媒体定向单元测试与本地 `aria2_live` 原生媒体场景，确认 GREEN。
- [ ] 提交 `refactor(media): delegate hls and dash to aria2 next`。

### Task 6: 发布验证与交付

**Files:**
- Modify: `docs/superpowers/specs/2026-09-12-aria2-next-ed2k-media-ftp-design.md`
- Modify: `docs/superpowers/plans/2026-09-13-aria2-next-native-state.md`

**Interfaces:**
- Produces: 可运行 Windows 二进制与逐项验收证据。

- [ ] 运行 `cargo fmt --check` 和 `git diff --check`。
- [ ] 运行 `cargo test --test aria2_process`、`cargo test --test aria2_recovery`、`cargo test --test engine_runtime` 及相关 `aria2_live` 场景。
- [ ] 运行受影响前端定向 Vitest（带 `--exclude '.worktrees/**'`）和 `npm run build`，仅在前端契约实际改动时执行。
- [ ] 运行 `scripts/build-windows.ps1` 生成发布二进制。
- [ ] 对照设计逐项记录通过、部分通过或剩余风险，并更新本计划复选框。
- [ ] 提交 `docs: finalize aria2 next native state delivery`。
