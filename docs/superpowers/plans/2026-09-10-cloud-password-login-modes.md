# Cloud Password Login Modes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Repair the broken cloud-capability IPC contract and show each provider's real account/password login path without pretending unsupported browser flows are direct Rust login.

**Architecture:** Rust defines a serializable provider-aware password-login mode and derives direct-password fallback policy from it. React consumes only a validated IPC response, renders either the established encrypted local form or an explicit official-WebView account-password action, and never uses a drifting provider map. Existing sessions retain their provider-specific token/Cookie renewal behavior.

**Tech Stack:** Rust, Tauri IPC, React, TypeScript, Vitest, Windows DPAPI, `zeroize`.

**Spec:** `docs/superpowers/specs/2026-09-10-cloud-password-login-modes-design.md`

## Global Constraints

- Quark must not advertise account/password login.
- Only verified `rustDirect` providers may receive, persist, or automatically reuse application password inputs.
- Do not bypass CAPTCHA, SMS, device verification, or provider risk controls.
- Keep browser-session Cookies/refresh tokens and DPAPI fallback passwords separate.
- On recovered mutation sessions, do not replay the mutation.
- Use TDD: observe each behavior test fail before production code is added.
- Run only targeted verification plus `scripts/build-windows.ps1`; never treat a build as a live-account validation.

---

### Task 1: Provider-aware capability contract

**Files:**
- Modify: `src-tauri/src/cloud/types.rs:30-49`
- Modify: `src-tauri/src/cloud/session.rs:16-27,106-117`
- Modify: `src-tauri/tests/cloud_provider_contract.rs:90-106`

**Interfaces:**
- Produces `PasswordLoginMode::{None,RustDirect,OfficialWebView}` and `CloudAuthorizationCapabilities { provider, web, password, password_login_mode }`.
- `provider_capabilities(provider)` is the sole source of provider policy.
- `cloud_session_policy(provider)` derives fallback eligibility from `RustDirect`.

- [ ] **Step 1: Write a failing serialization/policy test**

```rust
#[test]
fn capabilities_include_provider_and_distinguish_direct_from_official_web_password_login() {
    let baidu = provider_capabilities(CloudProvider::Baidu);
    assert_eq!(serde_json::to_value(baidu).unwrap()["provider"], "baidu");
    assert_eq!(baidu.password_login_mode, PasswordLoginMode::RustDirect);
    assert!(baidu.password);

    let aliyun = provider_capabilities(CloudProvider::Aliyun);
    assert_eq!(aliyun.password_login_mode, PasswordLoginMode::OfficialWebView);
    assert!(!aliyun.password);
}
```

- [ ] **Step 2: Run the test and observe the expected failure**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract capabilities_include_provider -- --exact`

Expected: FAIL because `provider` and `password_login_mode` do not exist in the serialized contract.

- [ ] **Step 3: Implement the minimal policy types and derivation**

```rust
pub fn provider_capabilities(provider: CloudProvider) -> CloudAuthorizationCapabilities {
    let password_login_mode = match provider {
        CloudProvider::Pikpak | CloudProvider::Baidu | CloudProvider::Tianyi => PasswordLoginMode::RustDirect,
        CloudProvider::Aliyun | CloudProvider::Xunlei | CloudProvider::Uc => PasswordLoginMode::OfficialWebView,
        CloudProvider::Quark => PasswordLoginMode::None,
    };
    CloudAuthorizationCapabilities {
        provider,
        web: true,
        password: password_login_mode == PasswordLoginMode::RustDirect,
        password_login_mode,
    }
}
```

Make `cloud_session_policy` and password-key eligibility use the same mode rather than a duplicate provider match.

- [ ] **Step 4: Re-run the focused Rust contract and session tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract --test cloud_session`

Expected: PASS. Verify a web-only provider never loads a password record.

### Task 2: Validated frontend capability state and login affordances

**Files:**
- Modify: `src/lib/cloudTypes.ts:22-24`
- Modify: `src/components/CloudManager.tsx:49-57,349-356,438-455,721-750`
- Modify: `src/components/CloudManager.test.tsx:35-172`
- Modify: `src/components/CloudWorkspace.test.tsx:45-51`
- Modify: `src/components/WorkspaceTabLayout.test.tsx:5-10`

**Interfaces:**
- Consumes `CloudAuthorizationCapabilities { provider, web, password, passwordLoginMode }` only after runtime validation.
- Renders direct password form only for `rustDirect`; official account-password web login for `officialWebView`; no password claim for `none`.

- [ ] **Step 1: Write failing component tests**

```tsx
it.each(['pikpak', 'baidu', 'tianyi'] as const)('shows encrypted local credentials for %s', async provider => {
  render(<CloudManager provider={provider} client={createCloudFake()} authorizationOnly />);
  expect(await screen.findByLabelText(/账号$/)).toBeVisible();
  expect(screen.getByText('账号和密码仅加密保存在本机，用于登录失效后的自动恢复。')).toBeVisible();
});

it.each(['aliyun', 'xunlei', 'uc'] as const)('opens official account-password login for %s without collecting a local password', async provider => {
  render(<CloudManager provider={provider} client={createCloudFake()} authorizationOnly />);
  expect(await screen.findByRole('button', { name: '在官方网页登录（可使用账号密码）' })).toBeVisible();
  expect(screen.queryByLabelText(/密码$/)).not.toBeInTheDocument();
});
```

Also cover missing `provider`, invalid mode and mismatched provider as fail-closed cases with a retry control.

- [ ] **Step 2: Run the component test and observe failure**

Run: `node node_modules/vitest/vitest.mjs run src/components/CloudManager.test.tsx --exclude '.worktrees/**'`

Expected: FAIL because current capability replies lack mode/provider validation and all web controls share one label.

- [ ] **Step 3: Implement the smallest validated state machine**

```tsx
function isCapabilities(value: unknown, provider: CloudProvider): value is CloudAuthorizationCapabilities {
  return typeof value === 'object' && value !== null
    && (value as CloudAuthorizationCapabilities).provider === provider
    && typeof (value as CloudAuthorizationCapabilities).web === 'boolean'
    && typeof (value as CloudAuthorizationCapabilities).password === 'boolean'
    && ['none', 'rustDirect', 'officialWebView'].includes((value as CloudAuthorizationCapabilities).passwordLoginMode);
}
```

Do not retain provider-specific frontend fallback data. Wire the official-WebView button to the existing `beginCloudAuthorization` flow and use a mode-specific accessible label and explanation.

- [ ] **Step 4: Run focused frontend verification**

Run: `node node_modules/vitest/vitest.mjs run src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx src/components/WorkspaceTabLayout.test.tsx --exclude '.worktrees/**'`

Expected: PASS, including the real cross-process provider field contract shape.

### Task 3: Unified WebView-required recovery presentation

**Files:**
- Modify: `src/components/CloudManager.tsx:480-690`
- Modify: `src/components/CloudManager.test.tsx`

**Interfaces:**
- Consumes existing `web_auth_required` / CAPTCHA provider error codes.
- Produces a provider-neutral reauthorization action without clearing the selected account or directory.

- [ ] **Step 1: Write a failing recovery test**

```tsx
it.each(['aliyun', 'xunlei', 'uc'] as const)('keeps the account and offers official WebView recovery for %s', async provider => {
  const client = createCloudFake({ listCloudDirectory: vi.fn().mockRejectedValue({ code: 'web_auth_required', message: 'verification required' }) });
  render(<CloudManager provider={provider} client={client} initialAccounts={[accountFor(provider)]} />);
  expect(await screen.findByRole('button', { name: '重新网页授权' })).toBeVisible();
  expect(screen.getByRole('tab', { name: new RegExp(providerLabel(provider)) })).toBeVisible();
});
```

- [ ] **Step 2: Run the component test and observe failure**

Run: `node node_modules/vitest/vitest.mjs run src/components/CloudManager.test.tsx --exclude '.worktrees/**'`

Expected: FAIL for providers which currently only turn an error into passive text.

- [ ] **Step 3: Implement a shared `requiresOfficialWebAuthorization` branch**

Recognize `web_auth_required`, CAPTCHA and explicit provider verification errors in password-submit, list and mutation flows. Set the existing reauthorization state, preserve the account/directory and offer the existing official authorization polling sequence. Never feed a browser-only provider into `loginCloudWithPassword`.

- [ ] **Step 4: Re-run focused recovery tests**

Run: `node node_modules/vitest/vitest.mjs run src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx --exclude '.worktrees/**'`

Expected: PASS.

### Task 4: Contract, component and release verification

**Files:**
- No production file beyond Tasks 1–3.

- [ ] **Step 1: Check only the task diff**

Run: `git diff --check -- src-tauri/src/cloud/types.rs src-tauri/src/cloud/session.rs src/lib/cloudTypes.ts src/components/CloudManager.tsx`

Expected: no whitespace errors.

- [ ] **Step 2: Run the affected Rust targets**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract --test cloud_session`

Expected: PASS. No real password, Cookie, token or authorization URL appears in output.

- [ ] **Step 3: Compile the frontend and runnable Windows binary**

Run: `npm run build` followed by `scripts/build-windows.ps1`

Expected: both complete successfully and `artifacts/alldownloader.exe` is refreshed without producing an installer.

- [ ] **Step 4: Manual WebView smoke test when a user is available**

Open Aliyun, Xunlei and UC official authorization windows. Verify the visible copy directs the user to the provider's account-password tab, and after normal provider login the existing polling opens that provider's file tab. Do not enter, log, or persist any password outside the official provider page.
