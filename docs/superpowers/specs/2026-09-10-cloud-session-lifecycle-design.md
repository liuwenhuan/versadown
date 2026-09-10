# Cloud Session Lifecycle Design

## Goal

All supported cloud providers use one lifecycle controller to validate a saved session, renew it when the provider supports renewal, and fall back exactly once to a locally encrypted password login when that provider supports it. A user should not need to manually reauthorize merely because a saved password-login session has expired.

## Scope

The supported providers are Quark, PikPak, Baidu, Tianyi, Aliyun Drive, Xunlei and UC Drive. This design uses their existing adapters; it does not bypass CAPTCHA, SMS, device verification, regional restrictions, or provider risk controls.

Direct password login is currently implemented and enabled only for PikPak, Baidu and Tianyi. The other providers keep their official web/QR authorization because this repository has no verified direct password protocol for them. New providers join the system by declaring one lifecycle policy and, optionally, a validated password relogin adapter.

## Session model

Each provider has one `CloudSessionPolicy`:

| Provider | Durable session | Renewal before fallback | Password fallback |
| --- | --- | --- | --- |
| Quark | Browser Cookie | Validate; no verified programmatic renewal | No |
| PikPak | Access + refresh token | Adapter refreshes access token | Yes |
| Baidu | Browser Cookie | Validate Cookie | Yes |
| Tianyi | Browser Cookie | Validate Cookie | Yes |
| Aliyun | Access + refresh token | Adapter refreshes token | No |
| Xunlei | Access + refresh token plus operation verification | Adapter refreshes token and action verification | No |
| UC | Browser Cookie | Validate Cookie | No |

The controller first executes the normal provider operation. For `AuthorizationExpired` or `NotAuthorized`, it loads a provider-specific password fallback only when the policy permits it. It performs one relogin, validates that the resulting account ID equals the ID saved with that password, saves the new provider session, and retries only read-safe work once. It never replays a mutation such as upload, create, rename, move, copy, trash, delete or logout because an authorization failure after an ambiguous provider response could otherwise duplicate or alter data.

After a mutation session failure, the controller may complete the safe relogin but returns a fixed `SessionRenewedRetryOperation` error. The UI tells the user that login was restored and asks them to perform the mutation again. A CAPTCHA, SMS, risk challenge, wrong-account result, regional restriction, provider rejection, network failure, or repeated expiry never triggers more attempts and is returned normally.

## Credential storage and safety

The existing provider session stays in its current encrypted DPAPI slot. A password-login fallback is a separate DPAPI record with `{version, provider, account_id, username, password}` and a redacted `Debug` implementation. It is only created after a successful password login and account/root validation. It is never included in `CloudAccount`, frontend RPC output, diagnostics, URL parameters, aria2 headers, or error messages.

Web authorization preserves an existing password fallback only if the final authorized account ID matches. A different account removes the fallback. Logout removes both the provider session and the fallback secret. The stored password is loaded into zeroizing memory for automatic relogin; an explicit user password is moved into zeroizing memory before provider calls.

## Shared interfaces

`cloud/session.rs` owns:

```rust
pub enum SessionWorkKind { Read, Mutation, Logout }
pub struct CloudPasswordCredential { provider: CloudProvider, account_id: String, username: String, password: String }
pub fn run_with_session_recovery<T>(
    provider: CloudProvider,
    store: &impl CredentialStore,
    kind: SessionWorkKind,
    work: impl FnMut() -> Result<T, QuarkError>,
    password_relogin: impl FnOnce(&CloudPasswordCredential) -> Result<(), QuarkError>,
) -> Result<T, QuarkError>;
```

`commands.rs` owns provider-specific `password_relogin`, which validates account identity before saving the provider-specific session. It is the only bridge from generic recovery to provider adapters. Existing token refresh stays in each adapter, where provider tokens and verification challenges are understood.

## UI behavior

The three verified password providers display the existing account/password form plus a concise notice that successful password login is encrypted locally to support automatic recovery. Password controls are not shown for providers without a verified direct password adapter. An expired password session that was automatically restored returns to its account/files view; CAPTCHA and other web-verification requirements continue to show the existing official-web action.

## Verification

- Pure Rust lifecycle tests prove one fallback attempt, safe-read retry, no mutation replay, account mismatch rejection, absent-secret behavior, logout cleanup and redacted debug output.
- Command tests cover session persistence after direct login, automatic relogin for PikPak/Baidu/Tianyi, web authorization account switching, and all providers’ policies.
- Component tests cover password-storage disclosure and provider capability gating.
- Existing adapter tests cover token refresh. Targeted Rust and Vitest suites, then `scripts/build-windows.ps1`, are required. No live password should be supplied to automated tests.
