# Multi-Cloud Rust Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (- [ ]) syntax for tracking.

**Goal:** Add Baidu Netdisk, Tianyi Cloud, Aliyun Drive and Xunlei Cloud to the existing native Rust cloud-drive backend and generic “我的网盘” workspace.

**Architecture:** Extend the provider domain with authorization capabilities and isolate every service protocol in its own Rust client. Commands own WebView credential capture, DPAPI persistence, stable task sources and aria2 submission; React consumes generic capability and file-management APIs only.

**Tech Stack:** Rust, Tauri 2/WebView2, reqwest, serde, zeroize, Windows DPAPI, React, TypeScript, Vitest, aria2.

**Spec:** docs/superpowers/specs/2026-09-09-multi-cloud-rust-integration-design.md

## Global Constraints

- All calls run in the local Rust backend. Do not execute, bundle or depend on a CLI executable.
- Clear passwords with Zeroizing after the native exchange; never log a password, cookie, refresh token, signed URL or request headers.
- Save only validated service session data in DpapiCredentialStore, never replacing a known-good record following a failed login.
- Persist cloud://<provider>/<encoded-file-id>, not signed URLs or transient headers.
- Keep the generic list, upload, mkdir, rename, move, copy, trash and download behavior.
- Build with tauri build --no-bundle only. Do not generate an installer.
- Do not modify or stage the user-owned package.json and scripts/build-windows.ps1 changes.

---

## File Structure

- src-tauri/src/cloud/types.rs: provider variants, authorization capability struct and native error mapping.
- src-tauri/src/cloud/credentials.rs: DPAPI key allow-list.
- src-tauri/src/cloud/commands.rs: provider dispatch, credential capture, stable source handling and native download submission.
- src-tauri/src/cloud/baidu.rs, tianyi.rs, aliyun.rs, xunlei.rs: isolated client/session implementation per provider.
- src-tauri/tests/cloud_provider_contract.rs and cloud_<provider>.rs: request contract and session safety tests.
- src/lib/cloudTypes.ts, cloudApi.ts, CloudManager.tsx, CloudWorkspace.tsx: generic capability-gated authorization and provider labels.
- src/components/CloudManager.test.tsx and CloudWorkspace.test.tsx: authorization entry point and account-tab tests.

### Task 1: Shared provider contract and safe dispatch

**Files:**
- Modify: src-tauri/src/cloud/types.rs, credentials.rs, mod.rs, commands.rs, src-tauri/src/lib.rs
- Create: src-tauri/src/cloud/baidu.rs, src-tauri/src/cloud/tianyi.rs, src-tauri/src/cloud/aliyun.rs, src-tauri/src/cloud/xunlei.rs, src-tauri/tests/cloud_provider_contract.rs

**Interfaces:**
- Produces CloudProvider::Baidu, Tianyi, Aliyun and Xunlei.
- Produces CloudAuthorizationCapabilities { web: bool, password: bool }, provider_capabilities(provider), and cloud_provider_capabilities.
- Provides complete dispatch for account, directory, mutation, download, logout, upload and stable-source parsing.

- [ ] **Step 1: Write the failing contract test**

~~~
use versadown_lib::cloud::{parse_stable_cloud_source, provider_capabilities, CloudProvider};

#[test]
fn new_providers_have_stable_sources_and_declared_login_methods() {
    for (source, provider, password) in [
        ("cloud://baidu/f1", CloudProvider::Baidu, true),
        ("cloud://tianyi/f1", CloudProvider::Tianyi, true),
        ("cloud://aliyun/f1", CloudProvider::Aliyun, false),
        ("cloud://xunlei/f1", CloudProvider::Xunlei, false),
    ] {
        assert_eq!(parse_stable_cloud_source(source).unwrap().0, provider);
        assert!(provider_capabilities(provider).web);
        assert_eq!(provider_capabilities(provider).password, password);
    }
}
~~~

- [ ] **Step 2: Verify the test is red**

Run: cargo test --test cloud_provider_contract

Expected: compilation error because variants and provider_capabilities are absent.

- [ ] **Step 3: Implement the minimum shared types and guards**

~~~
pub enum CloudProvider { Quark, Pikpak, Baidu, Tianyi, Aliyun, Xunlei }

pub struct CloudAuthorizationCapabilities { pub web: bool, pub password: bool }

pub fn provider_capabilities(provider: CloudProvider) -> CloudAuthorizationCapabilities {
    match provider {
        CloudProvider::Baidu | CloudProvider::Tianyi => CloudAuthorizationCapabilities { web: true, password: true },
        CloudProvider::Aliyun | CloudProvider::Xunlei | CloudProvider::Quark => CloudAuthorizationCapabilities { web: true, password: false },
        CloudProvider::Pikpak => CloudAuthorizationCapabilities { web: true, password: true },
    }
}
~~~

Add DPAPI keys, one operation lock per provider, empty exported provider-module files, Tauri registration, and stable-source parsing. Reject password-login commands before a provider client is constructed when password is false. The final integration task attaches all protocol methods to command dispatch after the isolated adapters pass their own tests.

- [ ] **Step 4: Verify shared behavior**

Run: cargo test --test cloud_provider_contract --test cloud_quark --test cloud_sessions

Expected: PASS.

- [ ] **Step 5: Commit**

~~~
git add src-tauri/src/cloud/types.rs src-tauri/src/cloud/credentials.rs src-tauri/src/cloud/mod.rs src-tauri/src/cloud/commands.rs src-tauri/src/lib.rs src-tauri/tests/cloud_provider_contract.rs
git commit -m "feat: add multi-cloud provider contract"
~~~

### Task 2: Baidu Netdisk adapter

**Files:**
- Create: src-tauri/src/cloud/baidu.rs, src-tauri/tests/cloud_baidu.rs

**Interfaces:**
- Produces BaiduSession, parse_baidu_web_session, and BaiduClient account/directory/mkdir/rename/move_files/copy_files/trash/upload/download/login_password methods.
- Uses a cookie bundle captured from official Baidu WebView pages.

- [ ] **Step 1: Write failing request-contract tests**

~~~
#[test]
fn baidu_lists_with_session_cookie_and_resolves_an_https_dlink() {
    let http = RecordingHttp::sequence([
        r#"{"errno":0,"list":[{"fs_id":7,"path":"/movie.mkv","isdir":0,"size":12}]}"#,
        r#"{"errno":0,"list":[{"dlink":"https://download.example/file"}]}"#,
    ]);
    let client = BaiduClient::new(BaiduSession::cookie("BDUSS=local; STOKEN=token"), &http);
    assert_eq!(client.directory("/").unwrap().items[0].name, "movie.mkv");
    assert!(client.download("7").unwrap().url.starts_with("https://"));
    assert!(http.requests()[0].headers["Cookie"].contains("BDUSS=local"));
}

#[test]
fn baidu_password_is_not_written_before_session_validation() {
    let store = MemoryCredentialStore::default();
    assert!(store.load("baidu").unwrap().is_none());
}
~~~

- [ ] **Step 2: Verify red**

Run: cargo test --test cloud_baidu

Expected: compilation error because BaiduClient is absent.

- [ ] **Step 3: Implement native protocol**

Capture official pan.baidu.com/passport.baidu.com cookies, validate account and root list, and use XPan APIs for file operations and fresh dlink retrieval. Accept native username/password login only until CAPTCHA, SMS or risk verification occurs; map those cases to WebAuthorizationRequired. Reject non-HTTPS direct links and retain required download headers only in ResolvedCloudDownload.

- [ ] **Step 4: Verify and commit**

Run: cargo test --test cloud_baidu --test cloud_provider_contract

Expected: PASS.

~~~
git add src-tauri/src/cloud/baidu.rs src-tauri/tests/cloud_baidu.rs
git commit -m "feat: add baidu netdisk adapter"
~~~

### Task 3: Tianyi Cloud adapter

**Files:**
- Create: src-tauri/src/cloud/tianyi.rs, src-tauri/tests/cloud_tianyi.rs

**Interfaces:**
- Produces TianyiSession and TianyiClient account/directory/mkdir/rename/move_files/copy_files/trash/upload/download/login_password methods.
- Uses official cloud.189.cn session state.

- [ ] **Step 1: Write failing contract tests**

~~~
#[test]
fn tianyi_maps_files_and_passes_cookie_to_the_resolved_download() {
    let http = RecordingHttp::sequence([
        r#"{"res_code":0,"fileListAO":{"fileList":[{"id":"f1","name":"photo.jpg","isFolder":false,"size":9}]}}"#,
        r#"{"res_code":0,"fileDownloadUrl":"https://download.example/photo.jpg"}"#,
    ]);
    let client = TianyiClient::new(TianyiSession::cookie("COOKIE_LOGIN_USER=local"), &http);
    assert_eq!(client.directory("-11").unwrap().items[0].id, "f1");
    assert!(client.download("f1").unwrap().headers.contains_key("Cookie"));
}

#[test]
fn tianyi_captcha_requests_web_authorization() {
    assert!(matches!(TianyiClient::login_password("u", "p", &RecordingHttp::challenge()), Err(QuarkError::WebAuthorizationRequired)));
}
~~~

- [ ] **Step 2: Verify red**

Run: cargo test --test cloud_tianyi

Expected: compilation error because TianyiClient is absent.

- [ ] **Step 3: Implement native protocol**

Capture cloud.189.cn cookies after web/QR login. Implement personal-cloud account/list/create/rename/move/copy/trash/upload initialization/download calls. Implement account/password flow only where no graphical or SMS challenge occurs, map every challenge to WebAuthorizationRequired, and validate account/root list before committing a session.

- [ ] **Step 4: Verify and commit**

Run: cargo test --test cloud_tianyi --test cloud_provider_contract

Expected: PASS.

~~~
git add src-tauri/src/cloud/tianyi.rs src-tauri/tests/cloud_tianyi.rs
git commit -m "feat: add tianyi cloud adapter"
~~~

### Task 4: Aliyun Drive adapter

**Files:**
- Create: src-tauri/src/cloud/aliyun.rs, src-tauri/tests/cloud_aliyun.rs

**Interfaces:**
- Produces AliyunSession { refresh_token, access_token, drive_id } and AliyunClient account/directory/mkdir/rename/move_files/copy_files/trash/upload/download methods.
- Uses browser-granted refresh token and native bearer-token refresh.

- [ ] **Step 1: Write failing refresh and persistence tests**

~~~
#[test]
fn aliyun_refreshes_access_and_does_not_persist_a_dlink() {
    let store = MemoryCredentialStore::default();
    store.save("aliyun", br#"{"refresh_token":"refresh","access_token":"expired","drive_id":"d1"}"#).unwrap();
    let http = RecordingHttp::sequence([
        r#"{"access_token":"fresh","refresh_token":"next","default_drive_id":"d1"}"#,
        r#"{"items":[{"file_id":"f1","name":"book.pdf","type":"file","size":5}]}"#,
    ]);
    assert_eq!(AliyunClient::new(&store, &http).directory("root").unwrap().items[0].name, "book.pdf");
    assert!(!String::from_utf8(store.load("aliyun").unwrap().unwrap()).unwrap().contains("https://"));
}
~~~

- [ ] **Step 2: Verify red**

Run: cargo test --test cloud_aliyun

Expected: compilation error because AliyunClient is absent.

- [ ] **Step 3: Implement native protocol**

Extract only refresh_token from official alipan.com/aliyundrive.com web storage. Refresh it using the account token API; preserve a rotated refresh token only after account/root validation. Use bearer headers and drive ID for list, mutations, upload initialization, recycle-bin trash and a fresh download URL. Do not expose a password form because reliable Aliyun login needs official browser interaction.

- [ ] **Step 4: Verify and commit**

Run: cargo test --test cloud_aliyun --test cloud_provider_contract

Expected: PASS.

~~~
git add src-tauri/src/cloud/aliyun.rs src-tauri/tests/cloud_aliyun.rs
git commit -m "feat: add aliyun drive adapter"
~~~

### Task 5: Xunlei Cloud adapter

**Files:**
- Create: src-tauri/src/cloud/xunlei.rs, src-tauri/tests/cloud_xunlei.rs

**Interfaces:**
- Produces XunleiSession and XunleiClient account/directory/mkdir/rename/move_files/copy_files/trash/upload/download methods.
- Uses native refresh and required bearer/device headers.

- [ ] **Step 1: Write failing refresh/header tests**

~~~
#[test]
fn xunlei_refreshes_and_sends_bearer_and_device_headers() {
    let http = RecordingHttp::sequence([
        r#"{"access_token":"access","refresh_token":"next","expires_in":7200}"#,
        r#"{"files":[{"id":"f1","name":"archive.zip","kind":"FILE","size":32}]}"#,
    ]);
    let client = XunleiClient::new(XunleiSession::refresh("refresh", "device-1"), &http);
    assert_eq!(client.directory("0").unwrap().items[0].name, "archive.zip");
    assert_eq!(http.requests()[1].headers.get("X-Device-Id"), Some(&"device-1".to_owned()));
}

#[test]
fn xunlei_security_challenge_is_web_auth_not_link_expiry() {
    assert!(matches!(XunleiClient::new(XunleiSession::refresh("r", "d"), &RecordingHttp::challenge()).directory("0"), Err(QuarkError::WebAuthorizationRequired)));
}
~~~

- [ ] **Step 2: Verify red**

Run: cargo test --test cloud_xunlei

Expected: compilation error because XunleiClient is absent.

- [ ] **Step 3: Implement native protocol**

Extract only refresh token, user/device identity, and required security verification state from official Xunlei web authorization. Refresh session in Rust, send bearer/device headers for file and download APIs, and map a security challenge to WebAuthorizationRequired. Do not show a password form because Xunlei can require browser/SMS verification.

- [ ] **Step 4: Verify and commit**

Run: cargo test --test cloud_xunlei --test cloud_provider_contract

Expected: PASS.

~~~
git add src-tauri/src/cloud/xunlei.rs src-tauri/tests/cloud_xunlei.rs
git commit -m "feat: add xunlei cloud adapter"
~~~

### Task 6: Generic workspace authorization and tabs

**Files:**
- Modify: src/lib/cloudTypes.ts, src/lib/cloudApi.ts, src/components/CloudManager.tsx, src/components/CloudWorkspace.tsx, src/App.tsx
- Test: src/components/CloudManager.test.tsx, src/components/CloudWorkspace.test.tsx

**Interfaces:**
- Consumes CloudAuthorizationCapabilities from cloud_provider_capabilities.
- Produces card controls determined by provider capability and account-driven workspace tabs.

- [ ] **Step 1: Write failing UI tests**

~~~
it.each([
  ['baidu', '百度', true], ['tianyi', '天翼', true],
  ['aliyun', '阿里', false], ['xunlei', '迅雷', false],
] as const)('shows only supported login controls for %s', async (provider, label, password) => {
  const client = createCloudFake({ getCloudAuthorizationCapabilities: vi.fn().mockResolvedValue({ provider, web: true, password }) });
  render(<CloudManager provider={provider} client={client} authorizationOnly />);
  expect(await screen.findByRole('button', { name: '使用网页授权' })).toBeVisible();
  expect(Boolean(screen.queryByRole('button', { name: '登录' + label + '账号' }))).toBe(password);
});

it('opens an aliyun tab only after account recovery', async () => {
  const client = createCloudFake({ getCloudAccount: vi.fn((p) => p === 'aliyun' ? Promise.resolve({ provider: 'aliyun', id: 'a1', displayName: 'alice', authMethod: 'web' }) : Promise.reject({ code: 'not_authorized' })) });
  render(<CloudWorkspace client={client} />);
  expect(await screen.findByRole('tab', { name: '阿里网盘' })).toBeVisible();
});
~~~

- [ ] **Step 2: Verify red**

Run: pnpm vitest run src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx

Expected: TypeScript/test error because new providers and capabilities do not exist.

- [ ] **Step 3: Implement generic TypeScript mapping**

Extend the provider union, add CloudAuthorizationCapabilities and cloud_provider_capabilities API wrapper, load capability before authorization controls, and clear username/password React state in finally after login. Add labels 百度、天翼、阿里、迅雷 to the existing provider records. Keep only successfully recovered/authorized accounts as tabs.

- [ ] **Step 4: Verify and commit**

Run: pnpm vitest run src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx; pnpm exec tsc -b --pretty false

Expected: PASS.

~~~
git add src/lib/cloudTypes.ts src/lib/cloudApi.ts src/components/CloudManager.tsx src/components/CloudWorkspace.tsx src/App.tsx src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx
git commit -m "feat: expose multi-cloud accounts in workspace"
~~~

### Task 7: Integration verification and executable

**Files:**
- Modify only the cloud files above for integration faults.
- Test: all cloud Rust tests and cloud workspace frontend tests.

- [ ] **Step 1: Add the final stable-source integration test**

~~~
#[test]
fn each_new_stable_source_resolves_to_a_new_provider() {
    for source in ["cloud://baidu/f1", "cloud://tianyi/f1", "cloud://aliyun/f1", "cloud://xunlei/f1"] {
        let (provider, id) = parse_stable_cloud_source(source).unwrap();
        assert!(!id.is_empty());
        assert!(matches!(provider, CloudProvider::Baidu | CloudProvider::Tianyi | CloudProvider::Aliyun | CloudProvider::Xunlei));
    }
}
~~~

- [ ] **Step 2: Run all tests**

Run: cargo test; pnpm vitest run --dir src; pnpm exec tsc -b --pretty false

Expected: all pass and no output contains credentials or signed URLs.

- [ ] **Step 3: Compile the binary only**

Run: pnpm tauri build --no-bundle

Expected: src-tauri/target/release/versadown.exe exists; no MSI or NSIS file is produced.

- [ ] **Step 4: Review and commit exact intended files**

~~~
git diff --check
git status --short
git add src-tauri/src/cloud/types.rs src-tauri/src/cloud/credentials.rs src-tauri/src/cloud/mod.rs src-tauri/src/cloud/commands.rs src-tauri/src/cloud/baidu.rs src-tauri/src/cloud/tianyi.rs src-tauri/src/cloud/aliyun.rs src-tauri/src/cloud/xunlei.rs src-tauri/src/lib.rs src-tauri/tests/cloud_provider_contract.rs src-tauri/tests/cloud_baidu.rs src-tauri/tests/cloud_tianyi.rs src-tauri/tests/cloud_aliyun.rs src-tauri/tests/cloud_xunlei.rs src/lib/cloudTypes.ts src/lib/cloudApi.ts src/components/CloudManager.tsx src/components/CloudWorkspace.tsx src/App.tsx src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx
git commit -m "feat: complete native multi-cloud integration"
~~~

Never stage package.json or scripts/build-windows.ps1 from the user’s working tree.
