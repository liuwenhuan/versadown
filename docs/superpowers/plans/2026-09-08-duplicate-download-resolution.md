# Duplicate Download Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve repeated downloads by safely resuming prior tasks, opening verified completed content, or obtaining explicit consent before overwrite.

**Architecture:** Add stable content keys and aria2 file manifests to `TaskStore`, then make `parse_download` return a tagged duplicate outcome before it creates a new task. A small Tauri confirmation command applies the selected decision; React renders a dedicated modal from that outcome.

**Tech Stack:** Rust, rusqlite, Tauri 2, React 18, TypeScript, Vitest.

**Spec:** `docs/superpowers/specs/2026-09-08-duplicate-download-resolution-design.md`

## Global Constraints

- Do not delete user files.
- Pass `allow-overwrite=true` only after `overwrite` is confirmed.
- Preserve the user-selected download directory for normal and restarted tasks.
- Keep this dirty shared worktree intact; do not commit unrelated changes.

---

### Task 1: Persist task identity and file manifest

**Files:**
- Modify: `src-tauri/src/models.rs`, `src-tauri/src/store.rs`, `src-tauri/src/commands.rs`
- Test: `src-tauri/src/store.rs`, `src-tauri/src/commands.rs`

**Interfaces:**
- Produces: `DownloadTask.content_key: String` and `DownloadTask.content_files: Vec<StoredDownloadFile>`.
- Produces: `TaskStore::find_by_content_key(&str)` and `TaskStore::store_content_files(Uuid, &[StoredDownloadFile])`.

- [x] Write tests that store a normalized magnet key, find its newest task, and preserve an absolute file manifest through `find`.
- [x] Run the focused Rust tests and confirm the new assertions fail because the columns and lookup do not exist.
- [x] Add SQLite migrations, row mapping, and manifest persistence; refresh the manifest from aria2 payload status.
- [x] Run the focused Rust tests and confirm they pass.

### Task 2: Decide and execute duplicate behavior

**Files:**
- Modify: `src-tauri/src/downloader.rs`, `src-tauri/src/aria2_rpc.rs`, `src-tauri/src/commands.rs`, `src-tauri/src/lib.rs`
- Test: `src-tauri/src/commands.rs`, `src-tauri/tests/aria2_rpc.rs`

**Interfaces:**
- Produces: `DuplicateDownloadState::{ExistingActive, ResumeAvailable, CompletedFilesPresent, CompletedFilesMissing, OverwriteRequired}`.
- Produces: `resolve_duplicate_download(task_id, action)` where action is `resume`, `restart_missing`, or `overwrite`.

- [x] Write tests for every manifest state and for overwrite being absent from a normal aria2 RPC request.
- [x] Run the focused Rust tests and confirm they fail because duplicate outcomes and action routing do not exist.
- [x] Add content-key normalization, manifest validation, decision routing, resume of the existing task, and per-request `allow-overwrite` support.
- [x] Run focused Rust tests and confirm they pass.

### Task 3: Render duplicate-resolution dialogs

**Files:**
- Create: `src/components/DuplicateDownloadDialog.tsx`, `src/components/DuplicateDownloadDialog.test.tsx`
- Modify: `src/lib/types.ts`, `src/lib/api.ts`, `src/App.tsx`, `src/styles.css`

**Interfaces:**
- Consumes: the duplicate outcome returned by `parse_download` and `resolveDuplicateDownload`.
- Produces: accessible dialogs with `打开下载目录`, `重新下载`, and `覆盖并重新下载` actions.

- [x] Write component tests for completed content, missing content, and overwrite confirmation.
- [x] Run the focused Vitest file and confirm it fails because the dialog does not exist.
- [x] Add the typed API boundary, modal, App state integration, and styles.
- [x] Run focused tests and confirm they pass.

### Task 4: Remove the temporary BT directory workaround and verify

**Files:**
- Modify: `src-tauri/src/commands.rs`
- Test: `src-tauri/src/commands.rs`

- [x] Replace the task-ID BT destination override with the approved duplicate-resolution flow.
- [x] Run the regression tests proving resumed tasks retain their original directory and new tasks use the configured directory.
- [x] Run `cargo test --manifest-path src-tauri/Cargo.toml`, the frontend Vitest suite, the production frontend build, and `scripts/build-windows.ps1`.
