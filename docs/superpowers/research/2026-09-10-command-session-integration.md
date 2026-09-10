# Command Session Integration Notes

## Evidence collected

`CloudState::with_provider` is the existing per-provider serialisation boundary
([commands.rs](../../../src-tauri/src/cloud/commands.rs#L119-L140)).  The
asynchronous `run`/`run_when_initialized` pair enters that boundary exactly
once after `CloudState` is registered ([commands.rs](../../../src-tauri/src/cloud/commands.rs#L145-L189)).
All normal UI cloud commands use that pair except the file-upload path.

The password-capable providers are already precisely gated by
`provider_capabilities`: PikPak, Baidu and Tianyi only
([types.rs](../../../src-tauri/src/cloud/types.rs#L35-L53)).  Their direct
login adapters and identity probes are:

| Provider | Direct login | Account-ID validation | Existing session write |
| --- | --- | --- | --- |
| PikPak | `PikPakClient::login` ([pikpak.rs](../../../src-tauri/src/cloud/pikpak.rs#L117-L160)) | `PikPakClient::account`, `sub` then `id` ([pikpak.rs](../../../src-tauri/src/cloud/pikpak.rs#L353-L365)) | temporary `MemoryCredentialStore` copied into `pikpak` |
| Baidu | `BaiduClient::login_password` ([baidu.rs](../../../src-tauri/src/cloud/baidu.rs#L544-L586)) | `BaiduClient::account`, `uk` then `user_id` ([baidu.rs](../../../src-tauri/src/cloud/baidu.rs#L181-L203)) | `save_provider_session(Baidu, …)` |
| Tianyi | `TianyiClient::login_password` ([tianyi.rs](../../../src-tauri/src/cloud/tianyi.rs#L375-L415)) | `TianyiClient::account`, `userId`, `id`, then `loginName` ([tianyi.rs](../../../src-tauri/src/cloud/tianyi.rs#L114-L141)) | `save_provider_session(Tianyi, …)` |

Each direct login already performs an account and root-directory probe.  The
command layer repeats those probes in `cloud_login_password`
([commands.rs](../../../src-tauri/src/cloud/commands.rs#L984-L1027)); the
new fallback must not weaken either check.

## Work-kind classification

### Authentication / no recovery wrapper

- `cloud_begin_authorization` (starts an official WebView only; line 436)
- `cloud_complete_authorization` (line 884): finalizes a new user-selected
  session, so it must never restore the old account first.
- `cloud_login_password` (line 984): creates the initial fallback and must
  not depend on one already existing.

After a *successful* web authorization, call
`clear_password_credential_if_account_differs(provider, account.id)` before
returning.  Do not clear on `AuthorizationPending` or any failed validation.

### Read (safe to execute once more after one successful fallback login)

- `cloud_account` (1030)
- `cloud_list_directory` (1040)
- `resolve_stable_cloud_download` (1351), which is used by direct downloads
  and aria2 signed-URL renewal from `src-tauri/src/commands.rs:392`
- the traversal planning call inside `cloud_download_directory` (1524)

`cloud_download_url` itself queues a local task but delegates provider work to
`resolve_stable_cloud_download`; classify that resolving operation as `Read`.
It does not create a task until resolution succeeded ([commands.rs](../../../src-tauri/src/cloud/commands.rs#L1383-L1436)).

### Mutation (perform safe reauthentication but never replay the operation)

- `cloud_create_directory` (1051)
- `cloud_rename` (1100)
- `cloud_move` (1133)
- `cloud_copy` (1169)
- `cloud_trash` (1205)
- `cloud_delete` (1234; permanent PikPak recycle-bin delete)
- `cloud_upload_files` (2723)

The first six already go through `run`; add an explicit `SessionWorkKind` to
each call.  `cloud_upload_files` is the one bypass: it calls
`CloudState::with_provider` directly for Baidu/Tianyi/Aliyun/Xunlei and uses
PikPak's internal lock during upload initialization ([commands.rs](../../../src-tauri/src/cloud/commands.rs#L2723-L2807)).

### Logout

`cloud_logout` (2809) must enter the provider lock but never attempt fallback
login.  `logout_provider` (349) must remove **both** the ordinary provider
session and the provider's password credential.

## Minimal command-layer seam

Keep the generic lifecycle module independent of adapters and put this
adapter bridge in `commands.rs`:

```rust
fn recover_provider_password_session(
    provider: CloudProvider,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
    credential: &CloudPasswordCredential,
) -> Result<(), QuarkError>;

fn run_cloud_work<T>(
    provider: CloudProvider,
    kind: SessionWorkKind,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
    work: impl FnMut() -> Result<T, QuarkError>,
) -> Result<T, QuarkError>;
```

`recover_provider_password_session` should use a temporary store for PikPak;
all three branches must obtain `CloudAccount`, require an exact,
non-empty `account.id == credential.account_id`, and only then write the new
provider session.  Mismatch should remove the fallback credential and return
`WebAuthorizationRequired` (or a dedicated non-secret error) rather than
silently replacing the account.

Change `run` and `run_when_initialized` from an unclassified `FnOnce` work
item to `kind: SessionWorkKind` plus `FnMut`.  Once inside
`with_provider`, call `run_cloud_work(provider, kind, &state.store,
&state.http, || work(&state, &state.http))`.  Read closures can run twice;
mutation and logout closures must be invoked at most once by the lifecycle.
The existing move closures capture their request inputs and use them by
reference, so they can normally satisfy `FnMut` without cloning user data.

For upload, avoid nesting `with_provider` for PikPak: `upload_pikpak_file`
already acquires the PikPak lock for session snapshots (lines 71, 86 and 93).
Wrap its *outer* attempt in the lifecycle without holding the provider lock;
the recovery callback may acquire `with_provider` only when a fallback login
is required.  The other upload branches can enter `with_provider` and call
`run_cloud_work` in the existing closure.  This prevents self-deadlock on the
non-reentrant `std::sync::Mutex`.

## Persistence points

In `cloud_login_password`, save `CloudPasswordCredential { provider,
account_id: account.id, username, password }` only after the existing account
and root-directory validation and after the provider session is persisted.
Keep the submitted password in `zeroize::Zeroizing` until the secure save is
complete.  Do not create password credentials from browser authorization.

`cloud_complete_authorization` currently closes the WebView within every
provider branch.  Refactor only enough to run account-switch cleanup after a
branch has successfully produced `CloudAccount`; preserve the existing
close-on-success behavior.

## TDD command seam

Use a small `CloudHttpTransport` queue fixture (the repository already uses
that style in `src-tauri/tests/cloud_baidu.rs:15-52`) and expose only the
pure `run_cloud_work`/recovery helper as `pub(crate)` for an integration test.
The focused tests should assert request/order counts rather than secret
contents:

1. a Baidu read returns `AuthorizationExpired`, logs in with a saved password,
   validates the same `uk`, then performs the read a second time;
2. a Tianyi mutation returns `AuthorizationExpired`, performs exactly one
   mutation request and returns the fixed retry-operation signal after one
   same-account relogin;
3. a mismatched PikPak `sub` removes the fallback and leaves the old session
   untouched;
4. direct password login persists an encrypted fallback only after the
   account/root validation succeeds;
5. logout removes ordinary and fallback records.

Do not unit-test commands through `AppHandle`: the existing
`run_when_initialized` tests are Tauri-state readiness tests, not an adapter
recovery seam.  Keep them compiling by passing a concrete work kind when its
signature changes.

## Risks to preserve

- `restore_account(Tianyi)` intentionally calls `web_account()` for a
  browser-auth session without a network probe ([commands.rs](../../../src-tauri/src/cloud/commands.rs#L205-L213)); only password fallback needs the stronger account/root validation.
- PikPak already has short-lived token and CAPTCHA recovery inside its client
  ([pikpak.rs](../../../src-tauri/src/cloud/pikpak.rs#L226-L287)); the generic
  lifecycle is a last fallback for `AuthorizationExpired` / `NotAuthorized`,
  never a workaround for `CaptchaRequired`, membership restriction, provider
  rejection, or transport failure.
- Xunlei's helper persists refresh rotation in `with_xunlei_client` (lines
  715-725).  Do not bypass it while adding the generic wrapper.
- A server may execute a mutation before returning an auth error.  Therefore
  the operation callback must never be replayed; the lifecycle can restore a
  session and return its fixed retry result only.
