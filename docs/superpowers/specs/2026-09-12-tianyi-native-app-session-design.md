# Tianyi Native App Session Design

## Goal

Use Tianyi's desktop-client session protocol as the primary account/password path, with the existing official WebView authorization and browser bridge retained as the fallback for CAPTCHA, SMS, device verification, protocol changes, and web-only operations.

## Reference behavior

`tickstep/cloudpan189-go` obtains an App session from the legacy PC/Mac login flow and signs `api.cloud.189.cn` requests with `SessionKey` and `SessionSecret`. `MaurUppi/cloudpan189-go` keeps the App session usable when conversion to `COOKIE_LOGIN_USER` fails; its synthetic Cookie is only an internal CLI bypass and must not be copied as an actual credential.

The useful fixes to carry forward are:

- validate empty credentials before network access;
- parse login parameters without panicking when provider markup changes;
- require non-empty App access/session tokens before accepting login;
- keep App login successful even if web-Cookie conversion fails;
- return precise, redacted error classes;
- retry only transient operations and fall back to the official WebView for provider challenges.

## Session model

`TianyiSession` remains backward-compatible with existing Web sessions and gains an optional `TianyiAppSession`. App secrets are serialized only inside the existing DPAPI credential store and are redacted from `Debug` and test diagnostics.

The App session contains the personal and family session keys/secrets, access and refresh tokens returned by Tianyi, the login name, and any successfully derived web Cookie. Existing WebView-only sessions continue to deserialize unchanged.

## Data flow

Account/password login performs the official legacy desktop sequence:

1. Read dynamic login parameters and the RSA public key from `unifyLoginForPC.action`.
2. RSA-encrypt account and password and submit `loginSubmit.do` as app `8025431004` / client type `10020`.
3. Exchange the returned redirect URL at `getSessionForPC.action` for `SessionKey` and `SessionSecret`.
4. Validate the session with a signed, read-only root-directory request.
5. Attempt to derive `COOKIE_LOGIN_USER` through `ssoLogin.action`; failure here does not discard a valid App session.
6. Persist the validated App session and the already-supported encrypted password record.

Normal account restoration, directory listing and download URL resolution prefer signed App API requests. Existing web sessions keep using the browser bridge. Operations not yet represented by the App adapter require a real web Cookie or trigger official WebView authorization rather than inventing a Cookie.

## Signing

App requests include `Date`, `SessionKey`, `Signature`, and `X-Request-ID`. The signature is uppercase HMAC-SHA1 over:

`SessionKey={key}&Operate={method}&RequestURI={path}&Date={http-date}`

The request URL carries the legacy PC client suffix (`clientType=TELEPC`, version `6.2`, channel `web_cloud.189.cn`, and a random nonce).

## Failure and fallback

- Missing/changed login markup, CAPTCHA, SMS, device lock, risk control, and rejected App sessions map to the existing official WebView fallback.
- Wrong-account protection remains enforced before replacing an existing session.
- A failed candidate session never overwrites the last valid session.
- App `InvalidSessionKey` uses the existing one-shot password recovery. If recovery still fails, the official webpage is offered.
- No account, password, Cookie, token, redirect URL, or signature is logged.

## Verification

- Unit tests cover request sequence, RSA ciphertext shape, App token validation, HMAC signing, App-directory parsing, download resolution, old Web-session compatibility, and WebView fallback classification.
- An isolated background live test loads the existing DPAPI password record, performs App login, lists the root, and resolves one download URL without downloading file bytes.
- Run only Tianyi/session/contract focused Rust tests, related frontend contract tests if the IPC shape changes, and `scripts/build-windows.ps1`.

