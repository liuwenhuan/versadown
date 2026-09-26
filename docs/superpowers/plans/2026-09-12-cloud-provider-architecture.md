# Cloud Provider Architecture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor all seven cloud providers into a registry-driven architecture with durable authorization maintenance, modular drive/share/download/upload capabilities, safer credential handling, and bounded transfer concurrency.

**Architecture:** A backend-owned `ProviderRegistry` becomes the only source of provider metadata and constructs capability-specific drivers around the existing protocol clients. A new `AuthSupervisor` owns refresh singleflight, persistent recovery backoff, and post-authorization task resumption while the Tauri command modules become thin facades. Share plans move to row-based SQLite storage and uploads use streaming file slices.

**Tech Stack:** Rust 2021, Tauri 2, Tokio, Reqwest blocking client, Rusqlite, Windows DPAPI, React 18, TypeScript, Vitest.

**Spec:** `docs/superpowers/specs/2026-09-12-cloud-provider-architecture.md`

## Global Constraints

- Continue supporting exactly `quark`, `pikpak`, `baidu`, `tianyi`, `aliyun`, `xunlei`, and `uc` throughout the migration.
- Preserve persisted `cloud://` and `cloud-share://` source formats and never persist temporary signed download URLs.
- Maintain exactly one account per provider; authorizing a replacement account removes the old provider session and saved password.
- Never return a saved plaintext password to the frontend.
- Automatically retry a read at most once after silent recovery; never automatically replay a mutation.
- Do not keep a resident hidden WebView; persistent browser profiles are opened only on demand and closed after credential capture.
- Unknown-expiry cookie sessions are checked on use or genuine authentication failure, not by frequent keepalive probes.
- Keep device identity stable per provider account and persist automatic-login backoff across restarts.
- New functions and behavior changes follow red-green-refactor; frontend Vitest commands include `--exclude '.worktrees/**'`.
- Use `scripts/build-windows.ps1` for the final release build and do not infer that this build replaces tests.

---

### Task 1: Correct Known Persistence, Pagination, Validation, and Bootstrap Defects

**Files:**
- Modify: `pnpm-workspace.yaml`
- Modify: `src-tauri/src/cloud/credentials.rs`
- Modify: `src-tauri/src/cloud/session.rs`
- Modify: `src-tauri/src/cloud/tianyi.rs`
- Modify: `src-tauri/src/cloud/aliyun.rs`
- Test: `src-tauri/tests/cloud_sessions.rs`
- Test: `src-tauri/tests/cloud_tianyi.rs`
- Test: `src-tauri/tests/cloud_aliyun.rs`

**Interfaces:**
- Consumes: existing `CredentialStore`, `DpapiCredentialStore`, `TianyiClient`, and `AliyunClient` APIs.
- Produces: `credential_key_is_supported(&str) -> bool`, fully paginated Tianyi directories, and optimistic Aliyun bearer usage with one refresh-on-auth-failure.

- [ ] **Step 1: Record the dependency bootstrap failure**

Run: `pnpm install --frozen-lockfile`

Expected: FAIL with `ERR_PNPM_IGNORED_BUILDS` because `pnpm-workspace.yaml` contains the literal placeholder value for esbuild.

- [ ] **Step 2: Write failing credential-key tests**

Add table-driven tests that call the real store-key validator for every session, password, draft, browser preference, browser profile owner, share-plan migration, and authorization-maintenance key. The mutation caught is deleting any production key from the allowlist.

```rust
for key in [
    "cloud-password-baidu-browser-preferred",
    "cloud-password-baidu-browser-profile-username",
    "cloud-auth-maintenance-baidu",
] {
    assert!(credential_key_is_supported(key), "missing credential key {key}");
}
```

- [ ] **Step 3: Write failing Tianyi pagination tests**

Add one App-session fixture and one browser-session fixture where page 1 contains 100 entries and page 2 contains one entry. Assert the returned directory contains 101 unique entries and that the second request uses `pageNum=2`.

- [ ] **Step 4: Write a failing Aliyun optimistic-operation test**

Use the existing fake transport with a current browser bearer and assert a root listing sends exactly one list request. Add a second case where the list call returns an authentication error, a refresh succeeds, and the list is retried exactly once.

- [ ] **Step 5: Verify all new tests fail for the intended reasons**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_sessions --test cloud_tianyi --test cloud_aliyun`

Expected: FAIL because the Baidu auxiliary keys are rejected, Tianyi stops after page 1, and Aliyun validates before the requested operation.

- [ ] **Step 6: Implement the minimal corrections**

Centralize the supported-key predicate, add the missing keys, loop Tianyi directory pages until an empty or short page with duplicate/cycle bounds, and replace Aliyun pre-validation with this operation pattern:

```rust
let first = self.list_with_session(&session, parent_id);
match first {
    Err(QuarkError::AuthorizationExpired) => {
        let refreshed = self.refresh_and_persist(&session)?;
        self.list_with_session(&refreshed, parent_id)
    }
    result => result,
}
```

Set `allowBuilds.esbuild` to boolean `true` in `pnpm-workspace.yaml`.

- [ ] **Step 7: Verify the fixes and bootstrap**

Run: `pnpm install --frozen-lockfile`

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_sessions --test cloud_tianyi --test cloud_aliyun`

Expected: installation exits 0 and all selected Rust tests pass.

- [ ] **Step 8: Commit**

```text
fix(cloud): correct credential persistence and provider paging
```

### Task 2: Make the Backend Provider Registry the Single Metadata Source

**Files:**
- Create: `src-tauri/src/cloud/registry.rs`
- Modify: `src-tauri/src/cloud/mod.rs`
- Modify: `src-tauri/src/cloud/types.rs`
- Modify: `src-tauri/src/cloud/commands.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/parser.rs`
- Modify: `src/lib/cloudTypes.ts`
- Modify: `src/lib/cloudApi.ts`
- Modify: `src/components/CloudWorkspace.tsx`
- Modify: `src/components/CloudManager.tsx`
- Test: `src-tauri/tests/cloud_provider_contract.rs`
- Test: `src-tauri/tests/parser_policy.rs`
- Test: `src/lib/cloudApi.test.ts`
- Test: `src/components/CloudWorkspace.test.tsx`
- Test: `src/components/CloudManager.test.tsx`

**Interfaces:**
- Consumes: `CloudProvider`, existing root/recycle constants, authorization URLs, share host checks, and parser rules.
- Produces: `ProviderDescriptor`, `ProviderCapabilities`, `ProviderRegistry::get`, `ProviderRegistry::all`, and IPC command `cloud_provider_catalog`.

- [ ] **Step 1: Write failing registry contract tests**

Assert all seven providers appear exactly once, keys are unique, roots/recycle IDs are non-empty, trusted hosts are HTTPS-compatible, and existing representative share links resolve through their descriptor.

```rust
let descriptors = ProviderRegistry::global().all();
assert_eq!(descriptors.len(), 7);
assert_eq!(descriptors.iter().map(|d| d.provider).collect::<HashSet<_>>().len(), 7);
```

- [ ] **Step 2: Write failing frontend catalog tests**

Make the fake client return descriptors in a deliberately different order. Assert `CloudWorkspace` renders that order without relying on a frontend provider array, and `CloudManager` displays the returned authorization capability rather than a fixed map.

- [ ] **Step 3: Verify the registry tests fail**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract --test parser_policy`

Run: `node_modules/.bin/vitest.cmd run src/lib/cloudApi.test.ts src/components/CloudWorkspace.test.tsx src/components/CloudManager.test.tsx --exclude '.worktrees/**'`

Expected: FAIL because no provider catalog exists and the frontend still owns fixed metadata.

- [ ] **Step 4: Implement descriptor and capability types**

Use serializable values with explicit capability fields rather than UI inference:

```rust
pub struct ProviderDescriptor {
    pub provider: CloudProvider,
    pub display_name: &'static str,
    pub root_directory_id: &'static str,
    pub recycle_bin_directory_id: &'static str,
    pub authorization: CloudAuthorizationCapabilities,
    pub capabilities: ProviderCapabilities,
    pub share_hosts: &'static [&'static str],
    pub share_patterns: &'static [&'static str],
    pub max_upload_concurrency: usize,
}
```

Implement `CloudProvider::ALL` and build the static registry from it. Parser and share-host validation must consult this registry.

- [ ] **Step 5: Add the catalog IPC and consume it in the frontend**

```rust
#[tauri::command]
pub fn cloud_provider_catalog() -> Vec<ProviderDescriptorDto> {
    ProviderRegistry::global().catalog()
}
```

Replace frontend root, recycle, label, provider-order, and authorization maps with the returned DTO. Show the management shell immediately and update each provider recovery result independently.

- [ ] **Step 6: Verify the registry and frontend behavior**

Run the commands from Step 3. Expected: all selected tests pass.

- [ ] **Step 7: Commit**

```text
refactor(cloud): centralize provider descriptors
```

### Task 3: Enforce a Safe Single-Account Credential Contract

**Files:**
- Modify: `src-tauri/src/cloud/types.rs`
- Modify: `src-tauri/src/cloud/session.rs`
- Modify: `src-tauri/src/cloud/commands.rs`
- Modify: `src/lib/cloudTypes.ts`
- Modify: `src/lib/cloudApi.ts`
- Modify: `src/components/CloudManager.tsx`
- Modify: `src/components/CloudWorkspace.tsx`
- Test: `src-tauri/tests/cloud_session.rs`
- Test: `src-tauri/tests/cloud_provider_contract.rs`
- Test: `src/lib/cloudApi.test.ts`
- Test: `src/components/CloudManager.test.tsx`
- Test: `src/components/CloudWorkspace.test.tsx`

**Interfaces:**
- Consumes: registry catalog and existing password credential store.
- Produces: `SavedCloudCredentialInfo { username, has_saved_password }` and `PasswordLoginRequest { provider, username, password: Option<String>, use_saved_password: bool }`.

- [ ] **Step 1: Write failing backend secrecy and replacement tests**

Assert the saved-credential command response serializes the username and `hasSavedPassword` but never the stored password. Authorize account B after account A and assert A's password/session/profile owner are removed before B is persisted.

- [ ] **Step 2: Write failing frontend single-account tests**

Assert saved credentials render the username plus a masked placeholder, submitting without editing sends `useSavedPassword: true`, editing sends only the new password, and no account-tab or account-switch state is created.

- [ ] **Step 3: Verify the new credential tests fail**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_session --test cloud_provider_contract`

Run: `node_modules/.bin/vitest.cmd run src/lib/cloudApi.test.ts src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx --exclude '.worktrees/**'`

Expected: FAIL because the current API returns the plaintext password and `CloudManager` stores account arrays.

- [ ] **Step 4: Implement the backend-only password handle**

Resolve the effective password only inside Rust:

```rust
let password = match (request.password.filter(|v| !v.is_empty()), request.use_saved_password) {
    (Some(password), false) => Zeroizing::new(password),
    (None, true) => Zeroizing::new(load_saved_password(request.provider, &request.username)?),
    _ => return Err(QuarkError::InvalidInput("请输入密码".into())),
};
```

Return only `SavedCloudCredentialInfo`; zeroize temporary plaintext values and overwrite the old provider credential when the username changes.

- [ ] **Step 5: Simplify React state to one account**

Use `CloudAccount | null` inside a provider manager. Remove `initialAccounts`, account-tab rendering, active-account switching, and array reconciliation; keep provider workspace tabs owned by `CloudWorkspace`.

- [ ] **Step 6: Verify backend and frontend behavior**

Run the commands from Step 3. Expected: all selected tests pass and serialized responses contain no password field.

- [ ] **Step 7: Commit**

```text
refactor(cloud): keep saved passwords behind the backend boundary
```

### Task 4: Introduce Capability Drivers and Split the Tauri Command Facade

**Files:**
- Create: `src-tauri/src/cloud/driver.rs`
- Create: `src-tauri/src/cloud/commands/auth.rs`
- Create: `src-tauri/src/cloud/commands/drive.rs`
- Create: `src-tauri/src/cloud/commands/download.rs`
- Create: `src-tauri/src/cloud/commands/share.rs`
- Create: `src-tauri/src/cloud/commands/upload.rs`
- Create: `src-tauri/src/cloud/commands/common.rs`
- Replace: `src-tauri/src/cloud/commands.rs` with module exports and shared `CloudState`
- Modify: `src-tauri/src/cloud/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: `src-tauri/tests/cloud_provider_contract.rs`
- Test: `src-tauri/tests/cloud_coordination.rs`
- Test: `src-tauri/tests/cloud_directory_download.rs`
- Test: `src-tauri/tests/cloud_upload.rs`

**Interfaces:**
- Consumes: `ProviderRegistry`, seven existing protocol clients, stable source parser, and current command signatures.
- Produces: object-safe `DriveOps`, `DownloadOps`, `ProviderAuth`, optional `UploadOps`, and a `ProviderDriverFactory` with one provider selection boundary.

- [ ] **Step 1: Write failing capability-dispatch tests**

Add a recording test driver to the registry and assert account, directory, mutation, download and upload facade calls reach the matching capability. Assert an absent capability returns `ProviderUnsupported` before network I/O.

- [ ] **Step 2: Write a structural behavior test for command compatibility**

Invoke representative existing commands through their public Rust functions for all seven providers and assert their serialized success/error codes remain unchanged. This test catches lost exports and accidental command renames rather than source layout.

- [ ] **Step 3: Verify the tests fail without driver interfaces**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract --test cloud_coordination --test cloud_directory_download --test cloud_upload`

Expected: FAIL because the registry cannot construct capability drivers.

- [ ] **Step 4: Implement capability interfaces around existing clients**

Keep protocol behavior in existing provider modules and add thin adapters:

```rust
pub trait DriveOps {
    fn account(&mut self) -> Result<CloudAccount, CloudError>;
    fn list_directory(&mut self, parent_id: &str) -> Result<CloudDirectory, CloudError>;
    fn create_directory(&mut self, parent_id: &str, name: &str) -> Result<CloudFile, CloudError>;
    fn rename(&mut self, file_id: &str, name: &str) -> Result<(), CloudError>;
    fn move_files(&mut self, file_ids: &[String], parent_id: &str) -> Result<(), CloudError>;
    fn copy_files(&mut self, file_ids: &[String], parent_id: &str) -> Result<(), CloudError>;
    fn trash(&mut self, file_ids: &[String]) -> Result<(), CloudError>;
    fn delete(&mut self, file_ids: &[String]) -> Result<(), CloudError>;
    fn restore(&mut self, file_ids: &[String]) -> Result<(), CloudError>;
}
```

Rename `QuarkError` to `CloudError` and replace `CloudFile.kind: String` with a serialized `CloudFileKind` enum.

- [ ] **Step 5: Move command implementations by responsibility**

Preserve every Tauri command name and public Rust export. `commands.rs` retains `CloudState`, initialization, and re-exports only; the five command modules call driver capabilities instead of repeating provider matches.

- [ ] **Step 6: Verify command compatibility and direct tests**

Run the command from Step 3. Expected: all selected tests pass.

- [ ] **Step 7: Commit**

```text
refactor(cloud): route commands through provider capabilities
```

### Task 5: Add the Durable Authorization Supervisor

**Files:**
- Create: `src-tauri/src/cloud/auth_supervisor.rs`
- Modify: `src-tauri/src/cloud/credentials.rs`
- Modify: `src-tauri/src/cloud/session.rs`
- Modify: `src-tauri/src/cloud/commands.rs`
- Modify: `src-tauri/src/cloud/commands/auth.rs`
- Modify: `src-tauri/src/cloud/commands/common.rs`
- Modify: `src-tauri/src/cloud/pikpak.rs`
- Modify: `src-tauri/src/cloud/aliyun.rs`
- Modify: `src-tauri/src/cloud/xunlei.rs`
- Modify: `src-tauri/src/cloud/tianyi_app.rs`
- Test: `src-tauri/tests/cloud_session.rs`
- Test: `src-tauri/tests/cloud_coordination.rs`
- Test: `src-tauri/tests/cloud_sessions.rs`
- Test: `src-tauri/tests/cloud_tianyi.rs`

**Interfaces:**
- Consumes: `ProviderAuth`, encrypted credential store, current session recovery policy, provider token expiry fields.
- Produces: `AuthSupervisor`, `AuthMaintenanceRecord`, `AuthHealth`, persistent backoff, per-provider refresh singleflight, and clock/randomness injection for deterministic tests.

- [ ] **Step 1: Write failing state-machine and backoff tests**

Using a fake clock, assert Ready transitions to Refreshing at 80% TTL or five minutes before expiry, jitter remains between 30 and 120 seconds, failures persist backoffs of 5 minutes, 30 minutes and 2 hours, and success resets the counters.

- [ ] **Step 2: Write failing concurrency and safety tests**

Start two concurrent reads for the same expired provider and assert only one refresh occurs while both reads complete. Assert another provider proceeds concurrently. Assert a mutation returns `SessionRenewedRetryOperation` after recovery and its closure is invoked exactly once.

- [ ] **Step 3: Write a failing restart test**

Persist a failed silent-login record, create a new supervisor over the same store, and assert it refuses another automatic attempt until `next_allowed_attempt_at`.

- [ ] **Step 4: Verify supervisor tests fail**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_session --test cloud_coordination --test cloud_sessions --test cloud_tianyi`

Expected: FAIL because recovery throttling is process-local and the provider mutex serializes ordinary operations.

- [ ] **Step 5: Implement the maintenance envelope and singleflight**

```rust
pub struct AuthMaintenanceRecord {
    pub schema_version: u32,
    pub provider: CloudProvider,
    pub account_id: Option<String>,
    pub auth_method: Option<CloudAuthMethod>,
    pub session_revision: u64,
    pub expires_at: Option<i64>,
    pub next_refresh_at: Option<i64>,
    pub consecutive_failures: u32,
    pub next_allowed_attempt_at: Option<i64>,
    pub health: AuthHealth,
}
```

Store no provider secret in this record. Use one Tokio mutex per provider only around refresh/relogin and revisioned persistence; copy a session snapshot before ordinary network work.

- [ ] **Step 6: Integrate provider strategies in fixed order**

Use refresh token/App refresh first for PikPak, Aliyun, Xunlei and Tianyi; saved-password relogin next where supported; persistent-profile capture only after a genuine auth error; visible WebView remains final fallback. Quark, Baidu and UC cookie sessions receive no timer keepalive.

- [ ] **Step 7: Verify the supervisor and provider integrations**

Run the command from Step 4. Expected: all selected tests pass without serializing unrelated reads.

- [ ] **Step 8: Commit**

```text
feat(cloud): maintain provider authorization durably
```

### Task 6: Make Authorization Completion Event-Driven and Resume Durable Failures

**Files:**
- Modify: `src-tauri/src/cloud/commands/auth.rs`
- Modify: `src-tauri/src/cloud/commands/download.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src/lib/cloudApi.ts`
- Modify: `src/components/CloudManager.tsx`
- Modify: `src/components/CloudWorkspace.tsx`
- Test: `src-tauri/tests/cloud_session.rs`
- Test: `src-tauri/tests/cloud_directory_download.rs`
- Test: `src/components/CloudManager.test.tsx`
- Test: `src/components/CloudWorkspace.test.tsx`

**Interfaces:**
- Consumes: `AuthSupervisor`, task database failure codes, existing authorization window and task retry commands.
- Produces: `cloud-authorization-state` event payload and `resume_reauthorized_provider_tasks(provider)` backed by task-store queries.

- [ ] **Step 1: Write failing durable-resumption tests**

Create failed personal and directory-leaf tasks with reauthorization failure codes, discard any in-memory pending set, complete authorization, and assert eligible tasks are found from the database and resumed exactly once. Non-auth failures must remain failed.

- [ ] **Step 2: Write failing frontend event tests**

Emit an authorization-completed event and assert the card immediately leaves waiting state, loads the account, and closes the auth window path without waiting for the 1.8-second poll. Assert the watchdog still detects completion if the event is lost.

- [ ] **Step 3: Verify the tests fail**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_session --test cloud_directory_download`

Run: `node_modules/.bin/vitest.cmd run src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx --exclude '.worktrees/**'`

Expected: FAIL because completion relies on polling and pending retries are process-local.

- [ ] **Step 4: Implement event and database recovery**

Emit this non-secret payload after successful credential persistence:

```rust
pub struct CloudAuthorizationStateEvent {
    pub provider: CloudProvider,
    pub state: AuthHealth,
    pub account: Option<CloudAccount>,
}
```

Query the task store for `not_authorized`, `credentials_expired`, and `web_auth_required`, then reuse existing idempotent retry functions. Keep the in-memory set only as a latency hint or remove it after tests prove database discovery covers all cases.

- [ ] **Step 5: Update the frontend listener**

Subscribe once per provider while authorization is active. Retain a low-rate watchdog at 10 seconds and stop it after completion or component unmount.

- [ ] **Step 6: Verify event-driven recovery**

Run the commands from Step 3. Expected: all selected tests pass.

- [ ] **Step 7: Commit**

```text
feat(cloud): resume authorized downloads from durable state
```

### Task 7: Split Share Capabilities and Move Share Plans to SQLite

**Files:**
- Create: `src-tauri/src/cloud/share/capabilities.rs`
- Create: `src-tauri/src/cloud/share/repository.rs`
- Create: `src-tauri/src/cloud/share/service.rs`
- Replace: `src-tauri/src/cloud/share.rs` with module exports and compatibility re-exports
- Modify: provider share implementations in `src-tauri/src/cloud/{quark,pikpak,baidu,tianyi,aliyun,xunlei,uc}.rs`
- Modify: `src-tauri/src/cloud/registry.rs`
- Test: `src-tauri/tests/cloud_provider_contract.rs`
- Test: `src-tauri/tests/cloud_directory_download.rs`
- Test: share module unit tests moved with production modules

**Interfaces:**
- Consumes: `ProviderRegistry`, stable share sources, current encrypted plan document, transfer journals, and owner fingerprint checks.
- Produces: `ShareInspect`, `ShareResolve`, optional `ShareTransfer`, optional `RecordedTransferResolver`, and `SqliteSharePlanRepository`.

- [ ] **Step 1: Write failing capability-presence tests**

Assert the registry reports each provider's actual share abilities and a provider without transfer support cannot be called through a default method that fails at runtime.

- [ ] **Step 2: Write failing repository migration and transaction tests**

Create a legacy encrypted plan file with two plans, open the SQLite repository, and assert both plans and journals migrate exactly once. Simulate a write failure and assert the prior row remains readable. Assert serialized database bytes contain no access code, cookie or token plaintext.

- [ ] **Step 3: Verify the tests fail**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract --test cloud_directory_download cloud::share`

Expected: FAIL because share behavior uses a fat optional trait and a whole-document encrypted file.

- [ ] **Step 4: Implement capability-specific traits**

```rust
pub trait ShareInspect { fn inspect(&self, input: &ShareInput) -> Result<ShareInspection, CloudError>; }
pub trait ShareResolve { fn resolve(&self, plan: &CloudSharePlan, entry: &CloudShareEntry) -> Result<ResolvedDownload, CloudError>; }
pub trait ShareTransfer { fn transfer(&self, plan: &CloudSharePlan, entries: &[CloudShareEntry]) -> Result<TransferOutcome, CloudError>; }
pub trait RecordedTransferResolver { fn refresh_recorded(&self, plan: &CloudSharePlan, entry: &CloudShareEntry) -> Result<ResolvedDownload, CloudError>; }
```

Register optional trait objects instead of default `ProviderUnsupported` methods.

- [ ] **Step 5: Implement row-based encrypted storage and migration**

Use SQLite transactions with one row per plan and journal entry. Encrypt each secret payload with DPAPI before insertion, retain public lookup fields separately, migrate the legacy file in one transaction, and rename it to a migrated backup only after commit.

- [ ] **Step 6: Preserve stable-source and security semantics**

Keep account-owner fingerprint validation, recorded personal-file refresh before expired-share access, signed URL non-persistence, cleanup journal behavior, and old source parsing byte-for-byte compatible.

- [ ] **Step 7: Verify share behavior**

Run the command from Step 3. Expected: all selected tests pass.

- [ ] **Step 8: Commit**

```text
refactor(cloud): split share capabilities and persist plans by row
```

### Task 8: Remove Long Network Work from Session Locks and Reclaim Source Gates

**Files:**
- Modify: `src-tauri/src/cloud/auth_supervisor.rs`
- Modify: `src-tauri/src/cloud/commands/share.rs`
- Modify: `src-tauri/src/cloud/commands/download.rs`
- Modify: `src-tauri/src/cloud/share/service.rs`
- Modify: `src-tauri/src/cloud/xunlei.rs`
- Test: `src-tauri/tests/cloud_coordination.rs`
- Test: `src-tauri/tests/cloud_directory_download.rs`
- Test: `src-tauri/tests/cloud_sessions.rs`

**Interfaces:**
- Consumes: session snapshot API, share-transfer capability, and `CloudDownloadSourceGate`.
- Produces: short refresh-only critical sections, cancellable transfer polling, and gate entries backed by `Weak<tokio::sync::Mutex<()>>`.

- [ ] **Step 1: Write failing lock-duration tests**

Pause a share transfer poll for one provider, then start an account or directory read for the same provider. Assert the read completes before polling is released. Assert a concurrent refresh still singleflights.

- [ ] **Step 2: Write failing source-gate reclamation tests**

Acquire and release 1,000 unique stable-source locks, invoke deterministic cleanup, and assert the gate map becomes empty. Hold one guard and assert its entry survives cleanup until release.

- [ ] **Step 3: Verify coordination tests fail**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_coordination --test cloud_directory_download --test cloud_sessions`

Expected: FAIL because transfer polling is performed under the provider operation lock and gate entries are retained forever.

- [ ] **Step 4: Implement snapshot-then-network orchestration**

Take immutable session and transfer-context snapshots under the supervisor, release the refresh lock, perform network polling, then compare-and-swap any rotated session by revision. Cancellation and timeout return without restoring stale credentials.

- [ ] **Step 5: Replace strong gate ownership with weak entries**

```rust
type SourceLockMap = HashMap<String, Weak<tokio::sync::Mutex<()>>>;
```

Upgrade or create on acquire and retain only entries whose weak reference has live owners during cleanup.

- [ ] **Step 6: Verify concurrency and cleanup**

Run the command from Step 3. Expected: all selected tests pass.

- [ ] **Step 7: Commit**

```text
perf(cloud): narrow session locks and reclaim source gates
```

### Task 9: Stream Upload Bodies and Schedule Bounded File Concurrency

**Files:**
- Modify: `src-tauri/src/cloud/upload.rs`
- Modify: `src-tauri/src/cloud/http.rs`
- Modify: `src-tauri/src/cloud/commands/upload.rs`
- Modify: provider upload implementations in `src-tauri/src/cloud/{pikpak,baidu,tianyi,aliyun,xunlei,uc}.rs`
- Modify: `src-tauri/src/cloud/registry.rs`
- Test: `src-tauri/tests/cloud_upload.rs`
- Test: `src-tauri/tests/cloud_xunlei_upload.rs`
- Test: `src-tauri/tests/cloud_baidu.rs`
- Test: `src-tauri/tests/cloud_tianyi.rs`
- Test: `src-tauri/tests/cloud_aliyun.rs`

**Interfaces:**
- Consumes: descriptor `max_upload_concurrency`, existing upload initialization/finalization, and local file paths.
- Produces: `UploadBody::Bytes` and `UploadBody::FileSlice`, streaming Reqwest bodies, cancellation-aware progress, and an ordered bounded scheduler.

- [ ] **Step 1: Write failing streaming tests**

Create a sparse fixture larger than the permitted in-memory test budget, request a file slice, and assert the fake transport reads exactly the requested offset and length without receiving a `Vec<u8>` containing the whole file. Assert a changed/truncated file fails before commit.

- [ ] **Step 2: Write failing scheduler tests**

With four recording uploads and a descriptor limit of two, assert maximum simultaneous transfers is two, results remain in input order, one failure does not cancel unrelated queued files, and cancellation prevents new transfers from starting.

- [ ] **Step 3: Verify upload tests fail**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_upload --test cloud_xunlei_upload --test cloud_baidu --test cloud_tianyi --test cloud_aliyun`

Expected: FAIL because request bodies are owned byte vectors and files are uploaded sequentially.

- [ ] **Step 4: Implement streaming request bodies**

```rust
pub enum UploadBody {
    Bytes(Vec<u8>),
    FileSlice { path: PathBuf, offset: u64, length: u64 },
}
```

Open and seek the file only when sending, wrap it in `Read::take(length)`, set an exact content length, report bytes as the reader advances, and reject metadata changes detected between initialization and transfer.

- [ ] **Step 5: Implement bounded scheduling**

Use the provider descriptor limit, default two, with a Tokio semaphore and blocking worker tasks. Collect `(input_index, result)` and restore input order. Do not hold the authorization refresh lock while reading or sending file data.

- [ ] **Step 6: Verify all affected upload implementations**

Run the command from Step 3. Expected: all selected tests pass.

- [ ] **Step 7: Commit**

```text
perf(cloud): stream uploads with bounded concurrency
```

### Task 10: Consolidate Contracts and Run Cross-Subsystem Verification

**Files:**
- Modify: `src-tauri/tests/cloud_provider_contract.rs`
- Modify: `src-tauri/tests/cloud_session.rs`
- Modify: `src/components/CloudManager.test.tsx`
- Modify: `src/components/CloudWorkspace.test.tsx`
- Modify: `docs/superpowers/specs/2026-09-12-cloud-provider-architecture.md` only if implementation names changed without changing the accepted behavior.

**Interfaces:**
- Consumes: all outputs from Tasks 1 through 9.
- Produces: a seven-provider contract matrix and release-build evidence.

- [ ] **Step 1: Add a table-driven seven-provider contract matrix**

For every provider, assert descriptor discovery, single-account replacement, authorization strategy order, root/recycle handling, stable personal source parsing, representative share parsing, drive capability, download capability, and declared upload/share optional capabilities.

- [ ] **Step 2: Verify the contract matrix can detect a missing provider**

Temporarily remove one descriptor in the test-controlled registry, run the test, and observe the expected failure; restore the descriptor and rerun to green before committing.

- [ ] **Step 3: Run the relevant frontend suite**

Run: `node_modules/.bin/vitest.cmd run src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx src/lib/cloudApi.test.ts src/lib/browserPasswordLogin.test.ts --exclude '.worktrees/**'`

Expected: all selected test files pass.

- [ ] **Step 4: Run the relevant Rust suite**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract --test cloud_sessions --test cloud_session --test cloud_tianyi --test cloud_aliyun --test cloud_baidu --test cloud_coordination --test cloud_directory_download --test cloud_upload --test cloud_xunlei_upload --test cloud_xunlei --test cloud_uc --test cloud_quark`

Expected: all selected test binaries pass with zero failures.

- [ ] **Step 5: Build the frontend**

Run: `pnpm run build`

Expected: TypeScript and Vite build exit 0.

- [ ] **Step 6: Build the Windows release binary**

Run: `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows.ps1`

Expected: release build exits 0 and publishes `artifacts/versadown.exe` with its required sidecar.

- [ ] **Step 7: Commit verification contracts**

```text
test(cloud): cover registry and authorization architecture
```
