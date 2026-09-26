# Aria2 Next、ED2K、流媒体与 FTP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace aria2 with Aria2 Next 2.7.5 and add ED2K, HLS, DASH, and resumable in-process FTP downloads.

**Architecture:** Aria2 Next remains the JSON-RPC transfer engine for HTTP, BT, ED2K and media segments. A focused Rust `MediaDownloadEngine` parses HLS/DASH manifests, selects a representation, submits ordered segments to Aria2 Next and emits one durable parent task. A Rust `FtpDownloadEngine` owns resumable FTP workers; `EngineRuntime` routes actions and aggregates engine statistics.

**Tech Stack:** Rust 2021, Tokio, suppaftp, Tauri 2, Aria2 Next 2.7.5 JSON-RPC, React/TypeScript, Vitest.

**Spec:** `docs/superpowers/specs/2026-09-12-aria2-next-ed2k-media-ftp-design.md`

## Global Constraints

- Work directly on the user-approved `master` checkout; do not create a temporary worktree.
- Preserve the existing uncommitted build-tool changes in `package.json`, `scripts/build-windows.ps1`, `scripts/build-tools.ps1`, `scripts/build-tools.tests.ps1`, and `src-tauri/tauri.conf.json`.
- Pin Aria2 Next to `2.7.5` and verify the official Windows x86_64 SHA-256 before replacing the bundled sidecar.
- Do not build an installer; publish only `artifacts/versadown.exe` with `scripts/build-windows.ps1`.
- Do not run full test suites unless targeted tests reveal cross-module risk.

---

### Task 1: Protocol classification and queue contracts

**Files:**
- Modify: `src-tauri/tests/parser_policy.rs`
- Modify: `src-tauri/src/parser.rs`
- Modify: `src-tauri/src/downloader.rs`
- Modify: `src/lib/types.ts`
- Modify: `src/lib/api.ts`
- Modify: `src/components/DownloadQueue.tsx`
- Modify: `src/components/DownloadQueue.test.tsx`

**Interfaces:**
- Produces: `CandidateKind::{Ed2k,Ftp,Hls,Dash}` and task kinds `ed2k | ftp | hls | dash`.
- Consumes: existing `prepare_download`, `extract_candidates`, and `QueueTask` contracts.

- [ ] Add failing parser tests using literal valid/invalid ED2K links plus FTP, `.m3u8`, and `.mpd` URLs; prove mixed text extraction preserves complete ED2K pipe syntax.
- [ ] Run `cargo test --manifest-path src-tauri/Cargo.toml --test parser_policy` and confirm the new cases fail because the variants are absent.
- [ ] Implement strict ED2K parsing and protocol classification, map task kinds, and keep ordinary direct links unchanged.
- [ ] Re-run the parser test and the focused downloader unit tests until green.
- [ ] Add failing `DownloadQueue` tests for readable ED2K/media/FTP summaries and icons, then implement the minimal TypeScript kinds/rendering and run the focused Vitest with `--exclude '.worktrees/**'`.

### Task 2: Replace and validate the sidecar

**Files:**
- Modify: `scripts/download-sidecars.ps1`
- Modify: `src-tauri/tauri.conf.json`
- Modify: `src-tauri/src/downloader.rs`
- Modify: `src-tauri/src/aria2/process.rs`
- Modify: `src-tauri/tests/aria2_sidecar.rs`
- Modify: `src-tauri/tests/aria2_process.rs`
- Modify: `src-tauri/tests/aria2_live.rs`
- Replace: `src-tauri/binaries/aria2c-x86_64-pc-windows-msvc.exe`
- Create: `THIRD_PARTY_NOTICES.md`

**Interfaces:**
- Produces: `aria2_next_binary_name() -> &'static str` and Aria2 Next launch arguments with `--state-dir`.
- Consumes: existing `Aria2LaunchConfig`, RPC secret, task/session storage.

- [ ] Change sidecar/process tests first to require the `aria2-next-x86_64-pc-windows-msvc.exe` resource, a version beginning with `2.7.5`, and a state directory argument; run them and observe failures.
- [ ] Update the download script to fetch the official 2.7.5 executable and checksum manifest, verify SHA-256, and atomically replace only the intended sidecar path.
- [ ] Rename resource discovery and Tauri packaging references, add `--state-dir`, and preserve loopback-only authenticated RPC.
- [ ] Run sidecar/process tests, then start the official binary and verify `aria2.getVersion`, ED2K `aria2.addUri`, removal, and local HTTP completion.
- [ ] Add accurate GPLv2 and suppaftp MIT/Apache notices.

### Task 3: One-time removal of incompatible legacy work

**Files:**
- Modify: `src-tauri/src/store.rs`
- Modify: `src-tauri/src/lib.rs`
- Create: `src-tauri/tests/aria2_next_migration.rs`

**Interfaces:**
- Produces: `TaskStore::purge_incompatible_engine_tasks(download_root: &Path) -> Result<LegacyPurgeReport, StoreError>`.
- Consumes: stored task/file rows, configured download directory, versioned migration marker.

- [ ] Add failing tests proving incomplete legacy tasks and their in-root payload/control files are removed, completed files remain, and out-of-root paths are never deleted.
- [ ] Run only `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_next_migration` and confirm failure for the missing purge API.
- [ ] Implement transactional record cleanup plus canonical path containment checks; write the marker only after successful cleanup.
- [ ] Re-run the migration test until green and add startup invocation before the new engine begins recovery.

### Task 4: Resumable in-process FTP engine

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Create: `src-tauri/src/ftp/mod.rs`
- Create: `src-tauri/src/ftp/url.rs`
- Create: `src-tauri/src/ftp/worker.rs`
- Create: `src-tauri/src/ftp/runtime.rs`
- Create: `src-tauri/tests/ftp_download.rs`
- Modify: `src-tauri/src/engine_runtime.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces: `FtpDownloadEngine::{submit,resubmit,control,recover,global_stat,shutdown}` using existing `Uuid`, `UriSubmissionOptions`, `DownloadAction`, `TaskStore`, and `EventSink` values.
- Consumes: `suppaftp` Tokio stream APIs and local output files.

- [ ] Build a test-only Tokio FTP fixture implementing USER/PASS, TYPE I, EPSV/PASV, SIZE, REST, RETR, ABOR and QUIT; add failing anonymous and authenticated download tests.
- [ ] Add `suppaftp` with only Tokio features and implement sanitized FTP URL parsing plus streamed download progress until the first tests pass.
- [ ] Add a failing test that interrupts a transfer, verifies the partial file remains, resumes, observes a non-zero REST offset, and validates the final SHA-256.
- [ ] Implement cooperative pause/cancel, REST resume, safe overwrite rules, retry, error mapping and restart recovery; re-run the focused FTP test.
- [ ] Route FTP submit/action/retry through `EngineRuntime`, aggregate FTP and Aria2 global speed, and preserve the existing Tauri event payloads.

### Task 5: Media orchestration, ED2K compatibility and final verification

**Files:**
- Create: `src-tauri/src/media.rs`
- Modify: `src-tauri/src/engine_runtime.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/tests/aria2_live.rs`

**Interfaces:**
- Consumes: all contracts produced by Tasks 1-4.
- Produces: a verified Windows application binary at `artifacts/versadown.exe`.

- [ ] Add a failing real-sidecar test proving direct manifest submission is insufficient: output must match the fixture media payload rather than the `.m3u8`/`.mpd` text.
- [ ] Implement the in-process HLS/DASH manifest orchestrator, ordered segment transfer/merge, pause/cancel/recovery and one-task progress projection.
- [ ] Run focused Rust tests: parser policy, media parsing/live transfer, downloader, aria2 process/sidecar/live contract, legacy migration, FTP download, engine runtime, and task commands.
- [ ] Run focused frontend tests for `NewDownloadDialog`, `ParseConfirmDialog`, `DownloadQueue`, and API mapping using `--exclude '.worktrees/**'`, then run `npm run build`.
- [ ] Run a local Aria2 Next smoke that accepts an ED2K file link and verifies local HLS/DASH fixture task submission without relying on public copyrighted content.
- [ ] Run `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows.ps1` and verify the published executable exists and starts its bundled Aria2 Next engine.
- [ ] Review `git diff` to ensure no unrelated user changes were overwritten and report the exact commands and results.
