# Cloud Upload and Recycle Bin Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Quark and UC uploads and provider-backed recycle bins, including restoration and permanent deletion, for every supported cloud provider.

**Architecture:** Provider adapters normalize their own upload and recycle APIs into `CloudDirectory` and `CloudFile` operations. The command layer dispatches virtual recycle directory IDs plus the new restore/permanent-delete commands; the React manager treats all virtual recycle folders uniformly and offers only safe recycle actions there.

**Tech Stack:** Rust/Tauri, reqwest blocking transport, React/TypeScript, Vitest, Rust integration tests with mock cloud transports.

**Spec:** `docs/superpowers/specs/2026-09-10-cloud-upload-recycle-design.md`

## Global Constraints

- Work only on branch `codex/cloud-upload-recycle` in its isolated worktree.
- Preserve provider browser cookies, OAuth tokens, temporary object-store URLs, upload signatures, and credentials as transient in-memory data; never log or serialize them.
- All provider mutations use `SessionWorkKind::Mutation`; a recovered session requires the user to re-submit the mutation.
- A recycle-bin list item must retain the exact provider record identifier required for restore and permanent delete.
- Test-first: each production behavior begins with a focused failing test whose failure is observed before implementation.
- Run targeted Rust tests and `npm run test:run -- src/components/CloudManager.test.tsx --exclude '.worktrees/**'`; run `npm run build`. Do not run full Rust or Vitest suites unless targeted failures prove shared risk.

---

### Task 1: Quark and UC upload plus recycle adapters

**Files:**
- Modify: `src-tauri/src/cloud/upload.rs`
- Modify: `src-tauri/src/cloud/quark.rs`
- Modify: `src-tauri/src/cloud/uc.rs`
- Test: `src-tauri/tests/cloud_quark.rs`
- Test: `src-tauri/tests/cloud_uc.rs`

**Interfaces:**
- Consumes: `CloudHttpTransport`, `CloudUploadTransport`, `UploadHttpRequest`, `UploadHttpResponse`, `CloudDirectory`, and `CloudFile`.
- Produces: `QuarkClient::upload_file`, `UcClient::upload_file`, `list_recycle_bin`, `restore_recycle_entries`, and `delete_recycle_entries` methods; provider-specific reserved recycle IDs exported from their adapter modules.

- [ ] **Step 1: Write failing Quark upload and recycle tests.**

```rust
#[test]
fn quark_upload_runs_pre_hash_authorize_multipart_and_finish() {
    let uploaded = client.upload_file("0", fixture.path(), &upload_http).unwrap();
    assert_eq!(uploaded.name, "upload.txt");
    assert!(requests.iter().any(|r| r.url.contains("file/upload/pre")));
    assert!(requests.iter().any(|r| r.url.contains("file/update/hash")));
    assert!(requests.iter().any(|r| r.url.contains("file/upload/finish")));
}

#[test]
fn quark_recycle_records_restore_and_permanent_delete_use_recycle_record_id() {
    let directory = client.list_recycle_bin().unwrap();
    client.restore_recycle_entries(&[directory.items[0].id.clone()]).unwrap();
    client.delete_recycle_entries(&[directory.items[0].id.clone()]).unwrap();
}
```

- [ ] **Step 2: Run the two Quark test names and confirm they fail because the methods are absent.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_quark quark_upload_runs_pre_hash_authorize_multipart_and_finish quark_recycle_records_restore_and_permanent_delete_use_recycle_record_id`

Expected: compilation failure naming the missing upload/recycle methods.

- [ ] **Step 3: Implement the Quark adapter behavior.**

Use CloudDrive `file/upload/pre`, `file/update/hash`, `file/upload/auth`, object-store multipart PUT/commit, and `file/upload/finish`; accept an immediate completed hash response without object transfer. Validate all server-provided HTTPS endpoints and ETags before sending bytes. Implement the provider recycle list/restore/permanent-delete APIs and add the synthetic root folder only through the normal directory method.

- [ ] **Step 4: Write and observe equivalent failing UC tests.**

```rust
#[test]
fn uc_upload_preserves_uc_headers_and_completes_cloud_drive_task() {
    client.upload_file("0", fixture.path(), &upload_http).unwrap();
    assert!(api_requests.iter().all(|r| r.headers["User-Agent"].contains("uc-cloud-drive/")));
}

#[test]
fn uc_recycle_list_restore_and_delete_use_uc_session_headers() {
    let entries = client.list_recycle_bin().unwrap();
    client.restore_recycle_entries(&[entries.items[0].id.clone()]).unwrap();
    client.delete_recycle_entries(&[entries.items[0].id.clone()]).unwrap();
}
```

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_uc uc_upload_preserves_uc_headers_and_completes_cloud_drive_task uc_recycle_list_restore_and_delete_use_uc_session_headers`

Expected: compilation failure naming the missing methods.

- [ ] **Step 5: Implement UC through the same focused CloudDrive transfer helper.**

Keep UC's API host, `pr=UCBrowser`, browser Origin/Referer, desktop user agent, and validated cookie intact for every CloudDrive request. Reuse only protocol-neutral hashing, multipart XML, and transient authorization helpers; do not copy Quark request headers into UC.

- [ ] **Step 6: Run focused adapter tests and commit.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_quark --test cloud_uc`

Commit: `git add src-tauri/src/cloud/upload.rs src-tauri/src/cloud/quark.rs src-tauri/src/cloud/uc.rs src-tauri/tests/cloud_quark.rs src-tauri/tests/cloud_uc.rs && git commit -m "feat(cloud): upload and recycle Quark UC files"`

### Task 2: Baidu and Tianyi recycle adapters

**Files:**
- Modify: `src-tauri/src/cloud/baidu.rs`
- Modify: `src-tauri/src/cloud/tianyi.rs`
- Test: `src-tauri/tests/cloud_baidu.rs`
- Test: `src-tauri/tests/cloud_tianyi.rs`

**Interfaces:**
- Consumes: each adapter's authenticated request/session helper and current `trash` behavior.
- Produces: exported virtual recycle IDs plus `list_recycle_bin`, `restore_recycle_entries`, and `delete_recycle_entries` methods for both adapters.

- [ ] **Step 1: Add one failing list/restore/permanent-delete test per adapter.**

```rust
#[test]
fn baidu_recycle_operations_use_the_recycle_record_not_a_live_path() {
    let entries = client.list_recycle_bin().unwrap();
    client.restore_recycle_entries(&[entries.items[0].id.clone()]).unwrap();
    client.delete_recycle_entries(&[entries.items[0].id.clone()]).unwrap();
}

#[test]
fn tianyi_recycle_operations_use_the_authenticated_web_session() {
    let entries = client.list_recycle_bin().unwrap();
    client.restore_recycle_entries(&[entries.items[0].id.clone()]).unwrap();
    client.delete_recycle_entries(&[entries.items[0].id.clone()]).unwrap();
}
```

- [ ] **Step 2: Run the named tests and confirm the methods are missing.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_baidu baidu_recycle_operations_use_the_recycle_record_not_a_live_path; cargo test --manifest-path src-tauri/Cargo.toml --test cloud_tianyi tianyi_recycle_operations_use_the_authenticated_web_session`

Expected: compile failures caused by missing recycle APIs.

- [ ] **Step 3: Implement adapter-local recycle list, restore, and permanent delete.**

Preserve Baidu's web token/path translation and Tianyi's open-platform form protocol. Map an entry's server recycle record ID into `CloudFile.id`, reject an empty or malformed record ID before every mutation, and keep the returned normal `CloudDirectory` free of recycle-only fields.

- [ ] **Step 4: Run focused tests and commit.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_baidu --test cloud_tianyi`

Commit: `git add src-tauri/src/cloud/baidu.rs src-tauri/src/cloud/tianyi.rs src-tauri/tests/cloud_baidu.rs src-tauri/tests/cloud_tianyi.rs && git commit -m "feat(cloud): add Baidu and Tianyi recycle bins"`

### Task 3: Aliyun, Xunlei, and PikPak recycle adapters

**Files:**
- Modify: `src-tauri/src/cloud/aliyun.rs`
- Modify: `src-tauri/src/cloud/xunlei.rs`
- Modify: `src-tauri/src/cloud/pikpak.rs`
- Test: `src-tauri/tests/cloud_aliyun.rs`
- Test: `src-tauri/tests/cloud_xunlei.rs`

**Interfaces:**
- Consumes: existing `trash` methods and PikPak's virtual recycle folder/list/delete behavior.
- Produces: adapter-local list/restore/permanent-delete methods; PikPak restore completes the common recycle contract without changing its reserved ID.

- [ ] **Step 1: Add focused failing restore/recycle tests.**

```rust
#[test]
fn aliyun_recycle_list_restore_and_delete_use_drive_scoped_ids() {
    let bin = client.list_recycle_bin().unwrap();
    client.restore_recycle_entries(&[bin.items[0].id.clone()]).unwrap();
    client.delete_recycle_entries(&[bin.items[0].id.clone()]).unwrap();
}

#[test]
fn xunlei_recycle_list_restore_and_delete_refreshes_the_client_session() {
    let bin = client.list_recycle_bin().unwrap();
    client.restore_recycle_entries(&[bin.items[0].id.clone()]).unwrap();
    client.delete_recycle_entries(&[bin.items[0].id.clone()]).unwrap();
}

#[test]
fn pikpak_restores_a_trashed_file_before_exposing_normal_actions() {
    client.restore_recycle_entries(&["trashed-id".into()]).unwrap();
}
```

- [ ] **Step 2: Run the named tests and verify their expected missing-method failures.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_aliyun aliyun_recycle_list_restore_and_delete_use_drive_scoped_ids; cargo test --manifest-path src-tauri/Cargo.toml --test cloud_xunlei xunlei_recycle_list_restore_and_delete_refreshes_the_client_session`

Expected: compile failures naming the new recycle methods.

- [ ] **Step 3: Implement provider-backed recycle actions.**

Use each provider's actual recycle endpoint or `trashed` filter; make Xunlei persist any refreshed session through its existing client wrapper, Aliyun retain the resolved drive ID, and PikPak call its restore mutation only for entries that are currently trashed. Reuse the existing PikPak list and permanent-delete checks rather than duplicating them.

- [ ] **Step 4: Run focused tests and commit.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_aliyun --test cloud_xunlei`

Commit: `git add src-tauri/src/cloud/aliyun.rs src-tauri/src/cloud/xunlei.rs src-tauri/src/cloud/pikpak.rs src-tauri/tests/cloud_aliyun.rs src-tauri/tests/cloud_xunlei.rs && git commit -m "feat(cloud): add provider recycle bin operations"`

### Task 4: Route uploads and recycle operations through Tauri commands

**Files:**
- Modify: `src-tauri/src/cloud/commands.rs`
- Test: `src-tauri/src/cloud/commands.rs`

**Interfaces:**
- Consumes: Task 1-3 adapter methods and their virtual recycle IDs.
- Produces: virtual recycle routing in `cloud_list_directory`, `cloud_restore(app, provider, file_ids)`, generalized `cloud_delete`, and Quark/UC branches in `cloud_upload_files`.

- [ ] **Step 1: Add command-layer tests before changing dispatch.**

```rust
#[test]
fn recycle_directory_ids_route_to_provider_recycle_lists() {
    assert!(is_provider_recycle_bin(CloudProvider::Quark, QUARK_RECYCLE_BIN_ID));
    assert!(is_provider_recycle_bin(CloudProvider::Uc, UC_RECYCLE_BIN_ID));
}

#[test]
fn non_recycle_file_ids_are_rejected_by_permanent_delete() {
    assert!(validate_recycle_entry_id("normal-file").is_err());
}
```

- [ ] **Step 2: Run only the new command test names and observe their initial failure.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml cloud::commands::tests::recycle_directory_ids_route_to_provider_recycle_lists cloud::commands::tests::non_recycle_file_ids_are_rejected_by_permanent_delete --lib`

Expected: compilation failure naming absent helpers.

- [ ] **Step 3: Implement command dispatch.**

Recognize a reserved recycle directory before normal directory dispatch, route `cloud_restore` and `cloud_delete` to the selected adapter, and require every permanent delete to originate from that provider's recycle list. Remove the Quark/UC `UploadUnavailable` guard and call their `upload_file` methods through existing provider locks and `run_cloud_work`. Keep picker code outside locks and maintain no-retry mutation semantics.

- [ ] **Step 4: Run targeted command/upload tests and commit.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_upload --test cloud_xunlei_upload --lib cloud::commands::tests`

Commit: `git add src-tauri/src/cloud/commands.rs && git commit -m "feat(cloud): route uploads and recycle commands"`

### Task 5: Generalize the cloud-manager recycle UI

**Files:**
- Modify: `src/lib/cloudTypes.ts`
- Modify: `src/lib/cloudApi.ts`
- Modify: `src/App.tsx`
- Modify: `src/components/CloudManager.tsx`
- Test: `src/components/CloudManager.test.tsx`

**Interfaces:**
- Consumes: `cloud_restore`, provider virtual recycle IDs, and the existing `cloud_delete` command.
- Produces: `restoreCloudFiles(provider, fileIds)` and one typed `cloudRecycleBinDirectoryId` mapping used by the manager.

- [ ] **Step 1: Add focused UI tests before production changes.**

```tsx
it.each(['quark', 'uc', 'baidu', 'tianyi', 'aliyun', 'xunlei', 'pikpak'] as const)(
  'shows only restore and permanent-delete actions in the %s recycle bin',
  async (provider) => {
    render(<CloudManager provider={provider} client={clientAtRecycleBin(provider)} />);
    expect(await screen.findByRole('button', { name: '恢复' })).toBeVisible();
    expect(screen.getByRole('button', { name: '永久删除' })).toBeVisible();
    expect(screen.queryByRole('button', { name: '上传' })).not.toBeInTheDocument();
  },
);
```

- [ ] **Step 2: Run the new UI test and observe the expected failure.**

Run: `npm run test:run -- src/components/CloudManager.test.tsx --exclude '.worktrees/**'`

Expected: the recycle-only controls and generic provider mapping are absent.

- [ ] **Step 3: Implement the typed client/UI flow.**

Replace `pikpakRecycleBinDirectoryId` checks with a mapping covering all providers. Add the restore API/client method, show a restore action in recycle bins, require confirmation for permanent deletion, and retain the current normal-directory move-to-recycle confirmation. Recycle bins must not offer upload, create, rename, move, copy, or download.

- [ ] **Step 4: Run the focused UI test and production build, then commit.**

Run: `npm run test:run -- src/components/CloudManager.test.tsx --exclude '.worktrees/**'`

Run: `npm run build`

Commit: `git add src/lib/cloudTypes.ts src/lib/cloudApi.ts src/App.tsx src/components/CloudManager.tsx src/components/CloudManager.test.tsx && git commit -m "feat(cloud): unify recycle bin controls"`
