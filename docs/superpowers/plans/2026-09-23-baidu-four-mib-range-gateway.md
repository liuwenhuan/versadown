# Baidu Four-MiB Range Gateway Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Download Baidu personal files and transferred shares using upstream Range requests of at most 4 MiB while preserving existing aria2 task behavior.

**Architecture:** An app-owned loopback HTTP gateway authenticates each task URL with an ephemeral random token. It subdivides aria2 ranges into validated upstream responses, then streams them back for aria2 to write and resume. The cloud submission path registers Baidu URLs with this gateway and bypasses the user's proxy only on the local hop.

**Tech Stack:** Rust, Tokio, Axum, Reqwest, aria2-next loopback RPC.

**Spec:** `docs/superpowers/specs/2026-09-23-baidu-four-mib-range-gateway-design.md`

## Global Constraints

- Every Baidu upstream GET uses an explicit Range no larger than 4,194,304 bytes; no ordinary Baidu GET or HEAD is sent.
- Only Baidu personal, transferred-share, and directory-member downloads change; other providers and generic HTTP retain their current path.
- The signed URL, Cookie, and proxy credentials remain in memory; the local URL contains only a random token and listens only on `127.0.0.1`.
- No new hard-coded interface strings; any new user-facing text must have Simplified Chinese and English localization keys.
- Use targeted Rust tests; do not build an application binary or run the entire suite without a risk-based reason.

## Review Focus

- Upstream 200 response to an explicit Range must fail without committing the segment.
- Redirects must not forward Baidu credentials to an unrelated host.
- A short final segment must use the exact remaining byte count.
- A changed file total across segments must abort the stream.
- An engine request with no Range must still avoid an upstream full-file GET.

---

### Task 1: Bounded, validated loopback gateway

**Files:** Create `src-tauri/src/cloud/baidu_range_gateway.rs`; modify `src-tauri/src/cloud/mod.rs` and `src-tauri/Cargo.toml`; test in `src-tauri/tests/baidu_range_gateway.rs`.

**Interfaces:** `BaiduRangeGateway::start() -> Result<Self, GatewayError>`; `register(task_id, upstream_url, headers, proxy) -> Result<GatewayRegistration, GatewayError>`; `retire(task_id, token)` and `remove_task(task_id)`; `GatewayRegistration::url()`.

- [x] Write an Axum test origin that returns 403 when the Range is absent or exceeds 4 MiB. Add a gateway test that requests a >12 MiB local Range, checks every captured upstream Range, and compares response bytes with a hand-generated payload.
- [x] Run `cargo test --manifest-path src-tauri/Cargo.toml --test baidu_range_gateway` and confirm it fails because the gateway is absent.
- [x] Implement loopback binding, random-token registry, explicit Range planning, exact `Content-Range`/body-length validation, 4 MiB bounded buffering, and local response streaming.
- [x] Add tests for 200, 403, truncated body, conflicting total, no Range, HEAD, and a short tail; rerun the same test target and verify green.

### Task 2: Cloud submission and lifecycle wiring

**Files:** Modify `src-tauri/src/cloud/commands/download.rs`, `src-tauri/src/commands.rs`, `src-tauri/src/lib.rs`, `src-tauri/src/aria2/service.rs`; test in `src-tauri/src/cloud/commands/tests.rs` and `src-tauri/tests/downloader.rs`.

**Interfaces:** Consume Task 1 `register` before aria2 submission, replace only Baidu URL, remove upstream authentication headers from aria2 options, and mark the loopback hop as direct while preserving the proxy in the gateway target. Retire old tokens only after the refreshed URL is accepted; remove terminal task targets.

- [x] Write a failing Baidu submission-policy test proving the upstream proxy and credentials are not sent to aria2's local hop, while non-Baidu options are unchanged.
- [x] Run the targeted test and confirm the intended failure.
- [x] Add a lazy app-owned gateway and a Baidu branch immediately after the existing `share_submission_options` call; preserve existing stable task identity, native checkpoint, and cloud refresh behavior.
- [x] Run targeted `cargo test` filters for cloud submission and aria2 options; verify green.

### Task 3: Real engine and Baidu verification

**Files:** Add `src-tauri/tests/baidu_range_gateway.rs` with bundled-engine and ignored live-account tests.

**Interfaces:** Use Task 1 gateway and Task 2 submission options with the bundled aria2-next executable and an isolated temporary output directory.

- [x] Add a failing >32 MiB integration test whose origin rejects oversized requests and whose expected output is a byte-for-byte fixture. Cover pause/resume and URL refresh with a changed token.
- [x] Run that one test to observe the pre-fix failure; implement only the integration adjustments needed for the public gateway contract; rerun it green.
- [x] With the current saved Baidu account, first probe multiple real 4 MiB ranges and then attempt the named failed file in a temporary output directory. Report whether the whole file completed, exact byte count, and any server/engine error; never print the signed URL or Cookie.
- [x] Run `git diff --check` and the targeted gateway, cloud, and aria2 tests. Do not claim the UI artifact was rebuilt.

**Native resume finding:** aria2-next requires the checkpoint's old URI to be present while a new GID matches saved ranges. Removing that URI before transfer starts causes error 13 (existing output). The fresh loopback URI is first; an integration test shuts down the original gateway and confirms the restored transfer finishes through the new gateway with byte-identical output.
