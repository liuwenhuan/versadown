# Linux 迅雷原生 Worker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable the existing Xunlei cloud-download path to submit direct-download tasks to an installed Linux Xunlei SDK through an isolated, credential-safe Rust worker, while retaining the same task semantics and Windows implementation where they are genuinely shared.

**Architecture:** Keep `XunleiNativeBackend` as the sole cross-platform application facade. Move task ownership, persisted native IDs, output reservation, state transitions, progress normalization, and fallback decisions behind a shared `NativeEngine` contract. Retain the existing Windows proxy/broker implementation behind a Windows engine adapter. On Linux, discover either installed DEB or Linyaps runtimes dynamically, start the application executable in a private `--xunlei-native-worker` mode, and communicate through a nonce-authenticated Unix socket. The worker is a logical sidecar but is deliberately a mode of the shipped executable, avoiding another binary/package-registration path.

**Tech Stack:** Rust 2021, Tauri 2, `serde`/`serde_json`, `uuid`, Unix-domain sockets and permissions from the Rust standard library, `libloading` for Linux `dlopen`, existing React/Tauri task state flow, pnpm quick Linux build.

**Spec:** `docs/superpowers/specs/2026-09-22-linux-xunlei-native-worker-design.md`

## Global constraints

- Reuse the cloud resolver's in-memory account identity, peer ID, file metadata, signed direct URL, headers, and short-lived acceleration token. Never read, write, parse, or log official Xunlei cookies, database files, task files, configuration, or IPC.
- Linux and the official Xunlei client share only immutable installed SDK files. They must never share task ownership, a task database, an IPC connection, or a backend process.
- Do not call an unverified private Linux SDK signature. A runtime fingerprint and its observed FFI definitions are one compatibility unit; unknown or mismatched libraries return `Ok(None)` before native task creation so aria2 remains eligible.
- Do not persist raw numerical SDK IDs as a sufficient authority. A native GID encodes the engine session UUID plus task ID, and only a currently owned matching session can be controlled.
- A native task that has already been created must be stopped and released if a later native step fails. It must not silently be sent to aria2, which could duplicate the output.
- The worker command line contains only runtime path and socket metadata. Its one-time authentication nonce enters through inherited stdin; URL, headers, token, user ID, and peer ID remain in process memory/socket messages and are excluded from Debug/error output.
- Preserve user-visible existing Xunlei strings during this first worker delivery. Any newly required message uses an i18n key with Simplified Chinese and English translations; do not add hard-coded UI text.
- Scope is Linux x86_64 direct cloud-file P2SP tasks. Do not add macOS ABI support, torrent/magnet/ed2k support, Electron/Node invocation, vendor file patching, or an official-client task bridge.

## Review focus

- Windows remains a thin adapter to the same shared contract; no Linux-only task-state fork may appear in dispatch or command handlers.
- The parent process must never call `dlopen` on the SDK, and every Linux SDK call stays on one worker thread.
- Runtime discovery must not hard-code an observed Linyaps layer hash and must select highest semantic version, with DEB as the equal-version tie break.
- Integration must keep current ordinary-download fallback only for pre-creation native unavailability, and retain correct scheduler slot release for terminal and worker-crash paths.
- Tests use fakes for all task lifecycle conditions. Dynamic-loader testing is opt-in and never turns SDK-less CI into a failure.

---

## File map

| Path | Responsibility |
| --- | --- |
| `src-tauri/src/xunlei_native.rs` | Existing public backend refactored into the platform-neutral facade and current Windows implementation wrapper. |
| `src-tauri/src/xunlei_native/engine.rs` | Shared `NativeEngine` contract, session-qualified handle/GID codec, normalized submission/progress/state types. |
| `src-tauri/src/xunlei_native/linux/runtime.rs` | DEB/Linyaps runtime discovery, validation, fingerprinting, deterministic selection. |
| `src-tauri/src/xunlei_native/linux/protocol.rs` | Versioned parent/worker request/response types and length-framed socket codec. |
| `src-tauri/src/xunlei_native/linux/worker_engine.rs` | Parent-side single-worker lifecycle, authenticated Unix socket client, and `NativeEngine` adapter. |
| `src-tauri/src/xunlei_native/linux/sdk.rs` | Fingerprint-gated Linux loader and verified SDK bindings; loaded only by worker mode. |
| `src-tauri/src/xunlei_native/linux/worker.rs` | Worker command loop, SDK lifetime, task ownership and secret-safe error boundary. |
| `src-tauri/src/main.rs`, `src-tauri/src/lib.rs` | Early `--xunlei-native-worker` multicall entry point before Tauri initialization; application data path supplied to backend. |
| `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`, `package.json` | Add loader dependency and ensure quick Linux builds build both executable modes. |
| `src-tauri/tests/xunlei_native_linux_worker.rs` | Public-contract integration tests with a fake worker; optional real runtime smoke test. |

## Task 1: Establish the shared engine contract and safe native task identity

**Files:**
- Create: `src-tauri/src/xunlei_native/engine.rs`
- Modify: `src-tauri/src/xunlei_native.rs`
- Modify: `src-tauri/src/cloud/download_dispatch.rs`
- Modify: `src-tauri/src/commands.rs`
- Test: `src-tauri/src/xunlei_native/engine.rs` (`#[cfg(test)]` module)

- [ ] **Step 1: Write failing contract and codec tests.**

  Cover parsing/formatting `xunlei-native:<session-uuid>:<task-id>`, reject the legacy ID-only format, reject malformed UUIDs/zero IDs, and verify a task handle from session A cannot be used by an engine instance for session B. Add tests that a resume clears the old handle and must submit a newly resolved request rather than calling control on the old numeric ID.

- [ ] **Step 2: Run the targeted test to prove the contract does not exist.**

  Run: `cargo test --manifest-path src-tauri/Cargo.toml xunlei_native::engine::tests -- --nocapture`

  Expected: compile/test failure because the shared engine module and session-qualified GID codec have not been introduced.

- [ ] **Step 3: Implement only the platform-neutral contract.**

  Add `NativeSubmission`, `NativeTaskHandle`, `NativeProgress`, `NativeEngine`, `NativeEngineError`, and a private session-qualified GID codec in `engine.rs`. Keep URL, headers, tokens, user ID, and peer ID out of `Debug` implementations; error variants carry stable reason codes or generic safe text only. Move `NativeProgress` and transfer-meter normalization from `xunlei_native.rs` to this module without assuming the Windows fixed-size response layout.

  Refactor `XunleiNativeBackend` to own a mutex-protected platform engine enum. Wrap `WindowsApi` in `WindowsNativeEngine` under `cfg(windows)` so its private proxy/broker behavior is unchanged. Update current dispatch/control paths to use the codec rather than `native_task_id_from_gid`; retain the existing re-resolve-on-resume behavior.

- [ ] **Step 4: Run targeted tests.**

  Run: `cargo test --manifest-path src-tauri/Cargo.toml xunlei_native::engine::tests -- --nocapture`

  Expected: PASS.

- [ ] **Step 5: Commit this isolated refactor.**

  ```bash
  git add src-tauri/src/xunlei_native.rs src-tauri/src/xunlei_native/engine.rs src-tauri/src/cloud/download_dispatch.rs src-tauri/src/commands.rs
  git commit -m "refactor: share Xunlei native engine contract"
  ```

## Task 2: Implement deterministic Linux runtime discovery

**Files:**
- Create: `src-tauri/src/xunlei_native/linux/mod.rs`
- Create: `src-tauri/src/xunlei_native/linux/runtime.rs`
- Modify: `src-tauri/src/xunlei_native.rs`
- Test: `src-tauri/src/xunlei_native/linux/runtime.rs` (`#[cfg(test)]` module)

- [ ] **Step 1: Write failing discovery tests against fixture roots.**

  Use temporary directories and injectable command/metadata readers to cover: valid DEB layout; a valid Linyaps layer whose `info.json` has exact `id: com.xunlei.download`; invalid package ID; missing stat SDK; non-ELF library; non-x86_64 ELF; highest version selection; equal-version DEB preference; and proof that a Linyaps candidate is represented only by its current root/version, never a persisted layer commit.

- [ ] **Step 2: Run discovery tests to prove the implementation is absent.**

  Run: `cargo test --manifest-path src-tauri/Cargo.toml xunlei_native::linux::runtime::tests -- --nocapture`

  Expected: compile/test failure before `linux` discovery exists.

- [ ] **Step 3: Implement discovery and validation.**

  Define `LinuxXunleiRuntime { source, version, root, stat_library, sdk_library, sdk_sha256 }`. Discover the DEB candidate only when `dpkg-query` confirms `com.xunlei.download`; discover Linyaps candidates from `ll-cli --json list` plus readable layer `info.json`. Validate both required files as ELF x86_64 before accepting. Recompute candidate paths on each worker launch; do not save layer hashes. Sort semver versions descending and use DEB only as the exact-version tie break.

  Do not load either library in discovery. Export a narrow `discover_best_runtime()` for the Linux engine; non-Linux compilation receives no platform implementation.

- [ ] **Step 4: Run discovery tests.**

  Run: `cargo test --manifest-path src-tauri/Cargo.toml xunlei_native::linux::runtime::tests -- --nocapture`

  Expected: PASS.

- [ ] **Step 5: Commit discovery.**

  ```bash
  git add src-tauri/src/xunlei_native.rs src-tauri/src/xunlei_native/linux/mod.rs src-tauri/src/xunlei_native/linux/runtime.rs
  git commit -m "feat: discover installed Linux Xunlei runtime"
  ```

## Task 3: Add a nonce-authenticated no-UI worker transport

**Files:**
- Create: `src-tauri/src/xunlei_native/linux/protocol.rs`
- Create: `src-tauri/src/xunlei_native/linux/worker_engine.rs`
- Create: `src-tauri/src/xunlei_native/linux/worker.rs`
- Modify: `src-tauri/src/xunlei_native/linux/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/main.rs`
- Test: `src-tauri/src/xunlei_native/linux/protocol.rs` (`#[cfg(test)]` module)
- Test: `src-tauri/tests/xunlei_native_linux_worker.rs`

- [ ] **Step 1: Write failing wire and lifecycle tests using a fake worker.**

  Test length framing rejects truncated/oversized frames and requests with a bad nonce. The fake worker must record initialize/submit/poll/pause/resume/cancel/release/shutdown commands. Verify credentials are present in an in-memory `Initialize` or `Submit` payload but are absent from `Debug` output and startup argv. Verify an unexpected child exit converts only that worker session's live native tasks to safely paused and rejects later control using those handles.

- [ ] **Step 2: Run the new targeted tests.**

  Run: `cargo test --manifest-path src-tauri/Cargo.toml xunlei_native_linux_worker -- --nocapture`

  Expected: compile/test failure because no protocol/client/worker entry point exists.

- [ ] **Step 3: Implement the transport and multicall worker entry.**

  Use a versioned, length-prefixed JSON protocol over `std::os::unix::net::UnixStream`. Parent creates a unique `0700` runtime directory and a `0600` listener socket, starts `current_exe --xunlei-native-worker --socket <path> --runtime-root <path>`, then writes a random one-use nonce to stdin. The worker authenticates before accepting commands and deletes nothing outside the parent-created directory.

  Add an early argument check in `main.rs` that calls a public, non-Tauri `run_xunlei_native_worker()` in `lib.rs`, then exits. The normal Tauri `run()` path must remain untouched when the flag is absent. `LinuxWorkerEngine` owns exactly one child/socket/session per application backend, serializes requests under one lock, and maps protocol replies into the shared `NativeEngine` contract. It stops/reaps the child during shutdown and returns a safe worker-unavailable error on I/O or child failure.

- [ ] **Step 4: Run targeted protocol/lifecycle tests.**

  Run: `cargo test --manifest-path src-tauri/Cargo.toml xunlei_native_linux_worker -- --nocapture`

  Expected: PASS. The fake worker verifies the parent/worker process contract without loading vendor code.

- [ ] **Step 5: Commit the worker transport.**

  ```bash
  git add src-tauri/src/main.rs src-tauri/src/lib.rs src-tauri/src/xunlei_native/linux src-tauri/tests/xunlei_native_linux_worker.rs
  git commit -m "feat: add isolated Linux Xunlei worker transport"
  ```

## Task 4: Bind only the verified Linux SDK ABI inside the worker

**Files:**
- Create: `src-tauri/src/xunlei_native/linux/sdk.rs`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/Cargo.lock`
- Modify: `src-tauri/src/xunlei_native/linux/worker.rs`
- Test: `src-tauri/src/xunlei_native/linux/sdk.rs` (`#[cfg(test)]` module)
- Test: `src-tauri/tests/xunlei_native_linux_worker.rs`

- [ ] **Step 1: Capture the installed SDK compatibility evidence before writing production calls.**

  In the dedicated ABI test harness only, record the SHA-256 of the discovered `libxl_thunder_sdk.so`, enumerate the required `XL*` symbols, and use a controlled disposable direct-file task plus `ThunderKernel.node` argument marshalling to establish the current signatures, initialization ownership, return conventions, and the exact `XLGetTaskInfo(Ex)` field layout. Store the observed fingerprint and reconstructed binding definition together in the code/test fixture. Stop this task and report the evidence if any function signature remains unproven; do not substitute guessed C declarations.

- [ ] **Step 2: Write failing fingerprint-gate tests.**

  Create a fake loader interface. Assert unknown fingerprint rejects before any `XL*` function is invoked; assert the stat library is opened before the SDK with `RTLD_GLOBAL`; assert missing exports fail initialization; assert response decoding uses the observed layout; and assert cancellation/release is invoked if start/DCDN setup fails after task creation.

- [ ] **Step 3: Run targeted loader tests.**

  Run: `cargo test --manifest-path src-tauri/Cargo.toml xunlei_native::linux::sdk::tests -- --nocapture`

  Expected: compile/test failure before the gate and loader exist.

- [ ] **Step 4: Implement the fingerprint-gated worker SDK.**

  Add `libloading` and a small `LinuxSdk` type with no public raw pointers. In worker mode only, open `libxl_stat.so` first using `RTLD_NOW | RTLD_GLOBAL`, retain its library handle, then open the SDK. Resolve exactly the required symbols. Accept the discovered runtime only when its SHA-256 is one of explicit ABI definitions backed by the Task 4 evidence. Translate the verified ABI to `initialize`, `submit`, `poll`, `pause`, `resume`, `cancel`, and `release`; make all FFI calls from the worker's command thread. Normalize total/completed bytes, speed, task state, and DCDN/VIP cumulative bytes into shared `NativeProgress`.

  No parent-side module may import `libloading` or `LinuxSdk`. Errors sent across the protocol are code-only and omit all credential values and URLs.

- [ ] **Step 5: Run loader and fake-worker tests.**

  Run: `cargo test --manifest-path src-tauri/Cargo.toml xunlei_native::linux::sdk::tests xunlei_native_linux_worker -- --nocapture`

  Expected: PASS.

- [ ] **Step 6: Commit the verified ABI adapter.**

  ```bash
  git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/xunlei_native/linux/sdk.rs src-tauri/src/xunlei_native/linux/worker.rs src-tauri/tests/xunlei_native_linux_worker.rs
  git commit -m "feat: bind verified Linux Xunlei SDK worker"
  ```

## Task 5: Connect common task lifecycle, fallback, and SVIP state to Linux

**Files:**
- Modify: `src-tauri/src/xunlei_native.rs`
- Modify: `src-tauri/src/xunlei_native/engine.rs`
- Modify: `src-tauri/src/xunlei_native/linux/worker_engine.rs`
- Modify: `src-tauri/src/cloud/download_dispatch.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: `src-tauri/src/xunlei_native.rs` (`#[cfg(test)]` module)
- Test: `src-tauri/tests/xunlei_native_linux_worker.rs`

- [ ] **Step 1: Write failing backend lifecycle tests through a fake `NativeEngine`.**

  Cover pre-submit runtime unavailable falling back once to aria2; successful submit reserving output and storing a session-qualified GID; completion, cancellation, and post-create error releasing the handle and scheduler slot; native pause stopping only the matching session; native resume re-resolving credentials then making a new native submission; worker loss moving task to paused rather than controlling a reused number; and SVIP state requiring token acceptance, DCDN enabled, and a later positive DCDN/VIP byte delta.

- [ ] **Step 2: Run the targeted lifecycle tests.**

  Run: `cargo test --manifest-path src-tauri/Cargo.toml xunlei_native::tests xunlei_native_linux_worker -- --nocapture`

  Expected: failures demonstrate the Linux worker is not yet attached to common backend semantics.

- [ ] **Step 3: Implement platform selection at the facade boundary.**

  Pass the app data/runtime directory to `XunleiNativeBackend` at construction. On Linux, lazily create `LinuxWorkerEngine` only after the existing resolver has supplied all mandatory marker fields and after output reservation is ready. On Windows, route the same `NativeSubmission` to `WindowsNativeEngine`. On other platforms return `Ok(None)` with no new user message.

  Keep all monitor updates in the shared backend. Update the task store with a session-qualified GID and never call `control` or `release` unless the stored session matches the live engine session. Distinguish pre-create `Unavailable` (regular aria2 may proceed) from post-create failures (release, mark the native task failed/paused, no automatic aria2 duplication). Keep existing user strings where possible; if a distinct worker-specific visible message is unavoidable, introduce a key and paired `zh-CN`/`en-US` translations before rendering it.

- [ ] **Step 4: Run targeted lifecycle tests.**

  Run: `cargo test --manifest-path src-tauri/Cargo.toml xunlei_native::tests xunlei_native_linux_worker -- --nocapture`

  Expected: PASS.

- [ ] **Step 5: Commit lifecycle integration.**

  ```bash
  git add src-tauri/src/xunlei_native.rs src-tauri/src/xunlei_native/engine.rs src-tauri/src/xunlei_native/linux/worker_engine.rs src-tauri/src/cloud/download_dispatch.rs src-tauri/src/commands.rs src-tauri/src/lib.rs src-tauri/tests/xunlei_native_linux_worker.rs
  git commit -m "feat: route Linux Xunlei tasks through shared native engine"
  ```

## Task 6: Verify the installed runtime and produce the credential-test binary

**Files:**
- Modify if required by the build result: `package.json`
- Test: `src-tauri/tests/xunlei_native_linux_worker.rs`

- [ ] **Step 1: Add an opt-in real runtime smoke test.**

  Gate it behind `VERSADOWN_XUNLEI_LIVE_TEST=1`. It may discover and load the local runtime inside worker mode, prove fingerprint/symbol readiness, create no production task by default, and always release/shut down. Without the environment flag or runtime, it is skipped rather than failing CI.

- [ ] **Step 2: Run all changed targeted Rust tests.**

  Run: `cargo test --manifest-path src-tauri/Cargo.toml xunlei_native -- --nocapture`

  Expected: PASS for shared engine, discovery, protocol, fake worker, and lifecycle tests.

- [ ] **Step 3: Run the opt-in local SDK smoke test after its ABI gate is present.**

  Run: `VERSADOWN_XUNLEI_LIVE_TEST=1 cargo test --manifest-path src-tauri/Cargo.toml xunlei_native_linux_worker::live_runtime -- --ignored --nocapture`

  Expected: PASS only when the installed SDK matches the recorded, verified fingerprint. A mismatch is reported as unavailable, not worked around.

- [ ] **Step 4: Produce the authorized intermediate Linux binary.**

  Run: `pnpm run build:linux:quick`

  Expected: PASS and emit the runnable debug binary at `/home/deepin/codework/versadown/.worktrees/cross-platform-desktop/src-tauri/target/debug/versadown`. This is a quick/debug build, not a release build or package.

- [ ] **Step 5: Hand off credential and SVIP validation.**

  Give the user that exact binary path. They log in through VersaDown's normal Xunlei flow so credentials enter its system keyring, then choose a user-authorized legal cloud file in a disposable output directory. With the user's explicit test-resource authorization, validate nonzero progress, pause, re-resolve-and-resume, cancel/cleanup, and that SVIP is displayed as confirmed only after the token, DCDN enablement, and DCDN/VIP byte growth evidence are all observed.

- [ ] **Step 6: Commit build/test wiring only if source changed.**

  ```bash
  git add package.json src-tauri/tests/xunlei_native_linux_worker.rs
  git commit -m "test: cover Linux Xunlei worker runtime"
  ```

## Plan self-review

- [x] Every behavior in the approved design is assigned: runtime discovery (Task 2), isolation/protocol (Task 3), evidence-gated ABI (Task 4), shared lifecycle/SVIP state (Task 5), and user credential verification (Task 6).
- [x] Shared Windows/Linux task semantics are implemented before platform routing; Windows ABI is explicitly preserved rather than copied into Linux.
- [x] All platform-sensitive work is confined to `xunlei_native/linux/*` or the existing Windows-gated implementation.
- [x] The plan forbids guessed FFI signatures, token leakage, client-data access, task takeover, duplicate aria2 fallback, and hard-coded Linyaps commits.
- [x] Each implementation unit starts with a focused failing test and ends with an exact targeted command and a scoped commit.
- [x] The only binary compilation is the quick Linux build explicitly requested for the intermediate credential test; no release build or installation package is planned.
