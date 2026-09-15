# Magnet Metadata Retry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Retry transient metadata-service failures before falling back to aria2 DHT, without retrying definite client errors such as HTTP 400.

**Architecture:** `resolve_magnet_torrent` owns the retry policy because all configured metadata-service requests pass through it. It will reuse one `reqwest::Client`, retry only network failures and 429/502/503/504 responses with bounded short backoff, and retain the existing `MagnetMetadataError` behaviour after the final attempt.

**Tech Stack:** Rust, Tokio, reqwest, Axum test server, Cargo.

**Spec:** User request in this conversation: retry the peer metadata service after HTTP 503 instead of abandoning it after one connection failure, while preserving accurate errors for invalid links.

## Global Constraints

- Attempt the metadata request at most three times.
- Retry only transport failures and HTTP 429, 502, 503 or 504; do not retry HTTP 400.
- Preserve the 20-second total request timeout and the existing safe torrent URL validation.
- Run `cargo test --test magnet_metadata` and `scripts/build-windows.ps1`; do not run unrelated full test suites.

---

### Task 1: Capture retry policy with focused integration tests

**Files:**
- Modify: `src-tauri/tests/magnet_metadata.rs`

**Interfaces:**
- Consumes: `resolve_magnet_torrent(&MagnetMetadataSettings, &str) -> Result<Option<ResolvedMagnetTorrent>, MagnetMetadataError>`.
- Produces: tests proving a 503 response is retried and an HTTP 400 response is not retried.

- [x] **Step 1: Write a test where two HTTP 503 responses are followed by a valid metadata response**

Add a Tokio test that increments an `Arc<AtomicUsize>`. Return `(StatusCode::SERVICE_UNAVAILABLE, "busy")` for requests 1 and 2; on request 3 return the existing JSON payload with `/download/...`, and serve the corresponding torrent bytes. Assert `result.unwrap().is_some()` and `attempts.load(Ordering::SeqCst) == 3`.

- [x] **Step 2: Write a test where HTTP 400 is returned once**

Add a Tokio test whose handler increments an `Arc<AtomicUsize>` and always returns `(StatusCode::BAD_REQUEST, "invalid magnet")`. Assert the result matches `Err(MagnetMetadataError::ResponseStatus(400))` and the counter is exactly 1.

- [x] **Step 3: Run the targeted test before implementation**

Run:

```powershell
Set-Location src-tauri
cargo test --test magnet_metadata
```

Expected: the new 503 test fails because the current implementation sends one request only; the 400 test passes or documents current non-retry behaviour.

### Task 2: Implement bounded transient retry behaviour

**Files:**
- Modify: `src-tauri/src/magnet_metadata.rs`

**Interfaces:**
- Consumes: `reqwest::Client` and the computed metadata endpoint.
- Produces: `send_metadata_request(&reqwest::Client, url::Url, &str) -> Result<reqwest::Response, MagnetMetadataError>` used only by `resolve_magnet_torrent`.

- [x] **Step 1: Add policy constants and a transient-status predicate**

Define these constants beside the existing timeout constants:

```rust
const METADATA_REQUEST_ATTEMPTS: usize = 3;
const METADATA_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
```

Add:

```rust
fn is_transient_metadata_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503 | 504)
}
```

- [x] **Step 2: Implement the request loop with two bounded waits**

Implement `send_metadata_request` so each iteration POSTs the same JSON body. Return immediately for a success or a non-transient HTTP response. For a transport error or transient status, wait `METADATA_RETRY_DELAY * attempt_number` with `tokio::time::sleep` unless it was the third attempt; after the third attempt return `MagnetMetadataError::Request` for transport errors or `MagnetMetadataError::ResponseStatus` for HTTP failures.

- [x] **Step 3: Replace the direct POST in `resolve_magnet_torrent`**

Replace the existing `.post(endpoint).json(...).send().await` expression with the helper call. Keep JSON decoding, `download_url` handling and `resolve_download_url` unchanged.

- [x] **Step 4: Run the focused test suite**

Run:

```powershell
Set-Location src-tauri
cargo test --test magnet_metadata
```

Expected: all `magnet_metadata` tests pass; the 503 case records exactly three attempts and the 400 case records one.

### Task 3: Build and commit the isolated application change

**Files:**
- Modify: `src-tauri/src/magnet_metadata.rs`
- Modify: `src-tauri/tests/magnet_metadata.rs`

**Interfaces:**
- Consumes: completed focused test results.
- Produces: a committed retry implementation on an isolated `codex/` worktree branch.

- [x] **Step 1: Run the required release build**

Run:

```powershell
Set-Location ..
scripts\build-windows.ps1
```

Expected: the Windows release build completes successfully.

- [x] **Step 2: Review only the planned files**

Run:

```powershell
git diff -- src-tauri/src/magnet_metadata.rs src-tauri/tests/magnet_metadata.rs
git status --short
```

Expected: only the two planned files are staged for this commit; existing unrelated worktree changes remain untouched.

- [x] **Step 3: Commit the verified retry implementation**

Run:

```powershell
git add src-tauri/src/magnet_metadata.rs src-tauri/tests/magnet_metadata.rs
git commit -m "fix: retry transient magnet metadata failures"
```
