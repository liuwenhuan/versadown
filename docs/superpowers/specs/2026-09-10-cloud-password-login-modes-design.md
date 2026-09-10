# Cloud Password Login Modes Design

## Goal

Make every supported cloud provider's actual login options visible in 网盘管理 without allowing the frontend to claim a credential flow the Rust backend cannot complete. PikPak, Baidu and Tianyi retain verified local account/password login with encrypted automatic recovery. Aliyun, Xunlei and UC explicitly expose that their official authorization page accepts account/password login; Quark remains the only provider with no account/password option.

## Problem

The Rust `CloudAuthorizationCapabilities` response currently contains only `web` and `password`. The TypeScript contract already requires `provider`, and `CloudManager` rejects responses whose provider does not match the active tab. Rust omits that field, causing every real response to be discarded. The static frontend fallback then disables password controls for every provider, including the three providers that Rust already supports.

The word "password" currently conflates two different flows:

1. A verified, pure-Rust username/password protocol that can create a durable provider session and can be used for one automatic relogin after expiry.
2. An official WebView page which offers an account/password tab but has no verified pure-Rust password protocol. Its anti-risk, SMS, CAPTCHA and browser-session rules remain provider-controlled.

Showing a local form for the second flow would submit credentials to an endpoint which does not exist or is not verified, then fail. It would also falsely promise automatic password recovery.

## Provider policy

| Provider | Login mode | Local password form | Durable session / renewal | Saved fallback password |
| --- | --- | --- | --- | --- |
| Quark | `none` | No | official browser Cookie only | No |
| PikPak | `rustDirect` | Yes | access/refresh; one automatic direct recovery | Yes, DPAPI |
| Baidu | `rustDirect` | Yes | browser Cookie; one automatic direct recovery | Yes, DPAPI |
| Tianyi | `rustDirect` | Yes | browser Cookie; one automatic direct recovery | Yes, DPAPI |
| Aliyun | `officialWebView` | No | rotating official refresh token | No |
| Xunlei | `officialWebView` | No | official access/refresh plus verification context | No |
| UC | `officialWebView` | No | official browser Cookie | No |

`officialWebView` is still an account/password login option for the user: the official page can offer account/password, SMS or QR tabs. It must be labelled exactly as such in the product, but it never receives application-form passwords and does not claim automatic password recovery. This design does not bypass CAPTCHA, SMS, device verification or risk controls.

The known third-party Xunlei Android login flow is deliberately not included in the shared mode. It is version/device/risk sensitive and must first get its own adapter, red-green tests, and a user-authorized real-account validation. After that validation it may be promoted from `officialWebView` to `rustDirect` without changing UI or lifecycle contracts.

## Capability contract

Rust owns the sole policy declaration. `CloudAuthorizationCapabilities` serializes:

```rust
pub struct CloudAuthorizationCapabilities {
    pub provider: CloudProvider,
    pub web: bool,
    pub password: bool,
    pub password_login_mode: PasswordLoginMode,
}

pub enum PasswordLoginMode { None, RustDirect, OfficialWebView }
```

`password` remains a compatibility shorthand for `password_login_mode == RustDirect`; it must not be hand-maintained elsewhere. `CloudSessionPolicy` derives password-fallback support from `PasswordLoginMode::RustDirect`, so session storage, commands, and visible controls cannot drift.

The frontend performs a runtime shape check (`provider`, booleans and a known mode) before displaying controls. It has no provider-specific capability fallback. While the command is pending or invalid it renders a non-actionable “正在检查登录方式” state; on error it offers an explicit retry and no password or web authorization action. This is fail-safe without hiding a valid backend response.

## UI behaviour

- `rustDirect`: existing account, password and “登录…账号” controls; retain the DPAPI automatic-recovery disclosure.
- `officialWebView`: no local password inputs. Show “在官方网页登录（可使用账号密码）” and a short explanation that the provider's official window may request SMS/CAPTCHA; retain the standard web authorization polling and completion handling.
- `none`: show only the normal official web authorization control, with no password-login claim.
- A `WebAuthorizationRequired` error from any read, mutation or password-flow transition must present the same official-web action without clearing the current account tab or directory.

## Verification

- A Rust serialization contract verifies every provider response includes the matching `provider`, `web`, `password`, and `passwordLoginMode`, and that only direct mode accepts password credentials or creates DPAPI fallback records.
- `CloudManager` tests prove real capability responses render PikPak/Baidu/Tianyi forms; Aliyun/Xunlei/UC render explicit official account-password web-login copy; Quark does not advertise password login; missing/malformed/mismatched capabilities fail closed and can be retried.
- Existing lifecycle tests keep one read retry, no mutation replay, password zeroization/redaction and CAPTCHA/SMS WebView fallback.
- Run the affected Rust contract/session tests, the affected Vitest components with `.worktrees/**` excluded, `npm run build`, and `scripts/build-windows.ps1`. Do not run unrelated full suites.
