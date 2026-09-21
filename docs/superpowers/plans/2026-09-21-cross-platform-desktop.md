# Cross-platform Desktop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build and run the Tauri desktop application on Windows, Linux, and macOS without Windows-only business dependencies.

**Architecture:** Extract target-dependent engine resource lookup, credential access, filesystem roots, and Xunlei-client availability behind small Rust modules. Startup and download scheduling consume their platform-neutral contracts. Tauri public APIs own window behavior; no WebView2, Win32 message hook, private DLL, or registry behavior remains in application code.

**Tech Stack:** Rust 2021, Tauri 2, Tokio, `keyring` Rust crate, Vite/TypeScript, aria2-next sidecars.

**Spec:** `docs/superpowers/specs/2026-09-21-cross-platform-desktop-design.md`

## Global Constraints

- Download scheduling, task persistence, cloud access, and frontend contracts must remain platform-neutral.
- Linux x86_64 packages `aria2-next-2.8.0-linux-x86_64`, SHA-256 `61a7a6e68dc24aed20c078f59aa0fee3c480c79dc62c4d3174711a1e29435be6`.
- Credential data must use the OS secure credential service and never fall back to plaintext files.
- Windows DPAPI/WebView2/Win32/private-Xunlei-SDK business dependencies are removed, rather than preserved as enhancements.
- Linux and macOS Xunlei adapters expose the same client integration capability as Windows but do not guess unresearched protocols.
- No compatibility reader or migration path is added for unreleased DPAPI blobs, Windows-only sidecar paths, or native Xunlei task records.
- Native release packaging is not part of this plan; only the explicitly requested local Linux compilation and targeted tests are run.

## Review Focus

- A Linux sidecar whose file bytes match the pinned hash but whose execute bit was lost must be rejected before child-process startup (Task 1).
- A locked or absent Linux Secret Service must return a storage error without writing a plaintext secret file (Task 2).
- A cloud download that has Xunlei markers but no available local client must submit to aria2 normally (Task 3).
- A path named `CON` or differing only by case must remain rejected on every OS, not only Windows (Task 4).
- Closing or showing a window on a compositor that lacks no-activation support must retain lifecycle correctness with Tauri's ordinary `show`/`hide` operations (Task 5).

## File structure

- Create `src-tauri/src/engine_binary.rs`: target/architecture resource selection, asset verification, and executable readiness.
- Modify `src-tauri/src/downloader.rs`: remove the Windows filename constant and export resolver types.
- Modify `src-tauri/src/lib.rs`: consume platform-neutral resolver, credentials, default download directory, adapter, and Tauri-only window state.
- Modify `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`: add `keyring` and platform-neutral resources.
- Create `src-tauri/src/xunlei_client.rs`: common client adapter interface and three pending-protocol adapters.
- Modify `src-tauri/src/cloud/credentials.rs`, `src-tauri/src/commands.rs`, and `src-tauri/src/cloud/share/repository.rs`: replace DPAPI store construction with the system keyring store.
- Modify `src-tauri/src/local_upload_picker.rs`: move root topology into a testable provider while retaining shared file listing code.
- Modify/remove `src-tauri/src/window_interaction.rs`, `src-tauri/src/launcher_drag.rs`, `src-tauri/src/window_lifecycle.rs`, and `src/lib/windowInteraction.ts`: remove native APIs and use public Tauri behavior.
- Add `src-tauri/binaries/aria2-next-x86_64-unknown-linux-gnu` from the supplied source with executable mode.

### Task 1: Resolve and validate platform engine assets

**Files:**
- Create: `src-tauri/src/engine_binary.rs`
- Modify: `src-tauri/src/downloader.rs`, `src-tauri/src/lib.rs`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`
- Create: `src-tauri/binaries/aria2-next-x86_64-unknown-linux-gnu`

**Interfaces:**
- Produces `EngineBinaryResolver::for_current_target() -> Result<EngineBinary, EngineBinaryError>` and `EngineBinary::resolve(manifest_dir, resource_dir) -> Result<PathBuf, EngineBinaryError>`.
- Consumes `tauri::path::PathResolver::resource_dir()` in startup.

- [ ] **Step 1: Write failing resolver tests**

```rust
#[test]
fn linux_x86_64_uses_the_pinned_resource_name() {
    assert_eq!(EngineBinary::for_target("linux", "x86_64").unwrap().resource_name(),
        "aria2-next-x86_64-unknown-linux-gnu");
}

#[test]
fn unix_sidecar_without_execute_permission_is_rejected() {
    let asset = tempfile::NamedTempFile::new().unwrap();
    assert!(matches!(verify_unix_executable(asset.path()), Err(EngineBinaryError::NotExecutable(_))));
}
```

- [ ] **Step 2: Run the new test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml engine_binary::tests --no-default-features`

Expected: FAIL because `engine_binary` and its types do not exist.

- [ ] **Step 3: Add the resolver and the Linux asset**

```rust
pub struct EngineBinary { resource_name: &'static str, sha256: Option<&'static str> }

impl EngineBinary {
    pub fn for_target(os: &str, arch: &str) -> Result<Self, EngineBinaryError> {
        match (os, arch) {
            ("windows", "x86_64") => Ok(Self { resource_name: "aria2-next-x86_64-pc-windows-msvc.exe", sha256: None }),
            ("linux", "x86_64") => Ok(Self { resource_name: "aria2-next-x86_64-unknown-linux-gnu", sha256: Some("61a7a6e68dc24aed20c078f59aa0fee3c480c79dc62c4d3174711a1e29435be6") }),
            _ => Err(EngineBinaryError::UnsupportedTarget { os: os.into(), arch: arch.into() }),
        }
    }
}
```

Copy the supplied asset with `install -m 755 /home/deepin/Downloads/aria2-next-2.8.0-linux-x86_64 src-tauri/binaries/aria2-next-x86_64-unknown-linux-gnu`; calculate the SHA-256 in the test before accepting it. Replace `aria2_binary_name()` startup calls with `EngineBinary::for_current_target()?.resolve(...)`. Use `dirs`/Tauri application download directory rather than `USERPROFILE` for the default destination. Add both resource filenames to Tauri configuration; resolver validation ensures a nonmatching resource is never launched.

- [ ] **Step 4: Run focused tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml engine_binary::tests`

Expected: PASS, including hash and execute-bit assertions.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/engine_binary.rs src-tauri/src/downloader.rs src-tauri/src/lib.rs src-tauri/Cargo.toml src-tauri/tauri.conf.json src-tauri/binaries/aria2-next-x86_64-unknown-linux-gnu
git commit -m "feat: resolve aria2 sidecars by platform"
```

### Task 2: Use the operating system credential service

**Files:**
- Modify: `src-tauri/Cargo.toml`, `src-tauri/src/cloud/credentials.rs`, `src-tauri/src/lib.rs`, `src-tauri/src/commands.rs`, `src-tauri/src/cloud/share/repository.rs`
- Test: `src-tauri/src/cloud/credentials.rs`

**Interfaces:**
- Produces `SystemCredentialStore::new(service: &str) -> Self`, implementing `CredentialStore`.
- Consumes existing `CredentialStore::{save,load,remove}` callers unchanged.

- [ ] **Step 1: Write failing unit tests around key validation and store construction**

```rust
#[test]
fn system_store_rejects_unknown_logical_keys_before_keyring_access() {
    let store = SystemCredentialStore::new("com.alldownloader.desktop");
    assert!(matches!(store.save("not-a-supported-key", b"secret"), Err(QuarkError::Storage)));
}

#[test]
fn memory_store_remains_the_only_test_persistence_backend() {
    let store = MemoryCredentialStore::default();
    store.save("ai-api-key", b"secret").unwrap();
    assert_eq!(store.load("ai-api-key").unwrap(), Some(b"secret".to_vec()));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml cloud::credentials::tests::system_store_rejects_unknown_logical_keys_before_keyring_access`

Expected: FAIL because `SystemCredentialStore` is undefined.

- [ ] **Step 3: Replace file/DPAPI storage with `keyring`**

```rust
pub struct SystemCredentialStore { service: String }

impl CredentialStore for SystemCredentialStore {
    fn save(&self, key: &str, value: &[u8]) -> Result<(), QuarkError> {
        let entry = keyring::Entry::new(&self.service, checked_key(key)?).map_err(|_| QuarkError::Storage)?;
        entry.set_secret(value).map_err(|_| QuarkError::Storage)
    }
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, QuarkError> {
        let entry = keyring::Entry::new(&self.service, checked_key(key)?).map_err(|_| QuarkError::Storage)?;
        match entry.get_secret() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(QuarkError::Storage),
        }
    }
    fn remove(&self, key: &str) -> Result<(), QuarkError> {
        let entry = keyring::Entry::new(&self.service, checked_key(key)?).map_err(|_| QuarkError::Storage)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(QuarkError::Storage),
        }
    }
}
```

Add the current stable `keyring` crate with its Linux Secret Service backend enabled. Delete `DpapiCredentialStore`, `protect`, `publish_file`, Windows imports, and tests that inspect DPAPI temporary blobs. Update `AppState`, `CloudSharePlanStore`, constructors, and tests to accept `SystemCredentialStore` in production and `MemoryCredentialStore` under test.

- [ ] **Step 4: Run focused credentials and cloud-share tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml cloud::credentials cloud::share::tests --no-fail-fast`

Expected: PASS without contacting a desktop keyring from unit tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/cloud/credentials.rs src-tauri/src/lib.rs src-tauri/src/commands.rs src-tauri/src/cloud/share/repository.rs src-tauri/src/engine_runtime.rs src-tauri/src/store_share_membership_tests.rs
git commit -m "feat: store credentials in system keyrings"
```

### Task 3: Replace Windows Xunlei SDK with unified client adapters

**Files:**
- Create: `src-tauri/src/xunlei_client.rs`
- Modify: `src-tauri/src/lib.rs`, `src-tauri/src/commands.rs`, `src-tauri/src/xunlei_native.rs`, and direct dispatcher call sites found with `rg 'xunlei_native_backend|XunleiNativeBackend' src-tauri/src`
- Test: `src-tauri/src/xunlei_client.rs`

**Interfaces:**
- Produces `trait XunleiClientAdapter`, `XunleiClientAvailability`, and `platform_adapter() -> Arc<dyn XunleiClientAdapter>`.
- Consumes `ResolvedCloudDownload`, `UriSubmissionOptions`, and existing `NativeSubmitError` semantics.

- [ ] **Step 1: Write failing fallback tests**

```rust
#[test]
fn pending_linux_adapter_is_unavailable_without_claiming_a_native_task() {
    assert_eq!(pending_adapter("linux").availability(), XunleiClientAvailability::PendingProtocol);
}

#[test]
fn unavailable_client_allows_ordinary_uri_submission() {
    assert_eq!(choose_submission_backend(XunleiClientAvailability::PendingProtocol), SubmissionBackend::Aria2);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml xunlei_client::tests`

Expected: FAIL because the common adapter module is missing.

- [ ] **Step 3: Implement the common interface and remove Windows private integration**

```rust
pub enum XunleiClientAvailability { Available, PendingProtocol, Unavailable(String) }
pub trait XunleiClientAdapter: Send + Sync {
    fn availability(&self) -> XunleiClientAvailability;
    fn submit(&self, resolved: &ResolvedCloudDownload, options: &UriSubmissionOptions) -> Result<Option<DownloadTask>, NativeSubmitError>;
}
```

Implement a pending adapter for each current target. Its `submit` returns `Ok(None)`, allowing the existing ordinary URI path to run. Delete `WindowsApi`, registry lookup, DLL loading, PowerShell process inspection, and all `cfg(windows)` behavior in `xunlei_native.rs`; rename the module and state field to `xunlei_client_adapter`.

- [ ] **Step 4: Run focused adapter and dispatch tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml xunlei_client cloud::commands --no-fail-fast`

Expected: PASS, including a marker-bearing Xunlei response that uses aria2 when the adapter is pending.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/xunlei_client.rs src-tauri/src/xunlei_native.rs src-tauri/src/lib.rs src-tauri/src/commands.rs src-tauri/src/cloud
git commit -m "refactor: unify Xunlei client integration"
```

### Task 4: Make filesystem browsing and download-path safety portable

**Files:**
- Modify: `src-tauri/src/local_upload_picker.rs`, `src-tauri/src/commands.rs`, `src-tauri/src/cloud/commands/common.rs`, `src-tauri/src/cloud/commands/download.rs`
- Test: `src-tauri/src/local_upload_picker.rs`, `src-tauri/src/cloud/commands/tests.rs`

**Interfaces:**
- Produces `filesystem_roots() -> Vec<LocalUploadEntry>` and keeps `local_upload_entries` response shape unchanged.
- Consumes existing conservative output-name validation functions.

- [ ] **Step 1: Write failing Unix-root and shared-name-validation tests**

```rust
#[cfg(unix)]
#[test]
fn unix_roots_include_the_filesystem_root() {
    assert!(filesystem_roots().iter().any(|entry| entry.path == "/"));
}

#[test]
fn reserved_windows_device_names_are_rejected_on_all_targets() {
    assert!(is_windows_device_name("CON"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml local_upload_picker::tests::unix_roots_include_the_filesystem_root`

Expected: FAIL because `filesystem_roots` is not exported.

- [ ] **Step 3: Extract root topology only**

```rust
pub(crate) fn filesystem_roots() -> Vec<LocalUploadEntry> {
    #[cfg(windows)] { windows_drive_roots() }
    #[cfg(unix)] { vec![root_entry(), home_entry_if_distinct()] }
}
```

Keep directory listing, canonicalization, and symlink rejection shared. Use `window.get_drag_region`/frontend timing instead of the Windows double-click API; return a documented fixed `500` ms interaction threshold across targets. Do not relax `is_windows_device_name` or path-collision checks.

- [ ] **Step 4: Run focused filesystem tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml local_upload_picker cloud::commands::tests --no-fail-fast`

Expected: PASS on Linux, with all Windows-safe filename tests still passing.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/local_upload_picker.rs src-tauri/src/commands.rs src-tauri/src/cloud/commands/common.rs src-tauri/src/cloud/commands/download.rs
git commit -m "refactor: make filesystem browsing portable"
```

### Task 5: Remove native Windows window integration

**Files:**
- Delete: `src-tauri/src/window_interaction.rs`
- Modify: `src-tauri/src/lib.rs`, `src-tauri/src/launcher_drag.rs`, `src-tauri/src/window_lifecycle.rs`, `src-tauri/src/window_icon.rs`, `src/lib/windowInteraction.ts`, `src/lib/windowInteraction.test.ts`, relevant frontend call sites

**Interfaces:**
- Produces the existing `main_window_interaction_state` command with `{ active: false, nativeDrag: false }` on every platform.
- Consumes public `WebviewWindow::{show,hide,start_dragging,set_icon}` methods.

- [ ] **Step 1: Write failing lifecycle fallback tests**

```rust
#[test]
fn interaction_snapshot_is_platform_neutral() {
    assert_eq!(interaction_snapshot(), serde_json::json!({"active": false, "nativeDrag": false}));
}

#[test]
fn launcher_drag_requests_the_tauri_fallback_when_no_native_loop_exists() {
    assert!(!native_drag_available());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml window_lifecycle::tests::interaction_snapshot_is_platform_neutral`

Expected: FAIL because the platform-neutral helper is absent.

- [ ] **Step 3: Delete Win32/WebView2 behavior and use Tauri APIs**

```rust
pub(crate) fn interaction_snapshot() -> serde_json::Value {
    serde_json::json!({ "active": false, "nativeDrag": false })
}

pub fn show_manager_without_activation(window: &WebviewWindow) -> Result<(), String> {
    window.show().map_err(|error| error.to_string())
}
```

Make frontend drag-region behavior unconditional; remove the `navigator.platform === 'Win32'` decision. Replace custom taskbar-handle code with `set_icon`. Remove `webview2-com`, `windows`, and `winreg` dependencies if no remaining source import requires them, then delete their target dependency block.

- [ ] **Step 4: Run focused Rust and frontend tests**

Run: `pnpm exec vitest run src/lib/windowInteraction.test.ts src/App.live.test.tsx --exclude '.worktrees/**' && cargo test --manifest-path src-tauri/Cargo.toml window_lifecycle launcher_drag`

Expected: PASS, with UI tests asserting Tauri drag fallback rather than Windows native drag.

- [ ] **Step 5: Commit**

```bash
git add -u src-tauri/src src/lib/windowInteraction.ts src/lib/windowInteraction.test.ts src/App.live.test.tsx src-tauri/Cargo.toml
git commit -m "refactor: use Tauri window APIs across platforms"
```

### Task 6: Compile, package configuration check, and document support

**Files:**
- Modify: `README.md`, `package.json`, CI configuration if present
- Test: `src-tauri/Cargo.toml`, frontend build

**Interfaces:**
- Produces documented Linux development build steps and explicit macOS engine-asset status.

- [ ] **Step 1: Update build documentation and scripts**

```json
{
  "scripts": {
    "build": "tsc -b && vite build",
    "check:linux": "pnpm run build && cargo check --manifest-path src-tauri/Cargo.toml"
  }
}
```

Document Linux prerequisites and `pnpm install`, replace Windows-only product claims, and state that macOS builds compile but need a verified macOS aria2-next asset to run downloads. Do not invoke any Windows PowerShell build script from Linux.

- [ ] **Step 2: Install declared dependencies and run local Linux verification**

Run: `pnpm install --frozen-lockfile && pnpm run build && cargo test --manifest-path src-tauri/Cargo.toml engine_binary local_upload_picker xunlei_client cloud::credentials --no-fail-fast && cargo check --manifest-path src-tauri/Cargo.toml`

Expected: frontend build, targeted Rust tests, and Linux Rust compilation all pass. If desktop library packages required by Tauri are missing, report their exact package names and do not substitute a Windows build.

- [ ] **Step 3: Commit**

```bash
git add README.md package.json pnpm-lock.yaml .github src-tauri
git commit -m "docs: document cross-platform desktop builds"
```

## Plan self-review

- Engine resource selection, executable validation, checksum, and supplied Linux binary are implemented in Task 1.
- Secure keyring storage and removal of DPAPI fallback are implemented in Task 2.
- Three-platform Xunlei adapter boundaries and ordinary-download fallback are implemented in Task 3.
- Filesystem root topology plus conservative cross-platform safety rules are implemented in Task 4.
- Removal of WebView2/Win32 window dependencies is implemented in Task 5.
- Local Linux frontend/Rust compilation and user documentation are implemented in Task 6.
- No placeholders, contradictory interface names, or unowned Review Focus condition remains.
