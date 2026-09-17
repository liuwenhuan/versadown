# Xunlei Authorization Compatibility Implementation Plan

> 已废弃，不再按本计划修补旧代码。现行实现与验证见 `../specs/2026-09-17-xunlei-auth-plugin-rewrite.md`。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore automatic Xunlei web authorization completion and add a secure username/password entry that tolerates future official webpage module renumbering.

**Architecture:** Discover only already-loaded webpack exports by stable public shapes, with legacy numeric IDs as fallback, and convert a confirmed unsupported webpage shape into a fixed diagnostic. Reuse the existing official WebView password form path, but validate Xunlei through its asynchronous token bridge and root-directory request before persisting the password-authenticated session.

**Tech Stack:** JavaScript WebView bridge and Node test runner, Rust/Tauri cloud drivers, React/TypeScript with Vitest.

**Spec:** `docs/superpowers/specs/2026-09-17-xunlei-auth-compatibility.md`

## Global Constraints

- Passwords and tokens must never be logged or returned in diagnostics.
- Password input may run only on exact provider-owned HTTPS hosts.
- Unknown future Xunlei webpage versions must produce a clear fixed error after a signed-in page exposes an incompatible SDK shape.
- Run targeted Xunlei/Rust/UI tests and `scripts/build-windows.ps1`; do not run unrelated full suites.

---

### Task 1: Version-resilient Xunlei webpage extraction

**Files:**
- Modify: `scripts/test-xunlei-web-session.cjs`
- Modify: `src-tauri/src/cloud/xunlei-web-session.js`
- Modify: `src-tauri/src/cloud/common.rs`

**Interfaces:**
- Consumes: webpack runtime cache, `window.PAN_VERSION`, official profile fields `clientId`, `packageName`, `funName`.
- Produces: the existing selected token object, or `{ authorization_error: 'unsupported_web_version', client_version: string }`.

- [x] **Step 1: Write failing bridge tests**

Add synthetic `1.92.101` modules where profile, SSO, OAuth, device and signer use new IDs and `_oAuth2Client`; add renumbered cached-module coverage and an incompatible signed-in webpage diagnostic assertion.

- [x] **Step 2: Verify RED**

Run: `node --test scripts/test-xunlei-web-session.cjs`

Expected: the `1.92.101`, renumbered module and explicit incompatibility cases fail.

- [x] **Step 3: Implement cached-export discovery**

Enumerate `requireModule.c` exports without executing unknown module factories; use stable object/function shapes and the profile-selected signer name, retaining old IDs only as fallback.

- [x] **Step 4: Map the fixed incompatibility diagnostic**

Parse only the fixed code and sanitized client version in Rust and return `QuarkError::Provider("迅雷网页版本 <version> 暂不兼容，请更新应用后重试")`.

- [x] **Step 5: Verify GREEN**

Run: `node --test scripts/test-xunlei-web-session.cjs`

Run: `cargo test --manifest-path src-tauri/Cargo.toml xunlei_`

Expected: all selected tests pass.

### Task 2: Xunlei official-WebView password login

**Files:**
- Modify: `src-tauri/src/cloud/registry.rs`
- Modify: `src-tauri/src/cloud/credentials.rs`
- Modify: `src-tauri/src/cloud/session.rs`
- Modify: `src-tauri/src/cloud/driver.rs`
- Modify: `src-tauri/src/cloud/driver/native/xunlei_driver.rs`
- Modify: `src-tauri/src/cloud/commands/browser.rs`
- Modify: `src-tauri/src/cloud/commands/auth.rs`
- Modify: `src-tauri/src/cloud/commands/password_login_tests.rs`

**Interfaces:**
- Consumes: `PasswordLoginMode::OfficialWebView`, trusted login hosts, `WebAuthorizationRecipe`.
- Produces: `WebAuthorizationInput.auth_method`, a validated `CloudAccount { auth_method: Password }`, encrypted Xunlei credential storage, and reusable Xunlei WebView profile.

- [x] **Step 1: Write failing Rust tests**

Assert Xunlei advertises password login, trusts only exact official HTTPS hosts, owns a password WebView profile/key, and completes a browser-password candidate as `CloudAuthMethod::Password` only after root validation.

- [x] **Step 2: Verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml password_login`

Expected: Xunlei capability/profile/auth-method assertions fail.

- [x] **Step 3: Implement the minimal official-WebView path**

Enable password capability only when a non-`None` mode has trusted hosts; add Xunlei credential/reset keys and profile ownership; make the browser loop collect the provider recipe and call `complete_web` with `CloudAuthMethod::Password` for `OfficialWebView` providers.

- [x] **Step 4: Verify GREEN**

Run: `cargo test --manifest-path src-tauri/Cargo.toml password_login`

Run: `cargo test --manifest-path src-tauri/Cargo.toml xunlei_`

Expected: all selected tests pass.

### Task 3: Xunlei card entry and release verification

**Files:**
- Modify: `src/test/cloudCatalog.ts`
- Modify: `src/components/CloudManager.tsx`
- Modify: `src/components/CloudManager.test.tsx`

**Interfaces:**
- Consumes: catalog authorization capability `{ password: true, passwordLoginMode: 'officialWebView' }`.
- Produces: Xunlei account/password fields and a login action that calls `loginCloudWithPassword` while retaining webpage authorization.

- [x] **Step 1: Write failing component tests**

Update the catalog fixture and assert the Xunlei card shows account/password fields, submits them once, and keeps the webpage authorization button.

- [x] **Step 2: Verify RED**

Run: `npx vitest run src/components/CloudManager.test.tsx --exclude '.worktrees/**'`

Expected: Xunlei password-entry assertions fail.

- [x] **Step 3: Render password controls for supported official WebView login**

Replace the Rust-only UI predicate with the backend `authorization.password` capability while preserving existing input clearing, busy state and saved-password behavior.

- [x] **Step 4: Run targeted verification**

Run: `node --test scripts/test-xunlei-web-session.cjs`

Run: `cargo test --manifest-path src-tauri/Cargo.toml password_login`

Run: `cargo test --manifest-path src-tauri/Cargo.toml xunlei_`

Run: `npx vitest run src/components/CloudManager.test.tsx --exclude '.worktrees/**'`

Expected: all selected tests pass.

- [x] **Step 5: Build the Windows release artifact**

Run: `powershell -ExecutionPolicy Bypass -File scripts/build-windows.ps1`

Expected: release build succeeds and updates the worktree artifact.
