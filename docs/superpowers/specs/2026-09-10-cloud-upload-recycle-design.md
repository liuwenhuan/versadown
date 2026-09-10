# Cloud Upload and Recycle Bin Design

## Goal

Enable authenticated file uploads for Quark and UC Drive, and give every supported cloud provider a consistent, provider-backed recycle-bin workflow: open, list, restore, and permanently delete recycled entries.

## Scope

Supported providers are Quark, PikPak, Baidu, Tianyi, Aliyun, Xunlei, and UC. The existing provider-specific soft-delete operations remain the sole way to move ordinary files into a recycle bin. This work does not introduce local emulation, automatic emptying, bulk background cleanup, or account scraping.

## Upload Architecture

Quark and UC use the CloudDrive web upload family but retain their own API host, origin, desktop user agent, cookie session, and query parameters. A focused shared upload helper will model the protocol stages: validate a regular local file; request pre-upload metadata; calculate MD5 and SHA-1; allow the provider's hash response to complete an instant upload; otherwise obtain short-lived authorization per object-storage request, upload every part, commit the object-storage multipart manifest, and notify CloudDrive that the upload is complete.

No short-lived URL, authorization key, cookie, or object-storage credential may be serialized into a task, log, error, or persistent credential store. Each provider adapter owns its authenticated CloudDrive requests; the shared helper only consumes already-issued transient contracts. A mutation authorization failure remains non-retriable: session recovery may complete, but the user must submit the upload again.

## Recycle-Bin Architecture

Each adapter exposes its provider's actual recycle listing, restore, and permanent-delete operations. The normal root directory will add a synthetic `回收站` folder with a reserved, provider-specific ID. Opening it calls that adapter's recycle-list endpoint rather than treating the ID as a real directory. Recycle entries preserve the provider's record identifier required for restore and permanent delete; providers whose recycle record ID differs from the original file ID normalize that detail inside their adapter.

The shared command layer gains `cloud_restore`. `cloud_delete` is generalized from its current PikPak-only path to permanent deletion in the active provider recycle bin. It must reject arbitrary non-recycle IDs. Every destructive command continues to execute through `SessionWorkKind::Mutation` and must not replay after a recovered session.

## UI Behavior

The manager recognizes every provider's reserved recycle-bin ID. In a recycle bin it hides upload, create-folder, rename, move, copy, download, and normal trash actions; it offers restore and permanent delete with an explicit confirmation. The root's synthetic recycle folder remains navigable. Ordinary directory deletion continues to ask for confirmation before moving files into the provider recycle bin.

All seven providers will support upload and recycle-bin operations after this work, so the existing always-visible upload affordance becomes truthful. The frontend keeps provider recycle IDs in one typed mapping rather than duplicating PikPak-specific conditions.

## Errors and Safety

Provider-side verification, expired credentials, malformed temporary upload credentials, incomplete multipart replies, and unsupported recycle responses return typed cloud errors. The UI never reports success until the provider confirms completion. Tests use fixture credentials and mock transports only. Any optional live check is gated by the repository's existing explicit live-cloud environment switch and operates only in a freshly created, provider test directory.

## Verification

Run only targeted verification for this cross-cloud feature:

- `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_quark --test cloud_uc`
- `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_baidu --test cloud_tianyi`
- `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_aliyun --test cloud_xunlei`
- `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_upload --test cloud_xunlei_upload --lib cloud::commands::tests`
- `npm run test:run -- src/components/CloudManager.test.tsx --exclude '.worktrees/**'`
- `npm run build`

No full Rust or full Vitest suite is required unless a targeted test reveals shared-infrastructure risk.
