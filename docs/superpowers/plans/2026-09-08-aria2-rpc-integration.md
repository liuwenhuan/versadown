# Aria2 RPC Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace per-download aria2 processes and optimistic UI state with one secure aria2 1.37 RPC engine that owns transfer state, controls, recovery, and shutdown.

**Architecture:** `Aria2Process` owns the sidecar and ephemeral loopback credentials; `Aria2RpcClient` is a typed JSON-RPC client; `DownloadService` maps application task IDs to aria2 GIDs and runs reconciliation. SQLite stores stable task/GID/file state, while Tauri events deliver snapshots to manager and launcher.

**Tech Stack:** Rust, Tauri async runtime, aria2c 1.37.0, HTTP JSON-RPC 2.0, WebSocket notifications, SQLite/rusqlite, React/TypeScript, Vitest, loopback fake RPC/HTTP servers.

**Spec:** `docs/superpowers/specs/2026-09-08-native-titlebar-aria2-rpc-design.md`

## Global Constraints

- RPC binds only to `127.0.0.1` with `--rpc-listen-all=false` and `--rpc-allow-origin-all=false`.
- Generate a 256-bit secret for every app run; never persist or log it. Protected requests prepend `token:<secret>`.
- Parse aria2 numeric strings with checked conversion; malformed values are typed errors, never zero.
- UI transfer state is never optimistic; it changes only after successful RPC and reconciliation.
- Migrate existing SQLite databases in place without deleting tasks.
- Hiding the manager does not stop aria2. Launcher “退出” saves the session and stops aria2; unfinished tasks resume next launch.
- Integration tests use loopback fixtures and hidden windows only, never public trackers or downloads.
- RPC reference: `https://aria2.github.io/manual/en/html/aria2c.html#rpc-interface`.

---

### Task 1: Define typed aria2 wire and domain models

**Files:**
- Create: `src-tauri/src/aria2/mod.rs`
- Create: `src-tauri/src/aria2/types.rs`
- Create: `src-tauri/tests/aria2_types.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces `Aria2Status`, `Aria2File`, `Aria2GlobalStat`, `Aria2Error`, `RpcSecret`, and `TransferSnapshot`.
- Produces `map_aria2_status(&str) -> Result<TaskStatus, Aria2Error>` and `TryFrom<Aria2Status> for TransferSnapshot`.

- [ ] **Step 1: Write failing strict-parsing tests**

```rust
#[test]
fn active_status_parses_string_lengths() {
    let raw = json!({"gid":"abc","status":"active","totalLength":"1000",
        "completedLength":"250","downloadSpeed":"50","uploadSpeed":"0",
        "connections":"2","files":[]});
    let snapshot = TransferSnapshot::try_from(
        serde_json::from_value::<Aria2Status>(raw).unwrap()
    ).unwrap();
    assert_eq!(snapshot.progress_percent, Some(25));
    assert_eq!(snapshot.eta_seconds, Some(15));
    assert_eq!(snapshot.status, TaskStatus::Downloading);
}

#[test]
fn malformed_completed_length_is_an_error() {
    let raw = json!({"gid":"abc","status":"active","totalLength":"1000",
        "completedLength":"not-a-number","downloadSpeed":"50","uploadSpeed":"0",
        "connections":"2","files":[]});
    let status = serde_json::from_value::<Aria2Status>(raw).unwrap();
    assert!(TransferSnapshot::try_from(status).is_err());
}
```

- [ ] **Step 2: Run and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_types`

Expected: aria2 module/types are missing.

- [ ] **Step 3: Implement serde wire types and checked conversion**

```rust
fn parse_u64(field: &'static str, value: &str) -> Result<u64, Aria2Error> {
    value.parse().map_err(|_| Aria2Error::InvalidField { field, value: value.into() })
}
```

Map `waiting/active/paused/complete/error/removed` exactly. Compute ETA only when total and positive speed are known.

- [ ] **Step 4: Run focused test and commit**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_types`

```powershell
git add src-tauri/src/aria2 src-tauri/src/lib.rs src-tauri/tests/aria2_types.rs
git commit -m "feat: model aria2 transfer state"
```

### Task 2: Implement authenticated HTTP JSON-RPC transport

**Files:**
- Create: `src-tauri/src/aria2/rpc.rs`
- Create: `src-tauri/tests/aria2_rpc_contract.rs`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/Cargo.lock`

**Interfaces:**
- Produces async trait `Aria2Api` with `add_uri`, `add_torrent`, `tell_status`, `tell_active`, `tell_waiting`, `tell_stopped`, `get_global_stat`, `multicall`, `pause`, `unpause`, `remove`, `force_remove`, `change_option`, `get_option`, `save_session`, and `shutdown`.
- Produces `Aria2RpcClient::new(endpoint, secret)`.

- [ ] **Step 1: Write a failing fake-server contract test**

```rust
#[tokio::test]
async fn protected_calls_prepend_secret() {
    let server = RpcFixture::reply(json!({"jsonrpc":"2.0","id":"1","result":"gid-1"}));
    let secret = RpcSecret::from_bytes([0xab; 32]);
    let expected_authorization = secret.authorization_param();
    let client = Aria2RpcClient::new(server.url(), secret);
    assert_eq!(client.add_uri(vec![server.file_url()], BTreeMap::new()).await.unwrap(), "gid-1");
    assert_eq!(server.last_params()[0], expected_authorization);
}
```

Add JSON-RPC error, HTTP failure, mismatched id, timeout, and malformed-result tests.

- [ ] **Step 2: Run and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_rpc_contract`

Expected: RPC client and fixture are missing.

- [ ] **Step 3: Add dependencies and one generic call path**

Add `reqwest` with JSON/rustls, `async-trait`, `url`, and direct `tokio` features; use `axum` under dev-dependencies for the loopback fixture. `RpcSecret([u8; 32])` exposes only `authorization_param()` and implements a redacted `Debug`.

```rust
async fn call<T: DeserializeOwned>(&self, method: &str, mut params: Vec<Value>) -> Result<T, Aria2Error> {
    params.insert(0, Value::String(self.secret.authorization_param()));
    let id = self.ids.fetch_add(1, Ordering::Relaxed).to_string();
    let request = RpcRequest { jsonrpc: "2.0", id: id.clone(), method, params };
    let envelope: RpcResponse<T> = self.client.post(self.endpoint.clone())
        .json(&request).send().await.map_err(Aria2Error::Transport)?
        .error_for_status().map_err(Aria2Error::Transport)?
        .json().await.map_err(Aria2Error::Transport)?;
    if envelope.id != id { return Err(Aria2Error::MismatchedResponseId); }
    match (envelope.result, envelope.error) {
        (Some(result), None) => Ok(result),
        (None, Some(error)) => Err(Aria2Error::Rpc(error)),
        _ => Err(Aria2Error::MalformedResponse),
    }
}
```

- [ ] **Step 4: Implement typed wrappers, run tests, and commit**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_rpc_contract`

```powershell
git add src-tauri/src/aria2/rpc.rs src-tauri/tests/aria2_rpc_contract.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat: add authenticated aria2 rpc client"
```

### Task 3: Manage one secure aria2 sidecar

**Files:**
- Create: `src-tauri/src/aria2/process.rs`
- Create: `src-tauri/tests/aria2_process.rs`
- Modify: `src-tauri/src/aria2/mod.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces `Aria2LaunchConfig { binary, data_dir, download_dir, parent_pid }`.
- Produces `Aria2Process::start(config) -> Result<(Aria2Process, Aria2RpcClient), Aria2Error>`, `wait_ready(Duration)`, and `terminate_fallback()`.

- [ ] **Step 1: Write failing launch-argument and readiness tests**

```rust
#[test]
fn launch_args_are_local_and_session_enabled() {
    let args = build_aria2_args(&fixture_config(), 6811, RpcSecret::from_bytes([0xab; 32]));
    assert!(args.contains(&"--enable-rpc=true".into()));
    assert!(args.contains(&"--rpc-listen-all=false".into()));
    assert!(args.contains(&"--rpc-allow-origin-all=false".into()));
    assert!(args.contains(&"--save-session-interval=10".into()));
    assert!(args.iter().any(|v| v.starts_with("--stop-with-process=")));
}
```

Add a bounded readiness timeout test and verify secret `Debug` output is redacted.

- [ ] **Step 2: Run and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_process`

Expected: process types are absent.

- [ ] **Step 3: Implement port selection, random secret, spawn, and readiness**

Select a candidate port through `TcpListener::bind("127.0.0.1:0")`, release immediately before spawn, and retry the whole sequence on collision. Generate 32 random bytes into `RpcSecret`, pass its hex authorization value with the session paths and `--stop-with-process=<PID>`, suppress console windows, and poll `getVersion` with a deadline.

- [ ] **Step 4: Run real bundled-sidecar probe and commit**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_process -- --nocapture`

Expected: bundled aria2 reaches RPC readiness and the test guard stops its child.

```powershell
git add src-tauri/src/aria2/process.rs src-tauri/src/aria2/mod.rs src-tauri/src/lib.rs src-tauri/tests/aria2_process.rs
git commit -m "feat: manage secure aria2 sidecar"
```

### Task 4: Migrate SQLite for GID and transfer snapshots

**Files:**
- Modify: `src-tauri/src/models.rs`
- Modify: `src-tauri/src/store.rs`
- Create: `src-tauri/tests/store_aria2_migration.rs`

**Interfaces:**
- Adds optional/default transfer fields to `DownloadTask`.
- Produces `TaskStore::bind_gid`, `replace_gid`, `apply_transfer_snapshot`, `upsert_files`, and `find_by_gid`.

- [ ] **Step 1: Write a failing legacy-database migration test**

```rust
#[test]
fn legacy_database_is_preserved_and_accepts_gid() {
    let db = legacy_database_with_one_task();
    let store = TaskStore::open_path(db.path()).unwrap();
    let task = store.list().unwrap().remove(0);
    assert_eq!(task.name, "legacy.zip");
    assert_eq!(task.aria_gid, None);
    store.bind_gid(task.id, "gid-1").unwrap();
}
```

- [ ] **Step 2: Run and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test store_aria2_migration`

Expected: fields and methods are missing.

- [ ] **Step 3: Implement transactional migration and updates**

Use `PRAGMA user_version`. Migrate legacy/version 0 using `ALTER TABLE` and create `task_files(task_id, file_index, path, length, completed_length, selected, PRIMARY KEY(task_id,file_index))`. Snapshot and file updates run transactionally.

- [ ] **Step 4: Run old/new store tests and commit**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test store_transitions --test store_aria2_migration`

```powershell
git add src-tauri/src/models.rs src-tauri/src/store.rs src-tauri/tests/store_aria2_migration.rs
git commit -m "feat: persist aria2 task state"
```

### Task 5: Replace per-task spawn with DownloadService submission

**Files:**
- Create: `src-tauri/src/aria2/service.rs`
- Modify: `src-tauri/src/downloader.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/tests/downloader.rs`

**Interfaces:**
- Consumes `Aria2Api`, `TaskStore`, and `Aria2Process`.
- Produces `DownloadService::submit_uri`, `submit_torrent`, and `control`.
- Registers `Arc<DownloadService>` as Tauri state and removes production use of `start_aria2`.

- [ ] **Step 1: Rewrite downloader tests against a recording API**

```rust
#[tokio::test]
async fn direct_http_binds_gid_only_after_rpc_succeeds() {
    let api = RecordingAria2::returns_gid("gid-1");
    let service = service_with(api.clone());
    let task = service.submit_uri(task_id, vec![url], options).await.unwrap();
    assert_eq!(task.aria_gid.as_deref(), Some("gid-1"));
    assert_eq!(api.methods(), ["aria2.addUri"]);
}
```

Add failure coverage proving no task enters downloading when `addUri` fails.

- [ ] **Step 2: Run and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test downloader`

Expected: old process API cannot satisfy the tests.

- [ ] **Step 3: Implement service and async Tauri commands**

Make `parse_download` and `confirm_download_command` async. Remove per-task `std::process::Command`. Bind GID after successful add; derive queue/download state from initial `tellStatus`.

- [ ] **Step 4: Run tests and commit**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test downloader --test store_transitions`

```powershell
git add src-tauri/src/aria2/service.rs src-tauri/src/downloader.rs src-tauri/src/commands.rs src-tauri/src/lib.rs src-tauri/tests/downloader.rs
git commit -m "feat: submit downloads through aria2 rpc"
```

### Task 6: Reconcile live state and publish Tauri events

**Files:**
- Create: `src-tauri/src/aria2/sync.rs`
- Create: `src-tauri/tests/aria2_sync.rs`
- Modify: `src-tauri/src/aria2/service.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/Cargo.toml`

**Interfaces:**
- Produces `TransferSynchronizer::run(cancel_token)` and `DownloadService::reconcile_now()`.
- Emits `download-task-updated` with `DownloadTask` and `download-global-stat-updated` with `Aria2GlobalStat`.

- [ ] **Step 1: Write failing change-detection and followedBy tests**

```rust
#[tokio::test(start_paused = true)]
async fn unchanged_snapshots_emit_once_then_only_on_change() {
    let api = ScriptedAria2::statuses([active("gid-1", 25), active("gid-1", 25), active("gid-1", 50)]);
    let events = RecordingEvents::default();
    reconcile_three_times(api, events.clone()).await;
    assert_eq!(events.progress_values(), [25, 50]);
}
```

Verify active interval 500ms, paused interval 2s, persistence-before-event, and `followedBy` GID replacement without a duplicate local task.

- [ ] **Step 2: Run and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_sync`

Expected: synchronizer is missing.

- [ ] **Step 3: Implement polling and event sink**

Use `system.multicall` for known GIDs. Persist before emit. Define an `EventSink` trait so tests record events without a Tauri app.

- [ ] **Step 4: Add WebSocket notification wakeups**

Add direct dependencies `tokio-tungstenite` and `futures-util`, and enable Tokio's `test-util` feature for paused-time tests. Connect to `ws://127.0.0.1:<port>/jsonrpc` using the same method schema. Accept only known `aria2.onDownload*` notifications. Reconnect with bounded backoff while periodic reconciliation continues.

- [ ] **Step 5: Run tests and commit**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_sync`

```powershell
git add src-tauri/src/aria2/sync.rs src-tauri/src/aria2/service.rs src-tauri/src/lib.rs src-tauri/src/aria2/rpc.rs src-tauri/tests/aria2_sync.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat: synchronize live aria2 state"
```

### Task 7: Wire real pause, resume, and cancel

**Files:**
- Modify: `src-tauri/src/aria2/service.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/tests/downloader.rs`
- Modify: `src/lib/api.ts`
- Modify: `src/App.tsx`

**Interfaces:**
- Produces `task_action(task_id, action)` returning the reconciled task.
- Consumes `DownloadService::control` and never writes an optimistic state.

- [ ] **Step 1: Write failing control tests**

```rust
#[tokio::test]
async fn failed_pause_does_not_change_store() {
    let api = RecordingAria2::fail("aria2.pause");
    let before = store.find(task_id).unwrap();
    assert!(service.control(task_id, DownloadAction::Pause).await.is_err());
    assert_eq!(store.find(task_id).unwrap().status, before.status);
}
```

Cover unpause, remove, force-remove fallback, parent fan-out, and preservation of partial files.

- [ ] **Step 2: Run and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test downloader`

Expected: current command mutates SQLite directly.

- [ ] **Step 3: Implement RPC-first controls and remove frontend fallback**

Delete `nextStatus` optimistic mapping in `App.tsx`. Failure retains the current task and shows the RPC error. Success immediately reconciles through `tellStatus`.

- [ ] **Step 4: Run Rust/frontend tests and commit**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test downloader; npm run test:run -- src/App.test.tsx src/components/DownloadQueue.test.tsx`

```powershell
git add src-tauri/src/aria2/service.rs src-tauri/src/commands.rs src-tauri/tests/downloader.rs src/lib/api.ts src/App.tsx
git commit -m "feat: control downloads through aria2 rpc"
```

### Task 8: Implement magnet metadata and BT file selection

**Files:**
- Modify: `src-tauri/src/aria2/service.rs`
- Modify: `src-tauri/src/models.rs`
- Modify: `src-tauri/src/store.rs`
- Create: `src-tauri/tests/aria2_bt.rs`
- Modify: `src/components/ParseConfirmDialog.tsx`
- Modify: `src/components/DownloadQueue.tsx`

**Interfaces:**
- Produces `DownloadService::prepare_magnet` and `select_files(task_id, indices: Vec<u32>)`.
- Produces child actions `select` and `skip` for files sharing one BT GID.

- [ ] **Step 1: Write failing metadata-flow test**

```rust
#[tokio::test]
async fn metadata_gid_is_replaced_and_selected_before_unpause() {
    let api = ScriptedAria2::metadata_flow("meta-gid", "download-gid", files());
    service.prepare_magnet(task_id, magnet).await.unwrap();
    service.select_files(task_id, vec![1, 3]).await.unwrap();
    assert_eq!(api.change_option_value("download-gid", "select-file"), "1,3");
    assert!(api.called("aria2.unpause", "download-gid"));
    assert_eq!(store.find(task_id).unwrap().aria_gid.as_deref(), Some("download-gid"));
}
```

- [ ] **Step 2: Run and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_bt`

Expected: metadata and selection APIs are absent.

- [ ] **Step 3: Implement metadata coordination and honest UI semantics**

Keep task awaiting confirmation while metadata downloads. Follow `followedBy`, persist files/new GID, apply `select-file`, then unpause. A BT child displays “下载此文件/跳过此文件”; it never claims independent pause.

- [ ] **Step 4: Run BT/UI tests and commit**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_bt; npm run test:run -- src/components/ParseConfirmDialog.test.tsx src/components/DownloadQueue.test.tsx`

```powershell
git add src-tauri/src/aria2/service.rs src-tauri/src/models.rs src-tauri/src/store.rs src-tauri/tests/aria2_bt.rs src/components/ParseConfirmDialog.tsx src/components/DownloadQueue.tsx
git commit -m "feat: support aria2 bt file selection"
```

### Task 9: Add graceful shutdown and session recovery

**Files:**
- Create: `src-tauri/src/aria2/recovery.rs`
- Create: `src-tauri/tests/aria2_recovery.rs`
- Modify: `src-tauri/src/aria2/process.rs`
- Modify: `src-tauri/src/aria2/service.rs`
- Modify: `src-tauri/src/launcher.rs`
- Modify: `src-tauri/src/window_lifecycle.rs`

**Interfaces:**
- Produces `DownloadService::shutdown(Duration) -> ShutdownReport` and `recover() -> RecoveryReport`.
- Consumes lifecycle exit flag from the window plan.

- [ ] **Step 1: Write failing shutdown-order and recovery tests**

```rust
#[tokio::test]
async fn exit_saves_before_shutdown_without_early_kill() {
    let api = RecordingAria2::default();
    service.shutdown(Duration::from_secs(5)).await.unwrap();
    assert_eq!(api.methods(), ["aria2.saveSession", "aria2.shutdown"]);
    assert!(!process.was_force_killed());
}
```

Add known GID, unknown session GID import, missing GID/output reconciliation, and one-restart-only cases.

- [ ] **Step 2: Run and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_recovery`

Expected: recovery and shutdown APIs are absent.

- [ ] **Step 3: Implement shutdown and asynchronous launcher quit**

Set exiting first, reject submissions, save, shutdown, wait five seconds, then kill only the owned child. Native menu quit runs on Tauri async runtime and calls `app.exit(0)` after shutdown.

- [ ] **Step 4: Implement startup recovery, run tests, and commit**

Query active/waiting/stopped after readiness; reconcile with SQLite; import unknown GIDs with sanitized names; emit recovered snapshots.

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_recovery --test window_lifecycle`

```powershell
git add src-tauri/src/aria2/recovery.rs src-tauri/src/aria2/process.rs src-tauri/src/aria2/service.rs src-tauri/src/launcher.rs src-tauri/src/window_lifecycle.rs src-tauri/tests/aria2_recovery.rs
git commit -m "feat: recover and gracefully stop aria2"
```

### Task 10: Render live aria2 snapshots

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/lib/api.ts`
- Create: `src/lib/downloadEvents.ts`
- Create: `src/lib/downloadEvents.test.ts`
- Modify: `src/App.tsx`
- Modify: `src/components/DownloadQueue.tsx`
- Modify: `src/components/TaskDetails.tsx`
- Create: `src/components/TaskDetails.test.tsx`
- Modify: `src/components/LauncherWindow.tsx`
- Modify: `src/styles.css`

**Interfaces:**
- Consumes `download-task-updated` and `download-global-stat-updated`.
- Produces `subscribeToDownloadEvents`, `upsertTask`, byte/s formatter, and ETA formatter.

- [ ] **Step 1: Write failing immutable-upsert and rendering tests**

```ts
it('replaces a task snapshot without duplication', () => {
  expect(upsertTask([task({ progress: 10 })], task({ progress: 40 })))
    .toEqual([task({ progress: 40 })]);
});
```

Add assertions for real speed, bytes, ETA, connections, error code, and unknown total length.

- [ ] **Step 2: Run and verify RED**

Run: `npm run test:run -- src/lib/downloadEvents.test.ts src/components/DownloadQueue.test.tsx src/components/TaskDetails.test.tsx`

Expected: adapter and fields are missing.

- [ ] **Step 3: Implement subscriptions and remove synthetic values**

Request `list_tasks` on mount and manager reveal, immutable-upsert events, format values through tested pure helpers, and push global active count/speed to launcher.

- [ ] **Step 4: Run frontend suite/build and commit**

Run: `npm run test:run; npm run build`

```powershell
git add src/lib/types.ts src/lib/api.ts src/lib/downloadEvents.ts src/lib/downloadEvents.test.ts src/App.tsx src/components/DownloadQueue.tsx src/components/TaskDetails.tsx src/components/LauncherWindow.tsx src/styles.css
git commit -m "feat: render live aria2 task state"
```

### Task 11: Prove live control, persistence, and packaging

**Files:**
- Create: `src-tauri/tests/aria2_live.rs`
- Modify: `scripts/smoke-launcher.cjs`
- Modify: `scripts/smoke-download.ps1`

**Interfaces:**
- Produces `LiveAria2Fixture::start()`, `add_slow_download`, `wait_completed_at_least`, `pause`, `status`, `unpause`, `wait_complete`, `download_sha256`, `expected_sha256`, `save_and_shutdown`, and `child_is_running`.

- [ ] **Step 1: Write a failing live integration test**

```rust
#[test]
fn bundled_aria2_downloads_pauses_resumes_and_stops() {
    let mut fixture = LiveAria2Fixture::start().unwrap();
    let gid = fixture.add_slow_download(8 * 1024 * 1024).unwrap();
    fixture.wait_completed_at_least(&gid, 64 * 1024, Duration::from_secs(5)).unwrap();
    fixture.pause(&gid).unwrap();
    assert_eq!(fixture.status(&gid).unwrap(), "paused");
    fixture.unpause(&gid).unwrap();
    fixture.wait_complete(&gid, Duration::from_secs(20)).unwrap();
    assert_eq!(fixture.download_sha256(), fixture.expected_sha256());
    fixture.save_and_shutdown(Duration::from_secs(5)).unwrap();
    assert!(!fixture.child_is_running());
}
```

The fixture starts the real bundled aria2 and a range-capable loopback server; it uses condition polling with explicit deadlines.

- [ ] **Step 2: Run and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_live -- --nocapture`

Expected: failure at the first not-yet-wired live behavior, never a public-network timeout.

- [ ] **Step 3: Make only integration fixes required for GREEN**

Keep all paths inside a temporary directory. Poll conditions with 100ms maximum intervals and explicit deadlines; never use a blocking sleep above 60 seconds.

- [ ] **Step 4: Run complete verification and package**

```powershell
npm run test:run
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
powershell -ExecutionPolicy Bypass -File scripts/build-windows.ps1
npm run test:launcher
powershell -ExecutionPolicy Bypass -File scripts/smoke-download.ps1
git diff --check
```

Expected: all tests pass; real loopback transfer/pause/resume/session shutdown passes; EXE/MSI/NSIS exist; hidden UI smoke leaves no AllDownloader or owned aria2 process.

- [ ] **Step 5: Commit live verification**

```powershell
git add src-tauri/tests/aria2_live.rs scripts/smoke-launcher.cjs scripts/smoke-download.ps1
git commit -m "test: verify live aria2 rpc lifecycle"
```
