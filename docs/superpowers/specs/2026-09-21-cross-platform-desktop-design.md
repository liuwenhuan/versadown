# Cross-platform Desktop Design

## Goal

Make AllDownloader build and run on Windows, Linux, and macOS without a
Windows-only business dependency. Download scheduling, task persistence, cloud
access, and the frontend must use the same contracts on all three platforms.

## Scope and success criteria

- The Rust crate and frontend compile on Linux; target-gated code permits the
  same crate to compile on Windows and macOS.
- aria2-next is resolved by target OS and CPU architecture. Linux x86_64 uses
  the supplied `aria2-next-2.8.0-linux-x86_64` asset with SHA-256
  `61a7a6e68dc24aed20c078f59aa0fee3c480c79dc62c4d3174711a1e29435be6`.
- Secrets use one application-level credential-store contract backed by each
  OS's secure credential service. No platform has a plaintext fallback.
- Window lifecycle and file browsing work through Tauri/Rust cross-platform
  APIs. Native Windows hooks are removed rather than retained as an enhancement.
- Xunlei local-client support is represented by one adapter contract on all
  platforms. Windows, Linux, and macOS may report that their integration is not
  configured until their respective client protocol has been researched; this
  must not prevent ordinary cloud/API downloads.
- This development-stage migration deliberately does not read old DPAPI
  credential blobs or Windows-only sidecar paths.

## Architecture

### Engine resources

Introduce an `EngineBinaryResolver` that maps `target_os` plus `target_arch`
to a resource filename. Startup asks the resolver for its manifest and bundled
paths, then retains the existing bundled-resource-first behavior. The resolver
returns a typed unsupported-target error when no matching asset is included.

Resource names follow Tauri sidecar target conventions. The repository bundles
the supplied Linux x86_64 executable with an executable mode and records its
SHA-256 in source. The packaging config lists platform artifacts rather than a
single Windows `.exe`; target build automation chooses only the matching one.
macOS assets are intentionally not fabricated: macOS compiles but reports the
engine unavailable until a verified macOS x86_64 or arm64 asset is supplied.

### Secure credentials

Replace the DPAPI-named store with a `SystemCredentialStore` implementation of
the existing `CredentialStore` trait. It stores each supported logical key in
the operating system credential service through a cross-platform Rust library:
Windows Credential Manager, macOS Keychain, and Linux Secret Service.

The logical key allowlist remains unchanged. Missing, locked, or unavailable
system credential services return the existing storage error to callers; they
never cause a write to an unencrypted application-data file. Test code uses
`MemoryCredentialStore`, so deterministic unit tests do not depend on an OS
keyring.

### Windows and filesystem behavior

Delete the WebView2 and Win32 window-hook modules. Main-window interaction
state uses a platform-neutral value, and frontend dragging uses the standard
Tauri drag-region behavior. Window visibility, close interception, tray, icon,
and launcher movement use public Tauri APIs; a request such as no-activation
that lacks a public equivalent safely uses normal show behavior.

Directory enumeration continues to use Rust `std::fs`. A small root-provider
adapter only represents OS topology: Windows drive letters, and Unix `/`, home,
and mounted volumes where discoverable. Symlink/reparse-point protection and
path validation remain conservative across all platforms. Windows filename
collision and reserved-device validation stay as cross-platform output safety
rules, not as Windows execution branches.

### Xunlei client integration

Replace `XunleiNativeBackend` with an `XunleiClientAdapter` trait covering
discovery, availability, task submission, and lifecycle operations. It accepts
the existing resolved cloud item and submission options, and returns either a
native task or an explicit availability status. Three platform constructors are
present from the first migration:

- Windows: no private DLL or registry integration remains; it reports pending
  protocol integration.
- Linux: reserves client discovery/configuration and invocation capability for
  the installed Linux Xunlei client, but does not guess its private protocol.
- macOS: reserves the equivalent client discovery/configuration and invocation
  capability.

The download dispatcher falls back to the ordinary URI/aria2 path whenever the
adapter is unavailable. No persisted task format changes are required for the
placeholder adapters.

## Failure handling

- Unsupported engine target, absent sidecar, failed checksum, or non-executable
  Unix sidecar produce an actionable engine-unavailable state without starting
  a child process.
- A missing desktop keyring reports a credential storage error and preserves
  existing in-memory state; it does not silently lose or expose a credential.
- An unavailable Xunlei client causes normal download fallback, unless the
  resolved item is native-only, in which case it returns the existing clear
  native-backend-unavailable error.
- Tauri operations that lack a platform-specific guarantee use their public
  fallback and report errors through the existing command result path.

## Testing and verification

- Unit-test engine resolution, Linux asset checksum, Unix execute-bit checking,
  and unsupported target errors.
- Unit-test credential logical-key behavior using the memory store; integration
  tests for a real keyring are opt-in because they require a logged-in desktop
  session.
- Unit-test root-provider output and verify symlink rejection on Unix.
- Unit-test adapter selection and ordinary-download fallback for all three
  platforms.
- Run the frontend build and targeted Rust tests on Linux. Add CI build jobs
  for Windows, Linux, and macOS once local changes are complete; native binary
  packaging is only performed when explicitly requested.

## Non-goals

- Implementing the undocumented Linux/macOS Xunlei client protocols now.
- Shipping unverified macOS aria2-next binaries.
- Maintaining compatibility with unreleased Windows-only credential files,
  sidecar layouts, or native Xunlei task records.
