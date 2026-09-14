# Cloud Download Concurrency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enforce provider-specific safety ceilings for cloud-file connections and admitted cloud tasks while preserving proxy use, fresh signed URLs, resume correctness, and a new application default of four connections per task.

**Architecture:** `ProviderRegistry` owns immutable provider policies. A focused cloud scheduler persists safe pending requests and admits stable-source tasks per provider before URL resolution; the shared personal/share submission paths apply the policy to per-URI aria2 options. Terminal task events release slots and startup recovery accounts for existing GIDs before dispatching pending work.

**Tech Stack:** Rust, Tokio, rusqlite, aria2 JSON-RPC, Tauri, TypeScript/React, Vitest.

**Spec:** `docs/superpowers/specs/2026-09-14-cloud-download-concurrency-design.md`

## Global Constraints

- `max_connections_per_file` and `max_concurrent_tasks` are safety ceilings, not defaults.
- Personal and share downloads consume the same provider pool.
- Signed URLs and authorization headers are resolved only after admission and are never persisted.
- Every cloud direct download continues to use the configured proxy; no bypass or direct fallback is allowed.
- New/default `maxConnectionsPerServer` is 4; existing persisted settings are not migrated.
- Run targeted Rust and frontend tests plus release builds; do not default to the full test suites.

---

### Task 1: Provider concurrency policy and public contract

**Files:**
- Modify: `src-tauri/src/cloud/registry.rs`
- Modify: `src-tauri/tests/cloud_provider_contract.rs`
- Modify: `src/lib/cloudTypes.ts`
- Modify: `src/test/cloudCatalog.ts`

**Interfaces:**
- Produces: `DownloadConcurrencyPolicy { max_connections_per_file: u8, max_concurrent_tasks: u16 }`
- Produces: `ProviderDescriptor::download_concurrency`
- Consumed by: Tasks 2 and 3.

- [ ] **Step 1: Write the failing registry contract test**

Add a literal table for all seven providers and assert the serialized `downloadConcurrency` object matches the spec matrix and contains no zero values.

- [ ] **Step 2: Run the test to verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract provider_download_concurrency`

Expected: compilation fails because `download_concurrency` does not exist.

- [ ] **Step 3: Add the minimal registry model and values**

Add the serializable policy struct, the descriptor field, and the exact matrix from the design. Mirror the DTO in `cloudTypes.ts` and complete the full provider fixture in `cloudCatalog.ts`.

- [ ] **Step 4: Run the contract test to verify GREEN**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract provider_download_concurrency`

Expected: the new contract test passes.

- [ ] **Step 5: Commit the provider contract**

```powershell
git add src-tauri/src/cloud/registry.rs src-tauri/tests/cloud_provider_contract.rs src/lib/cloudTypes.ts src/test/cloudCatalog.ts
git commit -m "feat: declare cloud download concurrency limits"
```

### Task 2: Per-URI aria2 connection ceiling

**Files:**
- Modify: `src-tauri/src/aria2/service.rs`
- Modify: `src-tauri/tests/downloader.rs`
- Modify: `src-tauri/src/cloud/commands/download.rs`
- Modify: `src-tauri/src/cloud/share/service.rs`

**Interfaces:**
- Produces: `UriSubmissionOptions::max_connections_per_server: Option<u8>`
- Produces: `apply_cloud_download_policy(provider, configured_connections, options) -> UriSubmissionOptions`
- Consumes: `ProviderDescriptor::download_concurrency` from Task 1.

- [ ] **Step 1: Write failing option-generation tests**

Cover a user value of 8 capped to Xunlei 1, a user value of 2 retained under a provider cap of 4, and both aria2 keys receiving the same literal value.

- [ ] **Step 2: Run the tests to verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test downloader cloud_download_connection_policy`

Expected: compilation fails because the per-URI field and policy function are absent.

- [ ] **Step 3: Implement the minimal per-URI option and shared policy application**

When present, `UriSubmissionOptions::into_aria2_options` inserts both `split` and `max-connection-per-server`. Apply `min(user, provider cap)` in the shared personal/share boundary; do not branch on Xunlei outside the registry.

- [ ] **Step 4: Run the tests to verify GREEN**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test downloader cloud_download_connection_policy`

Expected: all selected tests pass.

- [ ] **Step 5: Commit per-task connection enforcement**

```powershell
git add src-tauri/src/aria2/service.rs src-tauri/src/cloud/commands/download.rs src-tauri/src/cloud/share/service.rs src-tauri/tests/downloader.rs
git commit -m "fix: cap cloud file connections by provider"
```

### Task 3: Durable provider admission scheduler

**Files:**
- Create: `src-tauri/src/cloud/download_scheduler.rs`
- Modify: `src-tauri/src/cloud/mod.rs`
- Modify: `src-tauri/src/store.rs`
- Modify: `src-tauri/src/models.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: `src-tauri/tests/cloud_download_scheduler.rs`

**Interfaces:**
- Produces: `PendingCloudDownload { task_id, provider, directory, output_name, allow_overwrite, continue_download }`
- Produces: `CloudDownloadScheduler::new`, `restore_active`, `enqueue`, `release`, `cancel`, and `pending`.
- Persists: `cloud_download_requests` without URL, request headers, cookies, bearer tokens, or proxy credentials.
- Consumed by: Task 4 and the Tauri task event sink.

- [ ] **Step 1: Write failing scheduler tests**

Use literal provider limits and real Tokio tasks to prove: the fifth Quark task waits behind four; one Xunlei task does not block one Baidu task; release admits the oldest same-provider waiter; cancellation removes a waiter; restored active tasks consume slots; an active count above the limit admits nothing new until enough tasks end.

- [ ] **Step 2: Run the tests to verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_download_scheduler`

Expected: compilation fails because the scheduler module and persistence API are absent.

- [ ] **Step 3: Implement persistence and FIFO admission**

Add a schema migration for `cloud_download_requests`, typed store methods, a mutex-protected per-provider active/waiting state, and Tokio notifications. Persist only stable/local fields. Treat admitted tasks with a GID as active through terminal status.

- [ ] **Step 4: Add terminal-event and startup composition**

Store the scheduler in `AppState` and `TauriDownloadEvents`. On startup restore nonterminal cloud tasks with GIDs before pending requests; on completed/failed/cancelled/recycled task events release the provider slot.

- [ ] **Step 5: Run the scheduler tests to verify GREEN**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_download_scheduler`

Expected: all scheduler tests pass without timing sleeps longer than the test timeout.

- [ ] **Step 6: Commit the durable scheduler**

```powershell
git add src-tauri/src/cloud/download_scheduler.rs src-tauri/src/cloud/mod.rs src-tauri/src/store.rs src-tauri/src/models.rs src-tauri/src/commands.rs src-tauri/src/lib.rs src-tauri/tests/cloud_download_scheduler.rs
git commit -m "feat: schedule cloud downloads by provider"
```

### Task 4: Route personal, share, retry, pause and cancel through the scheduler

**Files:**
- Modify: `src-tauri/src/cloud/commands/download.rs`
- Modify: `src-tauri/src/cloud/commands/share.rs`
- Modify: `src-tauri/src/cloud/share/service.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/tests/downloader.rs`
- Modify: `src-tauri/tests/cloud_directory_download.rs`

**Interfaces:**
- Consumes: scheduler and pending request from Task 3.
- Produces: one shared dispatcher that resolves `cloud://` and `cloud-share://` only after admission, applies Task 2 limits, and submits/resubmits to aria2.

- [ ] **Step 1: Write failing shared-flow tests**

Prove with resolver counters that a fifth same-provider task is returned as queued without invoking its resolver; personal and share tasks share the same four slots; releasing a task invokes the queued resolver once; different providers resolve independently.

- [ ] **Step 2: Write failing control/retry tests**

Prove a GID-less queued task can pause and cancel without aria2; resume re-enqueues it; retry persists overwrite/continue intent and does not resolve until admitted; a valid resume passes `continue=true` and the provider connection ceiling.

- [ ] **Step 3: Run the selected tests to verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test downloader --test cloud_directory_download scheduler`

Expected: assertions fail because current commands resolve and submit immediately and GID-less queue control is unsupported.

- [ ] **Step 4: Implement the shared dispatcher and task actions**

Create/link the durable task and pending request first, enqueue it, then resolve and submit only in the admitted callback. Read proxy settings at dispatch time. Remove the pending request after successful aria2 acceptance or terminal cancellation; keep failure state retryable. Extend legal task transitions only for the new queued-without-GID lifecycle.

- [ ] **Step 5: Run the selected tests to verify GREEN**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test downloader --test cloud_directory_download scheduler`

Expected: all selected scheduler and cloud flow tests pass.

- [ ] **Step 6: Commit shared cloud dispatch**

```powershell
git add src-tauri/src/cloud/commands/download.rs src-tauri/src/cloud/commands/share.rs src-tauri/src/cloud/share/service.rs src-tauri/src/commands.rs src-tauri/tests/downloader.rs src-tauri/tests/cloud_directory_download.rs
git commit -m "feat: defer cloud URL resolution until admission"
```

### Task 5: Default connection count and frontend contract

**Files:**
- Modify: `src-tauri/src/models.rs`
- Modify: `src/lib/api.ts`
- Modify: `src/components/SettingsPanel.tsx`
- Modify: `src/components/SettingsPanel.test.tsx`
- Modify: `src/App.live.test.tsx`
- Modify: `src/LauncherWindow.test.tsx`

**Interfaces:**
- Produces: Rust and frontend fallback defaults of 4.
- Preserves: deserialized saved values, including 8.

- [ ] **Step 1: Write failing default-behavior tests**

Assert a missing/default settings object produces 4, while explicit JSON containing 8 deserializes and serializes as 8.

- [ ] **Step 2: Run the tests to verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml models::tests::aria2_default_connections -- --exact`

Run: `npm test -- --run src/components/SettingsPanel.test.tsx src/App.live.test.tsx src/LauncherWindow.test.tsx --exclude '.worktrees/**'`

Expected: default assertions receive 8.

- [ ] **Step 3: Change only defaults and complete test fixtures**

Set Rust `Aria2Settings::default`, `defaultSettings`, and `defaultAria2Settings` to 4. Update complete frontend fixtures that represent defaults; leave explicit nondefault 8 fixtures intact.

- [ ] **Step 4: Run the tests to verify GREEN**

Run the same Rust and Vitest commands from Step 2.

Expected: selected tests pass and the explicit-8 preservation test remains green.

- [ ] **Step 5: Commit the new default**

```powershell
git add src-tauri/src/models.rs src/lib/api.ts src/components/SettingsPanel.tsx src/components/SettingsPanel.test.tsx src/App.live.test.tsx src/LauncherWindow.test.tsx
git commit -m "chore: default file connections to four"
```

### Task 6: Read-only provider Range diagnostic

**Files:**
- Create: `src-tauri/tests/live_cloud_download_concurrency.rs`
- Modify: `docs/superpowers/specs/2026-09-14-cloud-download-concurrency-design.md` only if an observed provider must be lowered from 4 to 1.
- Modify: `src-tauri/src/cloud/registry.rs` only through a new RED/GREEN cycle if an observed provider must be lowered.

**Interfaces:**
- Produces: opt-in read-only diagnostic output containing provider key, HTTP status, validated byte range and pass/fail only.
- Consumes: saved cloud sessions and the current application proxy settings.

- [ ] **Step 1: Write the opt-in diagnostic**

For each enabled provider with an authenticated account and a file large enough for four small disjoint ranges, resolve a fresh URL, issue all four ranges concurrently through the configured proxy, require HTTP 206 plus matching `Content-Range` and exact byte counts, and redact URL/headers.

- [ ] **Step 2: Run the diagnostic through the configured proxy**

Run: `$env:ALLDOWNLOADER_LIVE_CLOUD_CONCURRENCY='1'; cargo test --manifest-path src-tauri/Cargo.toml --test live_cloud_download_concurrency -- --ignored --nocapture`

Expected: each test reports PASS, SKIP with a concrete non-secret reason, or FAIL without printing credentials. Xunlei's policy remains 1 regardless of a transient four-range success because the supplied large-file reproduction is authoritative.

- [ ] **Step 3: Lower any failing provider through TDD**

First change the literal contract expectation to 1 and verify RED, then lower only that provider's registry value and verify GREEN. Do not raise any provider above 4.

- [ ] **Step 4: Commit the redacted live diagnostic and any evidence-driven cap changes**

```powershell
git add src-tauri/tests/live_cloud_download_concurrency.rs src-tauri/src/cloud/registry.rs src-tauri/tests/cloud_provider_contract.rs docs/superpowers/specs/2026-09-14-cloud-download-concurrency-design.md
git commit -m "test: probe cloud range concurrency safely"
```

### Task 7: Scoped regression and release verification

**Files:**
- Verify only; no planned production edits.

**Interfaces:**
- Consumes: all prior tasks.
- Produces: fresh evidence for the final report and Windows binary.

- [ ] **Step 1: Format and inspect scope**

Run: `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`

Run: `git diff --check`

- [ ] **Step 2: Run related Rust tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract --test cloud_download_scheduler --test downloader --test cloud_directory_download`

- [ ] **Step 3: Run related frontend tests**

Run: `npm test -- --run src/components/SettingsPanel.test.tsx src/App.live.test.tsx src/LauncherWindow.test.tsx --exclude '.worktrees/**'`

- [ ] **Step 4: Build frontend and Windows release artifact**

Run: `npm run build`

Run: `powershell -ExecutionPolicy Bypass -File scripts/build-windows.ps1`

- [ ] **Step 5: Review final diff against the design**

Confirm the diff contains no proxy bypass, no persisted signed URL/header/credential, no old-settings migration, and no changes to unrelated dirty files.
