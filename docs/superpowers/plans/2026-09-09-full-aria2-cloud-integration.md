# Full aria2, Cloud, and Duplicate-Download Integration Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Integrate cloud-drive downloads and duplicate-download resolution into `master` while preserving the merged full aria2 runtime as the sole download engine.

**Architecture:** `aria2/service.rs` remains the only boundary that submits and controls transfers. Cloud code resolves a stable cloud task source to short-lived aria2 URI options, and `commands.rs` converts cloud/duplicate outcomes to Tauri payloads. The frontend receives the same task payload shape for direct and cloud downloads, with a dedicated duplicate-resolution dialog.

**Tech Stack:** Tauri 2, Rust, Tokio, Reqwest, SQLite/Rusqlite, React 18, TypeScript, Vitest, aria2 JSON-RPC.

**Spec:** `docs/superpowers/specs/2026-09-09-full-aria2-cloud-integration-design.md`

## Global Constraints

- Preserve `src-tauri/src/aria2/`, `EngineRuntime`, recovery, websocket synchronization, and window lifecycle; do not compile or start legacy `aria2_rpc.rs`.
- Persist only stable `cloud://provider/file-id` sources; signed URLs, request cookies, and authorization headers are transient.
- Do not build an installer. Final binary verification uses `tauri build --no-bundle`.
- Do not stage unrelated untracked artifacts or pre-existing files.

---

### Task 1: Establish the cloud module without a second engine

**Files:**
- Create: `src-tauri/src/cloud/mod.rs`, `src-tauri/src/cloud/commands.rs`, `src-tauri/src/cloud/credentials.rs`, `src-tauri/src/cloud/http.rs`, `src-tauri/src/cloud/pikpak.rs`, `src-tauri/src/cloud/quark.rs`, `src-tauri/src/cloud/types.rs`, `src-tauri/src/cloud/upload.rs`
- Modify: `src-tauri/src/lib.rs`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`, `src-tauri/capabilities/default.json`, `package.json`, `package-lock.json`
- Test: `src-tauri/tests/cloud_quark.rs`, `src-tauri/tests/cloud_sessions.rs`, `src-tauri/tests/cloud_upload.rs`

**Interfaces:**
- Consumes: `CloudProvider`, `CloudAccount`, and `QuarkError` from `cloud/types.rs`; `CloudState::with_provider` from `cloud/commands.rs`.
- Produces: a compiled `pub mod cloud` with no dependency on `Aria2Engine` or `aria2_rpc.rs`.

- [ ] **Step 1: Write a failing compile-facing cloud test**

```rust
#[test]
fn cloud_module_does_not_expose_or_require_the_legacy_aria2_engine() {
    let _provider = versadown::cloud::CloudProvider::Quark;
    assert_eq!(_provider.key(), "quark");
}
```

- [ ] **Step 2: Run the test to verify it fails because `cloud` is absent**

Run: `cargo test --test cloud_sessions cloud_module_does_not_expose_or_require_the_legacy_aria2_engine`

Expected: compilation error stating that `versadown::cloud` is unavailable.

- [ ] **Step 3: Add only the cloud provider, credential, HTTP, Quark, PikPak, upload, and command modules**

```rust
// src-tauri/src/lib.rs
pub mod aria2;
pub mod cloud;
// Do not add: mod aria2_rpc;
```

Keep the existing `EngineRuntime` initialization and register cloud state independently during Tauri setup.

- [ ] **Step 4: Run the three cloud-module test targets**

Run: `cargo test --test cloud_quark --test cloud_sessions --test cloud_upload`

Expected: PASS; no test starts a second aria2 process.

- [ ] **Step 5: Commit only the cloud module and declared dependency files**

```bash
git add src-tauri/src/cloud src-tauri/src/lib.rs src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/capabilities/default.json package.json package-lock.json
git commit -m "feat: add cloud provider module"
```

### Task 2: Add typed URI submission options to the full aria2 service

**Files:**
- Modify: `src-tauri/src/aria2/service.rs`, `src-tauri/src/aria2/rpc.rs`, `src-tauri/src/commands.rs`
- Test: `src-tauri/tests/cloud_coordination.rs`, `src-tauri/tests/aria2_rpc_contract.rs`

**Interfaces:**
- Consumes: `DownloadService::submit_uri(task_id, uris, options)` and `Aria2Api::add_uri(uris, options)`.
- Produces: `UriSubmissionOptions { directory, out, headers, proxy, allow_overwrite }` converted at the service boundary to aria2 RPC options.

- [ ] **Step 1: Write a failing service-level test for transient cloud options**

```rust
#[tokio::test]
async fn cloud_submission_forwards_cookie_referer_and_output_name_without_persisting_them() {
    let task = store.create(NewTask::direct("cloud://quark/file-1")).unwrap();
    service.submit_uri(task.id, vec!["https://signed.example/file".into()], cloud_options()).await.unwrap();
    assert_eq!(fake.added_options()["header"], "Cookie: sid=web\nReferer: https://pan.quark.cn/");
    assert_eq!(store.find(task.id).unwrap().source, "cloud://quark/file-1");
}
```

- [ ] **Step 2: Run the test and verify it fails because typed cloud options are unavailable**

Run: `cargo test --test cloud_coordination cloud_submission_forwards_cookie_referer_and_output_name_without_persisting_them`

Expected: compile failure for `cloud_options`/typed submission options.

- [ ] **Step 3: Implement the minimal typed option conversion at the service boundary**

```rust
pub struct UriSubmissionOptions {
    pub directory: Option<PathBuf>,
    pub output_name: Option<String>,
    pub headers: Vec<(String, String)>,
    pub proxy: Option<String>,
    pub allow_overwrite: bool,
}
```

Convert headers to aria2's newline-separated `header` option, validate output names, and set only explicit options.

- [ ] **Step 4: Run the cloud coordination and RPC contract tests**

Run: `cargo test --test cloud_coordination --test aria2_rpc_contract`

Expected: PASS; existing direct/torrent RPC contracts remain unchanged.

- [ ] **Step 5: Commit the service option boundary**

```bash
git add src-tauri/src/aria2/service.rs src-tauri/src/aria2/rpc.rs src-tauri/src/commands.rs src-tauri/tests/cloud_coordination.rs src-tauri/tests/aria2_rpc_contract.rs
git commit -m "feat: submit cloud URIs through aria2 service"
```

### Task 3: Adapt cloud download and retry commands to `EngineRuntime`

**Files:**
- Modify: `src-tauri/src/cloud/commands.rs`, `src-tauri/src/commands.rs`, `src-tauri/src/lib.rs`, `src-tauri/src/models.rs`, `src-tauri/src/store.rs`
- Test: `src-tauri/tests/cloud_coordination.rs`, `src-tauri/tests/cloud_quark.rs`, `src-tauri/tests/cloud_sessions.rs`

**Interfaces:**
- Consumes: `CloudState::with_provider`, `resolve_download(provider, file_id, credentials, transport)`, and `EngineRuntime::service()`.
- Produces: `cloud_download_url` that persists a stable cloud source, submits resolved URI options, and returns `DownloadTaskPayload`; one bounded refresh on an expired/rejected cloud link.

- [ ] **Step 1: Write failing tests for Quark session forwarding and one refresh**

```rust
#[tokio::test]
async fn quark_download_merges_download_response_cookie_before_service_submission() {
    let resolved = resolve_fixture_with_set_cookie("__puus=fresh");
    let options = cloud_uri_options(&resolved).unwrap();
    assert!(options.headers.iter().any(|(_, value)| value.contains("sid=web; __puus=fresh")));
}

#[tokio::test]
async fn expired_cloud_uri_is_resolved_once_before_task_is_failed() {
    let outcome = retry_cloud_task_with_first_submission_failure().await;
    assert_eq!(outcome.resolve_attempts, 2);
    assert_ne!(outcome.task.status, TaskStatus::Failed);
}
```

- [ ] **Step 2: Run the tests and verify they fail for the missing runtime adapter**

Run: `cargo test --test cloud_coordination quark_download_merges_download_response_cookie_before_service_submission --test cloud_coordination expired_cloud_uri_is_resolved_once_before_task_is_failed`

Expected: failure because `cloud_download_url` still targets the legacy engine or does not refresh.

- [ ] **Step 3: Implement the cloud-to-runtime adapter**

```rust
let service = state.runtime.service().await.map_err(|error| error.to_string())?;
let resolved = resolve_download(provider, &file_id, credentials, &transport)?;
service.submit_uri(task.id, vec![resolved.url], resolved.into_uri_options()).await
```

Keep the stable source in SQLite and re-resolve at most once for a retryable cloud failure.

- [ ] **Step 4: Run cloud command regression tests**

Run: `cargo test --test cloud_coordination --test cloud_quark --test cloud_sessions`

Expected: PASS with no secret or signed URL in stored task data.

- [ ] **Step 5: Commit cloud command adaptation**

```bash
git add src-tauri/src/cloud/commands.rs src-tauri/src/commands.rs src-tauri/src/lib.rs src-tauri/src/models.rs src-tauri/src/store.rs src-tauri/tests/cloud_coordination.rs src-tauri/tests/cloud_quark.rs src-tauri/tests/cloud_sessions.rs
git commit -m "feat: route cloud downloads through runtime service"
```

### Task 4: Port duplicate preflight and explicit resolution actions

**Files:**
- Modify: `src-tauri/src/commands.rs`, `src-tauri/src/models.rs`, `src-tauri/src/store.rs`, `src-tauri/src/aria2/service.rs`
- Test: `src-tauri/tests/downloader.rs`, `src-tauri/tests/store_transitions.rs`, `src-tauri/tests/aria2_recovery.rs`

**Interfaces:**
- Consumes: persisted `TransferFile` records from `TaskStore::files`, `DownloadService::control`, and URI submission options.
- Produces: `TaskActionOutcome { task, duplicate }`, `resolve_duplicate_download`, `open_task_directory`, and `remove_task_record` commands.

- [ ] **Step 1: Write failing tests for failed-task retry safety**

```rust
#[tokio::test]
async fn retry_returns_overwrite_required_without_submitting_when_payload_has_no_control_file() {
    let task = failed_task_with_existing_payload_without_aria2_control_file(&store);
    let result = task_action_for_retry(task.id, &state).await.unwrap();
    assert_eq!(result.duplicate, Some(DuplicateDownloadState::OverwriteRequired));
    assert!(fake.added_uris().is_empty());
}

#[test]
fn completed_task_with_missing_payload_requires_restart_missing_confirmation() {
    assert_eq!(duplicate_state(&completed_task_without_files()), DuplicateDownloadState::CompletedFilesMissing);
}
```

- [ ] **Step 2: Run the focused tests and verify the retry path currently submits aria2 or lacks a duplicate result**

Run: `cargo test --test downloader retry_returns_overwrite_required_without_submitting_when_payload_has_no_control_file --test store_transitions completed_task_with_missing_payload_requires_restart_missing_confirmation`

Expected: failure showing no duplicate preflight response.

- [ ] **Step 3: Implement duplicate state and resolution actions against the full service**

```rust
match duplicate_state(&task) {
    DuplicateDownloadState::ResumeAvailable => service.submit_uri(task.id, uris, resume_options).await,
    DuplicateDownloadState::OverwriteRequired => return Ok(TaskActionOutcome::duplicate(task, state)),
    DuplicateDownloadState::CompletedFilesMissing => return Ok(TaskActionOutcome::duplicate(task, state)),
    _ => { /* return current task payload */ }
}
```

Only `resolve_duplicate_download(..., "overwrite")` may set overwrite options. Opening a directory validates the resolved path before calling the platform opener.

- [ ] **Step 4: Run duplicate, persistence, and recovery tests**

Run: `cargo test --test downloader --test store_transitions --test aria2_recovery`

Expected: PASS; retry never lets aria2 truncate an existing payload without confirmation.

- [ ] **Step 5: Commit duplicate resolution commands**

```bash
git add src-tauri/src/commands.rs src-tauri/src/models.rs src-tauri/src/store.rs src-tauri/src/aria2/service.rs src-tauri/tests/downloader.rs src-tauri/tests/store_transitions.rs src-tauri/tests/aria2_recovery.rs
git commit -m "feat: protect duplicate downloads before retry"
```

### Task 5: Integrate cloud workspace and duplicate UI contracts

**Files:**
- Create: `src/components/CloudWorkspace.tsx`, `src/components/DuplicateDownloadDialog.tsx`, `src/components/NewDownloadDialog.tsx`, `src/components/RemoveTaskDialog.tsx`, `src/components/TaskDetailsDialog.tsx`, `src/lib/cloudApi.ts`, `src/lib/cloudTypes.ts`, `src/lib/taskPolling.ts`, `src/lib/theme.ts`
- Modify: `src/App.tsx`, `src/lib/api.ts`, `src/lib/types.ts`, `src/components/DownloadQueue.tsx`, `src/components/DownloadQueue.test.tsx`, `src/styles.css`
- Test: `src/components/CloudWorkspace.test.tsx`, `src/components/DuplicateDownloadDialog.test.tsx`, `src/components/NewDownloadDialog.test.tsx`, `src/components/RemoveTaskDialog.test.tsx`, `src/components/TaskDetailsDialog.test.tsx`, `src/lib/taskPolling.test.ts`, `src/lib/theme.test.ts`, `src/App.test.tsx`

**Interfaces:**
- Consumes: `cloudApi` Tauri calls and `runTaskAction(): Promise<{ task?: QueueTask; duplicate?: DuplicateDownloadState }>`.
- Produces: a cloud workspace that creates standard queue tasks and a duplicate dialog that explicitly invokes `resume`, `restart_missing`, or `overwrite` resolutions.

- [ ] **Step 1: Write a failing UI test for a duplicate retry response**

```tsx
it('shows overwrite confirmation instead of invoking aria2 retry when task action returns overwrite_required', async () => {
  mockedRunTaskAction.mockResolvedValue({ duplicate: 'overwrite_required' });
  await user.click(screen.getByRole('button', { name: '重试' }));
  expect(await screen.findByText('无法安全续传。继续会覆盖之前的下载内容。')).toBeVisible();
});
```

- [ ] **Step 2: Run the test and verify it fails because the existing action API returns only a task**

Run: `vitest run --dir src src/App.test.tsx`

Expected: failure that the duplicate dialog copy is not rendered.

- [ ] **Step 3: Implement typed API outcomes and dialog routing**

```ts
type TaskActionResult = { task?: QueueTask; duplicate?: DuplicateDownloadState };

const result = await runTaskAction(id, 'retry');
if (result.duplicate) setDuplicate({ task, state: result.duplicate });
```

Wire cloud file selection into `downloadCloudFile` and update queue state only from returned task payloads.

- [ ] **Step 4: Run all frontend integration tests**

Run: `vitest run --dir src`

Expected: PASS, including cloud workspace and duplicate dialog tests.

- [ ] **Step 5: Commit the frontend integration**

```bash
git add src/App.tsx src/lib src/components src/styles.css
git commit -m "feat: add cloud workspace and duplicate resolution UI"
```

### Task 6: Verify the integrated desktop application

**Files:**
- Modify only if required by a failing test: the smallest file identified by the failed assertion.
- Test: Rust and TypeScript suites plus the no-bundle Tauri build.

**Interfaces:**
- Consumes: the completed cloud, duplicate, and aria2 service contracts.
- Produces: a verified `target/release/versadown.exe` and no installer package.

- [ ] **Step 1: Run format and static checks**

Run: `cargo fmt --check`, `tsc -b`, and `vite build`.

Expected: exit code 0.

- [ ] **Step 2: Run the full suites**

Run: `cargo test` from `src-tauri` and `vitest run --dir src` from the repository root.

Expected: all test result lines report `0 failed`.

- [ ] **Step 3: Build the runnable binary without an installer**

Run: `tauri build --no-bundle`.

Expected: exit code 0 and `src-tauri/target/release/versadown.exe` exists; no `.msi`, `.exe` installer, or bundle artifact is requested.

- [ ] **Step 4: Inspect the final change set and commit only integration files**

```bash
git diff --check
git status --short
git add <verified integration files>
git commit -m "fix: integrate cloud downloads with full aria2 runtime"
```
