# 全网盘分享链接下载 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在新建下载中安全解析七家网盘分享链接，优先使用带授权身份的会员直链，并在不可直链时自动转存、下载和清理。

**Architecture:** 新增 DPAPI 加密的分享计划与 `cloud-share://` 稳定来源，置于现有 `cloud://` 个人盘来源旁。解析命令返回已入队单文件或分享文件确认清单；适配器在提交和短链失效时重新解析当前 URL/headers，转存仅作为已登录的自动回退。

**Tech Stack:** Rust 2021、Tauri 2、reqwest、Windows DPAPI、aria2 RPC、React 18、TypeScript、Vitest。

**Spec:** `docs/superpowers/specs/2026-09-10-cloud-share-download-design.md`

## Global Constraints

- 本期仅支持 Quark、PikPak、Baidu、Tianyi、Aliyun、Xunlei、UC。
- 先直链，直链必须携带当前登录/会员身份所需的 Cookie、token、UA、Referer、Origin 或 CSRF；无法获得直链才转存。
- 未登录且服务商必须转存时，返回登录提示且不创建任务；已登录时不再请求用户确认转存。
- 分享 URL、访问码、Cookie、token、签名 URL 只进入 DPAPI 存储，绝不写入 `tasks.sqlite`、任务来源、日志或 IPC 事件。
- `cloud-share://provider/share-fingerprint/entry-fingerprint` 只含不可逆 SHA-256 指纹。
- `deleteTransferredShareFilesAfterDownload` 默认为 `true`；仅删除本应用记录的转存对象。
- 单文件自动下载；多文件/目录必须选择确认。前端测试均带 `--exclude '.worktrees/**'`；不构建安装包。

---

### Task 1: 安全识别分享 URL 与访问码

**Files:**
- Modify: `src-tauri/src/parser.rs:6-63`
- Modify: `src-tauri/src/downloader.rs:181-240`
- Test: `src-tauri/src/parser.rs`

**Interfaces:**
- Produces `CloudShareCandidate { provider: CloudProvider, url: String, access_code: Option<String> }`.
- Produces `extract_cloud_share_candidate(input, entered_access_code) -> Result<Option<CloudShareCandidate>, ShareInputError>`.

- [ ] **Step 1: Write the failing parser tests**

```rust
#[test]
fn recognizes_all_provider_hosts_and_strips_pwd_from_canonical_url() {
    for (input, provider) in [
        ("https://pan.quark.cn/s/a?pwd=1234", CloudProvider::Quark),
        ("https://mypikpak.com/s/a 提取码: 1234", CloudProvider::Pikpak),
        ("https://pan.baidu.com/s/a?pwd=1234", CloudProvider::Baidu),
        ("https://cloud.189.cn/t/a 访问码：1234", CloudProvider::Tianyi),
        ("https://www.alipan.com/s/a?pwd=1234", CloudProvider::Aliyun),
        ("https://pan.xunlei.com/s/a?pwd=1234", CloudProvider::Xunlei),
        ("https://drive.uc.cn/s/a?pwd=1234", CloudProvider::Uc),
    ] {
        let value = extract_cloud_share_candidate(input, None).unwrap().unwrap();
        assert_eq!(value.provider, provider);
        assert_eq!(value.access_code.as_deref(), Some("1234"));
        assert!(!value.url.contains("pwd="));
    }
}
#[test]
fn rejects_multiple_share_urls() {
    assert!(matches!(extract_cloud_share_candidate("https://pan.baidu.com/s/a https://pan.xunlei.com/s/b", None), Err(ShareInputError::MultipleLinks)));
}
```

- [ ] **Step 2: Run the test and observe the expected failure**

Run: `cargo test -p alldownloader parser::tests::recognizes_all_provider_hosts_and_strips_pwd_from_canonical_url -- --exact`

Expected: FAIL because the candidate type and extractor do not exist.

- [ ] **Step 3: Implement exact-host recognition**

```rust
pub fn extract_cloud_share_candidate(input: &str, entered: Option<&str>) -> Result<Option<CloudShareCandidate>, ShareInputError> {
    let values = extract_candidates(input).into_iter().filter_map(CloudShareCandidate::from_candidate).collect::<Vec<_>>();
    match values.as_slice() {
        [] => Ok(None),
        [value] => Ok(Some(value.with_access_code(entered.or(value.access_code.as_deref()))?)),
        _ => Err(ShareInputError::MultipleLinks),
    }
}
```

Allow only the seven documented host families, remove `pwd`/`passcode` query values from `url`, and retain the existing direct/magnet/thunder/torrent behavior. Remove the raw-share-URL-as-`cloud` task behavior.

- [ ] **Step 4: Run focused tests**

Run: `cargo test -p alldownloader parser::tests downloader::tests --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/parser.rs src-tauri/src/downloader.rs
git commit -m "feat: recognize cloud share links safely"
```

### Task 2: Create encrypted share plans and opaque stable sources

**Files:**
- Create: `src-tauri/src/cloud/share.rs`
- Modify: `src-tauri/src/cloud/mod.rs`
- Modify: `src-tauri/src/cloud/credentials.rs:39-49`
- Test: `src-tauri/src/cloud/share.rs`

**Interfaces:**
- Produces `CloudSharePlanStore::{upsert, resolve_entry, record_transfer, mark_cleanup_attempted, prune}`.
- Produces `stable_share_source(provider, share_id, entry_id)` and `parse_stable_cloud_share_source(source)`.

- [ ] **Step 1: Write the failing privacy tests**

```rust
#[test]
fn stable_share_source_has_only_hashes() {
    let source = stable_share_source(CloudProvider::Baidu, "share-id", "file-id");
    assert!(source.starts_with("cloud-share://baidu/"));
    assert!(!source.contains("share-id") && !source.contains("file-id"));
    assert_eq!(parse_stable_cloud_share_source(&source).unwrap().0, CloudProvider::Baidu);
}
#[test]
fn dpapi_plan_round_trip_recovers_access_code_but_source_does_not() {
    let plan = CloudSharePlan::fixture("1234");
    let store = test_share_plan_store();
    store.upsert(&plan).unwrap();
    assert_eq!(store.resolve_entry(&plan.source_for("f1")).unwrap().access_code(), Some("1234"));
    assert!(!plan.source_for("f1").contains("1234"));
}
```

- [ ] **Step 2: Run the test and observe the expected failure**

Run: `cargo test -p alldownloader cloud::share::tests --lib`

Expected: FAIL because the `share` module does not exist.

- [ ] **Step 3: Implement versioned DPAPI storage**

```rust
pub const SHARE_PLAN_SECRET: &str = "cloud-share-plans-v1";
pub fn stable_share_source(provider: CloudProvider, share_id: &str, entry_id: &str) -> String {
    format!("cloud-share://{}/{}/{}", provider.key(), sha256_hex(share_id), sha256_hex(&format!("{share_id}\0{entry_id}")))
}
```

Persist a fixed-key versioned JSON map through DPAPI, keyed by `(provider, share_fingerprint)`. Accept only lowercase 64-character SHA-256 source segments. Cap at 256 least-recently-used plans and prune plans unused for 30 days.

- [ ] **Step 4: Run focused tests**

Run: `cargo test -p alldownloader cloud::share::tests --lib`

Expected: PASS, including malformed source and expiry cases.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/cloud/share.rs src-tauri/src/cloud/mod.rs src-tauri/src/cloud/credentials.rs
git commit -m "feat: store cloud share plans with DPAPI"
```

### Task 3: Expose tagged parse/confirm outcomes

**Files:**
- Modify: `src-tauri/src/commands.rs:135-275`
- Modify: `src-tauri/src/cloud/share.rs`
- Modify: `src/lib/api.ts:29-65`
- Modify: `src/lib/types.ts:1-90`
- Test: `src/lib/api.test.ts`

**Interfaces:**
- Produces Rust `ParseDownloadOutcome::{Task, ShareConfirmation, AccessCodeRequired}` tagged with `kind`.
- Produces `confirm_cloud_share_download(plan_source, selected_ids, destination_directory)`.
- Produces TypeScript `ParseDownloadResult` with `task`, `confirmation`, or `shareAccessCodeRequired`.

- [ ] **Step 1: Write a failing IPC-mapping test**

```ts
it('maps a share confirmation without inventing an aria2 task', async () => {
  native.invoke.mockResolvedValueOnce({ kind: 'share_confirmation', plan_source: 'cloud-share://uc/a/b', title: '照片', resources: [{ id: 'f1', name: '照片/a.jpg', size: '3 B', selected: true }] });
  await expect(parseDownload('https://drive.uc.cn/s/id')).resolves.toMatchObject({ confirmation: { taskId: 'cloud-share://uc/a/b' } });
});
```

- [ ] **Step 2: Run the test and observe the expected failure**

Run: `npm run test:run -- --exclude '.worktrees/**' src/lib/api.test.ts`

Expected: FAIL because a native parse outcome currently always has `task`.

- [ ] **Step 3: Implement the tagged union**

```rust
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParseDownloadOutcome {
    Task(ParseOutcome),
    ShareConfirmation(CloudShareConfirmation),
    AccessCodeRequired { provider: CloudProvider, message: &'static str },
}
```

The new confirmation command must only accept validated `cloud-share://` sources. Existing non-share callers always receive the `Task` branch.

- [ ] **Step 4: Run Rust and frontend contract tests**

Run: `cargo test -p alldownloader commands::tests --lib; npm run test:run -- --exclude '.worktrees/**' src/lib/api.test.ts`

Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/commands.rs src-tauri/src/cloud/share.rs src/lib/api.ts src/lib/types.ts
git commit -m "feat: expose cloud share parse outcomes"
```

### Task 4: Dispatch inspected plans, current direct URLs, and retry refreshes

**Files:**
- Modify: `src-tauri/src/cloud/share.rs`
- Modify: `src-tauri/src/cloud/commands.rs:1477-1905`
- Modify: `src-tauri/src/commands.rs:389-421`
- Test: `src-tauri/src/cloud/share.rs`
- Test: `src-tauri/tests/downloader.rs`

**Interfaces:**
- Produces `inspect_cloud_share`, `submit_cloud_share_file`, `resolve_stable_cloud_share_download`.
- Provider adapters return `ShareResolution::{Direct(ResolvedCloudDownload), TransferRequired(TransferRequest)}`.

- [ ] **Step 1: Write the failing current-URL test**

```rust
#[test]
fn expired_share_url_refreshes_once_with_new_headers_without_persisting_url() {
    let first = resolver.resolve("cloud-share://aliyun/a/b").unwrap();
    let second = resolver.resolve_after_status("cloud-share://aliyun/a/b", 403).unwrap();
    assert_ne!(first.url, second.url);
    assert!(second.headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("x-share-token")));
    assert!(!task_store_source().contains(&second.url));
}
```

- [ ] **Step 2: Run the test and observe the expected failure**

Run: `cargo test -p alldownloader cloud::share::tests::expired_share_url_refreshes_once_with_new_headers_without_persisting_url --lib`

Expected: FAIL because no `cloud-share://` resolver exists.

- [ ] **Step 3: Implement exact resolution behavior**

```rust
pub async fn resolve_stable_cloud_share_download(app: AppHandle, source: &str) -> Result<ResolvedCloudDownload, QuarkError> {
    let entry = app.state::<CloudState>().share_plans.resolve_entry(source)?;
    run_session_work(app, entry.provider, SessionWorkKind::Read, move |state, http| resolve_share_entry(&entry, state, http)).await
}
```

At submission and 401/403/412/signature expiry, refresh once and pass fresh headers to `UriSubmissionOptions`. Never retry access-code, captcha, web-auth, membership or forbidden errors automatically.

- [ ] **Step 4: Run common download tests**

Run: `cargo test -p alldownloader cloud::share::tests downloader --lib`

Expected: PASS while existing `cloud://` behavior is unchanged.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/cloud/share.rs src-tauri/src/cloud/commands.rs src-tauri/src/commands.rs src-tauri/tests/downloader.rs
git commit -m "feat: refresh cloud share downloads safely"
```

### Task 5: Implement Quark and Baidu adapters

**Files:**
- Modify: `src-tauri/src/cloud/quark.rs`
- Modify: `src-tauri/src/cloud/baidu.rs`
- Modify: `src-tauri/src/cloud/share.rs`
- Test: `src-tauri/src/cloud/quark.rs`
- Test: `src-tauri/src/cloud/baidu.rs`

**Interfaces:**
- Produces `resolve_quark_share` and `resolve_baidu_share`, both returning Task 4’s `ShareResolution`.

- [ ] **Step 1: Write a failing transfer fallback test**

```rust
#[test]
fn baidu_transfer_fallback_requires_current_login() {
    assert!(matches!(resolve_baidu_share(&anonymous_transfer_fixture()), Err(QuarkError::NotAuthorized)));
    assert!(matches!(resolve_baidu_share(&authorized_transfer_fixture()).unwrap(), ShareResolution::TransferRequired(_)));
}
```

- [ ] **Step 2: Run the test and observe the expected failure**

Run: `cargo test -p alldownloader cloud::baidu::tests::baidu_transfer_fallback_requires_current_login --lib`

Expected: FAIL because Baidu share resolution does not exist.

- [ ] **Step 3: Implement direct-first protocol and transfer fallback**

Quark validates access code, pages its share tree and uses existing authenticated Cookie/UA for a direct file route. Baidu validates access code and share listing with existing Netdisk identity, attempts its authenticated direct route, then automatically transfers only on `TransferRequired`. Record only returned personal file IDs for later cleanup.

- [ ] **Step 4: Run adapter tests**

Run: `cargo test -p alldownloader cloud::quark::tests cloud::baidu::tests --lib`

Expected: PASS for access code, pagination, direct headers, transfer and no-login fixtures.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/cloud/quark.rs src-tauri/src/cloud/baidu.rs src-tauri/src/cloud/share.rs
git commit -m "feat: download Quark and Baidu shares"
```

### Task 6: Implement PikPak, Tianyi, and Aliyun adapters

**Files:**
- Modify: `src-tauri/src/cloud/pikpak.rs`
- Modify: `src-tauri/src/cloud/tianyi.rs`
- Modify: `src-tauri/src/cloud/aliyun.rs`
- Modify: `src-tauri/src/cloud/share.rs`
- Test: `src-tauri/src/cloud/pikpak.rs`, `src-tauri/src/cloud/tianyi.rs`, `src-tauri/src/cloud/aliyun.rs`

**Interfaces:**
- Produces `resolve_pikpak_share`, `resolve_tianyi_share`, `resolve_aliyun_share` returning `ShareResolution`.

- [ ] **Step 1: Write the failing Aliyun header test**

```rust
#[test]
fn aliyun_share_url_uses_share_token_not_personal_bearer() {
    let direct = resolve_aliyun_share(&share_fixture()).unwrap().into_direct().unwrap();
    assert!(direct.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("x-share-token")));
    assert!(!direct.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("authorization")));
}
```

- [ ] **Step 2: Run the test and observe the expected failure**

Run: `cargo test -p alldownloader cloud::aliyun::tests::aliyun_share_url_uses_share_token_not_personal_bearer --lib`

Expected: FAIL because Aliyun only implements personal-drive download URLs.

- [ ] **Step 3: Implement direct share APIs**

PikPak uses the web share captcha context and maps regional membership limits. Tianyi validates access code and recursively pages share directories using compatible session headers. Aliyun obtains a share token, calls the official share `get_download_url` endpoint, and includes `x-share-token` only.

- [ ] **Step 4: Run adapter tests**

Run: `cargo test -p alldownloader cloud::pikpak::tests cloud::tianyi::tests cloud::aliyun::tests --lib`

Expected: PASS for code, expiry, recursion, headers and membership fixtures.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/cloud/pikpak.rs src-tauri/src/cloud/tianyi.rs src-tauri/src/cloud/aliyun.rs src-tauri/src/cloud/share.rs
git commit -m "feat: download PikPak Tianyi and Aliyun shares"
```

### Task 7: Implement Xunlei and UC adapters

**Files:**
- Modify: `src-tauri/src/cloud/xunlei.rs`
- Modify: `src-tauri/src/cloud/uc.rs`
- Modify: `src-tauri/src/cloud/share.rs`
- Test: `src-tauri/src/cloud/xunlei.rs`, `src-tauri/src/cloud/uc.rs`

**Interfaces:**
- Produces `resolve_xunlei_share` and `resolve_uc_share` returning `ShareResolution`.

- [ ] **Step 1: Write the failing UC header test**

```rust
#[test]
fn uc_share_direct_url_keeps_identity_headers_out_of_source() {
    let direct = resolve_uc_share(&authorized_fixture()).unwrap().into_direct().unwrap();
    assert!(direct.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("cookie")));
    assert!(direct.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("x-csrf-token")));
    assert!(!stable_share_source(CloudProvider::Uc, "share", "file").contains("cookie"));
}
```

- [ ] **Step 2: Run the test and observe the expected failure**

Run: `cargo test -p alldownloader cloud::uc::tests::uc_share_direct_url_keeps_identity_headers_out_of_source --lib`

Expected: FAIL because UC has no share implementation.

- [ ] **Step 3: Implement direct-first and automatic-transfer fallback**

Xunlei creates a share captcha context, pages share results, prefers share file-info URLs, and only transfers after confirming active Xunlei login. UC exchanges access code for its share token, recursively pages files, and submits `Cookie`, `x-csrf-token`, UC desktop UA, Origin and Referer on each direct URL. Both map risk verification to `WebAuthorizationRequired`.

- [ ] **Step 4: Run adapter tests**

Run: `cargo test -p alldownloader cloud::xunlei::tests cloud::uc::tests --lib`

Expected: PASS for direct URL, fallback/no-login, recursive, expiry and risk fixtures.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/cloud/xunlei.rs src-tauri/src/cloud/uc.rs src-tauri/src/cloud/share.rs
git commit -m "feat: download Xunlei and UC shares"
```

### Task 8: Add access-code UI, share confirmation, and the cleanup setting

**Files:**
- Modify: `src/components/NewDownloadDialog.tsx:4-85`
- Modify: `src/components/ParseConfirmDialog.tsx:7-130`
- Modify: `src/App.tsx:300-370`
- Modify: `src/components/SettingsPanel.tsx`
- Modify: `src/lib/types.ts`, `src/lib/api.ts`
- Modify: `src-tauri/src/models.rs`, `src-tauri/src/commands.rs`
- Test: `src/components/NewDownloadDialog.test.tsx`, `src/components/ParseConfirmDialog.test.tsx`, `src/components/SettingsPanel.test.tsx`

**Interfaces:**
- `NewDownloadDialogProps.onParse(input: string, accessCode?: string): Promise<ParseDownloadResult | void>`.
- `AppSettings.deleteTransferredShareFilesAfterDownload: boolean` defaults to `true`.

- [ ] **Step 1: Write the failing UI tests**

```tsx
it('shows and focuses an access-code field without closing when required', async () => {
  onParse.mockResolvedValue({ shareAccessCodeRequired: { provider: 'baidu', message: '请输入提取码' } });
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));
  expect(screen.getByLabelText('访问码')).toBeVisible();
  expect(screen.getByRole('dialog', { name: '新建下载' })).toBeVisible();
});
it('enables transfer cleanup by default', () => {
  render(<SettingsPanel initial={defaultSettings} onChange={vi.fn()} />);
  expect(screen.getByLabelText('下载完成后自动删除转存文件')).toBeChecked();
});
```

- [ ] **Step 2: Run the test and observe the expected failure**

Run: `npm run test:run -- --exclude '.worktrees/**' src/components/NewDownloadDialog.test.tsx src/components/ParseConfirmDialog.test.tsx src/components/SettingsPanel.test.tsx`

Expected: FAIL because the code input and setting do not exist.

- [ ] **Step 3: Implement the UI paths**

Keep URL text on `AccessCodeRequired`, reveal and focus an optional access-code field, and do not close the dialog. Reuse `ParseConfirmDialog` for `cloud-share://` resources; show provider and automatic-transfer status. Add the enabled-by-default setting with copy limiting deletion to application-created transfer files.

- [ ] **Step 4: Run UI tests and build**

Run: `npm run test:run -- --exclude '.worktrees/**' src/components/NewDownloadDialog.test.tsx src/components/ParseConfirmDialog.test.tsx src/components/SettingsPanel.test.tsx; npm run build`

Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/components/NewDownloadDialog.tsx src/components/ParseConfirmDialog.tsx src/components/SettingsPanel.tsx src/App.tsx src/lib/api.ts src/lib/types.ts src-tauri/src/models.rs src-tauri/src/commands.rs
git commit -m "feat: confirm cloud shares and configure cleanup"
```

### Task 9: Delete only recorded transfer objects after successful completion

**Files:**
- Modify: `src-tauri/src/engine_runtime.rs:340-360`
- Modify: `src-tauri/src/cloud/share.rs`
- Modify: `src-tauri/src/cloud/commands.rs`
- Test: `src-tauri/src/engine_runtime.rs`

**Interfaces:**
- Produces `CloudSharePlanStore::mark_local_completion(source)` returning cleanup only after every linked local task completes.

- [ ] **Step 1: Write failing cleanup tests**

```rust
#[test]
fn last_completed_local_task_trashes_only_recorded_transfer_when_enabled() {
    assert_eq!(complete_share_tasks(true), vec![CleanupAction::Trash { provider: CloudProvider::Baidu, ids: vec!["created-id".into()] }]);
}
#[test]
fn disabled_cleanup_or_failed_sibling_keeps_transfer() {
    assert!(complete_share_tasks(false).is_empty());
    assert!(fail_one_share_task(true).is_empty());
}
```

- [ ] **Step 2: Run the test and observe the expected failure**

Run: `cargo test -p alldownloader engine_runtime::tests::last_completed_local_task_trashes_only_recorded_transfer_when_enabled --lib`

Expected: FAIL because completion does not examine share plans.

- [ ] **Step 3: Implement one-shot cleanup**

After `Completed`, parse only `cloud-share://`, atomically mark the exact plan transfers as attempted once every linked task completes, and call the matching provider trash/delete API when the setting is enabled. A cleanup error leaves the local task completed and adds a non-secret retryable cleanup message.

- [ ] **Step 4: Run cleanup tests**

Run: `cargo test -p alldownloader engine_runtime::tests cloud::share::tests --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src-tauri/src/engine_runtime.rs src-tauri/src/cloud/share.rs src-tauri/src/cloud/commands.rs
git commit -m "feat: clean up transferred share files"
```

### Task 10: Security, integration, and authorized live verification

**Files:**
- Test: `src-tauri/tests/cloud_directory_download.rs`, `src-tauri/tests/downloader.rs`
- Test: `src/components/CloudManager.test.tsx`

**Interfaces:**
- Consumes all contracts from Tasks 1-9 and produces a master-only runnable binary after verification.

- [ ] **Step 1: Run the secret-persistence scan**

Run: `rg -n "secret-code|x-share-token|pass_code_token" src-tauri/tests src --glob '!**/target/**'`

Expected: test fixtures only; no task source, SQLite assertion, debug event or UI payload permits these values.

- [ ] **Step 2: Run all targeted tests**

Run: `cargo test -p alldownloader --lib cloud::; cargo test -p alldownloader --test cloud_directory_download --test downloader; npm run test:run -- --exclude '.worktrees/**' src/lib/api.test.ts src/components/NewDownloadDialog.test.tsx src/components/ParseConfirmDialog.test.tsx src/components/CloudManager.test.tsx src/components/SettingsPanel.test.tsx`

Expected: PASS.

- [ ] **Step 3: Run authorized provider verification**

For every currently authorized provider, use one user-supplied file share and one small folder share. Verify direct headers, recursive path preservation, forced URL-refresh behavior, automatic transfer only where needed, and cleanup enabled/disabled behavior. If a provider requests interactive authentication, present its authorization window and wait for the user.

- [ ] **Step 4: Build the executable from master only**

Run: `powershell -ExecutionPolicy Bypass -File scripts/build-windows.ps1`

Expected: successful runnable binary with no installer.

## Self-review

- Spec coverage: Tasks 1/3/8 cover discovery, access-code input and confirmation; Tasks 2/4 protect and refresh secret state; Tasks 5-7 implement all providers; Task 9 implements the default-on precise cleanup; Task 10 validates security, behavior and binary output.
- Placeholder scan: all new types and commands are named before a later task consumes them; each task has concrete tests and commands.
- Type consistency: `CloudShareCandidate` feeds inspection, `CloudSharePlanStore` feeds resolution and cleanup, `ParseDownloadOutcome` maps to `ParseDownloadResult`, and every adapter produces `ShareResolution`.
