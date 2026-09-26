# Cloud Session Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a reusable, encrypted cloud-session lifecycle that renews all existing provider sessions and automatically uses saved password credentials for PikPak, Baidu and Tianyi after expiration.

**Architecture:** A new pure Rust session module owns provider policies, separately encrypted password credentials and retry safety. Commands retain adapter-specific login/session writes and invoke the shared lifecycle for every cloud operation. The existing frontend exposes only supported password entry points and a security disclosure.

**Tech Stack:** Rust, Tauri, `zeroize`, Windows DPAPI, React, TypeScript, Vitest.

**Spec:** `docs/superpowers/specs/2026-09-10-cloud-session-lifecycle-design.md`

## Global Constraints

- Keep normal provider sessions and password fallback secrets in separate DPAPI records.
- Never log, serialize to UI, add to download headers, or include passwords in errors.
- Retry only `SessionWorkKind::Read`; never replay mutations or logout.
- CAPTCHA, SMS, risk challenge, network failure and provider rejection must not be converted into automatic password retries.
- Verified direct password login remains limited to PikPak, Baidu and Tianyi.
- Use TDD: observe each new test fail before adding its production behavior.
- Run targeted tests and `scripts/build-windows.ps1`; do not run unrelated full suites.

---

### Task 1: Shared lifecycle and encrypted fallback credentials

**Files:**
- Create: `src-tauri/src/cloud/session.rs`
- Create: `src-tauri/tests/cloud_session.rs`
- Modify: `src-tauri/src/cloud/mod.rs`
- Modify: `src-tauri/src/cloud/credentials.rs`
- Modify: `src-tauri/src/cloud/types.rs`

**Interfaces:**
- Produces `SessionWorkKind`, `CloudPasswordCredential`, `CloudSessionPolicy`, `save_password_credential`, `load_password_credential`, `remove_password_credential`, `clear_password_credential_if_account_differs`, and `run_with_session_recovery`.
- Consumed by `commands.rs` in Task 2. `run_with_session_recovery` invokes a relogin callback once only for `AuthorizationExpired` or `NotAuthorized` under a password-capable policy.

- [x] **Step 1: Write failing lifecycle tests**

```rust
#[test]
fn expired_read_relogs_once_and_retries_once() { /* first work returns AuthorizationExpired; second returns value */ }
#[test]
fn expired_mutation_relogs_but_never_replays_work() { /* assert one work call and fixed retry error */ }
#[test]
fn fallback_password_is_redacted_and_removed_on_logout_or_account_switch() { /* no plaintext in Debug */ }
```

- [x] **Step 2: Run the focused test target**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_session`

Expected: fail because the `session` module and exported API do not exist.

- [x] **Step 3: Implement the minimal generic lifecycle**

```rust
pub enum SessionWorkKind { Read, Mutation, Logout }
pub fn run_with_session_recovery<T>(provider: CloudProvider, store: &impl CredentialStore, kind: SessionWorkKind, work: impl FnMut() -> Result<T, QuarkError>, relogin: impl FnOnce(&CloudPasswordCredential) -> Result<(), QuarkError>) -> Result<T, QuarkError>;
```

Use distinct allowed DPAPI keys such as `cloud-password-pikpak`; zeroize password material after a relogin attempt; return a fixed non-secret error for mutation recovery.

- [x] **Step 4: Run focused lifecycle tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_session`

Expected: pass.

### Task 2: Command integration and provider password fallback

**Files:**
- Modify: `src-tauri/src/cloud/commands.rs`
- Modify: `src-tauri/tests/cloud_commands_session.rs` or existing `src-tauri/src/cloud/commands.rs` test module

**Interfaces:**
- Consumes all Task 1 lifecycle APIs.
- Produces `recover_provider_password_session(provider, store, http, credential)` and routes cloud commands through the lifecycle with `Read` or `Mutation` kind.

- [x] **Step 1: Write failing command tests**

```rust
#[test]
fn expired_baidu_read_uses_matching_saved_password_and_retries() { /* fake transport verifies one relogin */ }
#[test]
fn expired_tianyi_mutation_reauthenticates_without_replaying_mutation() { /* one mutation call */ }
#[test]
fn password_login_saves_fallback_only_after_matching_validated_account() { /* no raw password assertion */ }
```

- [x] **Step 2: Run command-session tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --lib cloud::commands::tests --test cloud_session`

Expected: fail because commands do not use the lifecycle wrapper or preserve fallback credentials.

- [x] **Step 3: Implement command integration**

```rust
fn run_cloud_work<T>(provider: CloudProvider, kind: SessionWorkKind, store: &impl CredentialStore, http: &ReqwestTransport, work: impl FnMut() -> Result<T, QuarkError>) -> Result<T, QuarkError>;
```

Use `PikPakClient::login`, `BaiduClient::login_password`, and `TianyiClient::login_password` in temporary sessions; validate the account ID equals the fallback credential before saving their real sessions. Save fallback credentials only after direct password login succeeds. Clear them on logout and different-account web authorization. Mark every list/account/download-resolution path `Read`; mark file changes and upload `Mutation`.

- [x] **Step 4: Run targeted Rust tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_session --test cloud_xunlei --test cloud_upload --lib cloud::commands::tests`

Expected: pass without printing password material.

### Task 3: Password-login UI disclosure and lifecycle UX

**Files:**
- Modify: `src/components/CloudManager.tsx`
- Modify: `src/components/CloudManager.test.tsx`
- Modify: `src/lib/cloudTypes.ts`

**Interfaces:**
- Consumes existing `CloudAuthorizationCapabilities.password` and backend’s fixed mutation-recovered error code.
- Produces safe password-storage messaging only for password-capable providers; no password state persists in React after submit.

- [x] **Step 1: Write failing component tests**

```tsx
it('explains encrypted automatic recovery only for a password-capable provider', async () => { /* PikPak text exists; Xunlei text absent */ });
it('asks to repeat a mutation after the session was automatically restored', async () => { /* fixed backend code */ });
```

- [x] **Step 2: Run the component test target**

Run: `node node_modules/vitest/vitest.mjs run src/components/CloudManager.test.tsx --exclude '.worktrees/**'`

Expected: fail because no disclosure or recovered-mutation message exists.

- [x] **Step 3: Implement minimal UI behavior**

```tsx
{effectiveAuthorizationCapabilities.password && <p className="cloud-login-security-note">账号和密码仅加密保存在本机，用于登录失效后的自动恢复。</p>}
```

Map only the backend’s fixed recovered-mutation code to a “已自动恢复登录，请重新执行本次操作” message; retain existing CAPTCHA/web authorization flow.

- [x] **Step 4: Run focused component tests**

Run: `node node_modules/vitest/vitest.mjs run src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx --exclude '.worktrees/**'`

Expected: pass.

### Task 4: Integration verification and release

**Files:**
- Modify: `docs/xunlei-protocol-analysis-2026-09-10.md` only if current behavior or verification claims need correction.

- [x] **Step 1: Verify formatting and selected tests**

Run: `git diff --check` and the Task 1–3 commands.

- [x] **Step 2: Build runnable binary**

Run: `scripts/build-windows.ps1`

Expected: publishes `artifacts/versadown.exe` without creating an installer.

- [x] **Step 3: Perform safe local smoke checks**

Start an isolated data-directory app and verify a saved session restores. Do not inject, print, or alter real passwords; do not delete user cloud files.
