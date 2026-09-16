# 全局带宽限速 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为所有下载和上传实现持久化、可动态调整的共享带宽限制。

**Architecture:** Aria2 的全局选项处理其托管传输；一个 Rust 带宽协调器为 FTP 与网盘 HTTP 流提供方向独立的共享配额，并在网盘上传活动期间将 P2P 上传降为近零。

**Tech Stack:** Tauri 2、Rust、Tokio、Reqwest blocking、React、Vitest。

**Spec:** `docs/superpowers/specs/2026-09-16-bandwidth-limits-design.md`

## Global Constraints

- `0` 表示不限制；P2P 上传默认 `1_048_576` B/s；临时 P2P 上限为 `1_024` B/s。
- 所有云上传终态都释放协调器注册。
- 前端 Vitest 必须带 `--exclude '.worktrees/**' --exclude '.pnpm-store/**'`。

---

### Task 1: 持久化设置与 Aria2 全局选项

**Files:**
- Modify: `src-tauri/src/models.rs`, `src-tauri/src/aria2/process.rs`, `src-tauri/src/aria2/{types,rpc}.rs`, `src-tauri/src/engine_runtime.rs`
- Test: `src-tauri/src/models.rs`, `src-tauri/src/aria2/process.rs`

- [ ] 先增加失败测试：缺失字段得到 `0/1 MiB` 默认值，负值或超上限被拒绝，启动参数包含两个全局限速选项。
- [ ] 实现 `Aria2Settings::{overall_download_limit,p2p_upload_limit}`、校验和启动参数；为 RPC 添加 `changeGlobalOption`。
- [ ] 运行 `cargo test models::tests aria2::process::tests --lib`。

### Task 2: 共享带宽与网盘上传协调

**Files:**
- Create: `src-tauri/src/bandwidth.rs`
- Modify: `src-tauri/src/lib.rs`, `src-tauri/src/engine_runtime.rs`, `src-tauri/src/ftp.rs`, `src-tauri/src/cloud/http.rs`, `src-tauri/src/cloud/commands/upload.rs`
- Test: `src-tauri/src/bandwidth.rs`, `src-tauri/src/cloud/commands/upload.rs`

- [ ] 写失败测试：共享上传注册在第二个任务结束前不恢复 P2P，最后一个注册释放后恢复最新用户值；限制为零不等待。
- [ ] 实现方向令牌桶和 `CloudUploadRegistration`；将 FTP/网盘 HTTP 的每个数据块接入对应令牌桶，并在 Aria2 可用时调用全局选项切换。
- [ ] 运行 `cargo test bandwidth::tests cloud::commands::upload::tests --lib`。

### Task 3: 网络设置界面与端到端契约

**Files:**
- Modify: `src/lib/types.ts`, `src/components/SettingsPanel.tsx`, `src/components/SettingsPanel.test.tsx`, `src/styles.css`

- [ ] 写失败测试：网络页显示三个限速输入，默认值正确，保存时把 MiB/s 转为完整设置 payload。
- [ ] 实现三个输入、单位/不限速说明与现有“保存 aria2 设置”提交路径。
- [ ] 运行 `node_modules/.bin/vitest.cmd run src/components/SettingsPanel.test.tsx --exclude '.worktrees/**' --exclude '.pnpm-store/**'`，再运行 `scripts/build-windows.ps1`。
