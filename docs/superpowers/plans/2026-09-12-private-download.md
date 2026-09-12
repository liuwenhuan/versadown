# 私密下载 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在本次进程受密码控制地隐藏私密下载任务，并支持普通/私密任务筛选。

**Architecture:** SQLite 为普通任务及云盘目录根持久化私密标记；DPAPI 单独保存密码，Rust `AppState` 保存易失解锁状态。所有任务读取和更新事件以该状态过滤，React 仅在解锁时提供筛选器与私密确认入口。

**Tech Stack:** Rust、Tauri 2、rusqlite、Windows DPAPI、React、TypeScript、Vitest。

**Spec:** `docs/superpowers/specs/2026-09-12-private-download-design.md`

## Global Constraints

- 只隐藏任务列表；不加密下载文件、目录或任务数据库。
- 密码不得写入 `settings.json`、IPC 响应或日志。
- 前端 Vitest 必须携带 `--exclude '.worktrees/**'`；只跑相关测试与发布构建。

---

### Task 1: 私密任务持久化与受控可见性

**Files:**
- Modify: `src-tauri/src/models.rs`, `src-tauri/src/store.rs`, `src-tauri/src/commands.rs`, `src-tauri/src/lib.rs`
- Test: `src-tauri/src/store.rs`, `src-tauri/src/commands.rs`

**Interfaces:**
- Produces `DownloadTask.is_private`, `CloudDirectoryGroup.is_private` 与 `PrivacyVisibility`。

- [ ] **Step 1: 写失败测试**：创建私密任务/目录根后，重新读取标记仍为真；锁定投影不返回它。
- [ ] **Step 2: 运行定向 Rust 测试并确认因为字段、迁移和过滤尚不存在而失败。**
- [ ] **Step 3: 实现 SQLite v8 迁移、创建/读取映射与按会话过滤投影。**
- [ ] **Step 4: 重跑定向测试并确认通过。**

### Task 2: 密码设置、会话解锁与私密提交

**Files:**
- Modify: `src-tauri/src/commands.rs`, `src-tauri/src/lib.rs`, `src-tauri/src/cloud/credentials.rs`, `src-tauri/src/cloud/commands.rs`
- Test: `src-tauri/src/commands.rs`

**Interfaces:**
- Produces `configure_privacy_download`, `unlock_privacy_tasks`, `lock_privacy_tasks`, `privacy_task_visibility` Tauri commands。

- [ ] **Step 1: 写失败测试**：空密码拒绝、错误密码不解锁、正确密码只影响当前会话、私密确认创建私密任务。
- [ ] **Step 2: 运行定向 Rust 测试并确认失败。**
- [ ] **Step 3: 复用 DPAPI 存储，设置后归零密码，并将私密标记传入普通和网盘确认提交。**
- [ ] **Step 4: 重跑定向测试并确认通过。**

### Task 3: 前端确认、设置、锁与筛选

**Files:**
- Modify: `src/lib/types.ts`, `src/lib/api.ts`, `src/components/SettingsPanel.tsx`, `src/components/ParseConfirmDialog.tsx`, `src/App.tsx`, `src/styles.css`
- Test: `src/components/SettingsPanel.test.tsx`, `src/components/ParseConfirmDialog.test.tsx`, `src/App.test.tsx`, `src/lib/api.test.ts`

**Interfaces:**
- Consumes `isPrivate`, privacy settings, visibility API and `privateDownload` confirmation argument。
- Produces settings password flow、锁定/解锁提示、及“全部/仅隐私/仅普通”筛选。

- [ ] **Step 1: 写失败组件/API 测试**：开关显示密码设置、确认框显示私密下载按钮、解锁时筛选私密/普通任务且无“按类型排序”。
- [ ] **Step 2: 运行定向 Vitest 并确认失败。**
- [ ] **Step 3: 最小化实现类型、IPC 封装、设置控件、确认按钮和锁/筛选控件。**
- [ ] **Step 4: 重跑定向 Vitest 并确认通过。**

### Task 4: 集成验证

**Files:**
- Verify only.

- [ ] **Step 1: 运行本功能 Rust 定向测试。**
- [ ] **Step 2: 运行本功能前端 Vitest（排除历史工作树）。**
- [ ] **Step 3: 运行 `scripts/build-windows.ps1` 发布构建。**
- [ ] **Step 4: 审查差异，确认不包含按网盘/文件类型排序与明文密码。**
