# Tianyi Native App Session Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Tianyi's signed desktop-client session the primary password-login transport and retain official WebView authorization as fallback.

**Architecture:** Add a focused App protocol adapter, embed its redacted session in the backward-compatible Tianyi session, and select App or browser transport per stored session. Validate the native path with an isolated real-account probe before promoting it to the default.

**Tech Stack:** Rust, Tauri, reqwest, RSA PKCS#1 v1.5, HMAC-SHA1, quick-xml, DPAPI, Vitest.

**Spec:** `docs/superpowers/specs/2026-09-12-tianyi-native-app-session-design.md`

## Global Constraints

- Never log or expose account, password, Cookie, token, redirect URL, session key/secret, or signature.
- Existing WebView sessions must remain readable and usable.
- Do not persist or accept MaurUppi's synthetic `APP_LOGIN_...` Cookie as authentication.
- Validate with read-only operations before replacing a stored session.
- Official WebView remains the fallback for provider challenges and protocol changes.
- Use focused validation only; do not run unrelated full suites.

---

### Task 1: Native App protocol and request signing

**Files:**
- Create: `src-tauri/src/cloud/tianyi_app.rs`
- Modify: `src-tauri/src/cloud/mod.rs`
- Modify: `src-tauri/Cargo.toml`
- Test: `src-tauri/tests/cloud_tianyi.rs`

**Interfaces:**
- Produce `TianyiAppSession`, `login_password`, `list_directory`, and `resolve_download`.
- Consume the existing `CloudHttpTransport` without WebView state.

- [ ] Write failing tests for the desktop login request sequence, safe parsing, signed root listing, and signed download resolution.
- [ ] Run the named Tianyi tests and confirm failure because the App protocol API does not exist.
- [ ] Implement RSA login parameter handling and HMAC request signing without logging secrets.
- [ ] Re-run the named tests and keep them green.

### Task 2: Backward-compatible session routing

**Files:**
- Modify: `src-tauri/src/cloud/tianyi.rs`
- Modify: `src-tauri/src/cloud/commands.rs`
- Test: `src-tauri/tests/cloud_tianyi.rs`
- Test: affected command/session unit tests in `src-tauri/src/cloud/commands.rs`

**Interfaces:**
- Extend `TianyiSession` with optional App state.
- Prefer App account/directory/download requests when available.
- Keep existing browser transport for Web sessions and official fallback.

- [ ] Write failing compatibility and routing tests.
- [ ] Run them and confirm App sessions still route through the old web API.
- [ ] Add App-first routing and preserve existing Web session behavior.
- [ ] Re-run focused tests.

### Task 3: Isolated real-account validation

**Files:**
- Modify: `scripts/verify-tianyi-password-live.cjs`
- Modify only test-gated command code if a safe diagnostic boundary is required.

**Interfaces:**
- Use the existing encrypted password record internally.
- Report only auth method, transport kind, safe status/error code, item count, and whether a download URL was resolved.

- [ ] Add a live mode that rejects browser-bridge use during the native probe.
- [ ] Build the candidate binary.
- [ ] Run the isolated read-only login/root/download-resolution probe.
- [ ] Promote App login as default only if the probe passes; otherwise retain current behavior and report the exact safe failure stage.

### Task 4: Recovery, regression, and release build

**Files:**
- Modify: `src-tauri/src/cloud/commands.rs`
- Test: `src-tauri/tests/cloud_tianyi.rs`
- Test: relevant cloud session/provider contract targets.

**Interfaces:**
- Reuse existing one-shot password recovery and official WebView authorization queue.

- [ ] Add failing tests for App expiry, wrong account, and WebView fallback.
- [ ] Implement minimal recovery/fallback routing.
- [ ] Run focused Tianyi/session/contract tests.
- [ ] Run relevant frontend tests only if the IPC contract changed.
- [ ] Run `scripts/build-windows.ps1` and report the produced binary.

