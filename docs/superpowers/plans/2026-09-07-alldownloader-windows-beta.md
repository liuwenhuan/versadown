# AllDownloader Windows Beta Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a runnable Windows-first Tauri desktop application that parses download inputs, manages persistent download tasks, supports local downloads and safe processing plans, and exposes first-beta Quark/PikPak account and file-management workflows.

**Architecture:** A React/TypeScript renderer presents the floating launcher, parse confirmation, download queue and cloud-manager UI. Tauri commands call a Rust core that owns SQLite persistence, deterministic parsing, policy validation, secure local configuration, aria2 RPC and connector contracts. Providers are isolated adapters; their real operations require user-authorized sessions, while a local mock implementation makes the entire app executable and testable without credentials.

**Tech Stack:** Tauri 2, Rust 2021, Tokio, SQLx/SQLite, Serde, Reqwest, React 18, TypeScript, Vite, Vitest, Zustand, aria2 JSON-RPC sidecar.

**Spec:** `docs/superpowers/specs/2026-09-07-alldownloader-windows-beta-design.md`

## Global Constraints

- Target Windows first with Tauri 2 and WebView2; no Electron runtime.
- Keep all task data, credentials and download activity local; never send cookies, tokens, passwords, QR/SMS material or local files to AI.
- Use deterministic rules first; complex text uses an explicitly enabled, schema-validated AI adapter only.
- Do not automate CAPTCHA, SMS, QR, DRM, real payments, password cracking or arbitrary page forms.
- Use only a whitelist for archive extraction, merge, rename and checksum processing; never execute downloaded programs, scripts or macros.
- Start direct single-file tasks automatically; require a confirmation tree for multi-file, torrent or directory results.
- Treat a directory as one parent row until expanded; keep child controls independent.
- Implement Quark/PikPak through declared connector capabilities and user authorization only; mock connector backs automated tests.
- Include the detailed Debug report behind `REPORT_DEBUG_TOOL`; it defaults off and may be removed at release build time.

---

## File Structure

- `package.json`, `vite.config.ts`, `tsconfig*.json`, `index.html`: frontend tooling and test scripts.
- `src/main.tsx`, `src/App.tsx`, `src/styles.css`: renderer bootstrap, navigation and visual system.
- `src/lib/api.ts`, `src/lib/types.ts`: typed Tauri command interface and shared UI models.
- `src/stores/taskStore.ts`: synchronized queue, selection and UI-only state.
- `src/components/*.tsx`: focused launcher, parser, queue, task details, cloud manager and settings widgets.
- `src/**/*.test.tsx`: Vitest coverage of UI state and visible behavior.
- `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `src-tauri/build.rs`: native package configuration and sidecar registration.
- `src-tauri/src/models.rs`: serializable domain types and state enums.
- `src-tauri/src/parser.rs`: deterministic classifier and candidate extraction.
- `src-tauri/src/policy.rs`: input/AI/processing validation and path safety.
- `src-tauri/src/store.rs`: SQLite schema and task persistence.
- `src-tauri/src/downloader.rs`: aria2 RPC adapter and direct local fallback.
- `src-tauri/src/connectors.rs`: provider trait, mock connector and account/directory operations.
- `src-tauri/src/processing.rs`: safe processing-plan validation/execution boundary.
- `src-tauri/src/commands.rs`, `src-tauri/src/lib.rs`: application state and Tauri command façade.
- `src-tauri/tests/*.rs`: native unit/integration tests.
- `scripts/download-sidecars.ps1`: repeatable aria2 sidecar fetch for Windows development/package builds.
- `README.md`: setup, safe-use boundaries, development and packaging instructions.

### Task 1: Bootstrap a testable Tauri 2 workspace

**Files:**
- Create: `package.json`, `vite.config.ts`, `tsconfig.json`, `tsconfig.node.json`, `index.html`
- Create: `src-tauri/Cargo.toml`, `src-tauri/build.rs`, `src-tauri/tauri.conf.json`, `src-tauri/src/lib.rs`, `src-tauri/src/main.rs`
- Create: `src/main.tsx`, `src/App.tsx`, `src/styles.css`
- Test: `src/App.test.tsx`, `src-tauri/tests/app_boot.rs`

**Interfaces:**
- Produces Tauri command `app_version() -> String` and React root `<App />`.

- [ ] **Step 1: Write the failing renderer and native smoke tests**

```tsx
// src/App.test.tsx
it('renders the AllDownloader shell', () => {
  render(<App />);
  expect(screen.getByRole('heading', { name: '下载管理' })).toBeVisible();
});
```

```rust
// src-tauri/tests/app_boot.rs
#[test]
fn application_name_is_stable() {
    assert_eq!(alldownloader_lib::app_version(), env!("CARGO_PKG_VERSION"));
}
```

- [ ] **Step 2: Run the tests and confirm they fail because the workspace is absent**

Run: `npm test -- --run src/App.test.tsx` and `cargo test --manifest-path src-tauri/Cargo.toml --test app_boot`  
Expected: both report missing project/configuration.

- [ ] **Step 3: Create the minimum Vite/Tauri bootstrap and app shell**

```rust
pub fn app_version() -> String { env!("CARGO_PKG_VERSION").to_string() }

#[tauri::command]
fn app_version_command() -> String { app_version() }
```

```tsx
export function App() {
  return <main><h1>下载管理</h1></main>;
}
```

- [ ] **Step 4: Run renderer and native smoke tests**

Run: `npm test -- --run src/App.test.tsx` and `cargo test --manifest-path src-tauri/Cargo.toml --test app_boot`  
Expected: PASS.

- [ ] **Step 5: Commit the workspace baseline**

```powershell
git add package.json vite.config.ts tsconfig*.json index.html src src-tauri
git commit -m "feat: bootstrap Tauri download manager"
```

### Task 2: Define domain types, SQLite persistence and task state transitions

**Files:**
- Create: `src-tauri/src/models.rs`, `src-tauri/src/store.rs`, `src-tauri/tests/store_transitions.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces `TaskStatus`, `DownloadTask`, `NewTask`, `TaskStore::open(path)`, `TaskStore::create(new_task)`, `TaskStore::transition(id, status)`, and `TaskStore::list()`.

- [ ] **Step 1: Write failing state and persistence tests**

```rust
#[test]
fn direct_task_can_move_from_parsing_to_downloading() {
    let store = test_store();
    let task = store.create(NewTask::direct("https://host/file.iso")).unwrap();
    assert_eq!(store.transition(task.id, TaskStatus::Downloading).unwrap().status, TaskStatus::Downloading);
}
```

- [ ] **Step 2: Run the test to verify state APIs are absent**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test store_transitions`  
Expected: FAIL with unresolved `TaskStore`/`TaskStatus`.

- [ ] **Step 3: Add serialized models, SQLite schema and legal transitions**

```rust
pub enum TaskStatus { Draft, Parsing, AwaitingUser, Queued, Downloading, Processing, Completed, Failed, Cancelled, Paused }
pub fn may_transition(from: TaskStatus, to: TaskStatus) -> bool { /* explicit state table */ }
```

- [ ] **Step 4: Run native persistence tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test store_transitions`  
Expected: PASS, including re-open/list round trip and invalid transition rejection.

- [ ] **Step 5: Commit persistence**

```powershell
git add src-tauri/src/models.rs src-tauri/src/store.rs src-tauri/tests/store_transitions.rs src-tauri/src/lib.rs
git commit -m "feat: persist download task state"
```

### Task 3: Implement deterministic parsing and safe policy checks

**Files:**
- Create: `src-tauri/src/parser.rs`, `src-tauri/src/policy.rs`, `src-tauri/tests/parser_policy.rs`
- Modify: `src-tauri/src/models.rs`, `src-tauri/src/lib.rs`

**Interfaces:**
- Produces `classify_input(input: &str) -> ParseResult`, `extract_candidates(text: &str) -> Vec<Candidate>`, `requires_confirmation(result: &ParseResult) -> bool`, `validate_processing(step: &ProcessStep) -> Result<(), PolicyError>`.

- [ ] **Step 1: Write failing parse/policy fixtures**

```rust
#[test]
fn recognizes_direct_http_magnet_and_thunder() {
    assert_eq!(classify_input("magnet:?xt=urn:btih:abc").kind, CandidateKind::Magnet);
    assert!(classify_input("https://example.com/a.zip").auto_start);
    assert_eq!(classify_input("thunder://QUFodHRwOi8vYS9iWlpa").kind, CandidateKind::Thunder);
}

#[test]
fn rejects_program_execution_processing() {
    assert!(validate_processing(&ProcessStep::execute("setup.exe")).is_err());
}
```

- [ ] **Step 2: Run parser tests and confirm they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test parser_policy`  
Expected: FAIL with unresolved parser/policy modules.

- [ ] **Step 3: Implement patterns and a whitelist-only policy**

```rust
pub enum AllowedAction { ExtractArchive, MergeParts, Rename, VerifyChecksum }
pub fn validate_processing(step: &ProcessStep) -> Result<(), PolicyError> {
    if step.action.is_safe() && !contains_parent_escape(&step.input) { Ok(()) } else { Err(PolicyError::Rejected) }
}
```

- [ ] **Step 4: Run parser/policy tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test parser_policy`  
Expected: PASS for recognized URLs, multiline extraction, confirmation rules and unsafe action/path rejection.

- [ ] **Step 5: Commit parser and policy**

```powershell
git add src-tauri/src/parser.rs src-tauri/src/policy.rs src-tauri/tests/parser_policy.rs src-tauri/src/models.rs src-tauri/src/lib.rs
git commit -m "feat: add deterministic parsing and execution policy"
```

### Task 4: Add aria2/direct download orchestration and Tauri commands

**Files:**
- Create: `src-tauri/src/downloader.rs`, `src-tauri/src/commands.rs`, `src-tauri/tests/downloader.rs`, `scripts/download-sidecars.ps1`
- Modify: `src-tauri/src/lib.rs`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`

**Interfaces:**
- Produces commands `parse_download`, `confirm_download`, `list_tasks`, `pause_task`, `resume_task`, `cancel_task`, `get_task_details`.
- `DownloadBackend` exposes `enqueue(candidate, directory)`, `tell_status(gid)`, `pause(gid)`, `unpause(gid)`, `remove(gid)`.

- [ ] **Step 1: Write failing backend and command tests**

```rust
#[tokio::test]
async fn single_http_result_queues_without_confirmation() {
    let result = parse_download("https://example.com/file.bin".into()).await.unwrap();
    assert_eq!(result.task.status, TaskStatus::Queued);
}
```

- [ ] **Step 2: Run tests and confirm native download façade is absent**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test downloader`  
Expected: FAIL with missing `parse_download`.

- [ ] **Step 3: Implement JSON-RPC aria2 adapter with explicit unavailable fallback**

```rust
async fn enqueue(&self, uri: String) -> Result<String, DownloadError> {
    self.rpc("aria2.addUri", json!([[uri], {"continue":"true"}])).await
}
```

The development fallback must create a visible `Failed` task with `ARIA2_UNAVAILABLE`, never silently claim a completed download.

- [ ] **Step 4: Run native orchestration tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test downloader`  
Expected: PASS using a local mock RPC endpoint.

- [ ] **Step 5: Download and register the Windows aria2 sidecar**

Run: `powershell -ExecutionPolicy Bypass -File scripts/download-sidecars.ps1`  
Expected: a checksum-verified `aria2c-x86_64-pc-windows-msvc.exe` is present under `src-tauri/binaries/`.

- [ ] **Step 6: Commit download service**

```powershell
git add src-tauri scripts/download-sidecars.ps1
git commit -m "feat: orchestrate aria2 download tasks"
```

### Task 5: Build the primary download experience and floating launcher

**Files:**
- Create: `src/lib/types.ts`, `src/lib/api.ts`, `src/stores/taskStore.ts`
- Create: `src/components/FloatingLauncher.tsx`, `src/components/ParseConfirmDialog.tsx`, `src/components/DownloadQueue.tsx`, `src/components/TaskDetails.tsx`
- Modify: `src/App.tsx`, `src/styles.css`
- Test: `src/components/FloatingLauncher.test.tsx`, `src/components/DownloadQueue.test.tsx`, `src/components/ParseConfirmDialog.test.tsx`

**Interfaces:**
- `api.parseDownload(input)`, `api.confirmDownload(id, selectedIds, directory)`, and `api.taskAction(id, action)` return typed native task data.
- `useTaskStore` exposes `tasks`, `selectedTaskId`, `refresh()`, `select(id)`.

- [ ] **Step 1: Write failing UI behavior tests**

```tsx
it('expands the 48px launcher and labels its action 开始解析下载', async () => {
  render(<FloatingLauncher onParse={vi.fn()} />);
  await userEvent.click(screen.getByLabelText('打开下载输入'));
  expect(screen.getByRole('button', { name: '开始解析下载' })).toBeVisible();
});
```

```tsx
it('keeps a folder as one row until the user expands it', async () => {
  render(<DownloadQueue tasks={[folderTask]} />);
  expect(screen.queryByText('episode-01.mkv')).not.toBeInTheDocument();
  await userEvent.click(screen.getByRole('button', { name: '展开 Season 1' }));
  expect(screen.getByText('episode-01.mkv')).toBeVisible();
});
```

- [ ] **Step 2: Run component tests and confirm they fail**

Run: `npm test -- --run src/components/FloatingLauncher.test.tsx src/components/DownloadQueue.test.tsx src/components/ParseConfirmDialog.test.tsx`  
Expected: FAIL because components do not exist.

- [ ] **Step 3: Implement responsive launcher, manager and multi-file dialog**

The launcher default is a 48×48 fixed round button. Its expanded panel is 470×300. The queue renders parent aggregate progress and child action buttons only after expansion; the confirmation dialog masks discovered passwords by default and explains planned processing in a collapsible section.

- [ ] **Step 4: Run component tests and build the web renderer**

Run: `npm test -- --run` and `npm run build`  
Expected: PASS and Vite emits `dist/`.

- [ ] **Step 5: Commit primary user experience**

```powershell
git add src
git commit -m "feat: add launcher and grouped download manager"
```

### Task 6: Add connector contracts, account state and self-managed cloud UI

**Files:**
- Create: `src-tauri/src/connectors.rs`, `src-tauri/tests/connectors.rs`
- Create: `src/components/CloudManager.tsx`, `src/components/AccountConnectDialog.tsx`, `src/components/CloudManager.test.tsx`
- Modify: `src-tauri/src/commands.rs`, `src-tauri/src/lib.rs`, `src/lib/api.ts`, `src/App.tsx`

**Interfaces:**
- `CloudConnector` declares `capabilities`, `check_session`, `list_directory`, `create_directory`, `rename`, `move_item`, `delete`, `upload`, `download`.
- Commands expose `list_accounts`, `start_connect`, `complete_connect`, `list_cloud_directory`, `cloud_action`.

- [ ] **Step 1: Write failing mock-connector and cloud UI tests**

```rust
#[tokio::test]
async fn mock_quark_account_lists_root_after_connection() {
    let connector = MockConnector::quark_connected();
    assert!(!connector.list_directory("/").await.unwrap().items.is_empty());
}
```

```tsx
it('shows an app-managed folder list rather than a provider website', () => {
  render(<CloudManager provider="quark" />);
  expect(screen.getByRole('tree', { name: '夸克网盘文件' })).toBeVisible();
});
```

- [ ] **Step 2: Run connector/UI tests and confirm they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test connectors` and `npm test -- --run src/components/CloudManager.test.tsx`  
Expected: FAIL with missing connector and component.

- [ ] **Step 3: Implement capability-gated mock adapters and visible authorization state**

The first renderer must allow account connection, expired-session display and mock directories for Quark/PikPak. A real provider adapter may only be selected after a user completes its supported auth flow; all unsupported operations return a typed `ConnectorUnavailable` error.

- [ ] **Step 4: Run connector/UI tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test connectors` and `npm test -- --run src/components/CloudManager.test.tsx`  
Expected: PASS for basic file operations, expired sessions and missing capabilities.

- [ ] **Step 5: Commit cloud management**

```powershell
git add src src-tauri
git commit -m "feat: add capability-gated cloud file management"
```

### Task 7: Add processing plans, privacy settings and optional diagnostics

**Files:**
- Create: `src-tauri/src/processing.rs`, `src-tauri/tests/processing.rs`
- Create: `src/components/SettingsPanel.tsx`, `src/components/DebugReport.tsx`, `src/components/SettingsPanel.test.tsx`
- Modify: `src-tauri/src/commands.rs`, `src-tauri/src/lib.rs`, `src/App.tsx`, `src/styles.css`

**Interfaces:**
- `plan_processing(resources, hints) -> Vec<ProcessStep>` produces only permitted actions.
- Commands expose `get_settings`, `update_settings`, `get_debug_report`, `rerun_step`.

- [ ] **Step 1: Write failing whitelist and settings tests**

```rust
#[test]
fn password_from_page_can_be_attached_without_being_serialized_in_trace() {
    let plan = plan_processing(&archives(), &hints_with_password("abc"));
    assert!(plan[0].password_reference.is_some());
    assert!(!format!("{plan:?}").contains("abc"));
}
```

```tsx
it('keeps detailed report collection off by default', () => {
  render(<SettingsPanel initial={{ debugReports: false }} />);
  expect(screen.getByRole('checkbox', { name: '启用调试报告' })).not.toBeChecked();
});
```

- [ ] **Step 2: Run tests and confirm they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test processing` and `npm test -- --run src/components/SettingsPanel.test.tsx`  
Expected: FAIL with absent processing/settings modules.

- [ ] **Step 3: Implement safe plan serialization, config persistence and compile-time report gate**

```rust
#[cfg(feature = "report-debug-tool")]
pub fn debug_trace(task_id: TaskId) -> Result<DebugTrace, StoreError> { /* redacted trace */ }
```

When the feature is disabled, `get_debug_report` returns `DebugFeatureDisabled`; no detailed event record is written.

- [ ] **Step 4: Run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test processing` and `npm test -- --run src/components/SettingsPanel.test.tsx`  
Expected: PASS for safe plans, redaction, default-off settings and disabled report behavior.

- [ ] **Step 5: Commit processing and diagnostics**

```powershell
git add src src-tauri
git commit -m "feat: add safe processing plans and optional diagnostics"
```

### Task 8: Verify development launch, package Windows artifact and document use

**Files:**
- Create: `README.md`
- Modify: `package.json`, `src-tauri/tauri.conf.json`, `.gitignore`
- Test: `scripts/smoke-download.ps1`

**Interfaces:**
- Produces `npm run dev`, `npm run test`, `npm run build`, `npm run tauri:dev`, `npm run tauri:build` and a Windows installer/executable in `src-tauri/target/release/bundle/`.

- [ ] **Step 1: Write a smoke script that rejects missing launcher output**

```powershell
$artifact = Get-ChildItem 'src-tauri/target/release/bundle' -Recurse -Include *.msi,*.exe | Select-Object -First 1
if (-not $artifact) { throw 'Windows package artifact was not produced.' }
```

- [ ] **Step 2: Run it before packaging**

Run: `powershell -ExecutionPolicy Bypass -File scripts/smoke-download.ps1`  
Expected: FAIL because no bundle exists.

- [ ] **Step 3: Document setup and build the actual application**

`README.md` must show the required Windows prerequisites, sidecar setup, development command, test command, build command, data location, secure-use boundaries, what requires user interaction, and the difference between mock and user-authorized connectors.

- [ ] **Step 4: Run full verification**

Run: `npm test -- --run`, `cargo test --manifest-path src-tauri/Cargo.toml`, `npm run build`, `npm run tauri:build`, and `powershell -ExecutionPolicy Bypass -File scripts/smoke-download.ps1`  
Expected: all tests PASS and a Windows package artifact exists.

- [ ] **Step 5: Commit delivery materials**

```powershell
git add README.md scripts/smoke-download.ps1 package.json src-tauri/tauri.conf.json .gitignore
git commit -m "docs: document and package Windows beta"
```

## Self-Review

- Spec coverage: Tasks 1–2 cover runtime, state, persistence and recovery; Task 3 covers rule-first parsing, AI boundary model and policy; Tasks 4–5 cover local downloads, confirmation and UI; Task 6 covers Quark/PikPak contracts and custom file manager; Task 7 covers safe processing and optional diagnostics; Task 8 covers Windows verification and documentation.
- No user-account sharing, CAPTCHA/DRM/payment bypass, password cracking or executable post-processing is introduced.
- Later task names use types produced by Tasks 1–3 (`TaskStatus`, `ParseResult`, `ProcessStep`, `CloudConnector`) consistently.
