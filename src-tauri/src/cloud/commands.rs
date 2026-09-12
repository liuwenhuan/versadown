use super::{http::response_json, types::required, *};
use crate::models::{
    CloudDirectoryGroup, NewCloudDirectoryGroup, NewCloudDirectoryGroupItem, QueueTaskPayload,
};
use crate::{
    commands::{
        cloud_directory_group_queue_task, task_action_outcome, AppState, TaskActionOutcome,
    },
    duplicate_state, DownloadTask, NewTask, TaskStatus, TaskStore, UriSubmissionOptions,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};
use tauri::{
    webview::{NewWindowResponse, PageLoadEvent},
    AppHandle, Manager, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_dialog::DialogExt;

const CLOUD_SHARE_PREPARE_FAILURE: &str = "网盘分享下载准备失败，请重试";

pub(crate) fn share_resolution_failure_message(error: &QuarkError) -> &'static str {
    match error {
        QuarkError::NotAuthorized => "尚未授权此网盘，请登录后使用",
        QuarkError::AuthorizationExpired => "网盘会话已过期，请重新登录授权",
        QuarkError::SessionRenewedRetryOperation => "登录已自动恢复，请重新执行本次操作",
        QuarkError::WebAuthorizationRequired => "此登录方式需要网页验证，请点击网页授权完成登录",
        QuarkError::CaptchaRequired => "网盘要求完成官方操作验证后重试",
        QuarkError::PikpakRegionalMembershipRestricted => {
            "区域会员在当前 IP 地区下载受限；请切换到有权限 IP 后重新登录重试，或升级全球会员"
        }
        QuarkError::AuthorizationPending => "正在等待网页登录，请在授权窗口完成登录",
        QuarkError::InvalidResponse => "网盘返回的下载信息无效，请刷新后重试",
        QuarkError::Storage => "安全凭据存储不可用",
        QuarkError::Transport(_) => "网盘分享下载地址网络请求失败，请检查网络后重试",
        QuarkError::Provider(_) => "网盘分享下载地址请求失败，请稍后重试",
        QuarkError::InvalidInput(_) => "网盘分享下载请求无效，请重新解析分享链接",
        QuarkError::UploadUnavailable | QuarkError::ProviderUnsupported => {
            "当前网盘分享下载暂不支持"
        }
    }
}

/// Keeps the durable failure code in one place for direct cloud downloads,
/// share downloads, and directory leaves. UI decisions must use this code,
/// never a localized provider message.
pub(crate) fn cloud_failure_code(error: &QuarkError) -> &'static str {
    match error {
        QuarkError::NotAuthorized => "not_authorized",
        QuarkError::AuthorizationExpired => "credentials_expired",
        QuarkError::SessionRenewedRetryOperation => "session_renewed_retry_operation",
        QuarkError::WebAuthorizationRequired | QuarkError::CaptchaRequired => "web_auth_required",
        QuarkError::PikpakRegionalMembershipRestricted => "pikpak_regional_membership_restricted",
        QuarkError::AuthorizationPending => "authorization_pending",
        QuarkError::InvalidResponse => "invalid_response",
        QuarkError::Storage => "credential_storage_error",
        QuarkError::Transport(_) => "network_error",
        QuarkError::Provider(_) => "provider_error",
        QuarkError::InvalidInput(_) => "invalid_input",
        QuarkError::UploadUnavailable | QuarkError::ProviderUnsupported => "provider_unsupported",
    }
}

pub(crate) fn requires_cloud_reauthorization_failure(code: Option<&str>) -> bool {
    matches!(
        code,
        Some("not_authorized" | "credentials_expired" | "web_auth_required")
    )
}

/// Personal files lock their exact source. Share files lock the stable plan,
/// so new siblings, overwrite, refresh and cleanup cannot bypass each other
/// while the plan's entry set changes. The guard covers resolution through
/// aria2 rebinding, or cleanup claiming through the provider mutation.
#[derive(Default)]
pub struct CloudDownloadSourceGate {
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

#[derive(Default)]
struct PendingCloudAuthorizationRetries {
    task_ids: HashSet<uuid::Uuid>,
    directory_items: HashSet<(uuid::Uuid, uuid::Uuid)>,
}

impl CloudDownloadSourceGate {
    pub async fn acquire(
        &self,
        source: &str,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, QuarkError> {
        let key = if source.starts_with("cloud-share://") {
            let (provider, share, _) = share::parse_stable_cloud_share_source(source)?;
            format!("cloud-share://{}/{share}", provider.key())
        } else {
            source.to_owned()
        };
        let lock = {
            let mut locks = self.locks.lock().map_err(|_| QuarkError::Storage)?;
            Arc::clone(
                locks
                    .entry(key)
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        Ok(lock.lock_owned().await)
    }
}

pub struct CloudState {
    pub store: DpapiCredentialStore,
    pub share_plans: share::CloudSharePlanStore,
    pub share_refresh_budget: share::CloudShareRefreshBudget,
    pub(crate) password_download_refresh_budget: browser_login::PasswordDownloadRefreshBudget,
    pending_authorization_retries: Mutex<HashMap<String, PendingCloudAuthorizationRetries>>,
    password_recovery_attempts: Mutex<HashMap<String, std::time::Instant>>,
    http: ReqwestTransport,
    quark_operation: Mutex<()>,
    pikpak_operation: Mutex<()>,
    baidu_operation: Mutex<()>,
    tianyi_operation: Mutex<()>,
    aliyun_operation: Mutex<()>,
    xunlei_operation: Mutex<()>,
    uc_operation: Mutex<()>,
}
impl CloudState {
    pub fn upload_pikpak_file(
        &self,
        http: &(impl CloudHttpTransport + CloudUploadTransport),
        parent: &str,
        path: &Path,
    ) -> Result<CloudFile, QuarkError> {
        super::upload_pikpak_file(http, parent, path, |payload| {
            self.initialize_pikpak_upload(http, payload)
        })
    }
    fn initialize_pikpak_upload(
        &self,
        http: &impl CloudHttpTransport,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, QuarkError> {
        const URL: &str = "https://api-drive.mypikpak.com/drive/v1/files";
        let mut session = self.with_provider(CloudProvider::Pikpak, |store| {
            PikPakClient::new(http, store).session_snapshot()
        })?;
        let mut access_retried = false;
        let mut captcha_retried = false;
        loop {
            // Both upload-init HTTPS and the subsequent OSS transfers are outside the session lock.
            let result = PikPakClient::new(http, &self.store).send_snapshot(
                "POST",
                URL,
                payload.clone(),
                &session,
            );
            match result {
                Err(QuarkError::AuthorizationExpired) if !access_retried => {
                    session = self.with_provider(CloudProvider::Pikpak, |store| {
                        PikPakClient::new(http, store)
                            .recover_snapshot(&session, false, "POST", URL)
                    })?;
                    access_retried = true;
                }
                Err(QuarkError::CaptchaRequired) if !captcha_retried => {
                    session = self.with_provider(CloudProvider::Pikpak, |store| {
                        PikPakClient::new(http, store).recover_snapshot(&session, true, "POST", URL)
                    })?;
                    captcha_retried = true;
                }
                Err(QuarkError::CaptchaRequired) => {
                    return Err(QuarkError::WebAuthorizationRequired)
                }
                other => return other,
            }
        }
    }
    pub fn new(root: PathBuf) -> Result<Self, QuarkError> {
        Ok(Self {
            share_plans: share::CloudSharePlanStore::new(DpapiCredentialStore::new(root.clone())?),
            share_refresh_budget: Default::default(),
            password_download_refresh_budget: Default::default(),
            pending_authorization_retries: Default::default(),
            password_recovery_attempts: Default::default(),
            store: DpapiCredentialStore::new(root)?,
            http: ReqwestTransport::new()?,
            quark_operation: Mutex::new(()),
            pikpak_operation: Mutex::new(()),
            baidu_operation: Mutex::new(()),
            tianyi_operation: Mutex::new(()),
            aliyun_operation: Mutex::new(()),
            xunlei_operation: Mutex::new(()),
            uc_operation: Mutex::new(()),
        })
    }

    fn queue_authorization_retries(
        &self,
        provider: CloudProvider,
        task_ids: impl IntoIterator<Item = uuid::Uuid>,
        directory_items: impl IntoIterator<Item = (uuid::Uuid, uuid::Uuid)>,
    ) -> Result<(), QuarkError> {
        let mut pending = self
            .pending_authorization_retries
            .lock()
            .map_err(|_| QuarkError::Storage)?;
        let entry = pending.entry(provider.key().to_string()).or_default();
        entry.task_ids.extend(task_ids);
        entry.directory_items.extend(directory_items);
        Ok(())
    }

    fn take_authorization_retries(
        &self,
        provider: CloudProvider,
    ) -> Result<PendingCloudAuthorizationRetries, QuarkError> {
        self.pending_authorization_retries
            .lock()
            .map_err(|_| QuarkError::Storage)?
            .remove(provider.key())
            .map_or_else(|| Ok(PendingCloudAuthorizationRetries::default()), Ok)
    }
    /// All session consumers, including recovery and logout, share this provider boundary.
    pub fn with_provider<T>(
        &self,
        provider: CloudProvider,
        work: impl FnOnce(&DpapiCredentialStore) -> Result<T, QuarkError>,
    ) -> Result<T, QuarkError> {
        let mutex = match provider {
            CloudProvider::Quark => &self.quark_operation,
            CloudProvider::Pikpak => &self.pikpak_operation,
            CloudProvider::Baidu => &self.baidu_operation,
            CloudProvider::Tianyi => &self.tianyi_operation,
            CloudProvider::Aliyun => &self.aliyun_operation,
            CloudProvider::Xunlei => &self.xunlei_operation,
            CloudProvider::Uc => &self.uc_operation,
        };
        let _guard = mutex.lock().map_err(|_| QuarkError::Storage)?;
        work(&self.store)
    }
}

struct TianyiBrowserValidationTransport<'a, T: CloudHttpTransport> {
    window: &'a tauri::WebviewWindow,
    native: &'a T,
}

impl<T: CloudHttpTransport> CloudHttpTransport for TianyiBrowserValidationTransport<'_, T> {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        if request.url.starts_with("https://cloud.189.cn/") {
            tianyi_browser_send(self.window, request)
        } else {
            self.native.send(request)
        }
    }
}

fn password_webview_data_directory_from_root(
    root: &Path,
    provider: CloudProvider,
) -> Result<PathBuf, QuarkError> {
    if !matches!(provider, CloudProvider::Baidu | CloudProvider::Tianyi) {
        return Err(QuarkError::InvalidInput(
            "该网盘不支持持久化账号密码登录".into(),
        ));
    }
    Ok(root.join("web-auth").join(provider.key()))
}

fn password_webview_data_directory(
    app: &AppHandle,
    provider: CloudProvider,
) -> Result<PathBuf, QuarkError> {
    let root = if std::env::var("ALLDOWNLOADER_BACKGROUND_UI_TEST")
        .ok()
        .as_deref()
        == Some("1")
    {
        std::env::var_os("ALLDOWNLOADER_TEST_DATA_DIR")
            .map(PathBuf::from)
            .ok_or(QuarkError::Storage)?
    } else {
        app.path().app_data_dir().map_err(|_| QuarkError::Storage)?
    };
    password_webview_data_directory_from_root(&root, provider)
}

fn prepare_baidu_account_login(
    store: &impl CredentialStore,
    submitted_username: &str,
) -> Result<bool, QuarkError> {
    let submitted_username = submitted_username.trim();
    if let Some(owner) = baidu_browser_profile_username(store)? {
        return Ok(owner.trim() != submitted_username);
    }
    // A session captured by manual web authorization proves that a Baidu
    // profile exists, but not that it belongs to the submitted username. On
    // the first password login after upgrading, clear that unbound profile
    // once instead of accepting its already logged-in account.
    if store.load(CloudProvider::Baidu.key())?.is_some() {
        return Ok(true);
    }
    let previous_username =
        if let Some(credential) = load_password_credential(store, CloudProvider::Baidu)? {
            Some(credential.username().to_owned())
        } else {
            load_password_login_draft(store, CloudProvider::Baidu)?
                .map(|draft| draft.username().to_owned())
        };
    Ok(previous_username.is_some_and(|username| username.trim() != submitted_username))
}

fn clear_password_webview_profile(
    app: &AppHandle,
    provider: CloudProvider,
) -> Result<(), QuarkError> {
    for label in [
        format!("cloud-auth-{}", provider.key()),
        format!("cloud-password-{}", provider.key()),
    ] {
        if let Some(window) = app.get_webview_window(&label) {
            let _ = window.close();
        }
    }
    let label = format!("cloud-password-reset-{}", provider.key());
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.close();
    }
    let window = WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::External(
            "about:blank"
                .parse()
                .map_err(|_| QuarkError::InvalidResponse)?,
        ),
    )
    .visible(false)
    .data_directory(password_webview_data_directory(app, provider)?)
    .build()
    .map_err(|_| QuarkError::Storage)?;
    let result = window
        .clear_all_browsing_data()
        .map_err(|_| QuarkError::Storage);
    let _ = window.close();
    result
}

fn tianyi_browser_request_script(request: &CloudHttpRequest) -> Result<String, QuarkError> {
    let url = validate_tianyi_browser_request(request.method, &request.url)?;
    let payload = serde_json::json!({
        "method": request.method,
        "url": url.as_str(),
        "headers": tianyi_browser_request_headers(request.headers.clone()),
        "body": request.body,
    });
    Ok(format!(
        r#"(() => {{
          if (location.origin !== 'https://cloud.189.cn') return {{done:false}};
          const key = '__alldownloader_tianyi_request';
          const current = window[key];
          if (current?.done) {{
            delete window[key];
            return {{done:true, value:current.value}};
          }}
          if (!current) {{
            const input = {payload};
            const slot = {{done:false}};
            window[key] = slot;
            fetch(input.url, {{
              method: input.method,
              headers: input.headers,
              body: input.body == null ? undefined : input.body,
              credentials: 'include',
              redirect: 'follow',
            }}).then(async response => {{
              slot.value = {{status:response.status, body:await response.text()}};
              slot.done = true;
            }}, () => {{
              slot.value = {{transportError:true}};
              slot.done = true;
            }});
          }}
          return {{done:false}};
        }})()"#
    ))
}

fn tianyi_browser_send(
    window: &tauri::WebviewWindow,
    request: CloudHttpRequest,
) -> Result<CloudHttpResponse, QuarkError> {
    let script = tianyi_browser_request_script(&request)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    while std::time::Instant::now() < deadline {
        if window
            .app_handle()
            .get_webview_window(window.label())
            .is_none()
        {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        match web_storage_tokens(window, &script) {
            Ok(state) if state["done"].as_bool() == Some(true) => {
                let value = state.get("value").ok_or(QuarkError::InvalidResponse)?;
                if value["transportError"].as_bool() == Some(true) {
                    return Err(QuarkError::Transport("天翼官网授权页请求失败".into()));
                }
                let status = value["status"]
                    .as_u64()
                    .and_then(|status| u16::try_from(status).ok())
                    .ok_or(QuarkError::InvalidResponse)?;
                let body = value["body"]
                    .as_str()
                    .ok_or(QuarkError::InvalidResponse)?
                    .to_owned();
                return Ok(CloudHttpResponse {
                    status,
                    body,
                    set_cookies: Vec::new(),
                });
            }
            Ok(_) | Err(QuarkError::AuthorizationPending) => {}
            Err(error) => return Err(error),
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(QuarkError::WebAuthorizationRequired)
}

fn cookie(store: &impl CredentialStore) -> Result<String, QuarkError> {
    String::from_utf8(
        store
            .load("quark")?
            .ok_or(QuarkError::AuthorizationExpired)?,
    )
    .map_err(|_| QuarkError::Storage)
}
async fn run<T: Send + 'static>(
    app: AppHandle,
    provider: CloudProvider,
    work: impl FnOnce(&CloudState, &ReqwestTransport) -> Result<T, QuarkError> + Send + 'static,
) -> Result<T, QuarkError> {
    run_when_initialized(
        move || {
            app.try_state::<Arc<CloudState>>()
                .map(|state| Arc::clone(state.inner()))
        },
        provider,
        work,
        std::time::Duration::from_secs(10),
    )
    .await
}

async fn run_when_initialized<T: Send + 'static>(
    mut lookup: impl FnMut() -> Option<Arc<CloudState>> + Send + 'static,
    provider: CloudProvider,
    work: impl FnOnce(&CloudState, &ReqwestTransport) -> Result<T, QuarkError> + Send + 'static,
    initialization_timeout: std::time::Duration,
) -> Result<T, QuarkError> {
    // The webview can issue IPC before setup has registered CloudState.
    // Wait without holding provider locks or occupying the blocking pool;
    // initialization errors remain distinct from errors in actual cloud work.
    let deadline = tokio::time::Instant::now() + initialization_timeout;
    let state = loop {
        if let Some(state) = lookup() {
            break state;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(QuarkError::Transport(
                "网盘服务初始化尚未完成，请稍后重试".into(),
            ));
        }
        tokio::time::sleep(remaining.min(std::time::Duration::from_millis(25))).await;
    };
    tauri::async_runtime::spawn_blocking(move || {
        state.with_provider(provider, |_| work(&state, &state.http))
    })
    .await
    .map_err(|_| QuarkError::Transport("网盘任务中断".into()))?
}

async fn run_session_work<T: Send + 'static>(
    app: AppHandle,
    provider: CloudProvider,
    kind: SessionWorkKind,
    mut work: impl FnMut(&CloudState, &ReqwestTransport) -> Result<T, QuarkError> + Send + 'static,
) -> Result<T, QuarkError> {
    let refresh_app = app.clone();
    run(app, provider, move |state, http| {
        let first = if browser_login::supports_browser_password(provider)
            && kind == SessionWorkKind::Read
        {
            run_with_browser_session_recovery(
                &refresh_app,
                provider,
                kind,
                state,
                http,
                true,
                || work(state, http),
            )
        } else {
            run_cloud_work(provider, kind, &state.store, http, || work(state, http))
        };
        let Err(error) = first else {
            return first;
        };
        // Mutating provider operations are never replayed. A provider captcha
        // is also never retried; the official page must handle it explicitly.
        if kind != SessionWorkKind::Read || !can_retry_read_with_web_session(&error) {
            return Err(error);
        }
        match retry_read_from_active_authorization_window(
            &refresh_app,
            provider,
            &state.store,
            || run_cloud_work(provider, kind, &state.store, http, || work(state, http)),
        ) {
            Ok(Some(value)) => Ok(value),
            Ok(None) | Err(_) => Err(error),
        }
    })
    .await
}

async fn run_web_session_read_work<T: Send + 'static>(
    app: AppHandle,
    provider: CloudProvider,
    mut work: impl FnMut(&CloudState, &ReqwestTransport) -> Result<T, QuarkError> + Send + 'static,
) -> Result<T, QuarkError> {
    let refresh_app = app.clone();
    run(app, provider, move |state, http| {
        let first = if browser_login::supports_browser_password(provider) {
            run_with_browser_session_recovery(
                &refresh_app,
                provider,
                SessionWorkKind::Read,
                state,
                http,
                false,
                || work(state, http),
            )
        } else {
            work(state, http)
        };
        let Err(error) = first else {
            return first;
        };
        if !can_retry_read_with_web_session(&error) {
            return Err(error);
        }
        match retry_read_from_active_authorization_window(
            &refresh_app,
            provider,
            &state.store,
            || work(state, http),
        ) {
            Ok(Some(value)) => Ok(value),
            Ok(None) | Err(_) => Err(error),
        }
    })
    .await
}

fn run_cloud_work<T>(
    provider: CloudProvider,
    kind: SessionWorkKind,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
    work: impl FnMut() -> Result<T, QuarkError>,
) -> Result<T, QuarkError> {
    run_with_session_recovery(provider, store, kind, work, |credential| {
        recover_provider_password_session(provider, store, http, credential)
    })
}

enum PasswordRecoveredSession {
    Pikpak(Vec<u8>),
    Baidu(baidu::BaiduSession),
    Tianyi(tianyi::TianyiSession),
}

fn should_attempt_native_password_recovery(
    provider: CloudProvider,
    kind: SessionWorkKind,
    error: &QuarkError,
) -> bool {
    kind == SessionWorkKind::Read
        && browser_login::supports_browser_password(provider)
        && matches!(
            error,
            QuarkError::AuthorizationExpired
                | QuarkError::NotAuthorized
                | QuarkError::WebAuthorizationRequired
                | QuarkError::CaptchaRequired
        )
}

fn run_with_browser_session_recovery<T>(
    app: &AppHandle,
    provider: CloudProvider,
    kind: SessionWorkKind,
    state: &CloudState,
    http: &impl CloudHttpTransport,
    allow_challenge_recovery: bool,
    mut work: impl FnMut() -> Result<T, QuarkError>,
) -> Result<T, QuarkError> {
    let error = match work() {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    if !should_attempt_native_password_recovery(provider, kind, &error) {
        return Err(error);
    }
    let Some(credential) = load_password_credential(&state.store, provider)? else {
        return Err(error);
    };
    {
        let mut attempts = state
            .password_recovery_attempts
            .lock()
            .map_err(|_| QuarkError::Storage)?;
        if attempts
            .get(provider.key())
            .is_some_and(|last| last.elapsed() < std::time::Duration::from_secs(300))
        {
            return Err(error);
        }
        attempts.insert(provider.key().into(), std::time::Instant::now());
    }
    let native_error =
        match recover_provider_password_session(provider, &state.store, http, &credential) {
            Ok(()) => return work(),
            Err(error) => error,
        };
    if !allow_challenge_recovery {
        return Err(native_error);
    }
    if !browser_login::can_fallback_to_browser(&native_error)
        && !matches!(native_error, QuarkError::CaptchaRequired)
    {
        return Err(native_error);
    }
    let challenge = matches!(
        native_error,
        QuarkError::WebAuthorizationRequired | QuarkError::CaptchaRequired
    );
    browser_password_login(
        app,
        provider,
        &state.store,
        http,
        credential.username(),
        credential.password(),
        Some(credential.account_id()),
        challenge,
    )?;
    // Exactly one retry with the validated new session; no recursion and no
    // replay of uploads, deletion, transfers or other mutating operations.
    work()
}

/// Runs on a provider's blocking worker. Waiting for the official browser must
/// never hold up the Windows message loop. The provider lock coalesces renewal
/// across concurrent downloads: later readers see the newly saved session.
fn browser_auth_diagnostic_script(stage: &str, reason: &str) -> String {
    let payload = serde_json::json!({ "stage": stage, "reason": reason });
    format!("window.__alldownloaderAuthDiagnostic = {payload};")
}

fn validate_tianyi_browser_request(method: &str, raw_url: &str) -> Result<url::Url, QuarkError> {
    if !matches!(method, "GET" | "POST") {
        return Err(QuarkError::InvalidInput(
            "天翼授权页请求方法不受支持".into(),
        ));
    }
    let url = url::Url::parse(raw_url).map_err(|_| QuarkError::InvalidResponse)?;
    let allowed_path = url.path().starts_with("/api/")
        || url.path().starts_with("/v2/")
        || url.path() == "/main.action";
    if url.scheme() != "https"
        || url.host_str() != Some("cloud.189.cn")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !allowed_path
    {
        return Err(QuarkError::InvalidInput(
            "天翼授权页请求地址不受支持".into(),
        ));
    }
    Ok(url)
}

fn tianyi_browser_request_headers(headers: BTreeMap<String, String>) -> BTreeMap<String, String> {
    headers
        .into_iter()
        .filter(|(name, value)| {
            matches!(
                name.to_ascii_lowercase().as_str(),
                "accept" | "content-type" | "browser-id"
            ) && !value.chars().any(char::is_control)
                && value.len() <= 1024
        })
        .collect()
}

fn browser_password_login(
    app: &AppHandle,
    provider: CloudProvider,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
    username: &str,
    password: &str,
    expected_account: Option<&str>,
    challenge: bool,
) -> Result<CloudAccount, QuarkError> {
    if !browser_login::supports_browser_password(provider) {
        return Err(QuarkError::WebAuthorizationRequired);
    }
    let label = format!("cloud-password-{}", provider.key());
    if app.get_webview_window(&label).is_some() {
        return Err(QuarkError::AuthorizationPending);
    }
    let script = Arc::new(browser_login::password_script(
        provider, username, password,
    )?);
    let url = match provider {
        CloudProvider::Baidu => "https://pan.baidu.com",
        CloudProvider::Tianyi => "https://cloud.189.cn",
        _ => return Err(QuarkError::WebAuthorizationRequired),
    };
    let mut builder = WebviewWindowBuilder::new(
        app,
        &label,
        WebviewUrl::External(url.parse().map_err(|_| QuarkError::InvalidResponse)?),
    )
    .title("网盘登录验证：请按官方页面提示完成")
    .inner_size(1024.0, 760.0)
    .visible(challenge)
    // Keep one provider profile so Baidu/Tianyi can reuse the official page's
    // stable device context. Explicit account replacement clears this profile
    // before this window is built; validated sessions and passwords remain
    // persisted separately with DPAPI.
    .initialization_script_for_all_frames(script.as_str())
    .on_new_window(|_, _| NewWindowResponse::Deny)
    .on_page_load(move |window, payload| {
        if payload.event() == PageLoadEvent::Finished
            && browser_login::trusted_login_url(provider, payload.url())
        {
            if provider == CloudProvider::Baidu && is_baidu_authorization_page(payload.url()) {
                let _ = window.eval(BAIDU_AUTO_OPEN_LOGIN_JS);
            }
        }
    });
    builder = builder.data_directory(password_webview_data_directory(app, provider)?);
    if provider == CloudProvider::Tianyi {
        builder = builder.initialization_script(include_str!("tianyi-browser-context.js"));
    }
    let window = builder
        .build()
        .map_err(|_| QuarkError::WebAuthorizationRequired)?;
    let started = std::time::Instant::now();
    let result = (|| {
        let mut shown = challenge;
        let mut last_notice = String::new();
        while started.elapsed() < std::time::Duration::from_secs(180) {
            if app.get_webview_window(&label).is_none() {
                return Err(QuarkError::WebAuthorizationRequired);
            }
            if !shown && started.elapsed() >= std::time::Duration::from_secs(12) {
                window
                    .show()
                    .map_err(|_| QuarkError::WebAuthorizationRequired)?;
                let _ = window.set_focus();
                shown = true;
            }
            let cookies = browser_cookies(&window, web_session_cookie_urls(provider))?;
            let browser_context = if provider == CloudProvider::Tianyi {
                tianyi_browser_context(&window).ok()
            } else {
                None
            };
            let mut validation_stage = "读取登录凭据";
            let candidate = if provider == CloudProvider::Tianyi {
                let validation_http = TianyiBrowserValidationTransport {
                    window: &window,
                    native: http,
                };
                validate_browser_password_session(
                    provider,
                    store,
                    &validation_http,
                    &cookies,
                    browser_context.as_ref(),
                    expected_account,
                    |stage| validation_stage = stage,
                )
            } else {
                validate_browser_password_session(
                    provider,
                    store,
                    http,
                    &cookies,
                    browser_context.as_ref(),
                    expected_account,
                    |stage| validation_stage = stage,
                )
            };
            match candidate {
                Ok(account) => return Ok(account),
                Err(QuarkError::Storage) => return Err(QuarkError::Storage),
                Err(QuarkError::InvalidInput(message)) => {
                    return Err(QuarkError::InvalidInput(message))
                }
                Err(error) => {
                    // Only fixed stage/error labels are exposed. Never show
                    // response bodies, account names, passwords or cookies.
                    let reason = match &error {
                        QuarkError::AuthorizationExpired | QuarkError::NotAuthorized => {
                            "凭据尚未生效"
                        }
                        QuarkError::InvalidResponse => "接口返回格式无法识别",
                        QuarkError::WebAuthorizationRequired | QuarkError::CaptchaRequired => {
                            "接口要求安全验证"
                        }
                        QuarkError::Transport(_) => "网络请求失败",
                        QuarkError::Provider(message) if provider == CloudProvider::Tianyi => {
                            message.as_str()
                        }
                        _ => "接口暂未接受凭据",
                    };
                    let notice = format!("自动登录 · {validation_stage}：{reason}");
                    if notice != last_notice {
                        let _ = window.set_title(&notice);
                        if std::env::var_os("ALLDOWNLOADER_BACKGROUND_UI_TEST").is_some() {
                            let _ = window
                                .eval(&browser_auth_diagnostic_script(validation_stage, reason));
                        }
                        last_notice = notice;
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1000));
        }
        Err(QuarkError::WebAuthorizationRequired)
    })();
    let _ = window.close();
    if result.is_ok() && provider == CloudProvider::Baidu {
        set_baidu_browser_profile_username(store, Some(username))?;
        set_browser_password_login_preferred(store, provider, true)?;
    }
    result
}

fn validate_browser_password_session(
    provider: CloudProvider,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
    cookies: &[String],
    browser_context: Option<&serde_json::Value>,
    expected_account: Option<&str>,
    mut report_stage: impl FnMut(&'static str),
) -> Result<CloudAccount, QuarkError> {
    report_stage("读取登录凭据");
    let (account, snapshot) = match provider {
        CloudProvider::Baidu => {
            let mut session = baidu::parse_baidu_web_session(cookies)?;
            session.auth_method = CloudAuthMethod::Password;
            let client = baidu::BaiduClient::new(session.clone(), http);
            report_stage("账号信息校验");
            let account = client.account()?;
            report_stage("目录访问校验");
            client.directory("/")?;
            (
                account,
                serde_json::to_vec(&session).map_err(|_| QuarkError::Storage)?,
            )
        }
        CloudProvider::Tianyi => {
            let mut session = tianyi::parse_tianyi_web_session(cookies)?;
            if let Some(context) = browser_context {
                session.apply_browser_context(context);
            }
            if !session.has_browser_context() {
                report_stage("等待官网浏览器标识");
                return Err(QuarkError::AuthorizationPending);
            }
            session.auth_method = CloudAuthMethod::Password;
            let client = tianyi::TianyiClient::new(session.clone(), http);
            // A generic "tianyi-web" account is not enough for auto-renewal.
            report_stage("账号信息校验");
            let account = client.account()?;
            report_stage("目录访问校验");
            client.directory("-11")?;
            (
                account,
                serde_json::to_vec(&session).map_err(|_| QuarkError::Storage)?,
            )
        }
        _ => return Err(QuarkError::WebAuthorizationRequired),
    };
    if account.id.is_empty() || expected_account.is_some_and(|expected| expected != account.id) {
        return Err(QuarkError::InvalidInput(
            "登录账号与原任务账号不一致，已保留原凭据，请使用原账号登录".into(),
        ));
    }
    report_stage("保存登录凭据");
    store.save(provider.key(), &snapshot)?;
    Ok(account)
}

fn recover_provider_password_session(
    provider: CloudProvider,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
    credential: &CloudPasswordCredential,
) -> Result<(), QuarkError> {
    if credential.provider() != provider {
        return Err(QuarkError::Storage);
    }
    if browser_password_login_preferred(store, provider)? {
        return Err(QuarkError::WebAuthorizationRequired);
    }

    let (account, session) = match provider {
        CloudProvider::Pikpak => {
            let temporary = MemoryCredentialStore::default();
            let client = PikPakClient::new(http, &temporary);
            client.login(credential.username(), credential.password())?;
            let account = client.account()?;
            client.list("0")?;
            let session = temporary
                .load(CloudProvider::Pikpak.key())?
                .ok_or(QuarkError::Storage)?;
            (account, PasswordRecoveredSession::Pikpak(session))
        }
        CloudProvider::Baidu => {
            let session = match baidu::BaiduClient::login_password(
                credential.username(),
                credential.password(),
                http,
            ) {
                Ok(session) => session,
                Err(
                    error @ (QuarkError::WebAuthorizationRequired | QuarkError::CaptchaRequired),
                ) => {
                    set_browser_password_login_preferred(store, provider, true)?;
                    return Err(error);
                }
                Err(error) => return Err(error),
            };
            set_browser_password_login_preferred(store, provider, false)?;
            let client = baidu::BaiduClient::new(session.clone(), http);
            let account = client.account()?;
            (account, PasswordRecoveredSession::Baidu(session))
        }
        CloudProvider::Tianyi => {
            let session = tianyi::TianyiClient::login_password(
                credential.username(),
                credential.password(),
                http,
            )?;
            let client = tianyi::TianyiClient::new(session.clone(), http);
            let account = client.account()?;
            (account, PasswordRecoveredSession::Tianyi(session))
        }
        CloudProvider::Quark
        | CloudProvider::Aliyun
        | CloudProvider::Xunlei
        | CloudProvider::Uc => return Err(QuarkError::WebAuthorizationRequired),
    };

    if account.id.is_empty() || account.id != credential.account_id() {
        // Baidu intentionally has one account slot. Selecting a different
        // browser account discards the old password instead of retaining an
        // inactive multi-account credential.
        if !account.id.is_empty() {
            select_password_credential_account(store, provider, &account.id)?;
        }
        return Err(QuarkError::WebAuthorizationRequired);
    }
    match session {
        PasswordRecoveredSession::Pikpak(session) => {
            store.save(CloudProvider::Pikpak.key(), &session)?
        }
        PasswordRecoveredSession::Baidu(session) => {
            save_provider_session(CloudProvider::Baidu, store, &session)?
        }
        PasswordRecoveredSession::Tianyi(session) => {
            save_provider_session(CloudProvider::Tianyi, store, &session)?
        }
    }
    Ok(())
}

pub fn restore_account(
    provider: CloudProvider,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
) -> Result<CloudAccount, QuarkError> {
    if store.load(provider.key())?.is_none() {
        return Err(QuarkError::NotAuthorized);
    }
    match provider {
        CloudProvider::Quark => QuarkClient::new(cookie(store)?, http).account(),
        CloudProvider::Pikpak => PikPakClient::new(http, store).account(),
        CloudProvider::Baidu => {
            let session = load_provider_session(CloudProvider::Baidu, store)?;
            baidu::BaiduClient::new(session, http).account()
        }
        CloudProvider::Tianyi => {
            let session: tianyi::TianyiSession =
                load_provider_session(CloudProvider::Tianyi, store)?;
            let client = tianyi::TianyiClient::new(session.clone(), http);
            if session.auth_method == CloudAuthMethod::Web {
                client.web_account()
            } else {
                client.account()
            }
        }
        CloudProvider::Aliyun => aliyun::AliyunClient::new(store, http).account(),
        CloudProvider::Xunlei => with_xunlei_client(store, http, |client| {
            if client.session().auth_method == CloudAuthMethod::Web
                && client.session().profile.client_secret.is_empty()
            {
                // Validate the root before restoring the workspace. The helper
                // also preserves modern refresh rotation when this probe fails.
                client.directory("0")?;
                return client.web_account();
            }
            client.account()
        }),
        CloudProvider::Uc => {
            let session = load_provider_session(CloudProvider::Uc, store)?;
            // A browser-granted UC session is sufficient to restore its tab.
            // File operations still validate the session against UC when the
            // user opens it, but an internal member probe must not hide a
            // previously authorized account at startup.
            uc::UcClient::new(session, http).web_account()
        }
    }
}

fn load_provider_session<T: DeserializeOwned>(
    provider: CloudProvider,
    store: &impl CredentialStore,
) -> Result<T, QuarkError> {
    let bytes = store
        .load(provider.key())?
        .ok_or(QuarkError::NotAuthorized)?;
    serde_json::from_slice(&bytes).map_err(|_| QuarkError::Storage)
}

fn save_provider_session<T: Serialize>(
    provider: CloudProvider,
    store: &impl CredentialStore,
    session: &T,
) -> Result<(), QuarkError> {
    let bytes = serde_json::to_vec(session).map_err(|_| QuarkError::Storage)?;
    store.save(provider.key(), &bytes)
}

/// Temporarily installs a fresh cookie snapshot captured from the already
/// authorized official web tab, then keeps it only if the original read-only
/// operation succeeds.  A rejected candidate never destroys the saved
/// session that the user could still use after reopening the tab.
fn retry_read_with_refreshed_web_session<T>(
    provider: CloudProvider,
    store: &impl CredentialStore,
    session: &[u8],
    retry: impl FnOnce() -> Result<T, QuarkError>,
) -> Result<T, QuarkError> {
    if session.is_empty() {
        return Err(QuarkError::AuthorizationExpired);
    }
    let previous = store.load(provider.key())?;
    store.save(provider.key(), session)?;
    match retry() {
        Ok(value) => Ok(value),
        Err(error) => {
            match previous {
                Some(previous) => store.save(provider.key(), &previous)?,
                None => store.remove(provider.key())?,
            }
            Err(error)
        }
    }
}

fn can_retry_read_with_web_session(error: &QuarkError) -> bool {
    matches!(
        error,
        QuarkError::AuthorizationExpired | QuarkError::WebAuthorizationRequired
    )
}

/// Captures only the provider-specific session material from an official
/// authorization WebView that is still open during a browser-login flow.
fn active_web_session_snapshot(
    app: &AppHandle,
    provider: CloudProvider,
) -> Result<Option<Vec<u8>>, QuarkError> {
    let Some(window) = app.get_webview_window(&format!("cloud-auth-{}", provider.key())) else {
        return Ok(None);
    };
    let cookies = browser_cookies(&window, web_session_cookie_urls(provider))?;
    let snapshot = match provider {
        CloudProvider::Quark => {
            let cookie = cookies.join("; ");
            if cookie.is_empty() {
                return Err(QuarkError::WebAuthorizationRequired);
            }
            cookie.into_bytes()
        }
        CloudProvider::Baidu => serde_json::to_vec(&baidu::parse_baidu_web_session(&cookies)?)
            .map_err(|_| QuarkError::Storage)?,
        CloudProvider::Tianyi => {
            let mut session = tianyi::parse_tianyi_web_session(&cookies)?;
            session.apply_browser_context(&tianyi_browser_context(&window)?);
            serde_json::to_vec(&session).map_err(|_| QuarkError::Storage)?
        }
        CloudProvider::Uc => serde_json::to_vec(&uc::parse_uc_web_session(&cookies)?)
            .map_err(|_| QuarkError::Storage)?,
        CloudProvider::Pikpak | CloudProvider::Aliyun | CloudProvider::Xunlei => return Ok(None),
    };
    Ok(Some(snapshot))
}

fn retry_read_from_active_authorization_window<T>(
    app: &AppHandle,
    provider: CloudProvider,
    store: &impl CredentialStore,
    retry: impl FnOnce() -> Result<T, QuarkError>,
) -> Result<Option<T>, QuarkError> {
    let Some(snapshot) = active_web_session_snapshot(app, provider)? else {
        return Ok(None);
    };
    retry_read_with_refreshed_web_session(provider, store, &snapshot, retry).map(Some)
}

/// The authorization page can visit several hosts, but a captured Cookie
/// header is replayed to one provider API host.  Do not merge same-named
/// host-only cookies from the login host into Tianyi's cloud API session.
fn tianyi_browser_context(window: &tauri::WebviewWindow) -> Result<serde_json::Value, QuarkError> {
    web_storage_tokens(window, "location.origin === 'https://cloud.189.cn' ? (window.__alldownloaderTianyiContext || {}) : {}")
}

fn web_session_cookie_urls(provider: CloudProvider) -> &'static [&'static str] {
    match provider {
        CloudProvider::Tianyi => &["https://cloud.189.cn"],
        CloudProvider::Quark => &["https://pan.quark.cn", "https://drive-pc.quark.cn"],
        CloudProvider::Baidu => &["https://pan.baidu.com"],
        CloudProvider::Uc => &["https://drive.uc.cn", "https://pc-api.uc.cn"],
        CloudProvider::Pikpak | CloudProvider::Aliyun | CloudProvider::Xunlei => &[],
    }
}

fn browser_cookies(
    window: &tauri::WebviewWindow,
    urls: &[&str],
) -> Result<Vec<String>, QuarkError> {
    let mut values = std::collections::BTreeMap::new();
    for url in urls {
        for cookie in window
            .cookies_for_url(url.parse().map_err(|_| QuarkError::InvalidResponse)?)
            .map_err(|_| QuarkError::WebAuthorizationRequired)?
        {
            values.insert(cookie.name().to_owned(), cookie.value().to_owned());
        }
    }
    Ok(values
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect())
}

fn xunlei_web_context_from_selected_tokens(value: &serde_json::Value) -> xunlei::XunleiWebContext {
    let selected = |key| value[key].as_str().unwrap_or_default().to_owned();
    xunlei::XunleiWebContext {
        session_id: selected("session_id"),
        peer_id: selected("peer_id"),
        device_id: selected("device_id"),
        credit_key: selected("credit_key"),
    }
}

fn web_value(raw: &str) -> Result<serde_json::Value, QuarkError> {
    let mut value: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| QuarkError::AuthorizationPending)?;
    if let Some(serialized) = value.as_str() {
        value = serde_json::from_str(serialized).map_err(|_| QuarkError::AuthorizationPending)?;
    }
    value
        .is_object()
        .then_some(value)
        .ok_or(QuarkError::AuthorizationPending)
}

fn web_storage_tokens(
    window: &tauri::WebviewWindow,
    script: &str,
) -> Result<serde_json::Value, QuarkError> {
    let (tx, rx) = std::sync::mpsc::channel();
    window
        .eval_with_callback(script, move |value| {
            let _ = tx.send(value);
        })
        .map_err(|_| QuarkError::WebAuthorizationRequired)?;
    let raw = rx
        .recv_timeout(std::time::Duration::from_secs(15))
        .map_err(|_| QuarkError::WebAuthorizationRequired)?;
    web_value(&raw)
}

/// WebView2's evaluation callback returns a Promise as `{}`, not its value.
/// Keep one pending capture in the page, including during a manual challenge.
fn asynchronous_web_tokens(
    window: &tauri::WebviewWindow,
    script: &str,
) -> Result<serde_json::Value, QuarkError> {
    let capture = include_str!("xunlei-web-capture.js");
    let state = web_storage_tokens(window, &format!("({capture})(() => ({script}))"))?;
    if state["done"].as_bool() != Some(true) {
        return Err(QuarkError::AuthorizationPending);
    }
    state
        .get("value")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or(QuarkError::AuthorizationPending)
}

#[tauri::command]
pub fn cloud_provider_capabilities(provider: CloudProvider) -> CloudAuthorizationCapabilities {
    provider_capabilities(provider)
}

#[tauri::command]
pub async fn cloud_saved_password_credentials(
    app: AppHandle,
    provider: CloudProvider,
) -> Result<Option<CloudCredentials>, QuarkError> {
    if !provider_capabilities(provider).password {
        return Ok(None);
    }
    run(app, provider, move |state, _| {
        let draft = load_saved_password_login(&state.store, provider)?;
        Ok(draft.map(|draft| CloudCredentials {
            username: draft.username().to_owned(),
            password: draft.password().to_owned(),
        }))
    })
    .await
}

fn tianyi_session_safe_shape(session: &tianyi::TianyiSession) -> serde_json::Value {
    let mut cookie_names = session
        .cookie
        .split(';')
        .filter_map(|pair| pair.trim().split_once('=').map(|(name, _)| name.trim()))
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 64
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    cookie_names.sort();
    cookie_names.dedup();
    serde_json::json!({
        "authMethod": session.auth_method,
        "transport": if session.app.is_some() { "nativeApp" } else { "web" },
        "cookieNames": cookie_names,
        "browserContext": session.has_browser_context(),
    })
}

#[tauri::command]
pub async fn cloud_tianyi_session_shape_for_test(
    app: AppHandle,
) -> Result<serde_json::Value, QuarkError> {
    if std::env::var("ALLDOWNLOADER_BACKGROUND_UI_TEST")
        .ok()
        .as_deref()
        != Some("1")
    {
        return Err(QuarkError::InvalidInput("仅后台诊断模式可用".into()));
    }
    run(app, CloudProvider::Tianyi, move |state, _| {
        let session: tianyi::TianyiSession =
            load_provider_session(CloudProvider::Tianyi, &state.store)?;
        Ok(tianyi_session_safe_shape(&session))
    })
    .await
}

#[tauri::command]
pub async fn cloud_tianyi_resolve_download_for_test(
    app: AppHandle,
    file_id: String,
) -> Result<bool, QuarkError> {
    if std::env::var("ALLDOWNLOADER_BACKGROUND_UI_TEST")
        .ok()
        .as_deref()
        != Some("1")
    {
        return Err(QuarkError::InvalidInput("仅后台诊断模式可用".into()));
    }
    run(app, CloudProvider::Tianyi, move |state, http| {
        let session: tianyi::TianyiSession =
            load_provider_session(CloudProvider::Tianyi, &state.store)?;
        if session.app.is_none() {
            return Err(QuarkError::InvalidInput("当前不是天翼原生 App 会话".into()));
        }
        let download = tianyi::TianyiClient::new(session, http).download(&file_id)?;
        Ok(download.url.starts_with("https://")
            && ["Date", "SessionKey", "Signature", "X-Request-ID"]
                .iter()
                .all(|name| download.headers.contains_key(*name)))
    })
    .await
}

fn require_connected_provider(provider: CloudProvider) -> Result<(), QuarkError> {
    match provider {
        CloudProvider::Quark
        | CloudProvider::Pikpak
        | CloudProvider::Baidu
        | CloudProvider::Tianyi
        | CloudProvider::Aliyun
        | CloudProvider::Xunlei
        | CloudProvider::Uc => Ok(()),
    }
}

pub fn logout_provider(
    provider: CloudProvider,
    store: &impl CredentialStore,
) -> Result<(), QuarkError> {
    require_connected_provider(provider)?;
    let session_result = store.remove(provider.key());
    let password_result = if cloud_session_policy(provider).supports_password_fallback {
        remove_password_credential(store, provider)
    } else {
        Ok(())
    };
    match (session_result, password_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

fn valid_name(name: &str) -> Result<(), QuarkError> {
    if name.trim().is_empty() || name.contains(['\r', '\n', '/', '\\']) {
        Err(QuarkError::InvalidInput("请输入有效的文件名称".into()))
    } else {
        Ok(())
    }
}

// Cloud vendors place their login controls at the upper right of their wide
// desktop pages.  Run after every completed navigation because login flows
// often redirect through one or more provider-owned pages before rendering.
const AUTHORIZATION_TOP_RIGHT_VIEWPORT_JS: &str = r#"(() => {
  const scrollToTopRight = () => {
    const root = document.scrollingElement || document.documentElement || document.body;
    const maxLeft = Math.max(0, root.scrollWidth - window.innerWidth);
    window.scrollTo({ left: maxLeft, top: 0, behavior: 'auto' });
  };
  scrollToTopRight();
  requestAnimationFrame(scrollToTopRight);
  for (const delay of [120, 480, 1000]) setTimeout(scrollToTopRight, delay);
})()"#;

// Baidu's landing page currently puts the real sign-in dialog behind its
// "去登录" action.  This is deliberately best-effort: a provider redesign or
// a slow page must leave the official page usable rather than fail the native
// authorization command.
const BAIDU_AUTO_OPEN_LOGIN_JS: &str = r#"(() => {
  if (window.__allDownloaderBaiduLoginTriggerInstalled) return;
  window.__allDownloaderBaiduLoginTriggerInstalled = true;

  const expectedLabel = "去登录";
  const normalize = (value) => String(value || "").replace(/\s+/g, "").trim();
  const visible = (element) => {
    const rect = element.getBoundingClientRect();
    const style = window.getComputedStyle(element);
    return rect.width > 0 && rect.height > 0 && style.display !== "none" && style.visibility !== "hidden";
  };
  const findLoginAction = () => Array.from(document.querySelectorAll(
    'button, a, [role="button"], input[type="button"], input[type="submit"]'
  )).find((element) => {
    const label = element.innerText || element.value || element.getAttribute("aria-label");
    return normalize(label) === expectedLabel && visible(element) && !element.disabled;
  });

  let attempts = 0;
  let timer = 0;
  const stop = () => {
    if (timer) window.clearInterval(timer);
    timer = 0;
  };
  const tryOpen = () => {
    try {
      const element = findLoginAction();
      if (!element) return false;
      element.click();
      stop();
      return true;
    } catch (_) {
      return false;
    }
  };

  timer = window.setInterval(() => {
    attempts += 1;
    if (tryOpen() || attempts >= 20) stop();
  }, 250);
  window.setTimeout(stop, 6000);
  tryOpen();
})()"#;

fn allows_cloud_authorization_popup(url: &url::Url) -> bool {
    url.scheme() == "https" || url.as_str() == "about:blank"
}

fn is_baidu_authorization_page(url: &url::Url) -> bool {
    matches!(url.host_str(), Some("pan.baidu.com"))
}

#[tauri::command]
pub async fn cloud_begin_authorization(
    app: AppHandle,
    provider: CloudProvider,
) -> Result<CloudAuthorization, QuarkError> {
    let label = format!("cloud-auth-{}", provider.key());
    if let Some(window) = app.get_webview_window(&label) {
        window
            .show()
            .map_err(|_| QuarkError::WebAuthorizationRequired)?;
        let _ = window.set_focus();
    } else {
        let url = match provider {
            CloudProvider::Quark => "https://pan.quark.cn",
            CloudProvider::Pikpak => "https://mypikpak.com/drive",
            CloudProvider::Baidu => "https://pan.baidu.com",
            CloudProvider::Tianyi => "https://cloud.189.cn",
            CloudProvider::Aliyun => "https://www.aliyundrive.com/drive",
            CloudProvider::Xunlei => "https://pan.xunlei.com",
            CloudProvider::Uc => "https://drive.uc.cn",
        };
        let mut builder = WebviewWindowBuilder::new(
            &app,
            &label,
            WebviewUrl::External(url.parse().map_err(|_| QuarkError::InvalidResponse)?),
        )
        .title(format!("{} 网页授权", provider.key()))
        .inner_size(1024.0, 760.0)
        .incognito(!browser_login::supports_browser_password(provider))
        .on_new_window(|url, _features| {
            if allows_cloud_authorization_popup(&url) {
                NewWindowResponse::Allow
            } else {
                NewWindowResponse::Deny
            }
        })
        .on_page_load(move |window, payload| {
            if payload.event() == PageLoadEvent::Finished {
                let _ = window.eval(AUTHORIZATION_TOP_RIGHT_VIEWPORT_JS);
                if provider == CloudProvider::Baidu && is_baidu_authorization_page(payload.url()) {
                    let _ = window.eval(BAIDU_AUTO_OPEN_LOGIN_JS);
                }
            }
        });
        if browser_login::supports_browser_password(provider) {
            builder = builder.data_directory(password_webview_data_directory(&app, provider)?);
        }
        if provider == CloudProvider::Tianyi {
            builder = builder.initialization_script(include_str!("tianyi-browser-context.js"));
        }
        builder
            .build()
            .map_err(|_| QuarkError::WebAuthorizationRequired)?;
    }
    Ok(CloudAuthorization {
        provider,
        window_label: label,
        instructions: "请在弹出的官方网盘窗口完成登录；验证成功后授权窗口会自动收起。".into(),
        completed: false,
    })
}

fn cloud_provider_from_download_source(source: &str) -> Result<CloudProvider, QuarkError> {
    if source.starts_with("cloud-share://") {
        return share::parse_stable_cloud_share_source(source).map(|(provider, _, _)| provider);
    }
    parse_stable_cloud_source(source).map(|(provider, _)| provider)
}

/// Registers every failed task of this provider that is blocked on a fresh
/// official authorization, then opens exactly one official WebView. The
/// pending list contains only durable IDs; tokens and signed URLs never enter
/// it, so a manual login can safely refresh the whole provider batch.
#[tauri::command]
pub async fn cloud_reauthorize_failed_task(
    app: AppHandle,
    task_id: String,
) -> Result<CloudAuthorization, QuarkError> {
    let id = uuid::Uuid::parse_str(&task_id)
        .map_err(|_| QuarkError::InvalidInput("任务 ID 无效".into()))?;
    let state = app.try_state::<AppState>().ok_or(QuarkError::Storage)?;

    let requested_provider = match state.store.find(id) {
        Ok(task) => {
            if task.status != TaskStatus::Failed
                || !requires_cloud_reauthorization_failure(task.failure_code.as_deref())
            {
                return Err(QuarkError::InvalidInput("当前任务不需要重新授权".into()));
            }
            cloud_provider_from_download_source(&task.source)?
        }
        Err(crate::StoreError::NotFound(_)) => {
            let mut provider = None;
            for group in state
                .store
                .cloud_directory_groups()
                .map_err(|_| QuarkError::Storage)?
            {
                let matches_requested = state
                    .store
                    .cloud_directory_group_items(group.id)
                    .map_err(|_| QuarkError::Storage)?
                    .into_iter()
                    .any(|item| {
                        item.id == id
                            && item.task_id.is_none()
                            && requires_cloud_reauthorization_failure(item.failure_code.as_deref())
                    });
                if matches_requested {
                    provider = Some(group.provider);
                    break;
                }
            }
            provider.ok_or_else(|| QuarkError::InvalidInput("当前任务不需要重新授权".into()))?
        }
        Err(_) => return Err(QuarkError::Storage),
    };

    let task_ids = state
        .store
        .list()
        .map_err(|_| QuarkError::Storage)?
        .into_iter()
        .filter(|task| {
            task.status == TaskStatus::Failed
                && requires_cloud_reauthorization_failure(task.failure_code.as_deref())
                && cloud_provider_from_download_source(&task.source)
                    .is_ok_and(|provider| provider == requested_provider)
        })
        .map(|task| task.id)
        .collect::<Vec<_>>();
    let mut directory_items = Vec::new();
    for group in state
        .store
        .cloud_directory_groups()
        .map_err(|_| QuarkError::Storage)?
        .into_iter()
        .filter(|group| group.provider == requested_provider)
    {
        let items = state
            .store
            .cloud_directory_group_items(group.id)
            .map_err(|_| QuarkError::Storage)?;
        directory_items.extend(items.into_iter().filter_map(|item| {
            (item.task_id.is_none()
                && requires_cloud_reauthorization_failure(item.failure_code.as_deref()))
            .then_some((group.id, item.id))
        }));
    }
    let cloud = app
        .try_state::<Arc<CloudState>>()
        .ok_or(QuarkError::Storage)?;
    cloud.queue_authorization_retries(requested_provider, task_ids, directory_items)?;
    if browser_login::supports_browser_password(requested_provider) {
        let browser_app = app.clone();
        let recovered = run(app.clone(), requested_provider, move |state, http| {
            let Some(credential) = load_password_credential(&state.store, requested_provider)?
            else {
                return Ok(false);
            };
            match recover_provider_password_session(
                requested_provider,
                &state.store,
                http,
                &credential,
            ) {
                Ok(()) => Ok(true),
                Err(error)
                    if browser_login::can_fallback_to_browser(&error)
                        || matches!(error, QuarkError::CaptchaRequired) =>
                {
                    browser_password_login(
                        &browser_app,
                        requested_provider,
                        &state.store,
                        http,
                        credential.username(),
                        credential.password(),
                        Some(credential.account_id()),
                        matches!(error, QuarkError::CaptchaRequired),
                    )?;
                    Ok(true)
                }
                Err(error) => Err(error),
            }
        })
        .await?;
        if recovered {
            resume_pending_authorization_retries(app, requested_provider).await?;
            return Ok(CloudAuthorization {
                provider: requested_provider,
                window_label: String::new(),
                instructions: "登录已自动恢复，正在继续下载".into(),
                completed: true,
            });
        }
    }
    cloud_begin_authorization(app, requested_provider).await
}

fn queue_provider_authorization_failures(
    app: &AppHandle,
    provider: CloudProvider,
) -> Result<(), QuarkError> {
    let state = app.try_state::<AppState>().ok_or(QuarkError::Storage)?;
    let cloud = app
        .try_state::<Arc<CloudState>>()
        .ok_or(QuarkError::Storage)?;
    let task_ids = state
        .store
        .list()
        .map_err(|_| QuarkError::Storage)?
        .into_iter()
        .filter(|task| {
            task.status == TaskStatus::Failed
                && requires_cloud_reauthorization_failure(task.failure_code.as_deref())
                && cloud_provider_from_download_source(&task.source).ok() == Some(provider)
        })
        .map(|task| task.id)
        .collect::<Vec<_>>();
    let mut directory_items = Vec::new();
    for group in state
        .store
        .cloud_directory_groups()
        .map_err(|_| QuarkError::Storage)?
    {
        if group.provider != provider {
            continue;
        }
        directory_items.extend(
            state
                .store
                .cloud_directory_group_items(group.id)
                .map_err(|_| QuarkError::Storage)?
                .into_iter()
                .filter(|item| {
                    item.task_id.is_none()
                        && requires_cloud_reauthorization_failure(item.failure_code.as_deref())
                })
                .map(|item| (group.id, item.id)),
        );
    }
    cloud.queue_authorization_retries(provider, task_ids, directory_items)
}

async fn resume_pending_authorization_retries(
    app: AppHandle,
    provider: CloudProvider,
) -> Result<(), QuarkError> {
    use tauri::Emitter;

    let cloud = app
        .try_state::<Arc<CloudState>>()
        .ok_or(QuarkError::Storage)?;
    let pending = cloud.take_authorization_retries(provider)?;
    let state = app.try_state::<AppState>().ok_or(QuarkError::Storage)?;
    for id in pending.task_ids {
        let Ok(task) = state.store.find(id) else {
            continue;
        };
        if task.status != TaskStatus::Failed
            || !requires_cloud_reauthorization_failure(task.failure_code.as_deref())
            || cloud_provider_from_download_source(&task.source).ok() != Some(provider)
            || !matches!(crate::retry_duplicate_state(&state.store, &task), Ok(None))
        {
            continue;
        }
        match crate::commands::resubmit_task(app.clone(), &state, task, Default::default()).await {
            Ok(task) => {
                if let Ok(outcome) = task_action_outcome(&state.store, task, None) {
                    if let Some(queue_task) = outcome.queue_task {
                        let _ = app.emit("download-task-updated", queue_task);
                    } else {
                        let _ = app.emit("download-task-updated", outcome.payload);
                    }
                }
            }
            Err(_) => {
                if let Ok(task) = state.store.find(id) {
                    if let Ok(payload) = crate::commands::task_update_payload(
                        &state.store,
                        Some(&cloud.share_plans),
                        task,
                    ) {
                        let _ = app.emit("download-task-updated", payload);
                    }
                }
            }
        }
    }
    for (group_id, item_id) in pending.directory_items {
        if let Ok(outcome) = retry_cloud_directory_group_item(app.clone(), group_id, item_id).await
        {
            if let Some(queue_task) = outcome.queue_task {
                let _ = app.emit("download-task-updated", queue_task);
            } else {
                let _ = app.emit("download-task-updated", outcome.payload);
            }
        }
    }
    Ok(())
}

// Only selected token fields cross the native boundary. No password, full storage dump, or DOM is returned.
const WEB_TOKENS_JS: &str = r#"(() => {
 if (location.protocol !== 'https:' || !(location.hostname === 'mypikpak.com' || location.hostname.endsWith('.mypikpak.com'))) return null;
 const out = {}; const seen = new Set();
 function inspect(v, depth) {
  if (!v || depth > 6) return;
  if (typeof v === 'string') { try { inspect(JSON.parse(v),depth+1); } catch (_) {} return; }
  if (typeof v !== 'object' || seen.has(v)) return; seen.add(v);
  for (const [k,x] of Object.entries(v)) {
   if (['access_token','accessToken'].includes(k) && typeof x === 'string') out.access_token=x;
   else if (['refresh_token','refreshToken'].includes(k) && typeof x === 'string') out.refresh_token=x;
   else if (['device_id','deviceId','deviceid'].includes(k) && typeof x === 'string') out.device_id=x;
   else if (['captcha_token','captchaToken'].includes(k) && typeof x === 'string') out.captcha_token=x;
   else if (!/password/i.test(k)) inspect(x,depth+1);
  }
 }
 for (let i=0;i<localStorage.length;i++) {const k=localStorage.key(i); inspect({[k]:localStorage.getItem(k)},0);}
 for (let i=0;i<localStorage.length;i++) {const k=localStorage.key(i); if(k.startsWith('credentials_')) {try {const v=JSON.parse(localStorage.getItem(k)); if(v.access_token && v.refresh_token) {inspect(v,0);out.client_id=k.slice(12);break;}} catch (_) {}}}
 return out.access_token && out.refresh_token ? out : null;
})()"#;

// The browser page is the only place where these renewable credentials are
// acquired.  The scripts return a narrowly selected object; they never return
// a storage dump, DOM content or passwords to Rust.
const ALIYUN_WEB_TOKENS_JS: &str = r#"(() => {
 if (location.protocol !== 'https:' || !(['aliyundrive.com','alipan.com'].some(h => location.hostname === h || location.hostname.endsWith('.' + h)))) return null;
 const out = {}; const seen = new Set();
 const fields = {
  access_token: ['access_token', 'accessToken'], refresh_token: ['refresh_token', 'refreshToken'],
  default_drive_id: ['default_drive_id', 'defaultDriveId', 'drive_id', 'driveId']
 };
 function inspect(v, depth) {
  if (!v || depth > 6) return;
  if (typeof v === 'string') { try { inspect(JSON.parse(v), depth + 1); } catch (_) {} return; }
  if (typeof v !== 'object' || seen.has(v)) return; seen.add(v);
  for (const [k, x] of Object.entries(v)) {
   const target = Object.entries(fields).find(([, names]) => names.includes(k));
   if (target && typeof x === 'string' && x) out[target[0]] = x;
   else if (!/password/i.test(k)) inspect(x, depth + 1);
  }
 }
 for (const storage of [localStorage, sessionStorage]) for (let i = 0; i < storage.length; i++) inspect({[storage.key(i)]: storage.getItem(storage.key(i))}, 0);
 return out.access_token && out.refresh_token && out.default_drive_id ? out : null;
})()"#;

const XUNLEI_WEB_TOKENS_JS: &str = include_str!("xunlei-web-session.js");

fn parse_aliyun_web_session(value: serde_json::Value) -> Result<aliyun::AliyunSession, QuarkError> {
    let required_field = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| value.get(*name).and_then(serde_json::Value::as_str))
            .filter(|field| !field.is_empty())
            .map(str::to_owned)
            .ok_or(QuarkError::AuthorizationPending)
    };
    Ok(aliyun::AliyunSession {
        access_token: required_field(&["access_token", "accessToken"])?,
        refresh_token: required_field(&["refresh_token", "refreshToken"])?,
        drive_id: required_field(&["default_drive_id", "defaultDriveId", "drive_id", "driveId"])?,
        browser_access_ready: false,
    })
}

fn find_created_directory(directory: CloudDirectory, name: &str) -> Result<CloudFile, QuarkError> {
    directory
        .items
        .into_iter()
        .find(|file| file.kind == "folder" && file.name == name)
        .ok_or(QuarkError::InvalidResponse)
}

const MAX_DIRECTORY_DOWNLOAD_DIRECTORIES: usize = 1_000;
const MAX_DIRECTORY_DOWNLOAD_FILES: usize = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedCloudFile {
    pub file_id: String,
    pub relative_directory: Vec<String>,
    pub output_name: String,
    pub size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudDirectoryDownloadPlan {
    pub files: Vec<PlannedCloudFile>,
}

/// Builds a bounded, path-safe file plan before any signed URL is resolved or
/// task is submitted. A failed plan therefore cannot leave a partial queue.
pub fn plan_cloud_directory_download(
    provider: CloudProvider,
    root_id: &str,
    root_name: &str,
    mut list_directory: impl FnMut(&str) -> Result<CloudDirectory, QuarkError>,
) -> Result<CloudDirectoryDownloadPlan, QuarkError> {
    if root_id.trim().is_empty() {
        return Err(QuarkError::InvalidInput("目录 ID 无效".into()));
    }
    if provider == CloudProvider::Pikpak && root_id == PIKPAK_RECYCLE_BIN_ID {
        return Err(QuarkError::InvalidInput("PikPak 回收站不能下载目录".into()));
    }

    let mut files = Vec::new();
    let mut visited_directories = HashSet::new();
    let mut visited_files = HashSet::new();
    let mut pending = vec![(
        root_id.to_owned(),
        vec![safe_directory_component(root_name)],
    )];

    while let Some((directory_id, relative_directory)) = pending.pop() {
        if visited_directories.len() >= MAX_DIRECTORY_DOWNLOAD_DIRECTORIES {
            return Err(QuarkError::InvalidInput("目录下载包含过多子目录".into()));
        }
        if !visited_directories.insert(directory_id.clone()) {
            return Err(QuarkError::InvalidInput(
                "目录下载检测到循环或重复目录".into(),
            ));
        }

        let directory = list_directory(&directory_id)?;
        if directory.id != directory_id {
            return Err(QuarkError::InvalidResponse);
        }
        for item in directory.items.into_iter().rev() {
            match item.kind.as_str() {
                "file" => {
                    if files.len() >= MAX_DIRECTORY_DOWNLOAD_FILES {
                        return Err(QuarkError::InvalidInput("目录下载包含过多文件".into()));
                    }
                    if item.id.trim().is_empty() || !visited_files.insert(item.id.clone()) {
                        return Err(QuarkError::InvalidInput("目录下载包含重复文件".into()));
                    }
                    files.push(PlannedCloudFile {
                        file_id: item.id,
                        relative_directory: relative_directory.clone(),
                        output_name: safe_filename(&item.name),
                        size: item.size,
                    });
                }
                "folder" => {
                    if item.id.trim().is_empty() {
                        return Err(QuarkError::InvalidResponse);
                    }
                    if provider == CloudProvider::Pikpak && item.id == PIKPAK_RECYCLE_BIN_ID {
                        return Err(QuarkError::InvalidInput("PikPak 回收站不能下载目录".into()));
                    }
                    let mut child_path = relative_directory.clone();
                    child_path.push(safe_directory_component(&item.name));
                    pending.push((item.id, child_path));
                }
                _ => return Err(QuarkError::InvalidResponse),
            }
        }
    }

    if files.is_empty() {
        return Err(QuarkError::InvalidInput("空目录不能下载".into()));
    }
    if has_windows_output_path_collision(&files) {
        return Err(QuarkError::InvalidInput("目录下载的输出路径冲突".into()));
    }
    Ok(CloudDirectoryDownloadPlan { files })
}

/// Converts an already-inspected share selection into the same durable plan
/// used by personal-cloud directories. Stable `cloud-share://` sources become
/// group item identities; provider credentials and signed URLs remain only in
/// the encrypted share-plan store.
pub fn plan_cloud_share_directory_download(
    selection: &share::CloudShareDownloadSelection,
) -> Result<CloudDirectoryDownloadPlan, QuarkError> {
    let root_name = selection
        .directory_name
        .as_deref()
        .ok_or_else(|| QuarkError::InvalidInput("该分享选择不是目录下载".into()))?;
    if selection.entries.is_empty() {
        return Err(QuarkError::InvalidInput("空目录不能下载".into()));
    }
    if selection.entries.len() > MAX_DIRECTORY_DOWNLOAD_FILES {
        return Err(QuarkError::InvalidInput("目录下载包含过多文件".into()));
    }
    let root = safe_directory_component(root_name);
    let strip_promoted_root = selection.entries.iter().all(|entry| {
        entry
            .relative_directory
            .first()
            .is_some_and(|component| component == root_name)
    });
    let mut sources = HashSet::new();
    let files = selection
        .entries
        .iter()
        .map(|entry| {
            share::parse_stable_cloud_share_source(&entry.source)?;
            if !sources.insert(entry.source.clone()) {
                return Err(QuarkError::InvalidInput("目录下载包含重复文件".into()));
            }
            let mut relative_directory = Vec::with_capacity(entry.relative_directory.len() + 1);
            relative_directory.push(root.clone());
            relative_directory.extend(
                entry
                    .relative_directory
                    .iter()
                    .skip(usize::from(strip_promoted_root))
                    .map(|component| safe_directory_component(component)),
            );
            Ok(PlannedCloudFile {
                file_id: entry.source.clone(),
                relative_directory,
                output_name: safe_filename(&entry.name),
                size: entry.size,
            })
        })
        .collect::<Result<Vec<_>, QuarkError>>()?;
    if has_windows_output_path_collision(&files) {
        return Err(QuarkError::InvalidInput("目录下载的输出路径冲突".into()));
    }
    Ok(CloudDirectoryDownloadPlan { files })
}

fn has_windows_output_path_collision(files: &[PlannedCloudFile]) -> bool {
    let mut file_targets = HashSet::new();
    let mut directories = HashSet::new();
    files.iter().any(|file| {
        let target = file
            .relative_directory
            .iter()
            .chain(std::iter::once(&file.output_name))
            .map(|component| component.to_lowercase())
            .collect::<Vec<_>>()
            .join("\\");
        if file_targets.contains(&target) || directories.contains(&target) {
            return true;
        }
        for (index, _) in target.match_indices('\\') {
            let ancestor = &target[..index];
            if file_targets.contains(ancestor) {
                return true;
            }
            directories.insert(ancestor.to_owned());
        }
        !file_targets.insert(target)
    })
}

fn safe_directory_component(name: &str) -> String {
    let value = safe_filename(name);
    if matches!(value.as_str(), "." | "..") {
        "folder".into()
    } else {
        value
    }
}

fn list_cloud_directory(
    provider: CloudProvider,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
    parent_id: &str,
) -> Result<CloudDirectory, QuarkError> {
    if is_provider_recycle_bin(provider, parent_id) {
        return list_provider_recycle_bin(provider, store, http);
    }
    match provider {
        CloudProvider::Quark => QuarkClient::new(cookie(store)?, http).directory(parent_id),
        CloudProvider::Pikpak => PikPakClient::new(http, store).list(parent_id),
        CloudProvider::Baidu => {
            baidu::BaiduClient::new(load_provider_session(CloudProvider::Baidu, store)?, http)
                .directory(parent_id)
        }
        CloudProvider::Tianyi => {
            tianyi::TianyiClient::new(load_provider_session(CloudProvider::Tianyi, store)?, http)
                .directory(parent_id)
        }
        CloudProvider::Aliyun => aliyun::AliyunClient::new(store, http).directory(parent_id),
        CloudProvider::Xunlei => {
            with_xunlei_client(store, http, |client| client.directory(parent_id))
        }
        CloudProvider::Uc => {
            uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, http)
                .directory(parent_id)
        }
    }
}

fn is_provider_recycle_bin(provider: CloudProvider, directory_id: &str) -> bool {
    match provider {
        CloudProvider::Quark => directory_id == QUARK_RECYCLE_BIN_ID,
        CloudProvider::Pikpak => directory_id == PIKPAK_RECYCLE_BIN_ID,
        CloudProvider::Baidu => directory_id == baidu::BAIDU_RECYCLE_BIN_ID,
        CloudProvider::Tianyi => directory_id == tianyi::TIANYI_RECYCLE_BIN_ID,
        CloudProvider::Aliyun => directory_id == aliyun::ALIYUN_RECYCLE_BIN_ID,
        CloudProvider::Xunlei => directory_id == xunlei::XUNLEI_RECYCLE_BIN_ID,
        CloudProvider::Uc => directory_id == uc::UC_RECYCLE_BIN_ID,
    }
}

fn list_provider_recycle_bin(
    provider: CloudProvider,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
) -> Result<CloudDirectory, QuarkError> {
    match provider {
        CloudProvider::Quark => QuarkClient::new(cookie(store)?, http).list_recycle_bin(),
        CloudProvider::Pikpak => PikPakClient::new(http, store).list(PIKPAK_RECYCLE_BIN_ID),
        CloudProvider::Baidu => {
            baidu::BaiduClient::new(load_provider_session(CloudProvider::Baidu, store)?, http)
                .list_recycle_bin()
        }
        CloudProvider::Tianyi => {
            tianyi::TianyiClient::new(load_provider_session(CloudProvider::Tianyi, store)?, http)
                .list_recycle_bin()
        }
        CloudProvider::Aliyun => aliyun::AliyunClient::new(store, http).list_recycle_bin(),
        CloudProvider::Xunlei => {
            with_xunlei_client(store, http, |client| client.list_recycle_bin())
        }
        CloudProvider::Uc => {
            uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, http)
                .list_recycle_bin()
        }
    }
}

fn validate_recycle_entry_ids(
    file_ids: &[String],
    recycle_bin: &CloudDirectory,
) -> Result<(), QuarkError> {
    if file_ids.is_empty() {
        return Err(QuarkError::InvalidInput("请选择至少一个回收站项目".into()));
    }
    let entries = recycle_bin
        .items
        .iter()
        .filter(|item| item.parent_id == recycle_bin.id)
        .map(|item| item.id.as_str())
        .collect::<HashSet<_>>();
    if file_ids
        .iter()
        .any(|id| id.trim().is_empty() || !entries.contains(id.as_str()))
    {
        return Err(QuarkError::InvalidInput(
            "只能操作当前网盘回收站中的项目".into(),
        ));
    }
    Ok(())
}

fn restore_provider_recycle_entries(
    provider: CloudProvider,
    store: &DpapiCredentialStore,
    http: &impl CloudHttpTransport,
    file_ids: &[String],
) -> Result<(), QuarkError> {
    match provider {
        CloudProvider::Quark => {
            let client = QuarkClient::new(cookie(store)?, http);
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.restore_recycle_entries(file_ids)
        }
        CloudProvider::Pikpak => {
            let client = PikPakClient::new(http, store);
            let recycle_bin = client.list(PIKPAK_RECYCLE_BIN_ID)?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.restore_recycle_entries(file_ids)
        }
        CloudProvider::Baidu => {
            let client =
                baidu::BaiduClient::new(load_provider_session(CloudProvider::Baidu, store)?, http);
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.restore_recycle_entries(file_ids)
        }
        CloudProvider::Tianyi => {
            let client = tianyi::TianyiClient::new(
                load_provider_session(CloudProvider::Tianyi, store)?,
                http,
            );
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.restore_recycle_entries(file_ids)
        }
        CloudProvider::Aliyun => {
            let client = aliyun::AliyunClient::new(store, http);
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.restore_recycle_entries(file_ids)
        }
        CloudProvider::Xunlei => with_xunlei_client(store, http, |client| {
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.restore_recycle_entries(file_ids)
        }),
        CloudProvider::Uc => {
            let client = uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, http);
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.restore_recycle_entries(file_ids)
        }
    }
}

fn delete_provider_recycle_entries(
    provider: CloudProvider,
    store: &DpapiCredentialStore,
    http: &impl CloudHttpTransport,
    file_ids: &[String],
) -> Result<(), QuarkError> {
    match provider {
        CloudProvider::Quark => {
            let client = QuarkClient::new(cookie(store)?, http);
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.delete_recycle_entries(file_ids)
        }
        CloudProvider::Pikpak => {
            let client = PikPakClient::new(http, store);
            let recycle_bin = client.list(PIKPAK_RECYCLE_BIN_ID)?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.delete(file_ids)
        }
        CloudProvider::Baidu => {
            let client =
                baidu::BaiduClient::new(load_provider_session(CloudProvider::Baidu, store)?, http);
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.delete_recycle_entries(file_ids)
        }
        CloudProvider::Tianyi => {
            let client = tianyi::TianyiClient::new(
                load_provider_session(CloudProvider::Tianyi, store)?,
                http,
            );
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.delete_recycle_entries(file_ids)
        }
        CloudProvider::Aliyun => {
            let client = aliyun::AliyunClient::new(store, http);
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.delete_recycle_entries(file_ids)
        }
        CloudProvider::Xunlei => with_xunlei_client(store, http, |client| {
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.delete_recycle_entries(file_ids)
        }),
        CloudProvider::Uc => {
            let client = uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, http);
            let recycle_bin = client.list_recycle_bin()?;
            validate_recycle_entry_ids(file_ids, &recycle_bin)?;
            client.delete_recycle_entries(file_ids)
        }
    }
}

fn with_xunlei_client<T, H: CloudHttpTransport>(
    store: &impl CredentialStore,
    http: &H,
    work: impl FnOnce(&xunlei::XunleiClient<'_, H>) -> Result<T, QuarkError>,
) -> Result<T, QuarkError> {
    let session = load_provider_session(CloudProvider::Xunlei, store)?;
    let client = xunlei::XunleiClient::new(session, http);
    let result = work(&client);
    save_provider_session(CloudProvider::Xunlei, store, &client.session())?;
    result
}

fn parse_xunlei_web_session(value: serde_json::Value) -> Result<xunlei::XunleiSession, QuarkError> {
    if value["authorization_error"].as_str() == Some("captcha_verification_failed") {
        return Err(QuarkError::Provider(
            "迅雷操作验证未完成，请在授权页完成验证后重试".into(),
        ));
    }
    let field =
        |key| super::types::required(&value, key).map_err(|_| QuarkError::AuthorizationPending);
    let optional = |key| value[key].as_str().unwrap_or_default().to_owned();
    let mut session = xunlei::XunleiSession::from_web_authorization(
        value["access_token"].as_str().unwrap_or_default(),
        optional("refresh_token"),
        field("user_id")?,
        optional("security_token"),
        xunlei::XunleiClientProfile {
            client_id: optional("client_id"),
            // The official browser session does not need, and must never
            // export, a native client secret through the WebView bridge.
            client_secret: String::new(),
            device_id: optional("device_id"),
            client_version: optional("client_version"),
            package_name: optional("package_name"),
            user_agent: optional("user_agent"),
        },
        value["expires_at"].as_u64().unwrap_or(0),
    )?;
    if value["web_security"].is_object() {
        session.web_security = serde_json::from_value(value["web_security"].clone())
            .map_err(|_| QuarkError::AuthorizationPending)?;
    }
    Ok(session)
}

fn parse_xunlei_web_authorization(
    value: serde_json::Value,
) -> Result<(xunlei::XunleiSession, xunlei::XunleiWebContext), QuarkError> {
    let context = xunlei_web_context_from_selected_tokens(&value);
    Ok((parse_xunlei_web_session(value)?, context))
}
pub fn parse_web_tokens(raw: &str) -> Result<PikPakSession, QuarkError> {
    let mut v: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| QuarkError::AuthorizationPending)?;
    if let Some(s) = v.as_str() {
        v = serde_json::from_str(s).map_err(|_| QuarkError::AuthorizationPending)?;
    }
    Ok(PikPakSession {
        access_token: required(&v, "access_token").map_err(|_| QuarkError::AuthorizationPending)?,
        refresh_token: required(&v, "refresh_token")
            .map_err(|_| QuarkError::AuthorizationPending)?,
        expires_at: u64::MAX,
        device_id: v["device_id"].as_str().unwrap_or("").into(),
        captcha_token: v["captcha_token"].as_str().unwrap_or("").into(),
        client_id: v["client_id"]
            .as_str()
            .filter(|v| !v.is_empty())
            .map(str::to_owned),
        user_id: v["user_id"].as_str().unwrap_or("").into(),
        captcha_expires_at: 0,
        auth_method: CloudAuthMethod::Web,
    })
}

pub fn authorization_result<T>(result: Result<T, QuarkError>) -> Result<T, QuarkError> {
    result.map_err(|e| match e {
        QuarkError::AuthorizationExpired
        | QuarkError::WebAuthorizationRequired
        | QuarkError::CaptchaRequired => QuarkError::AuthorizationPending,
        other => other,
    })
}

fn finalize_baidu_web_authorization(
    session: baidu::BaiduSession,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
) -> Result<CloudAccount, QuarkError> {
    let client = baidu::BaiduClient::new(session.clone(), http);
    let account = authorization_result(client.account())?;
    // The UI loads the root directory immediately after it accepts this
    // account. Do not close the official browser window or replace a saved
    // session until that exact request accepts the candidate cookie jar.
    // Authentication and verification failures map to AuthorizationPending,
    // so frontend polling keeps the window open for the user to finish the
    // official flow instead of creating an immediately-expired tab.
    authorization_result(client.directory("/"))?;
    save_provider_session(CloudProvider::Baidu, store, &session)?;
    Ok(account)
}

fn finalize_tianyi_web_authorization(
    session: tianyi::TianyiSession,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
) -> Result<CloudAccount, QuarkError> {
    let client = tianyi::TianyiClient::new(session.clone(), http);
    // The web window owns login, but a candidate session must prove it can
    // read the drive before replacing a previously usable stored session.
    // The caller only closes the authorization window after this succeeds.
    client.directory("-11").map_err(|error| match error {
        QuarkError::AuthorizationExpired
        | QuarkError::WebAuthorizationRequired
        | QuarkError::CaptchaRequired => QuarkError::AuthorizationPending,
        other => other,
    })?;
    let account = client.web_account()?;
    save_provider_session(CloudProvider::Tianyi, store, &session)?;
    Ok(account)
}

fn finalize_xunlei_web_authorization(
    mut session: xunlei::XunleiSession,
    context: xunlei::XunleiWebContext,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
) -> Result<CloudAccount, QuarkError> {
    session.web_context = context;
    let modern = session.profile.is_modern_web();
    // Modern web SSO proves access with the drive API, not legacy cookie keys.
    let client = xunlei::XunleiClient::new(session, http);
    // Do not make the UI create a tab or close the official browser window
    // until the same root request that initializes the workspace succeeds.
    // A captured bearer plus browser context alone can still be rejected by
    // Xunlei's live drive endpoint.
    let final_error = |error| match error {
        QuarkError::AuthorizationExpired if modern => {
            QuarkError::Provider("迅雷登录凭据已失效，请在授权页重新登录后重试".into())
        }
        QuarkError::WebAuthorizationRequired | QuarkError::CaptchaRequired if modern => {
            QuarkError::Provider("迅雷目录访问仍需操作验证，请在授权页完成验证后重试".into())
        }
        QuarkError::AuthorizationExpired
        | QuarkError::WebAuthorizationRequired
        | QuarkError::CaptchaRequired => QuarkError::AuthorizationPending,
        other => other,
    };
    client.directory("0").map_err(final_error)?;
    let account = client.web_account().map_err(final_error)?;
    save_provider_session(CloudProvider::Xunlei, store, &client.session())?;
    Ok(account)
}

fn finalize_uc_web_authorization(
    session: uc::UcSession,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
) -> Result<CloudAccount, QuarkError> {
    let client = uc::UcClient::new(session.clone(), http);
    // A cookie pair only says that the WebView completed part of its flow.
    // Verify the exact first read the workspace needs before replacing the
    // stored account and closing the official authorization window.
    authorization_result(client.directory("0"))?;
    let account = client.web_account()?;
    save_provider_session(CloudProvider::Uc, store, &session)?;
    Ok(account)
}

#[tauri::command]
pub async fn cloud_complete_authorization(
    app: AppHandle,
    provider: CloudProvider,
) -> Result<CloudAccount, QuarkError> {
    let window = app
        .get_webview_window(&format!("cloud-auth-{}", provider.key()))
        .ok_or(QuarkError::WebAuthorizationRequired)?;
    let account = run(app.clone(), provider, move |state, http| {
        let account = match provider {
            CloudProvider::Quark => {
                // Must stay on the blocking worker, never the Windows UI thread (WebView2 deadlock).
                let cookie =
                    browser_cookies(&window, web_session_cookie_urls(CloudProvider::Quark))?
                        .join("; ");
                if cookie.is_empty() {
                    return Err(QuarkError::AuthorizationPending);
                }
                let account =
                    authorization_result(QuarkClient::new(cookie.clone(), http).account())?;
                state.store.save("quark", cookie.as_bytes())?;
                let _ = window.close();
                Ok(account)
            }
            CloudProvider::Pikpak => {
                let session =
                    parse_web_tokens(&web_storage_tokens(&window, WEB_TOKENS_JS)?.to_string())?;
                // Validate in a temporary store before replacing a previously authorized account.
                let temporary = MemoryCredentialStore::default();
                let client = PikPakClient::new(http, &temporary);
                client.save_session(&session)?;
                let account = authorization_result(client.account())?;
                state.store.save(
                    "pikpak",
                    &temporary.load("pikpak")?.ok_or(QuarkError::Storage)?,
                )?;
                let _ = window.close();
                Ok(account)
            }
            CloudProvider::Baidu => {
                let session = baidu::parse_baidu_web_session(&browser_cookies(
                    &window,
                    web_session_cookie_urls(CloudProvider::Baidu),
                )?)?;
                let account = finalize_baidu_web_authorization(session, &state.store, http)?;
                let _ = window.close();
                Ok(account)
            }
            CloudProvider::Tianyi => {
                let mut session = tianyi::parse_tianyi_web_session(&browser_cookies(
                    &window,
                    web_session_cookie_urls(CloudProvider::Tianyi),
                )?)?;
                session.apply_browser_context(&tianyi_browser_context(&window)?);
                let validation_http = TianyiBrowserValidationTransport {
                    window: &window,
                    native: http,
                };
                let account =
                    finalize_tianyi_web_authorization(session, &state.store, &validation_http)?;
                let _ = window.close();
                Ok(account)
            }
            CloudProvider::Aliyun => {
                let temporary = MemoryCredentialStore::default();
                let client = aliyun::AliyunClient::new(&temporary, http);
                let session =
                    parse_aliyun_web_session(web_storage_tokens(&window, ALIYUN_WEB_TOKENS_JS)?)?;
                let account = authorization_result(client.authorize_browser_session(session))?;
                state.store.save(
                    CloudProvider::Aliyun.key(),
                    &temporary
                        .load(CloudProvider::Aliyun.key())?
                        .ok_or(QuarkError::Storage)?,
                )?;
                let _ = window.close();
                Ok(account)
            }
            CloudProvider::Xunlei => {
                let (session, captured_context) = parse_xunlei_web_authorization(
                    asynchronous_web_tokens(&window, XUNLEI_WEB_TOKENS_JS)?,
                )?;
                let account = finalize_xunlei_web_authorization(
                    session,
                    captured_context,
                    &state.store,
                    http,
                )?;
                let _ = window.close();
                Ok(account)
            }
            CloudProvider::Uc => {
                let session = uc::parse_uc_web_session(&browser_cookies(
                    &window,
                    web_session_cookie_urls(CloudProvider::Uc),
                )?)?;
                let account = finalize_uc_web_authorization(session, &state.store, http)?;
                let _ = window.close();
                Ok(account)
            }
        }?;
        if cloud_session_policy(provider).supports_password_fallback {
            select_password_credential_account(&state.store, provider, &account.id)?;
        }
        if provider == CloudProvider::Baidu {
            let profile_username = load_password_credential(&state.store, provider)?
                .filter(|credential| credential.account_id() == account.id)
                .map(|credential| credential.username().to_owned());
            set_baidu_browser_profile_username(&state.store, profile_username.as_deref())?;
            set_browser_password_login_preferred(&state.store, provider, true)?;
        }
        Ok(account)
    })
    .await?;
    // The session is persisted only after the official page has proved it can
    // access the provider. Every queued failed task therefore resolves with
    // the new shared credentials, not a stale per-task snapshot.
    resume_pending_authorization_retries(app, provider).await?;
    Ok(account)
}
#[tauri::command]
pub async fn cloud_login_password(
    app: AppHandle,
    provider: CloudProvider,
    username: String,
    password: String,
) -> Result<CloudAccount, QuarkError> {
    if !provider_capabilities(provider).password {
        return Err(QuarkError::WebAuthorizationRequired);
    }
    let username = zeroize::Zeroizing::new(username);
    let password = zeroize::Zeroizing::new(password);
    let browser_app = app.clone();
    let account = run(app.clone(), provider, move |state, http| {
        if provider == CloudProvider::Baidu && prepare_baidu_account_login(&state.store, &username)?
        {
            clear_password_webview_profile(&browser_app, provider)?;
            set_baidu_browser_profile_username(&state.store, None)?;
            set_browser_password_login_preferred(&state.store, provider, false)?;
        }
        // Remember the latest submitted input before contacting the provider,
        // so a rejected or interrupted attempt does not force re-entry.
        save_password_login_draft(&state.store, provider, &username, &password)?;
        let account = match provider {
            CloudProvider::Pikpak => {
                let temporary = MemoryCredentialStore::default();
                let client = PikPakClient::new(http, &temporary);
                client.login(&username, &password)?;
                let account = client.account()?;
                client.list("0")?;
                state.store.save(
                    "pikpak",
                    &temporary.load("pikpak")?.ok_or(QuarkError::Storage)?,
                )?;
                account
            }
            CloudProvider::Baidu => {
                let native_result = if browser_password_login_preferred(&state.store, provider)? {
                    Err(QuarkError::WebAuthorizationRequired)
                } else {
                    baidu::BaiduClient::login_password(&username, &password, http)
                };
                let session = match native_result {
                    Ok(session) => session,
                    Err(error)
                        if browser_login::can_fallback_to_browser(&error)
                            || matches!(error, QuarkError::CaptchaRequired) =>
                    {
                        set_browser_password_login_preferred(&state.store, provider, true)?;
                        let account = browser_password_login(
                            &browser_app,
                            provider,
                            &state.store,
                            http,
                            &username,
                            &password,
                            None,
                            matches!(error, QuarkError::CaptchaRequired),
                        )?;
                        save_password_credential(
                            &state.store,
                            CloudPasswordCredential::new(
                                provider,
                                account.id.clone(),
                                username.as_str(),
                                password.as_str(),
                            ),
                        )?;
                        return Ok(account);
                    }
                    Err(error) => return Err(error),
                };
                set_browser_password_login_preferred(&state.store, provider, false)?;
                let client = baidu::BaiduClient::new(session.clone(), http);
                let account = client.account()?;
                client.directory("/")?;
                save_provider_session(CloudProvider::Baidu, &state.store, &session)?;
                account
            }
            CloudProvider::Tianyi => {
                let session = match tianyi::TianyiClient::login_password(&username, &password, http)
                {
                    Ok(session) => session,
                    Err(error)
                        if browser_login::can_fallback_to_browser(&error)
                            || matches!(error, QuarkError::CaptchaRequired) =>
                    {
                        let account = browser_password_login(
                            &browser_app,
                            provider,
                            &state.store,
                            http,
                            &username,
                            &password,
                            None,
                            matches!(error, QuarkError::CaptchaRequired),
                        )?;
                        save_password_credential(
                            &state.store,
                            CloudPasswordCredential::new(
                                provider,
                                account.id.clone(),
                                username.as_str(),
                                password.as_str(),
                            ),
                        )?;
                        return Ok(account);
                    }
                    Err(error) => return Err(error),
                };
                let client = tianyi::TianyiClient::new(session.clone(), http);
                let account = client.account()?;
                client.directory("-11")?;
                save_provider_session(CloudProvider::Tianyi, &state.store, &session)?;
                account
            }
            CloudProvider::Quark
            | CloudProvider::Aliyun
            | CloudProvider::Xunlei
            | CloudProvider::Uc => return Err(QuarkError::WebAuthorizationRequired),
        };
        save_password_credential(
            &state.store,
            CloudPasswordCredential::new(
                provider,
                account.id.clone(),
                username.as_str(),
                password.as_str(),
            ),
        )?;
        Ok(account)
    })
    .await?;
    queue_provider_authorization_failures(&app, provider)?;
    resume_pending_authorization_retries(app, provider).await?;
    Ok(account)
}
#[tauri::command]
pub async fn cloud_account(
    app: AppHandle,
    provider: CloudProvider,
) -> Result<CloudAccount, QuarkError> {
    run_session_work(app, provider, SessionWorkKind::Read, move |s, h| {
        restore_account(provider, &s.store, h)
    })
    .await
}
#[tauri::command]
pub async fn cloud_list_directory(
    app: AppHandle,
    provider: CloudProvider,
    parent_id: String,
) -> Result<CloudDirectory, QuarkError> {
    run_session_work(app, provider, SessionWorkKind::Read, move |s, h| {
        list_cloud_directory(provider, &s.store, h, &parent_id)
    })
    .await
}
#[tauri::command]
pub async fn cloud_create_directory(
    app: AppHandle,
    provider: CloudProvider,
    parent_id: String,
    name: String,
) -> Result<CloudFile, QuarkError> {
    valid_name(&name)?;
    run_session_work(
        app,
        provider,
        SessionWorkKind::Mutation,
        move |s, h| match provider {
            CloudProvider::Quark => {
                let v = response_json(
                    &QuarkClient::new(cookie(&s.store)?, h).mkdir(&parent_id, &name)?,
                )?;
                Ok(CloudFile {
                    id: required(&v["data"], "fid")?,
                    parent_id: parent_id.clone(),
                    name: name.clone(),
                    kind: "folder".into(),
                    size: None,
                    modified_at: None,
                })
            }
            CloudProvider::Pikpak => PikPakClient::new(h, &s.store).mkdir(&parent_id, &name),
            CloudProvider::Baidu => {
                let client = baidu::BaiduClient::new(
                    load_provider_session(CloudProvider::Baidu, &s.store)?,
                    h,
                );
                client.mkdir(&parent_id, &name)?;
                find_created_directory(client.directory(&parent_id)?, &name)
            }
            CloudProvider::Tianyi => {
                let client = tianyi::TianyiClient::new(
                    load_provider_session(CloudProvider::Tianyi, &s.store)?,
                    h,
                );
                client.mkdir(&parent_id, &name)?;
                find_created_directory(client.directory(&parent_id)?, &name)
            }
            CloudProvider::Aliyun => {
                aliyun::AliyunClient::new(&s.store, h).mkdir(&parent_id, &name)
            }
            CloudProvider::Xunlei => with_xunlei_client(&s.store, h, |client| {
                client.mkdir(&parent_id, &name)?;
                find_created_directory(client.directory(&parent_id)?, &name)
            }),
            CloudProvider::Uc => {
                let client =
                    uc::UcClient::new(load_provider_session(CloudProvider::Uc, &s.store)?, h);
                client.mkdir(&parent_id, &name)?;
                find_created_directory(client.directory(&parent_id)?, &name)
            }
        },
    )
    .await
}
#[tauri::command]
pub async fn cloud_rename(
    app: AppHandle,
    provider: CloudProvider,
    file_id: String,
    name: String,
) -> Result<(), QuarkError> {
    valid_name(&name)?;
    run_session_work(
        app,
        provider,
        SessionWorkKind::Mutation,
        move |s, h| match provider {
            CloudProvider::Quark => QuarkClient::new(cookie(&s.store)?, h)
                .rename(&file_id, &name)
                .map(|_| ()),
            CloudProvider::Pikpak => PikPakClient::new(h, &s.store).rename(&file_id, &name),
            CloudProvider::Baidu => {
                baidu::BaiduClient::new(load_provider_session(CloudProvider::Baidu, &s.store)?, h)
                    .rename(&file_id, &name)
            }
            CloudProvider::Tianyi => tianyi::TianyiClient::new(
                load_provider_session(CloudProvider::Tianyi, &s.store)?,
                h,
            )
            .rename(&file_id, &name),
            CloudProvider::Aliyun => aliyun::AliyunClient::new(&s.store, h).rename(&file_id, &name),
            CloudProvider::Xunlei => {
                with_xunlei_client(&s.store, h, |client| client.rename(&file_id, &name))
            }
            CloudProvider::Uc => {
                uc::UcClient::new(load_provider_session(CloudProvider::Uc, &s.store)?, h)
                    .rename(&file_id, &name)
                    .map(|_| ())
            }
        },
    )
    .await
}
#[tauri::command]
pub async fn cloud_move(
    app: AppHandle,
    provider: CloudProvider,
    file_ids: Vec<String>,
    target_parent_id: String,
) -> Result<(), QuarkError> {
    run_session_work(
        app,
        provider,
        SessionWorkKind::Mutation,
        move |s, h| match provider {
            CloudProvider::Quark => QuarkClient::new(cookie(&s.store)?, h)
                .move_files(&file_ids, &target_parent_id)
                .map(|_| ()),
            CloudProvider::Pikpak => {
                PikPakClient::new(h, &s.store).move_files(&file_ids, &target_parent_id)
            }
            CloudProvider::Baidu => {
                baidu::BaiduClient::new(load_provider_session(CloudProvider::Baidu, &s.store)?, h)
                    .move_files(&file_ids, &target_parent_id)
            }
            CloudProvider::Tianyi => tianyi::TianyiClient::new(
                load_provider_session(CloudProvider::Tianyi, &s.store)?,
                h,
            )
            .move_files(&file_ids, &target_parent_id),
            CloudProvider::Aliyun => {
                aliyun::AliyunClient::new(&s.store, h).move_files(&file_ids, &target_parent_id)
            }
            CloudProvider::Xunlei => with_xunlei_client(&s.store, h, |client| {
                client.move_files(&file_ids, &target_parent_id)
            }),
            CloudProvider::Uc => {
                uc::UcClient::new(load_provider_session(CloudProvider::Uc, &s.store)?, h)
                    .move_files(&file_ids, &target_parent_id)
                    .map(|_| ())
            }
        },
    )
    .await
}
#[tauri::command]
pub async fn cloud_copy(
    app: AppHandle,
    provider: CloudProvider,
    file_ids: Vec<String>,
    target_parent_id: String,
) -> Result<(), QuarkError> {
    run_session_work(
        app,
        provider,
        SessionWorkKind::Mutation,
        move |s, h| match provider {
            CloudProvider::Quark => QuarkClient::new(cookie(&s.store)?, h)
                .copy_files(&file_ids, &target_parent_id)
                .map(|_| ()),
            CloudProvider::Pikpak => {
                PikPakClient::new(h, &s.store).copy_files(&file_ids, &target_parent_id)
            }
            CloudProvider::Baidu => {
                baidu::BaiduClient::new(load_provider_session(CloudProvider::Baidu, &s.store)?, h)
                    .copy_files(&file_ids, &target_parent_id)
            }
            CloudProvider::Tianyi => tianyi::TianyiClient::new(
                load_provider_session(CloudProvider::Tianyi, &s.store)?,
                h,
            )
            .copy_files(&file_ids, &target_parent_id),
            CloudProvider::Aliyun => {
                aliyun::AliyunClient::new(&s.store, h).copy_files(&file_ids, &target_parent_id)
            }
            CloudProvider::Xunlei => with_xunlei_client(&s.store, h, |client| {
                client.copy_files(&file_ids, &target_parent_id)
            }),
            CloudProvider::Uc => {
                uc::UcClient::new(load_provider_session(CloudProvider::Uc, &s.store)?, h)
                    .copy_files(&file_ids, &target_parent_id)
                    .map(|_| ())
            }
        },
    )
    .await
}
#[tauri::command]
pub async fn cloud_trash(
    app: AppHandle,
    provider: CloudProvider,
    file_ids: Vec<String>,
) -> Result<(), QuarkError> {
    run_session_work(app, provider, SessionWorkKind::Mutation, move |s, h| {
        trash_provider_files(provider, &s.store, h, &file_ids)
    })
    .await
}

fn trash_provider_files(
    provider: CloudProvider,
    store: &DpapiCredentialStore,
    h: &impl CloudHttpTransport,
    file_ids: &[String],
) -> Result<(), QuarkError> {
    match provider {
        CloudProvider::Quark => QuarkClient::new(cookie(store)?, h)
            .trash(&file_ids)
            .map(|_| ()),
        CloudProvider::Pikpak => PikPakClient::new(h, store).trash(&file_ids),
        CloudProvider::Baidu => {
            baidu::BaiduClient::new(load_provider_session(CloudProvider::Baidu, store)?, h)
                .trash(&file_ids)
        }
        CloudProvider::Tianyi => {
            tianyi::TianyiClient::new(load_provider_session(CloudProvider::Tianyi, store)?, h)
                .trash(&file_ids)
        }
        CloudProvider::Aliyun => aliyun::AliyunClient::new(store, h).trash(&file_ids),
        CloudProvider::Xunlei => with_xunlei_client(store, h, |client| client.trash(&file_ids)),
        CloudProvider::Uc => uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, h)
            .trash(&file_ids)
            .map(|_| ()),
    }
}
#[tauri::command]
pub async fn cloud_delete(
    app: AppHandle,
    provider: CloudProvider,
    file_ids: Vec<String>,
) -> Result<(), QuarkError> {
    run_session_work(app, provider, SessionWorkKind::Mutation, move |s, h| {
        delete_provider_recycle_entries(provider, &s.store, h, &file_ids)
    })
    .await
}

#[tauri::command]
pub async fn cloud_restore(
    app: AppHandle,
    provider: CloudProvider,
    file_ids: Vec<String>,
) -> Result<(), QuarkError> {
    run_session_work(app, provider, SessionWorkKind::Mutation, move |s, h| {
        restore_provider_recycle_entries(provider, &s.store, h, &file_ids)
    })
    .await
}
pub fn enqueue_cloud_download(
    store: &TaskStore,
    url: &str,
    name: &str,
    _directory: &Path,
    enqueue: impl FnOnce(&str) -> Result<String, String>,
) -> Result<DownloadTask, QuarkError> {
    let task = store
        .create(NewTask {
            source: url.into(),
            name: name.into(),
            kind: "cloud".into(),
        })
        .map_err(|_| QuarkError::Storage)?;
    // Keep parsing until engine acceptance so a failure can be recorded with a valid transition.
    let gid = match enqueue(url) {
        Ok(g) => g,
        Err(message) => {
            let _ = store.fail(task.id, &message);
            return Err(QuarkError::Transport(message));
        }
    };
    store
        .bind_gid(task.id, &gid)
        .map_err(|_| QuarkError::Storage)?;
    store
        .transition(task.id, TaskStatus::Queued)
        .and_then(|_| store.transition(task.id, TaskStatus::Downloading))
        .map_err(|_| QuarkError::Storage)
}

pub fn create_cloud_task(
    store: &TaskStore,
    provider: CloudProvider,
    file_id: &str,
    name: &str,
) -> Result<DownloadTask, QuarkError> {
    if file_id.trim().is_empty() {
        return Err(QuarkError::InvalidInput("网盘文件 ID 无效".into()));
    }
    store
        .create(NewTask {
            source: format!(
                "cloud://{}/{}",
                provider.key(),
                super::types::encoded(file_id)
            ),
            name: safe_filename(name),
            kind: "cloud".into(),
        })
        .map_err(|_| QuarkError::Storage)
}

/// Decodes the durable cloud task source. Its restrictive shape prevents a
/// signed URL or arbitrary endpoint from being substituted during retry.
pub fn parse_stable_cloud_source(source: &str) -> Result<(CloudProvider, String), QuarkError> {
    let invalid = || QuarkError::InvalidInput("网盘任务来源无效，请从网盘重新创建下载".into());
    let (provider, encoded) = source
        .strip_prefix("cloud://")
        .and_then(|value| value.split_once('/'))
        .ok_or_else(invalid)?;
    let provider = match provider {
        "quark" => CloudProvider::Quark,
        "pikpak" => CloudProvider::Pikpak,
        "baidu" => CloudProvider::Baidu,
        "tianyi" => CloudProvider::Tianyi,
        "aliyun" => CloudProvider::Aliyun,
        "xunlei" => CloudProvider::Xunlei,
        "uc" => CloudProvider::Uc,
        _ => return Err(invalid()),
    };
    if encoded.is_empty() || encoded.contains(['/', '?', '#', '&', '=']) || !encoded.is_ascii() {
        return Err(invalid());
    }
    for (index, byte) in encoded.bytes().enumerate() {
        if byte == b'%'
            && !encoded
                .as_bytes()
                .get(index + 1..index + 3)
                .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
        {
            return Err(invalid());
        }
    }
    let id = url::form_urlencoded::parse(format!("id={encoded}").as_bytes())
        .next()
        .ok_or_else(invalid)?
        .1
        .into_owned();
    if id.trim().is_empty()
        || id
            .chars()
            .any(|character| character.is_control() || character == '\u{fffd}')
    {
        return Err(invalid());
    }
    Ok((provider, id))
}

/// Retrieves a fresh signed URL and request headers only for immediate aria2
/// submission. Neither value is stored in `TaskStore`.
pub async fn resolve_stable_cloud_download(
    app: AppHandle,
    source: &str,
) -> Result<ResolvedCloudDownload, QuarkError> {
    let (provider, file_id) = parse_stable_cloud_source(source)?;
    run_session_work(app, provider, SessionWorkKind::Read, move |state, http| {
        resolve_download(provider, &file_id, &state.store, http)
    })
    .await
}

pub async fn inspect_cloud_share(
    app: AppHandle,
    candidate: crate::parser::CloudShareCandidate,
) -> Result<share::ShareConfirmationOutcome, QuarkError> {
    // Share challenges must not trigger password recovery. An already-open
    // official authorization WebView may, however, safely refresh a rejected
    // read-only inspection cookie once.
    run_web_session_read_work(app, candidate.provider, move |state, http| {
        let adapter = share::provider_share_adapter(candidate.provider, &state.store, http)?;
        share::inspect_share_with_adapter(&state.share_plans, adapter.as_ref(), &candidate)
    })
    .await
}

pub async fn resolve_stable_cloud_share_download(
    app: AppHandle,
    source: &str,
) -> Result<ResolvedCloudDownload, QuarkError> {
    let (provider, _, _) = share::parse_stable_cloud_share_source(source)?;
    let source = source.to_owned();
    run_web_session_read_work(app, provider, move |state, http| {
        let adapter = share::provider_share_adapter(provider, &state.store, http)?;
        share::resolve_current_share_download(&state.share_plans, adapter.as_ref(), &source)
    })
    .await
}

pub async fn submit_cloud_share_file(
    app: AppHandle,
    state: &AppState,
    source: String,
    configured: UriSubmissionOptions,
) -> Result<TaskActionOutcome, QuarkError> {
    let submission =
        submit_cloud_share_file_internal(app.clone(), state, source, configured, None, true)
            .await?;
    let outcome = match submission {
        CloudShareFileSubmission::Queued(outcome)
        | CloudShareFileSubmission::Duplicate(outcome) => outcome,
    };
    schedule_cloud_share_cleanup(app, &outcome.payload.task);
    Ok(outcome)
}

enum CloudShareFileSubmission {
    Queued(TaskActionOutcome),
    Duplicate(TaskActionOutcome),
}

async fn submit_cloud_share_file_internal(
    app: AppHandle,
    state: &AppState,
    source: String,
    configured: UriSubmissionOptions,
    directory_context: Option<DirectoryLeafContext>,
    append_entry_directory: bool,
) -> Result<CloudShareFileSubmission, QuarkError> {
    share::parse_stable_cloud_share_source(&source)?;
    let _source_submission = state.cloud_download_sources.acquire(&source).await?;
    if let Some(existing) = state
        .store
        .find_by_source(&source)
        .map_err(|_| QuarkError::Storage)?
        .filter(|task| task.status != TaskStatus::Recycled)
    {
        if !is_retryable_cloud_share_prepare(&existing) {
            let duplicate =
                duplicate_state(&state.store, &existing).map_err(|_| QuarkError::Storage)?;
            return Ok(CloudShareFileSubmission::Duplicate(
                task_action_outcome(&state.store, existing, Some(duplicate))
                    .map_err(QuarkError::Transport)?,
            ));
        }
    }
    let cloud = app
        .try_state::<Arc<CloudState>>()
        .ok_or(QuarkError::Storage)?;
    let resolve_source = source.clone();
    let outcome = submit_new_cloud_share_file_scoped_with(
        state.store.clone(),
        &cloud.share_plans,
        source,
        configured,
        directory_context,
        append_entry_directory,
        || async {
            state
                .runtime
                .service()
                .await
                .map_err(|_| QuarkError::Transport("下载引擎不可用".into()))
        },
        || resolve_stable_cloud_share_download(app.clone(), &resolve_source),
    )
    .await?;
    Ok(CloudShareFileSubmission::Queued(outcome))
}

pub async fn submit_cloud_share_directory_leaf(
    app: AppHandle,
    state: &AppState,
    source: String,
    configured: UriSubmissionOptions,
    context: DirectoryLeafContext,
) -> Result<DirectoryLeafSubmission, QuarkError> {
    match submit_cloud_share_file_internal(
        app.clone(),
        state,
        source,
        configured,
        Some(context),
        false,
    )
    .await?
    {
        CloudShareFileSubmission::Queued(outcome) => {
            schedule_cloud_share_cleanup(app, &outcome.payload.task);
            Ok(DirectoryLeafSubmission::Queued(outcome.payload.task.id))
        }
        CloudShareFileSubmission::Duplicate(_) => Ok(DirectoryLeafSubmission::Duplicate),
    }
}

pub(crate) async fn submit_new_cloud_share_file_with<S, SFut, R, RFut>(
    tasks: Arc<TaskStore>,
    plans: &share::CloudSharePlanStore,
    source: String,
    configured: UriSubmissionOptions,
    service: S,
    resolve: R,
) -> Result<TaskActionOutcome, QuarkError>
where
    S: FnOnce() -> SFut,
    SFut: std::future::Future<Output = Result<Arc<crate::DownloadService>, QuarkError>>,
    R: FnOnce() -> RFut,
    RFut: std::future::Future<Output = Result<ResolvedCloudDownload, QuarkError>>,
{
    submit_new_cloud_share_file_scoped_with(
        tasks, plans, source, configured, None, true, service, resolve,
    )
    .await
}

pub(crate) async fn submit_new_cloud_share_file_scoped_with<S, SFut, R, RFut>(
    tasks: Arc<TaskStore>,
    plans: &share::CloudSharePlanStore,
    source: String,
    mut configured: UriSubmissionOptions,
    directory_context: Option<DirectoryLeafContext>,
    append_entry_directory: bool,
    service: S,
    resolve: R,
) -> Result<TaskActionOutcome, QuarkError>
where
    S: FnOnce() -> SFut,
    SFut: std::future::Future<Output = Result<Arc<crate::DownloadService>, QuarkError>>,
    R: FnOnce() -> RFut,
    RFut: std::future::Future<Output = Result<ResolvedCloudDownload, QuarkError>>,
{
    share::parse_stable_cloud_share_source(&source)?;
    let entry = plans.resolve_entry(&source)?;
    if append_entry_directory {
        if let Some(directory) = configured.directory.as_ref() {
            configured.directory = Some(directory_download_output_directory(
                directory,
                &entry.entry.relative_directory,
            )?);
        } else if !entry.entry.relative_directory.is_empty() {
            return Err(QuarkError::InvalidInput(
                "分享目录下载需要设置绝对下载目录".into(),
            ));
        }
    }
    let output_name = configured
        .output_name
        .clone()
        .unwrap_or_else(|| safe_filename(&entry.entry.name));
    configured.output_name = Some(output_name.clone());
    configured
        .clone()
        .into_aria2_options()
        .map_err(|_| QuarkError::InvalidInput("下载路径或文件名无效".into()))?;

    // Preserve the single-file contract: an unavailable engine must not leave
    // a task behind. A directory leaf is different because the durable group
    // must retain a failed child for Failed/Recycle projections.
    let mut service_factory = Some(service);
    let eager_service = if directory_context.is_none() {
        Some(
            service_factory
                .take()
                .expect("service factory is available")()
            .await?,
        )
    } else {
        None
    };

    let prepared = match tasks
        .find_by_source(&source)
        .map_err(|_| QuarkError::Storage)?
        .filter(is_retryable_cloud_share_prepare)
    {
        Some(mut prepared) => {
            prepared.status = TaskStatus::Parsing;
            prepared.error = None;
            prepared.failure_code = None;
            tasks
                .apply_transfer_snapshot(prepared)
                .map_err(|_| QuarkError::Storage)?
        }
        None => tasks
            .create(crate::NewTask {
                source: source.clone(),
                name: output_name,
                kind: "cloud".into(),
            })
            .map_err(|_| QuarkError::Storage)?,
    };
    if let Err(error) = plans.reserve_local_task(&source, prepared.id) {
        return match tasks.remove_task_record(prepared.id) {
            Ok(()) | Err(crate::StoreError::NotFound(_)) => Err(error),
            // The task may still be durable, so report the conservative
            // recoverable state instead of claiming creation was rolled back.
            Err(_) => Ok(cloud_share_failure_outcome(
                &tasks,
                prepared,
                None,
                CLOUD_SHARE_PREPARE_FAILURE,
            )),
        };
    }
    if let Some(context) = directory_context {
        tasks
            .link_cloud_directory_group_item_task(context.group_id, context.item_id, prepared.id)
            .map_err(|_| QuarkError::Storage)?;
    }

    let service = match eager_service {
        Some(service) => service,
        None => match service_factory
            .take()
            .expect("directory service factory is available")()
        .await
        {
            Ok(service) => service,
            Err(error) => {
                return Ok(cloud_share_failure_outcome(
                    &tasks,
                    prepared,
                    Some(cloud_failure_code(&error)),
                    share_resolution_failure_message(&error),
                ))
            }
        },
    };

    let resolved = match resolve().await {
        Ok(resolved) => resolved,
        Err(error) => {
            return Ok(cloud_share_failure_outcome(
                &tasks,
                prepared,
                Some(cloud_failure_code(&error)),
                share_resolution_failure_message(&error),
            ))
        }
    };
    let task = match share::submit_current_share_download(&service, &prepared, resolved, configured)
        .await
    {
        Ok(task) => task,
        Err(_) => {
            return Ok(cloud_share_failure_outcome(
                &tasks,
                prepared,
                None,
                "aria2 未能接受网盘分享下载任务",
            ))
        }
    };
    Ok(cloud_share_task_outcome(&tasks, task))
}

fn cloud_share_failure_outcome(
    tasks: &TaskStore,
    prepared: DownloadTask,
    failure_code: Option<&str>,
    message: &'static str,
) -> TaskActionOutcome {
    let task = tasks
        .fail_with_code(prepared.id, failure_code, message)
        .or_else(|_| tasks.find(prepared.id))
        .unwrap_or(prepared);
    cloud_share_task_outcome(tasks, task)
}

fn is_retryable_cloud_share_prepare(task: &DownloadTask) -> bool {
    task.kind == "cloud"
        && task.aria_gid.is_none()
        && (task.status == TaskStatus::Parsing
            || (task.status == TaskStatus::Failed
                && task.error.as_deref() == Some(CLOUD_SHARE_PREPARE_FAILURE)))
}

fn cloud_share_task_outcome(tasks: &TaskStore, task: DownloadTask) -> TaskActionOutcome {
    if let Ok(outcome) = task_action_outcome(tasks, task.clone(), None) {
        return outcome;
    }
    let files = tasks.files(task.id).unwrap_or_default();
    let reauthorization_required =
        requires_cloud_reauthorization_failure(task.failure_code.as_deref());
    TaskActionOutcome {
        payload: crate::commands::DownloadTaskPayload {
            task,
            files,
            cleanup_pending: false,
            reauthorization_required,
        },
        duplicate: None,
        queue_task: None,
    }
}

pub(crate) fn schedule_cloud_share_cleanup(app: AppHandle, task: &DownloadTask) {
    if task.status != TaskStatus::Completed || !task.source.starts_with("cloud-share://") {
        return;
    }
    let id = task.id;
    tauri::async_runtime::spawn(async move {
        let _ = cleanup_cloud_share_task(app, id, false).await;
    });
}

/// Explicit retry uses the same exact-object journal, and never resolves a URL
/// or resubmits a local download. It also works when aria2 is unavailable.
pub(crate) async fn cleanup_cloud_share_task(
    app: AppHandle,
    id: uuid::Uuid,
    retry: bool,
) -> Result<DownloadTask, QuarkError> {
    use tauri::Emitter;
    let state = app.try_state::<AppState>().ok_or(QuarkError::Storage)?;
    let task = state.store.find(id).map_err(|_| QuarkError::Storage)?;
    share::parse_stable_cloud_share_source(&task.source)?;
    if task.status != TaskStatus::Completed {
        return Err(QuarkError::InvalidInput(
            "只有已完成的分享任务可以重试清理".into(),
        ));
    }
    if !state
        .settings
        .lock()
        .map_err(|_| QuarkError::Storage)?
        .delete_transferred_share_files_after_download
    {
        return Ok(task);
    }
    match cleanup_cloud_share_task_inner(app.clone(), id, retry).await {
        Ok(task) => Ok(task),
        Err(_) => {
            let task = state
                .store
                .set_completed_cleanup_message(id, true)
                .map_err(|_| QuarkError::Storage)?;
            let cloud = app.try_state::<Arc<CloudState>>();
            if let Ok(payload) = crate::commands::task_update_payload(
                &state.store,
                cloud.as_ref().map(|cloud| &cloud.share_plans),
                task.clone(),
            ) {
                let _ = app.emit("download-task-updated", payload);
            }
            Ok(task)
        }
    }
}

async fn cleanup_cloud_share_task_inner(
    app: AppHandle,
    id: uuid::Uuid,
    retry: bool,
) -> Result<DownloadTask, QuarkError> {
    use tauri::Emitter;
    let state = app.try_state::<AppState>().ok_or(QuarkError::Storage)?;
    let task = state.store.find(id).map_err(|_| QuarkError::Storage)?;
    if task.status != TaskStatus::Completed {
        return Err(QuarkError::InvalidInput(
            "只有已完成的分享任务可以重试清理".into(),
        ));
    }
    let (provider, _, _) = share::parse_stable_cloud_share_source(&task.source)?;
    let cloud = app
        .try_state::<Arc<CloudState>>()
        .ok_or(QuarkError::Storage)?;
    // Acquire the stable plan lock before reading its current linked tasks.
    // New entries share this lock even if an earlier plan snapshot omitted them.
    let _plan_guard = state.cloud_download_sources.acquire(&task.source).await?;
    cloud.share_plans.link_local_task(&task.source, id)?;
    let tasks = Arc::clone(&state.store);
    let worker_app = app.clone();
    // No automatic session-recovery replay around a destructive action.
    let updated = run(app.clone(), provider, move |cloud, http| {
        let state = worker_app
            .try_state::<AppState>()
            .ok_or(QuarkError::Storage)?;
        let enabled = state
            .settings
            .lock()
            .map_err(|_| QuarkError::Storage)?
            .delete_transferred_share_files_after_download;
        let entry = cloud.share_plans.resolve_entry(&task.source)?;
        let adapter = share::provider_share_adapter(provider, &cloud.store, http)?;
        let owner = adapter.transfer_owner_fingerprint(&entry)?;
        crate::engine_runtime::cleanup_completed_share_for_owner(
            &cloud.share_plans,
            &tasks,
            &task,
            enabled,
            retry,
            owner.as_deref(),
            |provider, ids| {
                if provider == CloudProvider::Baidu {
                    baidu::BaiduClient::new(load_provider_session(provider, &cloud.store)?, http)
                        .trash_transferred_files(ids)
                } else {
                    trash_provider_files(provider, &cloud.store, http, ids)
                }
            },
        )
    })
    .await?;
    if let Some(updated) = updated {
        if let Ok(payload) = crate::commands::task_update_payload(
            &state.store,
            Some(&cloud.share_plans),
            updated.clone(),
        ) {
            let _ = app.emit("download-task-updated", payload);
        }
        Ok(updated)
    } else {
        state.store.find(id).map_err(|_| QuarkError::Storage)
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudDirectoryDownloadFailure {
    pub relative_path: String,
    pub code: &'static str,
    pub message: &'static str,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudDirectoryDownloadOutcome {
    pub task: TaskActionOutcome,
    pub queued_count: usize,
    pub failures: Vec<CloudDirectoryDownloadFailure>,
}

/// The only two durable outcomes of submitting one planned directory leaf.
/// The resolver and aria2 details stay behind the submission boundary.
pub enum DirectoryLeafSubmission {
    Queued(uuid::Uuid),
    Duplicate,
}

#[derive(Clone, Copy, Debug)]
pub struct DirectoryLeafContext {
    pub group_id: uuid::Uuid,
    pub item_id: uuid::Uuid,
}

/// Persists the complete safe directory plan before any leaf receives a
/// signed URL. The journal contains only stable provider IDs and local output
/// components, never a URL or session material.
pub fn create_cloud_directory_group_for_download(
    store: &TaskStore,
    provider: CloudProvider,
    directory_id: &str,
    plan: &CloudDirectoryDownloadPlan,
    output_directory: &Path,
) -> Result<CloudDirectoryGroup, QuarkError> {
    let relative_directory = plan
        .files
        .first()
        .and_then(|file| file.relative_directory.first())
        .ok_or_else(|| QuarkError::InvalidInput("空目录不能下载".into()))?
        .clone();
    let items = plan
        .files
        .iter()
        .map(|file| {
            NewCloudDirectoryGroupItem::new(
                file.file_id.clone(),
                &file.relative_directory,
                file.output_name.clone(),
                file.size,
            )
        })
        .collect::<Vec<_>>();
    store
        .create_cloud_directory_group(
            NewCloudDirectoryGroup::new(
                provider,
                directory_id,
                relative_directory,
                output_directory,
            ),
            &items,
        )
        .map_err(|_| QuarkError::Storage)
}

enum CloudFileSubmission {
    Queued(TaskActionOutcome),
    Duplicate(TaskActionOutcome),
}

async fn submit_cloud_file(
    app: AppHandle,
    state: &AppState,
    provider: CloudProvider,
    file_id: String,
    configured: UriSubmissionOptions,
    directory_context: Option<DirectoryLeafContext>,
) -> Result<CloudFileSubmission, QuarkError> {
    let stable_source = format!(
        "cloud://{}/{}",
        provider.key(),
        super::types::encoded(&file_id)
    );
    let _source_submission = state.cloud_download_sources.acquire(&stable_source).await?;
    if let Some(existing) = state
        .store
        .find_by_source(&stable_source)
        .map_err(|_| QuarkError::Storage)?
        .filter(|task| task.status != TaskStatus::Recycled)
    {
        let duplicate =
            duplicate_state(&state.store, &existing).map_err(|_| QuarkError::Storage)?;
        return Ok(CloudFileSubmission::Duplicate(
            task_action_outcome(&state.store, existing, Some(duplicate))
                .map_err(QuarkError::Transport)?,
        ));
    }

    let resolved = resolve_stable_cloud_download(app.clone(), &stable_source).await?;
    let task = create_cloud_task(&state.store, provider, &file_id, &resolved.name)?;
    if let Some(context) = directory_context {
        state
            .store
            .link_cloud_directory_group_item_task(context.group_id, context.item_id, task.id)
            .map_err(|_| QuarkError::Storage)?;
    }
    let signed_url = resolved.url.clone();
    let mut options = resolved.into_uri_submission_options();
    options.directory = configured.directory;
    options.output_name = configured.output_name.or(options.output_name);
    options.proxy = configured.proxy;
    options.allow_overwrite = configured.allow_overwrite;
    options.continue_download = configured.continue_download;
    let service = state
        .runtime
        .service()
        .await
        .map_err(|error| QuarkError::Transport(error.to_string()))?;
    let task = match service
        .submit_uri_with_options(task.id, vec![signed_url], options)
        .await
    {
        Ok(task) => task,
        Err(error) => {
            let message = format!("aria2 未能接受网盘下载任务：{error}");
            let _ = state.store.fail(task.id, &message);
            return Err(QuarkError::Transport(message));
        }
    };
    Ok(CloudFileSubmission::Queued(
        task_action_outcome(&state.store, task, None).map_err(QuarkError::Transport)?,
    ))
}

fn planned_relative_path(file: &PlannedCloudFile) -> String {
    file.relative_directory
        .iter()
        .chain(std::iter::once(&file.output_name))
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("/")
}

fn directory_download_output_directory(
    base: &Path,
    components: &[String],
) -> Result<PathBuf, QuarkError> {
    if !base.is_absolute() {
        return Err(QuarkError::InvalidInput("下载目录必须是绝对路径".into()));
    }
    let mut target = base.to_path_buf();
    for component in components {
        target.push(provider_directory_component(component)?);
    }
    // Check lexical containment before touching the filesystem, even if the
    // component normalization above changes in the future.
    if !target.strip_prefix(base).is_ok_and(|relative| {
        relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    }) {
        return Err(QuarkError::InvalidResponse);
    }
    std::fs::create_dir_all(&target)
        .map_err(|_| QuarkError::Transport("无法创建目录下载目标文件夹".into()))?;
    Ok(target)
}

/// Restores the exact target of a linked directory member before a retry.
/// This is needed when resolution or engine submission failed before aria2
/// could persist a file path for the local task.
pub fn cloud_directory_member_retry_options(
    store: &TaskStore,
    task_id: uuid::Uuid,
    mut configured: UriSubmissionOptions,
) -> Result<UriSubmissionOptions, QuarkError> {
    let Some(group) = store
        .cloud_directory_group_for_task(task_id)
        .map_err(|_| QuarkError::Storage)?
    else {
        return Ok(configured);
    };
    let item = store
        .cloud_directory_group_items(group.id)
        .map_err(|_| QuarkError::Storage)?
        .into_iter()
        .find(|item| item.task_id == Some(task_id))
        .ok_or(QuarkError::Storage)?;
    configured.directory = Some(directory_download_output_directory(
        &group.output_directory,
        &item.relative_directory,
    )?);
    configured.output_name = Some(item.output_name);
    Ok(configured)
}

fn provider_directory_component(component: &str) -> Result<String, QuarkError> {
    let mut path_components = Path::new(component).components();
    // Reject drive prefixes, ADS and device names before safe_filename could
    // hide the unsafe provider input. Validate all components before mkdir.
    if component.contains(['/', '\\', ':'])
        || is_windows_device_name(component)
        || component.chars().any(char::is_control)
        || !matches!(path_components.next(), Some(Component::Normal(_)))
        || path_components.next().is_some()
    {
        return Err(QuarkError::InvalidResponse);
    }
    // Confirmation renders these names with the same filename normalization.
    Ok(safe_filename(component))
}

fn directory_download_failure(
    file: &PlannedCloudFile,
    error: &QuarkError,
) -> CloudDirectoryDownloadFailure {
    let (code, message) = directory_download_failure_fields(error);
    CloudDirectoryDownloadFailure {
        relative_path: planned_relative_path(file),
        code,
        message,
    }
}

fn directory_download_failure_fields(error: &QuarkError) -> (&'static str, &'static str) {
    match error {
        QuarkError::AuthorizationExpired | QuarkError::NotAuthorized => {
            ("credentials_expired", "网盘会话已过期")
        }
        QuarkError::SessionRenewedRetryOperation => (
            "session_renewed_retry_operation",
            "登录已自动恢复，请重新执行本次操作",
        ),
        QuarkError::WebAuthorizationRequired | QuarkError::CaptchaRequired => {
            ("web_auth_required", "网盘需要网页验证")
        }
        QuarkError::Transport(_) => ("network_error", "网络或下载引擎错误"),
        QuarkError::Provider(_) => ("provider_error", "网盘拒绝下载请求"),
        QuarkError::InvalidResponse => ("invalid_response", "网盘返回无效下载数据"),
        QuarkError::Storage => ("credential_storage_error", "本机凭据或任务存储不可用"),
        QuarkError::InvalidInput(_) => ("invalid_input", "下载参数无效"),
        QuarkError::AuthorizationPending => ("authorization_pending", "网页授权尚未完成"),
        QuarkError::UploadUnavailable | QuarkError::ProviderUnsupported => {
            ("provider_unsupported", "当前网盘不支持该下载")
        }
        QuarkError::PikpakRegionalMembershipRestricted => (
            "pikpak_regional_membership_restricted",
            "PikPak 当前地区下载受限",
        ),
    }
}

fn cloud_directory_group_task_outcome(
    store: &TaskStore,
    group: &CloudDirectoryGroup,
) -> Result<TaskActionOutcome, QuarkError> {
    Ok(TaskActionOutcome {
        payload: crate::commands::DownloadTaskPayload {
            task: DownloadTask {
                id: group.id,
                source: format!("cloud-directory://{}/{}", group.provider.key(), group.id),
                name: group.relative_directory.clone(),
                kind: "folder".into(),
                status: TaskStatus::Queued,
                progress: 0,
                error: None,
                failure_code: None,
                aria_gid: None,
                followed_by_gid: None,
                metadata_gid: None,
                total_length: None,
                completed_length: 0,
                download_speed: 0,
                upload_speed: 0,
                connections: 0,
                eta_seconds: None,
                aria_error_code: None,
                created_at: group.created_at,
                updated_at: group.updated_at,
                completed_at: None,
            },
            files: Vec::new(),
            cleanup_pending: false,
            reauthorization_required: false,
        },
        duplicate: None,
        queue_task: Some(
            cloud_directory_group_queue_task(store, group.id).map_err(QuarkError::Transport)?,
        ),
    })
}

/// Executes a previously validated plan through a narrow, injectable leaf
/// submission boundary. This is shared by the Tauri command and deterministic
/// connector tests, so persistence/linking semantics cannot drift from the
/// real directory-download flow.
pub async fn submit_cloud_directory_plan<F, Fut>(
    store: &TaskStore,
    provider: CloudProvider,
    directory_id: &str,
    plan: CloudDirectoryDownloadPlan,
    configured: UriSubmissionOptions,
    mut submit: F,
) -> Result<CloudDirectoryDownloadOutcome, QuarkError>
where
    F: FnMut(CloudProvider, String, UriSubmissionOptions) -> Fut,
    Fut: Future<Output = Result<DirectoryLeafSubmission, QuarkError>>,
{
    submit_cloud_directory_plan_with_group_event(
        store,
        provider,
        directory_id,
        plan,
        configured,
        |_| {},
        move |provider, file_id, options, _| submit(provider, file_id, options),
    )
    .await
}

/// Publishes the durable directory root before starting any leaf submission.
/// The leaf context lets the real downloader bind each local task to that root
/// before aria2 can publish its first status event.
pub async fn submit_cloud_directory_plan_with_group_event<F, Fut, G>(
    store: &TaskStore,
    provider: CloudProvider,
    directory_id: &str,
    plan: CloudDirectoryDownloadPlan,
    configured: UriSubmissionOptions,
    mut group_created: G,
    mut submit: F,
) -> Result<CloudDirectoryDownloadOutcome, QuarkError>
where
    F: FnMut(CloudProvider, String, UriSubmissionOptions, DirectoryLeafContext) -> Fut,
    Fut: Future<Output = Result<DirectoryLeafSubmission, QuarkError>>,
    G: FnMut(QueueTaskPayload),
{
    let base_directory = configured
        .directory
        .clone()
        .ok_or_else(|| QuarkError::InvalidInput("未配置下载目录".into()))?;
    let group = create_cloud_directory_group_for_download(
        store,
        provider,
        directory_id,
        &plan,
        &base_directory,
    )?;
    group_created(
        cloud_directory_group_queue_task(store, group.id).map_err(QuarkError::Transport)?,
    );
    let items = store
        .cloud_directory_group_items(group.id)
        .map_err(|_| QuarkError::Storage)?;
    if items.len() != plan.files.len() {
        return Err(QuarkError::Storage);
    }
    let mut failures = Vec::new();
    let mut queued_count = 0;
    for (file, item) in plan.files.into_iter().zip(items) {
        let relative_path = planned_relative_path(&file);
        let output_directory =
            match directory_download_output_directory(&base_directory, &file.relative_directory) {
                Ok(directory) => directory,
                Err(error) => {
                    persist_directory_failure(
                        store,
                        group.id,
                        item.id,
                        &file,
                        &error,
                        &mut failures,
                    )?;
                    continue;
                }
            };
        let mut options = configured.clone();
        options.directory = Some(output_directory);
        options.output_name = Some(file.output_name.clone());
        let context = DirectoryLeafContext {
            group_id: group.id,
            item_id: item.id,
        };
        match submit(provider, file.file_id.clone(), options, context).await {
            Ok(DirectoryLeafSubmission::Queued(task_id)) => {
                store
                    .link_cloud_directory_group_item_task(group.id, item.id, task_id)
                    .map_err(|_| QuarkError::Storage)?;
                queued_count += 1;
            }
            Ok(DirectoryLeafSubmission::Duplicate) => {
                let failure = CloudDirectoryDownloadFailure {
                    relative_path,
                    code: "duplicate",
                    message: "已存在下载任务，未重复入队",
                };
                store
                    .fail_cloud_directory_group_item(
                        group.id,
                        item.id,
                        failure.code,
                        failure.message,
                    )
                    .map_err(|_| QuarkError::Storage)?;
                failures.push(failure);
            }
            Err(error) => {
                persist_directory_failure(store, group.id, item.id, &file, &error, &mut failures)?;
            }
        }
    }
    Ok(CloudDirectoryDownloadOutcome {
        task: cloud_directory_group_task_outcome(store, &group)?,
        queued_count,
        failures,
    })
}

fn persist_directory_failure(
    store: &TaskStore,
    group_id: uuid::Uuid,
    item_id: uuid::Uuid,
    file: &PlannedCloudFile,
    error: &QuarkError,
    failures: &mut Vec<CloudDirectoryDownloadFailure>,
) -> Result<(), QuarkError> {
    let failure = directory_download_failure(file, error);
    store
        .fail_cloud_directory_group_item(group_id, item_id, failure.code, failure.message)
        .map_err(|_| QuarkError::Storage)?;
    failures.push(failure);
    Ok(())
}

#[tauri::command]
pub async fn cloud_download_url(
    app: AppHandle,
    provider: CloudProvider,
    file_id: String,
) -> Result<TaskActionOutcome, QuarkError> {
    let app_handle = app.clone();
    let state = app_handle.state::<AppState>();
    let configured =
        crate::commands::download_submission_options(&state).map_err(QuarkError::Transport)?;
    match submit_cloud_file(app, &state, provider, file_id, configured, None).await? {
        CloudFileSubmission::Queued(outcome) | CloudFileSubmission::Duplicate(outcome) => {
            Ok(outcome)
        }
    }
}

/// Queues every real file below one selected cloud folder. The plan is built in
/// full before any download URL is resolved so cycles, duplicate nodes, empty
/// folders, and the traversal limits fail without enqueuing a partial tree.
#[tauri::command]
pub async fn cloud_download_directory(
    app: AppHandle,
    provider: CloudProvider,
    directory_id: String,
    directory_name: String,
) -> Result<CloudDirectoryDownloadOutcome, QuarkError> {
    let planned_directory_id = directory_id.clone();
    let plan = run_session_work(
        app.clone(),
        provider,
        SessionWorkKind::Read,
        move |state, http| {
            plan_cloud_directory_download(provider, &planned_directory_id, &directory_name, |id| {
                list_cloud_directory(provider, &state.store, http, id)
            })
        },
    )
    .await?;

    let state = app.state::<AppState>();
    let configured =
        crate::commands::download_submission_options(&state).map_err(QuarkError::Transport)?;
    let store = Arc::clone(&state.store);
    let submit_app = app.clone();
    let root_event_app = app.clone();
    submit_cloud_directory_plan_with_group_event(
        &store,
        provider,
        &directory_id,
        plan,
        configured,
        move |root| {
            use tauri::Emitter;
            let _ = root_event_app.emit("download-task-updated", root);
        },
        move |provider, file_id, options, context| {
            let app = submit_app.clone();
            async move {
                let state = app.state::<AppState>();
                match submit_cloud_file(
                    app.clone(),
                    &state,
                    provider,
                    file_id,
                    options,
                    Some(context),
                )
                .await?
                {
                    CloudFileSubmission::Queued(outcome) => {
                        Ok(DirectoryLeafSubmission::Queued(outcome.payload.task.id))
                    }
                    CloudFileSubmission::Duplicate(_) => Ok(DirectoryLeafSubmission::Duplicate),
                }
            }
        },
    )
    .await
}

/// Resubmits exactly one previously failed, unlinked directory leaf. The
/// durable item is the authority for provider ID and output target; retry never
/// scans by filename and never replays the rest of the directory plan.
pub async fn retry_cloud_directory_group_item(
    app: AppHandle,
    group_id: uuid::Uuid,
    item_id: uuid::Uuid,
) -> Result<TaskActionOutcome, QuarkError> {
    let state = app.state::<AppState>();
    let configured =
        crate::commands::download_submission_options(&state).map_err(QuarkError::Transport)?;
    let store = Arc::clone(&state.store);
    let submit_app = app.clone();
    retry_cloud_directory_group_item_with(
        &store,
        group_id,
        item_id,
        configured,
        move |provider, file_id, options, context| {
            let app = submit_app.clone();
            async move {
                let state = app.state::<AppState>();
                if file_id.starts_with("cloud-share://") {
                    return submit_cloud_share_directory_leaf(
                        app.clone(),
                        &state,
                        file_id,
                        options,
                        context,
                    )
                    .await;
                }
                match submit_cloud_file(
                    app.clone(),
                    &state,
                    provider,
                    file_id,
                    options,
                    Some(context),
                )
                .await?
                {
                    CloudFileSubmission::Queued(outcome) => {
                        Ok(DirectoryLeafSubmission::Queued(outcome.payload.task.id))
                    }
                    CloudFileSubmission::Duplicate(_) => Ok(DirectoryLeafSubmission::Duplicate),
                }
            }
        },
    )
    .await
}

/// Resubmits every unlinked failed leaf in one durable directory group. Linked
/// leaves are deliberately excluded: their existing local task remains the
/// authority and must never be submitted a second time.
pub async fn retry_cloud_directory_group_with<F, Fut>(
    store: &TaskStore,
    group_id: uuid::Uuid,
    configured: UriSubmissionOptions,
    mut submit: F,
) -> Result<TaskActionOutcome, QuarkError>
where
    F: FnMut(CloudProvider, String, UriSubmissionOptions, DirectoryLeafContext) -> Fut,
    Fut: Future<Output = Result<DirectoryLeafSubmission, QuarkError>>,
{
    let group = store
        .cloud_directory_group(group_id)
        .map_err(|_| QuarkError::Storage)?
        .ok_or_else(|| QuarkError::InvalidInput("网盘目录下载任务不存在".into()))?;
    let pending = store
        .cloud_directory_group_items(group.id)
        .map_err(|_| QuarkError::Storage)?
        .into_iter()
        .filter(|item| item.task_id.is_none())
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Err(QuarkError::InvalidInput(
            "目录内没有可重新提交的网盘文件".into(),
        ));
    }
    for item in pending {
        // A failed provider attempt is recorded by the single-item path. Keep
        // trying sibling leaves so one transient failure cannot starve the
        // remainder of a directory retry.
        let _ = retry_cloud_directory_group_item_with(
            store,
            group.id,
            item.id,
            configured.clone(),
            |provider, file_id, options, context| submit(provider, file_id, options, context),
        )
        .await;
    }
    cloud_directory_group_task_outcome(store, &group)
}

pub async fn retry_cloud_directory_group(
    app: AppHandle,
    group_id: uuid::Uuid,
) -> Result<TaskActionOutcome, QuarkError> {
    let state = app.state::<AppState>();
    let configured =
        crate::commands::download_submission_options(&state).map_err(QuarkError::Transport)?;
    let store = Arc::clone(&state.store);
    let submit_app = app.clone();
    retry_cloud_directory_group_with(
        &store,
        group_id,
        configured,
        move |provider, file_id, options, context| {
            let app = submit_app.clone();
            async move {
                let state = app.state::<AppState>();
                if file_id.starts_with("cloud-share://") {
                    return submit_cloud_share_directory_leaf(
                        app.clone(),
                        &state,
                        file_id,
                        options,
                        context,
                    )
                    .await;
                }
                match submit_cloud_file(
                    app.clone(),
                    &state,
                    provider,
                    file_id,
                    options,
                    Some(context),
                )
                .await?
                {
                    CloudFileSubmission::Queued(outcome) => {
                        Ok(DirectoryLeafSubmission::Queued(outcome.payload.task.id))
                    }
                    CloudFileSubmission::Duplicate(_) => Ok(DirectoryLeafSubmission::Duplicate),
                }
            }
        },
    )
    .await
}

pub async fn retry_cloud_directory_group_item_with<F, Fut>(
    store: &TaskStore,
    group_id: uuid::Uuid,
    item_id: uuid::Uuid,
    configured: UriSubmissionOptions,
    mut submit: F,
) -> Result<TaskActionOutcome, QuarkError>
where
    F: FnMut(CloudProvider, String, UriSubmissionOptions, DirectoryLeafContext) -> Fut,
    Fut: Future<Output = Result<DirectoryLeafSubmission, QuarkError>>,
{
    let group = store
        .cloud_directory_group(group_id)
        .map_err(|_| QuarkError::Storage)?
        .ok_or_else(|| QuarkError::InvalidInput("网盘目录下载任务不存在".into()))?;
    let item = store
        .cloud_directory_group_items(group_id)
        .map_err(|_| QuarkError::Storage)?
        .into_iter()
        .find(|item| item.id == item_id)
        .ok_or_else(|| QuarkError::InvalidInput("网盘目录文件不存在".into()))?;
    if item.task_id.is_some() {
        return Err(QuarkError::InvalidInput(
            "网盘目录文件已经入队，不能重复提交".into(),
        ));
    }
    let output_directory =
        directory_download_output_directory(&group.output_directory, &item.relative_directory)?;
    let mut options = configured;
    options.directory = Some(output_directory);
    options.output_name = Some(item.output_name.clone());
    let context = DirectoryLeafContext {
        group_id: group.id,
        item_id: item.id,
    };
    match submit(
        group.provider,
        item.remote_file_id.clone(),
        options,
        context,
    )
    .await
    {
        Ok(DirectoryLeafSubmission::Queued(task_id)) => {
            store
                .link_cloud_directory_group_item_task(group.id, item.id, task_id)
                .map_err(|_| QuarkError::Storage)?;
            let mut outcome = task_action_outcome(
                store,
                store.find(task_id).map_err(|_| QuarkError::Storage)?,
                None,
            )
            .map_err(QuarkError::Transport)?;
            outcome.queue_task = Some(
                cloud_directory_group_queue_task(store, group.id).map_err(QuarkError::Transport)?,
            );
            Ok(outcome)
        }
        Ok(DirectoryLeafSubmission::Duplicate) => {
            store
                .fail_cloud_directory_group_item(
                    group.id,
                    item.id,
                    "duplicate",
                    "已存在下载任务，未重复入队",
                )
                .map_err(|_| QuarkError::Storage)?;
            Err(QuarkError::InvalidInput(
                "已存在下载任务，未重复入队".into(),
            ))
        }
        Err(error) => {
            let (code, message) = directory_download_failure_fields(&error);
            store
                .fail_cloud_directory_group_item(group.id, item.id, code, message)
                .map_err(|_| QuarkError::Storage)?;
            Err(error)
        }
    }
}

pub struct ResolvedCloudDownload {
    pub url: String,
    pub name: String,
    pub headers: Vec<(String, String)>,
}

impl ResolvedCloudDownload {
    pub fn into_uri_submission_options(self) -> UriSubmissionOptions {
        UriSubmissionOptions {
            output_name: Some(safe_filename(&self.name)),
            headers: self.headers,
            ..Default::default()
        }
    }
}

fn cookie_pair(value: &str) -> Option<(&str, &str)> {
    let pair = value.split(';').next()?.trim();
    let (name, _) = pair.split_once('=')?;
    (!name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        && !pair.contains(['\r', '\n']))
    .then_some((name, pair))
}

fn cookie_for_download(web_cookie: &str, response_cookies: &[String]) -> String {
    let mut cookies: Vec<(String, String)> = web_cookie
        .split(';')
        .filter_map(cookie_pair)
        .map(|(name, pair)| (name.to_owned(), pair.to_owned()))
        .collect();
    for response_cookie in response_cookies {
        if let Some((name, pair)) = cookie_pair(response_cookie) {
            cookies.retain(|(current, _)| current != name);
            cookies.push((name.to_owned(), pair.to_owned()));
        }
    }
    cookies
        .into_iter()
        .map(|(_, pair)| pair)
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn resolve_download(
    provider: CloudProvider,
    file_id: &str,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
) -> Result<ResolvedCloudDownload, QuarkError> {
    match provider {
        CloudProvider::Quark => {
            let cookie = cookie(store)?;
            let r = QuarkClient::new(cookie.clone(), http).download_url(file_id)?;
            let v = response_json(&r)?;
            Ok(ResolvedCloudDownload {
                url: parse_quark_download_url(&r.body)?,
                name: v
                    .pointer("/data/0/file_name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(file_id)
                    .into(),
                headers: vec![
                    (
                        "Cookie".into(),
                        cookie_for_download(&cookie, &r.set_cookies),
                    ),
                    ("Referer".into(), "https://pan.quark.cn/".into()),
                    ("User-Agent".into(), QUARK_DESKTOP_USER_AGENT.into()),
                ],
            })
        }
        CloudProvider::Pikpak => {
            let (url, name) = PikPakClient::new(http, store).download(file_id)?;
            Ok(ResolvedCloudDownload {
                url,
                name,
                headers: Vec::new(),
            })
        }
        CloudProvider::Baidu => {
            let download =
                baidu::BaiduClient::new(load_provider_session(CloudProvider::Baidu, store)?, http)
                    .download(file_id)?;
            Ok(ResolvedCloudDownload {
                url: download.url,
                name: download.name,
                headers: download.headers.into_iter().collect(),
            })
        }
        CloudProvider::Tianyi => {
            let download = tianyi::TianyiClient::new(
                load_provider_session(CloudProvider::Tianyi, store)?,
                http,
            )
            .download(file_id)?;
            Ok(ResolvedCloudDownload {
                url: download.url,
                name: download.name,
                headers: download.headers.into_iter().collect(),
            })
        }
        CloudProvider::Aliyun => {
            let (url, name) = aliyun::AliyunClient::new(store, http).download(file_id)?;
            Ok(ResolvedCloudDownload {
                url,
                name,
                headers: vec![("Referer".into(), "https://www.aliyundrive.com/".into())],
            })
        }
        CloudProvider::Xunlei => with_xunlei_client(store, http, |client| {
            let (url, name) = client.download(file_id)?;
            // Keep the authenticated browser's public identity on the CDN
            // request, but never forward bearer/captcha/cookies off the API.
            let session = client.session();
            let mut headers = Vec::new();
            if !session.profile.user_agent.is_empty() {
                headers.push(("User-Agent".into(), session.profile.user_agent.clone()));
            }
            if session.profile.is_modern_web() {
                headers.push(("Referer".into(), "https://pan.xunlei.com/".into()));
            }
            Ok(ResolvedCloudDownload { url, name, headers })
        }),
        CloudProvider::Uc => {
            let download =
                uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, http)
                    .download(file_id)?;
            Ok(ResolvedCloudDownload {
                url: download.url,
                name: download.name,
                headers: download.headers.into_iter().collect(),
            })
        }
    }
}
pub(crate) fn safe_filename(name: &str) -> String {
    let result: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let result = result.trim_end_matches(['.', ' ']);
    if result.is_empty() {
        "download".into()
    } else if is_windows_device_name(result) {
        format!("_{result}")
    } else {
        result.into()
    }
}

fn is_windows_device_name(name: &str) -> bool {
    // DOS device names stay reserved with extensions, trailing spaces, or the
    // superscript digits recognized by Windows (COM¹, LPT², etc.).
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || matches!(
            stem.strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT")),
            Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
        )
}

#[cfg(test)]
#[path = "commands_startup_tests.rs"]
mod startup_tests;

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        collections::{BTreeMap, VecDeque},
        sync::Mutex,
    };

    use super::{
        cloud_failure_code, create_cloud_task, password_webview_data_directory_from_root,
        prepare_baidu_account_login, recover_provider_password_session,
        requires_cloud_reauthorization_failure, resolve_download,
        retry_read_with_refreshed_web_session,
    };
    use crate::cloud::share::{CloudShareDownloadEntry, CloudShareDownloadSelection};
    use crate::cloud::{
        baidu::{parse_baidu_web_session, BaiduSession},
        browser_password_login_preferred, save_password_credential, save_password_login_draft,
        set_baidu_browser_profile_username, set_browser_password_login_preferred,
        tianyi::TianyiSession,
        uc::UcSession,
        xunlei::{XunleiClientProfile, XunleiSession, XunleiWebContext},
        CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudPasswordCredential,
        CloudProvider, CredentialStore, MemoryCredentialStore, QuarkError, SessionWorkKind,
        QUARK_DESKTOP_USER_AGENT,
    };
    use crate::TaskStore;

    #[test]
    fn only_authorization_failures_request_a_new_official_login() {
        assert!(requires_cloud_reauthorization_failure(Some(
            cloud_failure_code(&QuarkError::AuthorizationExpired,)
        )));
        assert!(requires_cloud_reauthorization_failure(Some(
            cloud_failure_code(&QuarkError::CaptchaRequired,)
        )));
        assert!(!requires_cloud_reauthorization_failure(Some(
            cloud_failure_code(&QuarkError::Transport("offline".into()),)
        )));
        assert!(!requires_cloud_reauthorization_failure(Some(
            cloud_failure_code(&QuarkError::Provider("temporary failure".into()),)
        )));
    }

    #[test]
    fn shared_directory_plan_does_not_repeat_the_promoted_root_in_its_file_tree() {
        let selection = CloudShareDownloadSelection {
            provider: CloudProvider::Quark,
            plan_source: "cloud-share://quark/plan".into(),
            directory_name: Some("YM-JFSMBYHS".into()),
            entries: vec![
                CloudShareDownloadEntry {
                    selected_id: "episode".into(),
                    source: crate::cloud::share::stable_share_source(
                        CloudProvider::Quark,
                        "share",
                        "episode",
                    ),
                    name: "episode.mp4".into(),
                    relative_directory: vec!["YM-JFSMBYHS".into()],
                    size: Some(100),
                },
                CloudShareDownloadEntry {
                    selected_id: "bonus".into(),
                    source: crate::cloud::share::stable_share_source(
                        CloudProvider::Quark,
                        "share",
                        "bonus",
                    ),
                    name: "bonus.mp4".into(),
                    relative_directory: vec!["YM-JFSMBYHS".into(), "Extras".into()],
                    size: Some(200),
                },
            ],
        };

        let plan = super::plan_cloud_share_directory_download(&selection).unwrap();

        assert_eq!(plan.files[0].relative_directory, vec!["YM-JFSMBYHS"]);
        assert_eq!(
            plan.files[1].relative_directory,
            vec!["YM-JFSMBYHS", "Extras"]
        );
    }

    #[test]
    fn newly_failed_share_download_returns_the_reauthorization_action_hint() {
        let store = TaskStore::open_in_memory().unwrap();
        let prepared = store
            .create(crate::NewTask::direct(
                "cloud-share://baidu/share-id/file-id",
            ))
            .unwrap();

        let outcome = super::cloud_share_failure_outcome(
            &store,
            prepared,
            Some("web_auth_required"),
            "网盘要求完成官方操作验证后重试",
        );

        assert_eq!(
            outcome.payload.task.failure_code.as_deref(),
            Some("web_auth_required")
        );
        assert!(outcome.payload.reauthorization_required);
    }

    #[test]
    fn recycle_directory_ids_route_to_provider_recycle_lists() {
        for (provider, recycle_bin_id) in [
            (CloudProvider::Quark, super::QUARK_RECYCLE_BIN_ID),
            (CloudProvider::Pikpak, super::PIKPAK_RECYCLE_BIN_ID),
            (
                CloudProvider::Baidu,
                crate::cloud::baidu::BAIDU_RECYCLE_BIN_ID,
            ),
            (
                CloudProvider::Tianyi,
                crate::cloud::tianyi::TIANYI_RECYCLE_BIN_ID,
            ),
            (
                CloudProvider::Aliyun,
                crate::cloud::aliyun::ALIYUN_RECYCLE_BIN_ID,
            ),
            (
                CloudProvider::Xunlei,
                crate::cloud::xunlei::XUNLEI_RECYCLE_BIN_ID,
            ),
            (CloudProvider::Uc, crate::cloud::uc::UC_RECYCLE_BIN_ID),
        ] {
            assert!(super::is_provider_recycle_bin(provider, recycle_bin_id));
        }
    }

    #[test]
    fn non_recycle_file_ids_are_rejected_by_permanent_delete() {
        let recycle_bin = crate::cloud::CloudDirectory {
            id: super::PIKPAK_RECYCLE_BIN_ID.into(),
            items: vec![crate::cloud::CloudFile {
                id: "recycle-record".into(),
                parent_id: super::PIKPAK_RECYCLE_BIN_ID.into(),
                name: "deleted.txt".into(),
                kind: "file".into(),
                size: None,
                modified_at: None,
            }],
        };

        assert!(super::validate_recycle_entry_ids(&["normal-file".into()], &recycle_bin).is_err());
    }

    #[test]
    fn tianyi_web_session_capture_uses_only_the_cloud_host_cookie_jar() {
        // Break caught: a same-named open.e.189.cn cookie overwrote the
        // cloud.189.cn cookie and made only share-download signing fail.
        assert_eq!(
            super::web_session_cookie_urls(CloudProvider::Tianyi),
            ["https://cloud.189.cn"]
        );
    }

    #[test]
    fn baidu_web_session_capture_uses_the_pan_api_cookie_jar() {
        // Break caught: a host-only Passport cookie with the same name must
        // not replace the Pan cookie that signs file-download requests.
        assert_eq!(
            super::web_session_cookie_urls(CloudProvider::Baidu),
            ["https://pan.baidu.com"]
        );
    }

    #[test]
    fn web_session_refresh_never_retries_a_provider_captcha_challenge() {
        // Break caught: retrying a provider risk challenge can create more
        // challenges. Only a rejected/expired local session is safe to renew
        // from the already-authorized official webview.
        assert!(super::can_retry_read_with_web_session(
            &QuarkError::AuthorizationExpired
        ));
        assert!(super::can_retry_read_with_web_session(
            &QuarkError::WebAuthorizationRequired
        ));
        assert!(!super::can_retry_read_with_web_session(
            &QuarkError::CaptchaRequired
        ));
    }

    #[test]
    fn share_resolution_errors_keep_actionable_types_without_echoing_provider_secrets() {
        let typed = [
            (QuarkError::NotAuthorized, "尚未授权此网盘，请登录后使用"),
            (
                QuarkError::AuthorizationExpired,
                "网盘会话已过期，请重新登录授权",
            ),
            (
                QuarkError::SessionRenewedRetryOperation,
                "登录已自动恢复，请重新执行本次操作",
            ),
            (
                QuarkError::WebAuthorizationRequired,
                "此登录方式需要网页验证，请点击网页授权完成登录",
            ),
            (
                QuarkError::CaptchaRequired,
                "网盘要求完成官方操作验证后重试",
            ),
            (
                QuarkError::AuthorizationPending,
                "正在等待网页登录，请在授权窗口完成登录",
            ),
            (
                QuarkError::PikpakRegionalMembershipRestricted,
                "区域会员在当前 IP 地区下载受限；请切换到有权限 IP 后重新登录重试，或升级全球会员",
            ),
        ];
        for (error, expected) in typed {
            assert_eq!(super::share_resolution_failure_message(&error), expected);
        }
        assert_eq!(
            QuarkError::CaptchaRequired.to_string(),
            "网盘要求完成官方操作验证后重试"
        );

        let secret = "https://secret.example/file?token=do-not-leak";
        for error in [
            QuarkError::Transport(secret.into()),
            QuarkError::Provider(secret.into()),
            QuarkError::InvalidInput(secret.into()),
        ] {
            let message = super::share_resolution_failure_message(&error);
            assert!(!message.contains(secret));
            assert!(!message.contains("token="));
        }
    }

    fn windows_device_name_variants() -> Vec<String> {
        [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
            "COM¹", "COM²", "COM³", "LPT¹", "LPT²", "LPT³",
        ]
        .into_iter()
        .flat_map(|name| {
            [
                name.to_owned(),
                name.to_ascii_lowercase(),
                format!("{name}.txt"),
                format!("{name}."),
                format!("{name} "),
                format!("{name} .txt. "),
            ]
        })
        .collect()
    }

    #[test]
    fn directory_output_rejects_all_device_names_before_any_filesystem_changes() {
        for name in windows_device_name_variants() {
            let root = tempfile::tempdir().expect("temporary selected download root");
            let result = super::directory_download_output_directory(
                root.path(),
                &["first".into(), "second".into(), name.clone()],
            );
            assert!(
                !root.path().join("first").exists(),
                "device name {name:?} must be rejected before creating ancestor directories"
            );
            assert!(
                matches!(result, Err(QuarkError::InvalidResponse)),
                "device name {name:?} must fail closed, got {result:?}"
            );
        }
    }

    #[test]
    fn directory_output_file_names_avoid_devices_in_confirmation_and_submission() {
        for name in windows_device_name_variants() {
            let expected = format!("_{}", name.trim_end_matches(['.', ' ']));
            let confirmed = super::safe_filename(&name);
            let submitted = super::ResolvedCloudDownload {
                url: "https://download.example/file".into(),
                name: name.clone(),
                headers: vec![],
            }
            .into_uri_submission_options();
            assert_eq!(confirmed, expected, "unsafe confirmation name for {name:?}");
            assert_eq!(submitted.output_name.as_deref(), Some(expected.as_str()));
        }
    }

    #[test]
    fn directory_output_rejects_ads_and_preserves_normal_unicode_names() {
        let root = tempfile::tempdir().expect("temporary selected download root");
        for name in ["notes:stream", "NUL:data", "folder::$INDEX_ALLOCATION"] {
            let result = super::directory_download_output_directory(
                root.path(),
                &["must-not-be-created".into(), name.into()],
            );
            assert!(
                matches!(result, Err(QuarkError::InvalidResponse)),
                "ADS component {name:?} must fail closed, got {result:?}"
            );
            assert!(!root.path().join("must-not-be-created").exists());
        }
        for name in [
            "中文笔记",
            "资料¹",
            "COM10",
            "LPT0",
            "CONSOLE",
            "辅助文件.txt",
        ] {
            let output = super::directory_download_output_directory(root.path(), &[name.into()])
                .expect("ordinary Unicode and device-like names remain valid");
            assert_eq!(output, root.path().join(name));
            assert_eq!(super::safe_filename(name), name);
        }
        assert_eq!(super::safe_filename("notes:stream"), "notes_stream");
    }

    #[test]
    fn directory_output_rejects_provider_path_components_before_creating_directories() {
        let root = tempfile::tempdir().expect("temporary selected download root");
        for component in [
            "C:",
            "C:outside",
            r"C:\outside",
            r"\\server\share",
            r"\outside",
            "/outside",
            ".",
            "..",
            "",
            "nested/file",
            r"nested\file",
            "line\nbreak",
            "control\u{7f}",
        ] {
            let result = super::directory_download_output_directory(
                root.path(),
                &["must-not-be-created".into(), component.into()],
            );
            assert!(
                matches!(result, Err(QuarkError::InvalidResponse)),
                "provider component {component:?} must be rejected, got {result:?}"
            );
            assert!(!root.path().join("must-not-be-created").exists());
        }
    }

    #[test]
    fn directory_output_uses_the_confirmed_names_inside_the_selected_root() {
        let root = tempfile::tempdir().expect("temporary selected download root");
        let output = super::directory_download_output_directory(
            root.path(),
            &["课程资料".into(), "2026?笔记. ".into()],
        )
        .expect("valid provider names should download below the selected root");
        assert_eq!(output, root.path().join("课程资料").join("2026_笔记"));
        assert!(output.is_dir());
        assert!(output.starts_with(root.path()));
    }

    #[test]
    fn cloud_authorization_window_repositions_each_loaded_page_to_the_top_right() {
        // All providers go through the same external WebView builder.  The
        // post-navigation hook must reset the document to its top-right login
        // area even when a provider performs an intermediate redirect.
        let source = include_str!("commands.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source precedes its tests");
        assert!(source.contains("AUTHORIZATION_TOP_RIGHT_VIEWPORT_JS"));
        assert!(source.contains(".on_page_load(move |window, payload|"));
        assert!(source.contains("PageLoadEvent::Finished"));
        assert!(source.contains("window.eval(AUTHORIZATION_TOP_RIGHT_VIEWPORT_JS)"));
        assert!(source.contains("window.scrollTo({ left: maxLeft, top: 0"));
    }

    #[test]
    fn cloud_authorization_window_allows_safe_oauth_popups() {
        // Social OAuth, including PikPak's Google sign-in, is launched with
        // window.open.  The shared authorization WebView must explicitly let
        // safe popup URLs reach WebView2 instead of silently handling them.
        let source = include_str!("commands.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source precedes its tests");
        assert!(source.contains(".on_new_window(|url, _features|"));
        assert!(source.contains("allows_cloud_authorization_popup(&url)"));
        assert!(source.contains("NewWindowResponse::Allow"));
        assert!(source.contains("NewWindowResponse::Deny"));

        assert!(super::allows_cloud_authorization_popup(
            &url::Url::parse("https://accounts.google.com/o/oauth2/v2/auth").unwrap()
        ));
        assert!(super::allows_cloud_authorization_popup(
            &url::Url::parse("about:blank").unwrap()
        ));
        assert!(!super::allows_cloud_authorization_popup(
            &url::Url::parse("file:///C:/private.txt").unwrap()
        ));
        assert!(!super::allows_cloud_authorization_popup(
            &url::Url::parse("javascript:alert(1)").unwrap()
        ));
    }

    #[test]
    fn baidu_authorization_auto_opens_login_without_breaking_changed_pages() {
        // Baidu currently exposes its login dialog behind a landing-page
        // button.  The WebView hook should attempt that exact button only,
        // then quietly give up when the provider changes its page.
        let source = include_str!("commands.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source precedes its tests");
        assert!(source.contains("BAIDU_AUTO_OPEN_LOGIN_JS"));
        assert!(source.contains("\"去登录\""));
        assert!(source.contains("element.click()"));
        assert!(source.contains("attempts >= 20"));
        assert!(source.contains("is_baidu_authorization_page(payload.url())"));
        assert!(source.contains("window.eval(BAIDU_AUTO_OPEN_LOGIN_JS)"));

        assert!(super::is_baidu_authorization_page(
            &url::Url::parse("https://pan.baidu.com/disk/main").unwrap()
        ));
        assert!(!super::is_baidu_authorization_page(
            &url::Url::parse("https://passport.baidu.com/v2/").unwrap()
        ));
        assert!(!super::is_baidu_authorization_page(
            &url::Url::parse("https://pan.quark.cn/").unwrap()
        ));
    }

    #[test]
    fn stable_cloud_source_decodes_the_file_id_without_accepting_url_parts() {
        assert_eq!(
            super::parse_stable_cloud_source("cloud://quark/file%2Fid%2Btest").unwrap(),
            (CloudProvider::Quark, "file/id+test".to_string())
        );
        for source in [
            "https://example.test/file",
            "cloud://quark/",
            "cloud://unknown/file",
            "cloud://quark/file?token=temporary",
        ] {
            assert!(
                super::parse_stable_cloud_source(source).is_err(),
                "{source}"
            );
        }
    }

    struct CapturingTransport(Mutex<Option<CloudHttpRequest>>);

    impl CloudHttpTransport for CapturingTransport {
        fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            *self.0.lock().unwrap() = Some(request);
            Ok(CloudHttpResponse {
                status: 200,
                body: r#"{"data":[{"download_url":"https://cdn.example.test/fixture","file_name":"fixture.jpg"}]}"#.into(),
                ..Default::default()
            })
        }
    }

    #[test]
    fn quark_download_uses_the_desktop_client_user_agent_for_api_and_aria2() {
        let credentials = MemoryCredentialStore::default();
        credentials.save("quark", b"session=fixture").unwrap();
        let transport = CapturingTransport(Mutex::new(None));

        let download =
            resolve_download(CloudProvider::Quark, "fixture", &credentials, &transport).unwrap();
        let api_request = transport.0.lock().unwrap().clone().unwrap();
        let aria2_user_agent = download
            .headers
            .iter()
            .find(|(name, _)| name == "User-Agent")
            .map(|(_, value)| value);

        assert!(api_request
            .headers
            .get("User-Agent")
            .is_some_and(|value| value.contains("quark-cloud-drive/")));
        assert!(aria2_user_agent.is_some_and(|value| value.contains("quark-cloud-drive/")));
    }

    struct DownloadCookieTransport;

    impl CloudHttpTransport for DownloadCookieTransport {
        fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            Ok(CloudHttpResponse {
                status: 200,
                body: r#"{"data":[{"download_url":"https://cdn.example.test/fixture","file_name":"fixture.jpg"}]}"#.into(),
                set_cookies: vec![
                    "__puus=fresh-download-session; Path=/; HttpOnly; Secure".into(),
                ],
            })
        }
    }

    #[test]
    fn quark_download_passes_the_response_session_cookie_to_aria2() {
        let credentials = MemoryCredentialStore::default();
        credentials
            .save("quark", b"sid=web-session; __puus=stale-session")
            .unwrap();

        let download = resolve_download(
            CloudProvider::Quark,
            "fixture",
            &credentials,
            &DownloadCookieTransport,
        )
        .unwrap();
        let cookie = download
            .headers
            .iter()
            .find(|(name, _)| name == "Cookie")
            .map(|(_, value)| value);

        assert_eq!(
            cookie.map(String::as_str),
            Some("sid=web-session; __puus=fresh-download-session")
        );
    }

    #[test]
    fn resolved_quark_download_keeps_fresh_cookie_when_building_aria2_options() {
        let credentials = MemoryCredentialStore::default();
        credentials
            .save("quark", b"sid=web-session; __puus=stale-session")
            .unwrap();

        let options = resolve_download(
            CloudProvider::Quark,
            "fixture",
            &credentials,
            &DownloadCookieTransport,
        )
        .unwrap()
        .into_uri_submission_options();

        assert_eq!(options.output_name.as_deref(), Some("fixture.jpg"));
        assert_eq!(
            options.headers,
            vec![
                (
                    "Cookie".into(),
                    "sid=web-session; __puus=fresh-download-session".into()
                ),
                ("Referer".into(), "https://pan.quark.cn/".into()),
                ("User-Agent".into(), QUARK_DESKTOP_USER_AGENT.into()),
            ]
        );
    }

    #[test]
    fn aliyun_download_passes_its_official_web_referer_to_aria2() {
        // The live signed-object endpoint rejects a request without this
        // origin context, regardless of whether it uses a Chrome or aria2 UA.
        let credentials = MemoryCredentialStore::default();
        credentials
            .save(
                "aliyun",
                br#"{"refresh_token":"r","access_token":"a","drive_id":"d","browser_access_ready":true}"#,
            )
            .unwrap();
        let transport = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"user_id":"u1","nick_name":"alice"}"#,
            r#"{"items":[]}"#,
            r#"{"name":"fixture.txt"}"#,
            r#"{"url":"https://cdn.example.test/fixture?signature=temporary"}"#,
        ])));

        let options = resolve_download(CloudProvider::Aliyun, "fixture", &credentials, &transport)
            .unwrap()
            .into_uri_submission_options();

        assert_eq!(
            options.headers,
            vec![("Referer".into(), "https://www.aliyundrive.com/".into())]
        );
    }

    #[test]
    fn cloud_task_persists_encoded_stable_source_not_signed_url() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = create_cloud_task(
            &store,
            CloudProvider::Quark,
            "folder/file?signature=temporary",
            "fixture.jpg",
        )
        .unwrap();

        assert_eq!(
            task.source,
            "cloud://quark/folder%2Ffile%3Fsignature%3Dtemporary"
        );
        assert_eq!(task.name, "fixture.jpg");
        assert_eq!(task.kind, "cloud");
    }

    struct SequenceTransport(Mutex<VecDeque<&'static str>>);

    fn modern_xunlei_fixture() -> XunleiSession {
        let mut session = XunleiSession::from_web_authorization(
            "fixture-access",
            "fixture-refresh",
            "fixture-user",
            "",
            XunleiClientProfile {
                client_id: "Xqp0kJBXWhwaTpB6".into(),
                client_secret: String::new(),
                device_id: "fixture-device".into(),
                client_version: "1.92.98".into(),
                package_name: "pan.xunlei.com".into(),
                user_agent: "Mozilla/5.0 fixture-browser".into(),
            },
            u64::MAX,
        )
        .unwrap();
        session.web_security.captcha_token = "fixture-captcha".into();
        session.web_security.expires_at = u64::MAX;
        session
    }

    #[test]
    fn xunlei_cdn_download_keeps_browser_identity_without_leaking_api_credentials() {
        let store = MemoryCredentialStore::default();
        super::save_provider_session(CloudProvider::Xunlei, &store, &modern_xunlei_fixture())
            .unwrap();
        let transport = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"id":"fixture-id","name":"fixture.txt","web_content_link":"https://cdn.example.test/fixture"}"#,
        ])));
        let download =
            resolve_download(CloudProvider::Xunlei, "fixture-id", &store, &transport).unwrap();
        assert!(download
            .headers
            .contains(&("User-Agent".into(), "Mozilla/5.0 fixture-browser".into())));
        assert!(download
            .headers
            .contains(&("Referer".into(), "https://pan.xunlei.com/".into())));
        assert!(!download.headers.iter().any(|(key, _)| [
            "authorization",
            "cookie",
            "x-captcha-token"
        ]
        .contains(&key.to_ascii_lowercase().as_str())));
    }

    #[test]
    fn xunlei_operation_failure_does_not_discard_an_already_rotated_refresh_token() {
        let store = MemoryCredentialStore::default();
        super::save_provider_session(CloudProvider::Xunlei, &store, &modern_xunlei_fixture())
            .unwrap();
        let transport = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"error_code":16,"error":"unauthenticated"}"#,
            r#"{"access_token":"rotated-access","refresh_token":"rotated-refresh","sub":"fixture-user","expires_in":3600}"#,
            r#"{"error_code":9,"error":"failed_precondition"}"#,
        ])));
        let result = super::with_xunlei_client(&store, &transport, |client| client.directory("0"));
        assert!(matches!(result, Err(QuarkError::Provider(_))));
        let saved: XunleiSession =
            super::load_provider_session(CloudProvider::Xunlei, &store).unwrap();
        assert_eq!(saved.refresh_token, "rotated-refresh");
    }

    #[test]
    fn xunlei_modern_restore_failure_preserves_an_already_rotated_refresh_token() {
        let store = MemoryCredentialStore::default();
        super::save_provider_session(CloudProvider::Xunlei, &store, &modern_xunlei_fixture())
            .unwrap();
        let transport = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"error_code":16,"error":"unauthenticated"}"#,
            r#"{"access_token":"rotated-access","refresh_token":"rotated-refresh","sub":"fixture-user","expires_in":3600}"#,
            r#"{"error_code":9,"error":"failed_precondition"}"#,
        ])));
        let result = super::restore_account(CloudProvider::Xunlei, &store, &transport);
        assert!(matches!(result, Err(QuarkError::Provider(_))));
        let saved: XunleiSession =
            super::load_provider_session(CloudProvider::Xunlei, &store).unwrap();
        assert_eq!(saved.refresh_token, "rotated-refresh");
        assert_eq!(saved.user_id, "fixture-user");
    }

    impl CloudHttpTransport for SequenceTransport {
        fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            Ok(CloudHttpResponse {
                status: 200,
                body: self
                    .0
                    .lock()
                    .unwrap()
                    .pop_front()
                    .ok_or(QuarkError::InvalidResponse)?
                    .into(),
                ..Default::default()
            })
        }
    }

    struct SessionRecoveryTransport(Mutex<VecDeque<CloudHttpResponse>>);

    impl SessionRecoveryTransport {
        fn baidu_password_relogin(account_id: &str) -> Self {
            const PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----\nMIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDXN3F7epcT2k2yjuw95Nd3iEDS\nz04mTwPUAOaZIQ+JWtawDWMh6+hpwnT8STs1fa5CZwb1L/nam4zqu/Y4zETCp/uL\nSl8/6jdNsjVtiyq+IlQ0IkmPHiSk022BfBw0RGrSMUGuPPvdjy0eJ5a5pzRYWfK7\nvJ0QdYEdTovg7hm70QIDAQAB\n-----END PUBLIC KEY-----";
            Self(Mutex::new(VecDeque::from([
                CloudHttpResponse {
                    status: 200,
                    body: String::new(),
                    set_cookies: vec!["BAIDUID=seed".into()],
                    ..Default::default()
                },
                CloudHttpResponse {
                    status: 200,
                    body: r#"{"errInfo":{"no":"0"},"data":{"token":"login-token"}}"#.into(),
                    ..Default::default()
                },
                CloudHttpResponse {
                    status: 200,
                    body: serde_json::json!({
                        "errno": "0",
                        "pubkey": PUBLIC_KEY,
                        "key": "rsa-key-id"
                    })
                    .to_string(),
                    ..Default::default()
                },
                CloudHttpResponse {
                    status: 200,
                    body: r#"{"errInfo":{"no":"0"},"data":{"codeString":""}}"#.into(),
                    ..Default::default()
                },
                CloudHttpResponse {
                    status: 200,
                    body: r#"{"errInfo":{"no":"0"},"data":{}}"#.into(),
                    set_cookies: vec!["BDUSS=renewed".into()],
                    ..Default::default()
                },
                CloudHttpResponse {
                    status: 200,
                    body: format!(
                        r#"{{"errno":0,"user":{{"uk":"{account_id}","netdisk_name":"fixture"}}}}"#
                    ),
                    ..Default::default()
                },
                CloudHttpResponse {
                    status: 200,
                    body: r#"{"errno":0,"list":[]}"#.into(),
                    ..Default::default()
                },
                CloudHttpResponse {
                    status: 200,
                    body: format!(
                        r#"{{"errno":0,"user":{{"uk":"{account_id}","netdisk_name":"fixture"}}}}"#
                    ),
                    ..Default::default()
                },
            ])))
        }

        fn remaining(&self) -> usize {
            self.0
                .lock()
                .map(|responses| responses.len())
                .unwrap_or_default()
        }
    }

    impl CloudHttpTransport for SessionRecoveryTransport {
        fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            self.0
                .lock()
                .map_err(|_| QuarkError::Storage)?
                .pop_front()
                .ok_or(QuarkError::InvalidResponse)
        }
    }

    #[test]
    fn baidu_password_webview_uses_one_stable_provider_profile() {
        let root = std::path::Path::new("C:/fixture/app-data");
        assert_eq!(
            password_webview_data_directory_from_root(root, CloudProvider::Baidu).unwrap(),
            root.join("web-auth").join("baidu")
        );
        assert_eq!(
            password_webview_data_directory_from_root(root, CloudProvider::Tianyi).unwrap(),
            root.join("web-auth").join("tianyi")
        );
        assert!(password_webview_data_directory_from_root(root, CloudProvider::Quark).is_err());
    }

    #[test]
    fn baidu_native_risk_rejection_marks_future_recovery_for_the_browser() {
        let store = MemoryCredentialStore::default();
        let credential =
            CloudPasswordCredential::new(CloudProvider::Baidu, "account-1", "user", "secret");
        let transport = SessionRecoveryTransport(Mutex::new(VecDeque::from([
            CloudHttpResponse {
                status: 200,
                body: String::new(),
                set_cookies: vec!["BAIDUID=seed".into()],
            },
            CloudHttpResponse {
                status: 200,
                body: r#"{"errInfo":{"no":"0"},"data":{"token":"login-token"}}"#.into(),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: serde_json::json!({
                    "errno": "0",
                    "pubkey": "-----BEGIN PUBLIC KEY-----\nMIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDXN3F7epcT2k2yjuw95Nd3iEDS\nz04mTwPUAOaZIQ+JWtawDWMh6+hpwnT8STs1fa5CZwb1L/nam4zqu/Y4zETCp/uL\nSl8/6jdNsjVtiyq+IlQ0IkmPHiSk022BfBw0RGrSMUGuPPvdjy0eJ5a5pzRYWfK7\nvJ0QdYEdTovg7hm70QIDAQAB\n-----END PUBLIC KEY-----",
                    "key": "rsa-key-id"
                }).to_string(),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: r#"{"errInfo":{"no":"0"},"data":{"codeString":""}}"#.into(),
                ..Default::default()
            },
            CloudHttpResponse {
                status: 200,
                body: r#"{"errInfo":{"no":"50052","msg":""},"data":{}}"#.into(),
                ..Default::default()
            },
        ])));

        assert!(matches!(
            recover_provider_password_session(
                CloudProvider::Baidu,
                &store,
                &transport,
                &credential,
            ),
            Err(QuarkError::WebAuthorizationRequired)
        ));
        assert!(browser_password_login_preferred(&store, CloudProvider::Baidu).unwrap());
    }

    #[test]
    fn baidu_browser_preference_skips_repeating_the_native_password_attempt() {
        let store = MemoryCredentialStore::default();
        set_browser_password_login_preferred(&store, CloudProvider::Baidu, true).unwrap();
        let credential =
            CloudPasswordCredential::new(CloudProvider::Baidu, "account-1", "user", "secret");
        let transport = SessionRecoveryTransport(Mutex::new(VecDeque::new()));

        assert!(matches!(
            recover_provider_password_session(
                CloudProvider::Baidu,
                &store,
                &transport,
                &credential,
            ),
            Err(QuarkError::WebAuthorizationRequired)
        ));
        assert_eq!(transport.remaining(), 0);
    }

    #[test]
    fn changing_the_only_baidu_username_requires_profile_replacement() {
        let store = MemoryCredentialStore::default();
        save_password_credential(
            &store,
            CloudPasswordCredential::new(CloudProvider::Baidu, "account-1", "old-user", "secret"),
        )
        .unwrap();
        set_baidu_browser_profile_username(&store, Some("old-user")).unwrap();
        set_browser_password_login_preferred(&store, CloudProvider::Baidu, true).unwrap();

        assert!(!prepare_baidu_account_login(&store, "old-user").unwrap());
        assert!(browser_password_login_preferred(&store, CloudProvider::Baidu).unwrap());

        assert!(prepare_baidu_account_login(&store, "new-user").unwrap());
        assert!(browser_password_login_preferred(&store, CloudProvider::Baidu).unwrap());
    }

    #[test]
    fn unbound_baidu_web_session_is_reset_before_accepting_a_password_username() {
        let store = MemoryCredentialStore::default();
        store
            .save(CloudProvider::Baidu.key(), b"old-web-session")
            .unwrap();

        assert!(prepare_baidu_account_login(&store, "new-user").unwrap());
    }

    #[test]
    fn changed_baidu_login_draft_resets_a_profile_without_a_validated_credential() {
        let store = MemoryCredentialStore::default();
        save_password_login_draft(&store, CloudProvider::Baidu, "old-user", "secret").unwrap();

        assert!(!prepare_baidu_account_login(&store, "old-user").unwrap());
        assert!(prepare_baidu_account_login(&store, "new-user").unwrap());
    }

    #[test]
    fn refreshed_web_session_retries_read_work_and_keeps_the_verified_snapshot() {
        // Break caught: a download-signing request can see an expired stored
        // cookie while the already-authorized official tab has a newer one.
        // The retry must use and retain that newer snapshot.
        let store = MemoryCredentialStore::default();
        store.save("uc", b"previous-session").unwrap();
        let calls = Cell::new(0);

        let result = retry_read_with_refreshed_web_session(
            CloudProvider::Uc,
            &store,
            b"browser-session",
            || {
                calls.set(calls.get() + 1);
                assert_eq!(
                    store.load("uc").unwrap().as_deref(),
                    Some(b"browser-session".as_slice())
                );
                Ok("signed-download")
            },
        );

        assert_eq!(result.unwrap(), "signed-download");
        assert_eq!(calls.get(), 1);
        assert_eq!(
            store.load("uc").unwrap().as_deref(),
            Some(b"browser-session".as_slice())
        );
    }

    #[test]
    fn rejected_refreshed_web_session_restores_the_previous_snapshot() {
        // Break caught: a stale or incomplete WebView cookie capture must not
        // overwrite a previously usable stored authorization.
        let store = MemoryCredentialStore::default();
        store.save("tianyi", b"previous-session").unwrap();

        let result: Result<(), QuarkError> = retry_read_with_refreshed_web_session(
            CloudProvider::Tianyi,
            &store,
            b"browser-session",
            || Err(QuarkError::AuthorizationExpired),
        );

        assert!(matches!(result, Err(QuarkError::AuthorizationExpired)));
        assert_eq!(
            store.load("tianyi").unwrap().as_deref(),
            Some(b"previous-session".as_slice())
        );
    }

    #[test]
    fn expired_baidu_read_uses_a_matching_saved_password_and_retries_once() {
        let store = MemoryCredentialStore::default();
        super::save_provider_session(
            CloudProvider::Baidu,
            &store,
            &BaiduSession::cookie("BDUSS=expired"),
        )
        .unwrap();
        save_password_credential(
            &store,
            CloudPasswordCredential::new(CloudProvider::Baidu, "account-1", "user", "secret"),
        )
        .unwrap();
        let transport = SessionRecoveryTransport::baidu_password_relogin("account-1");
        let calls = Cell::new(0);

        let result = super::run_cloud_work(
            CloudProvider::Baidu,
            SessionWorkKind::Read,
            &store,
            &transport,
            || {
                calls.set(calls.get() + 1);
                if calls.get() == 1 {
                    Err(QuarkError::AuthorizationExpired)
                } else {
                    Ok("listed after recovery")
                }
            },
        );

        assert_eq!(result.unwrap(), "listed after recovery");
        assert_eq!(calls.get(), 2);
        let saved: BaiduSession =
            super::load_provider_session(CloudProvider::Baidu, &store).unwrap();
        assert!(saved.cookie.contains("BDUSS=renewed"));
        assert_eq!(transport.remaining(), 0);
    }

    #[test]
    fn tianyi_recoverable_read_errors_always_try_native_password_login_first() {
        for error in [
            QuarkError::AuthorizationExpired,
            QuarkError::NotAuthorized,
            QuarkError::WebAuthorizationRequired,
            QuarkError::CaptchaRequired,
        ] {
            assert!(super::should_attempt_native_password_recovery(
                CloudProvider::Tianyi,
                SessionWorkKind::Read,
                &error,
            ));
        }

        assert!(!super::should_attempt_native_password_recovery(
            CloudProvider::Tianyi,
            SessionWorkKind::Mutation,
            &QuarkError::AuthorizationExpired,
        ));
    }

    #[test]
    fn expired_baidu_mutation_restores_the_session_without_replaying_the_request() {
        let store = MemoryCredentialStore::default();
        super::save_provider_session(
            CloudProvider::Baidu,
            &store,
            &BaiduSession::cookie("BDUSS=expired"),
        )
        .unwrap();
        save_password_credential(
            &store,
            CloudPasswordCredential::new(CloudProvider::Baidu, "account-1", "user", "secret"),
        )
        .unwrap();
        let transport = SessionRecoveryTransport::baidu_password_relogin("account-1");
        let calls = Cell::new(0);

        let result: Result<(), QuarkError> = super::run_cloud_work(
            CloudProvider::Baidu,
            SessionWorkKind::Mutation,
            &store,
            &transport,
            || {
                calls.set(calls.get() + 1);
                Err(QuarkError::AuthorizationExpired)
            },
        );

        assert!(matches!(
            result,
            Err(QuarkError::SessionRenewedRetryOperation)
        ));
        assert_eq!(calls.get(), 1);
        let saved: BaiduSession =
            super::load_provider_session(CloudProvider::Baidu, &store).unwrap();
        assert!(saved.cookie.contains("BDUSS=renewed"));
        assert_eq!(transport.remaining(), 0);
    }

    struct XunleiRootAcceptingTransport(Mutex<Vec<CloudHttpRequest>>);

    impl CloudHttpTransport for XunleiRootAcceptingTransport {
        fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            self.0.lock().unwrap().push(request);
            Ok(CloudHttpResponse {
                status: 200,
                body: r#"{"files":[],"next_page_token":""}"#.into(),
                ..Default::default()
            })
        }
    }

    struct TianyiRootAcceptingTransport(Mutex<u8>);

    impl CloudHttpTransport for TianyiRootAcceptingTransport {
        fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            *self.0.lock().unwrap() += 1;
            Ok(CloudHttpResponse {
                status: 200,
                body: r#"{"res_code":0,"fileListAO":{"fileList":[{"id":"root-fixture","name":"fixture","isFolder":false}]}}"#.into(),
                ..Default::default()
            })
        }
    }

    struct TianyiRootRejectingTransport;

    impl CloudHttpTransport for TianyiRootRejectingTransport {
        fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            Ok(CloudHttpResponse {
                status: 400,
                body: r#"{"success":false,"errorCode":"session_not_accepted"}"#.into(),
                ..Default::default()
            })
        }
    }

    struct TianyiMalformedRootTransport;

    impl CloudHttpTransport for TianyiMalformedRootTransport {
        fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            Ok(CloudHttpResponse {
                status: 200,
                body: r#"{"res_code":0,"fileListAO":{"fileList":null,"folderList":[]}}"#.into(),
                ..Default::default()
            })
        }
    }

    #[derive(Clone, Copy)]
    enum TianyiRootAuthorizationError {
        Expired,
        WebVerification,
        Captcha,
    }

    struct TianyiRootAuthorizationErrorTransport(TianyiRootAuthorizationError);

    impl CloudHttpTransport for TianyiRootAuthorizationErrorTransport {
        fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            Err(match self.0 {
                TianyiRootAuthorizationError::Expired => QuarkError::AuthorizationExpired,
                TianyiRootAuthorizationError::WebVerification => {
                    QuarkError::WebAuthorizationRequired
                }
                TianyiRootAuthorizationError::Captcha => QuarkError::CaptchaRequired,
            })
        }
    }

    #[test]
    fn baidu_web_authorization_keeps_the_window_open_until_the_root_directory_is_readable() {
        let credentials = MemoryCredentialStore::default();
        let previous = b"previous-verified-session";
        credentials.save("baidu", previous).unwrap();
        let transport = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"errno":0,"user":{"uk":"b1","netdisk_name":"baidu"}}"#,
            r#"{"errno":-9}"#,
        ])));

        let result = super::finalize_baidu_web_authorization(
            BaiduSession::cookie("BDUSS=browser-session"),
            &credentials,
            &transport,
        );

        // The UI loads `/` immediately after accepting this account.  Saving
        // after only the account probe creates a new tab that immediately
        // reports credentials_expired, despite the browser still being able
        // to complete its official verification flow.
        assert!(matches!(result, Err(QuarkError::AuthorizationPending)));
        assert_eq!(credentials.load("baidu").unwrap(), Some(previous.to_vec()));
    }

    #[test]
    fn browser_password_session_never_replaces_a_different_accounts_credentials() {
        let credentials = MemoryCredentialStore::default();
        credentials.save("baidu", b"previous-session").unwrap();
        let transport = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"errno":0,"user":{"uk":"other-account","netdisk_name":"baidu"}}"#,
            r#"{"errno":0,"list":[]}"#,
        ])));
        let result = super::validate_browser_password_session(
            CloudProvider::Baidu,
            &credentials,
            &transport,
            &["BDUSS=new-session".into()],
            None,
            Some("original-account"),
            |_| {},
        );
        assert!(matches!(result, Err(QuarkError::InvalidInput(_))));
        assert_eq!(
            credentials.load("baidu").unwrap().unwrap(),
            b"previous-session"
        );
    }

    #[test]
    fn browser_auth_diagnostic_contains_only_safe_status_fields() {
        let script = super::browser_auth_diagnostic_script(
            "账号信息校验",
            "网盘请求失败：天翼云盘拒绝请求（错误码 InvalidDeviceStatus）",
        );

        assert!(script.contains("__alldownloaderAuthDiagnostic"));
        assert!(script.contains("账号信息校验"));
        assert!(script.contains("InvalidDeviceStatus"));
        assert!(!script.contains("username"));
        assert!(!script.contains("password"));
        assert!(!script.contains("cookie"));
    }

    #[test]
    fn tianyi_authorization_window_accepts_only_official_read_and_api_urls() {
        for (method, url) in [
            (
                "GET",
                "https://cloud.189.cn/api/open/user/getUserInfoForPortal.action",
            ),
            (
                "POST",
                "https://cloud.189.cn/api/open/file/createFolder.action",
            ),
            (
                "GET",
                "https://cloud.189.cn/v2/deleteFile.action?fileIdList=1",
            ),
            ("GET", "https://cloud.189.cn/main.action"),
        ] {
            assert!(super::validate_tianyi_browser_request(method, url).is_ok());
        }
        for (method, url) in [
            ("DELETE", "https://cloud.189.cn/api/open/file/item"),
            ("GET", "http://cloud.189.cn/api/open/file/listFiles.action"),
            ("GET", "https://evil.example/api/open/file/listFiles.action"),
            (
                "GET",
                "https://cloud.189.cn.evil.example/api/open/file/listFiles.action",
            ),
            ("GET", "https://cloud.189.cn/private/account"),
        ] {
            assert!(super::validate_tianyi_browser_request(method, url).is_err());
        }
    }

    #[test]
    fn tianyi_authorization_window_strips_native_identity_and_cookie_headers() {
        let filtered = super::tianyi_browser_request_headers(BTreeMap::from([
            ("Accept".into(), "application/json".into()),
            (
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            ),
            (
                "Browser-Id".into(),
                "1234567890abcdef1234567890abcdef".into(),
            ),
            ("Cookie".into(), "secret=session".into()),
            ("User-Agent".into(), "Native".into()),
            ("Origin".into(), "https://cloud.189.cn".into()),
            ("Referer".into(), "https://cloud.189.cn/".into()),
        ]));

        assert_eq!(
            filtered.get("Accept").map(String::as_str),
            Some("application/json")
        );
        assert!(filtered.contains_key("Content-Type"));
        assert!(filtered.contains_key("Browser-Id"));
        assert!(!filtered.contains_key("Cookie"));
        assert!(!filtered.contains_key("User-Agent"));
        assert!(!filtered.contains_key("Origin"));
        assert!(!filtered.contains_key("Referer"));
        assert!(!filtered.values().any(|value| value.contains("secret")));
    }

    #[test]
    fn tianyi_session_diagnostic_exposes_names_but_never_cookie_values() {
        let mut session = TianyiSession::cookie(
            "COOKIE_LOGIN_USER=private-login; JSESSIONID=private-session; apm_sid=private-device",
        );
        session.auth_method = super::CloudAuthMethod::Password;
        session.apply_browser_context(&serde_json::json!({
            "browser_id":"1234567890abcdef1234567890abcdef",
            "user_agent":"OfficialBrowser/140"
        }));

        let shape = super::tianyi_session_safe_shape(&session);

        assert_eq!(shape["authMethod"], "password");
        assert_eq!(
            shape["cookieNames"],
            serde_json::json!(["COOKIE_LOGIN_USER", "JSESSIONID", "apm_sid"])
        );
        assert_eq!(shape["browserContext"], true);
        assert_eq!(shape["transport"], "web");
        let serialized = shape.to_string();
        assert!(!serialized.contains("private-login"));
        assert!(!serialized.contains("private-session"));
        assert!(!serialized.contains("private-device"));
    }

    #[test]
    fn tianyi_session_diagnostic_identifies_native_app_without_exposing_tokens() {
        let session: TianyiSession = serde_json::from_value(serde_json::json!({
            "cookie": "",
            "authMethod": "password",
            "app": {
                "sessionKey": "private-session-key",
                "sessionSecret": "private-session-secret",
                "accessToken": "private-access-token",
                "loginName": "private-account"
            }
        }))
        .unwrap();

        let shape = super::tianyi_session_safe_shape(&session);

        assert_eq!(shape["transport"], "nativeApp");
        assert_eq!(shape["cookieNames"], serde_json::json!([]));
        let serialized = shape.to_string();
        assert!(!serialized.contains("private-session"));
        assert!(!serialized.contains("private-access"));
        assert!(!serialized.contains("private-account"));
    }

    #[test]
    fn browser_password_session_preserves_previous_credentials_when_directory_rejects_login() {
        let credentials = MemoryCredentialStore::default();
        credentials.save("baidu", b"previous-session").unwrap();
        let transport = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"errno":0,"user":{"uk":"b1","netdisk_name":"baidu"}}"#,
            r#"{"errno":-9}"#,
        ])));
        assert!(super::validate_browser_password_session(
            CloudProvider::Baidu,
            &credentials,
            &transport,
            &["BDUSS=new-session".into()],
            None,
            Some("b1"),
            |_| {},
        )
        .is_err());
        assert_eq!(
            credentials.load("baidu").unwrap().unwrap(),
            b"previous-session"
        );
    }

    #[test]
    fn browser_password_session_persists_real_tianyi_account_after_validation() {
        let credentials = MemoryCredentialStore::default();
        let transport = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"res_code":0,"userInfo":{"userId":"t1","nickName":"tianyi"}}"#,
            r#"{"res_code":0,"fileListAO":{"fileList":[]}}"#,
        ])));
        let account = super::validate_browser_password_session(
            CloudProvider::Tianyi,
            &credentials,
            &transport,
            &["SSON=new-session".into()],
            Some(&serde_json::json!({"browser_id":"1234567890abcdef1234567890abcdef", "user_agent":"OfficialBrowser/140"})),
            Some("t1"),
            |_| {},
        )
        .unwrap();
        assert_eq!(account.id, "t1");
        let session: TianyiSession =
            super::load_provider_session(CloudProvider::Tianyi, &credentials).unwrap();
        assert_eq!(session.auth_method, super::CloudAuthMethod::Password);
        assert!(session.cookie.contains("SSON=new-session"));
        assert_eq!(session.browser_id, "1234567890abcdef1234567890abcdef");
        assert_eq!(session.user_agent, "OfficialBrowser/140");
    }

    #[test]
    fn browser_password_session_waits_for_tianyi_browser_identity_before_native_validation() {
        let credentials = MemoryCredentialStore::default();
        let transport = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"res_code":0,"loginName":"alice@example.org"}"#,
        ])));
        let mut stage = "";
        let result = super::validate_browser_password_session(
            CloudProvider::Tianyi,
            &credentials,
            &transport,
            &["SSON=new-session".into()],
            None,
            None,
            |current| stage = current,
        );

        assert!(matches!(result, Err(QuarkError::AuthorizationPending)));
        assert_eq!(stage, "等待官网浏览器标识");
    }

    #[test]
    fn browser_password_session_accepts_tianyi_files_and_profile_containing_challenge_words() {
        for (profile, directory) in [
            (
                r#"{"res_code":0,"loginName":"sms@example.org","nickName":"risk analyst"}"#,
                r#"{"res_code":0,"fileListAO":{"fileList":[]}}"#,
            ),
            (
                r#"{"res_code":0,"loginName":"alice@example.org"}"#,
                r#"{"res_code":0,"fileListAO":{"fileList":[{"id":"f1","name":"短信备份-verify.txt","size":10}]}}"#,
            ),
        ] {
            let credentials = MemoryCredentialStore::default();
            let transport = SequenceTransport(Mutex::new(VecDeque::from([profile, directory])));
            let account = super::validate_browser_password_session(
                CloudProvider::Tianyi,
                &credentials,
                &transport,
                &["SSON=new-session".into()],
                Some(&serde_json::json!({"browser_id":"1234567890abcdef1234567890abcdef", "user_agent":"OfficialBrowser/140"})),
                None,
                |_| {},
            )
            .expect("valid profile and filenames must finish authorization");
            assert!(!account.id.is_empty());
            assert!(credentials.load("tianyi").unwrap().is_some());
        }
    }

    #[test]
    fn browser_password_session_reports_tianyi_rejection_without_replacing_credentials() {
        for (responses, expected_stage) in [
            (vec![r#"{"res_code":0}"#], "账号信息校验"),
            (
                vec![
                    r#"{"res_code":0,"loginName":"alice@example.org"}"#,
                    r#"{"res_code":-1,"msg":"需要短信验证码"}"#,
                ],
                "目录访问校验",
            ),
        ] {
            let credentials = MemoryCredentialStore::default();
            credentials.save("tianyi", b"previous-session").unwrap();
            let transport = SequenceTransport(Mutex::new(VecDeque::from(responses)));
            let mut stage = "";
            assert!(super::validate_browser_password_session(
                CloudProvider::Tianyi,
                &credentials,
                &transport,
                &["SSON=new-session".into()],
                Some(&serde_json::json!({"browser_id":"1234567890abcdef1234567890abcdef", "user_agent":"OfficialBrowser/140"})),
                None,
                |current| stage = current,
            )
            .is_err());
            assert_eq!(stage, expected_stage);
            assert_eq!(
                credentials.load("tianyi").unwrap().unwrap(),
                b"previous-session"
            );
        }
    }

    #[test]
    fn baidu_web_authorization_persists_a_browser_session_that_restores_the_root_directory() {
        let credentials = MemoryCredentialStore::default();
        let transport = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"errno":0,"user":{"uk":"b1","netdisk_name":"baidu"}}"#,
            r#"{"errno":0,"list":[]}"#,
            r#"{"errno":0,"user":{"uk":"b1","netdisk_name":"baidu"}}"#,
            r#"{"errno":0,"list":[]}"#,
        ])));
        let session = parse_baidu_web_session(&[
            "BDUSS_BFESS=browser-session".into(),
            "PANPSC=browser-context".into(),
        ])
        .expect("the official WebView should provide its authenticated cookie pairs");

        let account = super::finalize_baidu_web_authorization(session, &credentials, &transport)
            .expect("a browser session with a readable root should authorize");
        let restored = super::restore_account(CloudProvider::Baidu, &credentials, &transport)
            .expect("the saved browser session should restore after reopening the tab");
        let root = super::list_cloud_directory(CloudProvider::Baidu, &credentials, &transport, "/")
            .expect("the restored tab should load its root directory");

        assert_eq!(account.id, "b1");
        assert_eq!(restored.id, "b1");
        assert!(root.items.iter().any(|item| {
            item.id == crate::cloud::baidu::BAIDU_RECYCLE_BIN_ID
                && item.parent_id == "/"
                && item.name == "回收站"
                && item.kind == "folder"
        }));
    }

    #[test]
    fn tianyi_web_authorization_persists_a_session_after_root_directory_validates() {
        let credentials = MemoryCredentialStore::default();
        let transport = TianyiRootAcceptingTransport(Mutex::new(0));

        let account = super::finalize_tianyi_web_authorization(
            TianyiSession::cookie("SSON=official-browser-session"),
            &credentials,
            &transport,
        )
        .expect("a browser session with a readable root directory should authorize");

        assert_eq!(account.id, "tianyi-web");
        assert_eq!(account.provider, CloudProvider::Tianyi);
        assert_eq!(*transport.0.lock().unwrap(), 1);
        assert!(credentials.load("tianyi").unwrap().is_some());
    }

    #[test]
    fn tianyi_web_authorization_does_not_persist_a_session_when_root_is_unreadable() {
        let credentials = MemoryCredentialStore::default();

        let result = super::finalize_tianyi_web_authorization(
            TianyiSession::cookie("SSON=browser-session"),
            &credentials,
            &TianyiRootRejectingTransport,
        );

        assert!(matches!(result, Err(QuarkError::Provider(_))));
        assert!(credentials.load("tianyi").unwrap().is_none());
    }

    #[test]
    fn xunlei_web_authorization_uses_bearer_when_the_cookie_context_has_no_session_id() {
        // Break caught: rejecting a completed official browser login before
        // the documented `sessionid || access_token` fallback can run leaves
        // the authorization window polling forever.
        let credentials = MemoryCredentialStore::default();
        let session = XunleiSession::from_web_authorization(
            "browser-access",
            "browser-refresh",
            "browser-user",
            "",
            XunleiClientProfile::default(),
            0,
        )
        .unwrap();
        let context = XunleiWebContext {
            session_id: String::new(),
            peer_id: "official-peer".into(),
            device_id: String::new(),
            credit_key: "official-credit".into(),
        };

        let root = XunleiRootAcceptingTransport(Mutex::new(Vec::new()));
        let account =
            super::finalize_xunlei_web_authorization(session, context, &credentials, &root)
                .expect("a complete browser context can use its current bearer as session id");

        assert_eq!(account.id, "browser-user");
        let saved: XunleiSession = serde_json::from_slice(
            &credentials
                .load("xunlei")
                .unwrap()
                .expect("session is saved"),
        )
        .unwrap();
        assert_eq!(saved.web_context.session_id, "browser-access");
    }

    #[test]
    fn xunlei_web_authorization_accepts_a_current_bearer_without_a_refresh_token() {
        // Break caught: treating an optional native refresh credential as a
        // prerequisite for the already-current official browser bearer keeps
        // a completed login stuck in its authorization window.
        let credentials = MemoryCredentialStore::default();
        let session = super::parse_xunlei_web_session(serde_json::json!({
            "access_token": "browser-access",
            "user_id": "browser-user",
        }))
        .expect("the current browser bearer needs no native refresh token");
        let context = XunleiWebContext {
            session_id: String::new(),
            peer_id: "official-peer".into(),
            device_id: String::new(),
            credit_key: "official-credit".into(),
        };

        let root = XunleiRootAcceptingTransport(Mutex::new(Vec::new()));
        let account =
            super::finalize_xunlei_web_authorization(session, context, &credentials, &root)
                .expect("the current bearer completes authorization until it expires");

        assert_eq!(account.id, "browser-user");
        let saved: XunleiSession = serde_json::from_slice(
            &credentials
                .load("xunlei")
                .unwrap()
                .expect("session is saved"),
        )
        .unwrap();
        assert!(saved.refresh_token.is_empty());
        assert_eq!(saved.web_context.session_id, "browser-access");
    }

    #[test]
    fn xunlei_web_authorization_persists_only_after_the_root_directory_is_readable() {
        // Break caught: accepting extracted browser state without probing the
        // exact root request the workspace needs closes the official window
        // and creates a tab backed by an unusable session.
        let credentials = MemoryCredentialStore::default();
        let transport = XunleiRootAcceptingTransport(Mutex::new(Vec::new()));
        let session = XunleiSession::from_web_authorization(
            "browser-access",
            "",
            "browser-user",
            "",
            XunleiClientProfile::default(),
            0,
        )
        .unwrap();
        let context = XunleiWebContext {
            session_id: String::new(),
            peer_id: "official-peer".into(),
            device_id: String::new(),
            credit_key: "official-credit".into(),
        };

        let account =
            super::finalize_xunlei_web_authorization(session, context, &credentials, &transport)
                .expect("a browser session with a readable root directory should authorize");

        assert_eq!(account.id, "browser-user");
        let requests = transport.0.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].headers["Authorization"],
            "Bearer browser-access"
        );
        assert_eq!(requests[0].headers["Peer-Id"], "official-peer");
        assert!(requests[0].url.contains("_sessid=browser-access"));
        assert!(credentials.load("xunlei").unwrap().is_some());
    }

    #[test]
    fn xunlei_web_authorization_keeps_the_window_open_when_root_is_unreadable() {
        // Break caught: a displayable account is not an authorized workspace.
        // The candidate must not replace stored credentials until the browser
        // bearer can read the root directory.
        let credentials = MemoryCredentialStore::default();
        let previous = b"previous-verified-xunlei-session".to_vec();
        credentials.save("xunlei", &previous).unwrap();
        let session = XunleiSession::from_web_authorization(
            "browser-access",
            "",
            "browser-user",
            "",
            XunleiClientProfile::default(),
            0,
        )
        .unwrap();
        let context = XunleiWebContext {
            session_id: String::new(),
            peer_id: "official-peer".into(),
            device_id: String::new(),
            credit_key: "official-credit".into(),
        };
        let rejecting = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"error_code":4121,"error":"expired"}"#,
        ])));

        let error =
            super::finalize_xunlei_web_authorization(session, context, &credentials, &rejecting)
                .unwrap_err();

        assert!(matches!(error, QuarkError::AuthorizationPending));
        assert_eq!(credentials.load("xunlei").unwrap(), Some(previous));
    }

    #[test]
    fn xunlei_modern_authorization_reports_final_root_rejection_instead_of_polling_forever() {
        // Once the SDK has returned an account, exhausted authentication or
        // captcha retries are a visible failure, not another "not logged in" poll.
        for rejection in [
            r#"{"error_code":16,"error":"unauthenticated","error_description":"private-response"}"#,
            r#"{"error_code":4001,"error":"captcha_required","error_description":"private-response"}"#,
            r#"{"error_code":4002,"error":"captcha_invalid","error_description":"private-response"}"#,
        ] {
            let credentials = MemoryCredentialStore::default();
            let previous = b"previous-verified-xunlei-session".to_vec();
            credentials.save("xunlei", &previous).unwrap();
            let mut session = modern_xunlei_fixture();
            session.refresh_token.clear();
            let rejecting = SequenceTransport(Mutex::new(VecDeque::from([rejection, rejection])));
            let error = super::finalize_xunlei_web_authorization(
                session,
                XunleiWebContext::default(),
                &credentials,
                &rejecting,
            )
            .unwrap_err();
            assert!(matches!(&error, QuarkError::Provider(message) if !message.is_empty()));
            assert!(!error.to_string().contains("private-response"));
            assert_eq!(credentials.load("xunlei").unwrap(), Some(previous));
        }
    }

    #[test]
    fn xunlei_modern_authorization_saves_the_account_after_a_successful_root_probe() {
        let credentials = MemoryCredentialStore::default();
        let transport = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"files":[],"next_page_token":""}"#,
        ])));
        let account = super::finalize_xunlei_web_authorization(
            modern_xunlei_fixture(),
            XunleiWebContext::default(),
            &credentials,
            &transport,
        )
        .unwrap();
        assert_eq!(account.id, "fixture-user");
        let saved: XunleiSession =
            super::load_provider_session(CloudProvider::Xunlei, &credentials).unwrap();
        assert_eq!(saved.user_id, account.id);
    }

    #[test]
    fn xunlei_web_authorization_uses_the_selected_context_from_official_web_storage() {
        // Break caught: Xunlei's current browser login keeps the credentials
        // and drive challenge context in separate Web Storage records. If the
        // native bridge reads only cookies, a completed login remains pending
        // and the authorization window never closes.
        let (session, context) = super::parse_xunlei_web_authorization(serde_json::json!({
            "access_token": "browser-access",
            "user_id": "browser-user",
            "peer_id": "official-peer",
            "device_id": "official-device",
            "credit_key": "official-credit"
        }))
        .expect(
            "the selected fields emitted from official Web Storage form a usable browser session",
        );

        assert_eq!(session.user_id, "browser-user");
        assert_eq!(context.peer_id, "official-peer");
        assert_eq!(context.device_id, "official-device");
        assert_eq!(context.credit_key, "official-credit");
    }

    #[test]
    fn xunlei_web_authorization_discards_a_native_client_secret_from_web_storage() {
        // Break caught: the official web authorization bridge must never
        // transfer a native client secret out of Web Storage or persist it.
        let session = super::parse_xunlei_web_session(serde_json::json!({
            "access_token": "browser-access",
            "user_id": "browser-user",
            "client_secret": "must-not-cross-webview-boundary"
        }))
        .expect("the browser bearer itself remains a valid candidate");

        assert!(session.profile.client_secret.is_empty());
    }

    #[test]
    fn xunlei_restore_rejects_a_saved_browser_session_when_its_root_is_expired() {
        // Break caught: restoring the display account without the root probe
        // recreates an empty Xunlei tab after the saved browser bearer has
        // expired.
        let credentials = MemoryCredentialStore::default();
        let mut session = XunleiSession::from_web_authorization(
            "browser-access",
            "",
            "browser-user",
            "",
            XunleiClientProfile::default(),
            0,
        )
        .unwrap();
        session.web_context = XunleiWebContext {
            session_id: String::new(),
            peer_id: "official-peer".into(),
            device_id: String::new(),
            credit_key: "official-credit".into(),
        };
        credentials
            .save("xunlei", &serde_json::to_vec(&session).unwrap())
            .unwrap();
        let expired_root = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"error_code":4121,"error":"expired"}"#,
        ])));

        let result = super::restore_account(CloudProvider::Xunlei, &credentials, &expired_root);

        assert!(matches!(result, Err(QuarkError::AuthorizationExpired)));
    }

    #[test]
    fn uc_web_authorization_keeps_unverified_browser_cookies_out_of_storage() {
        // Break caught: saving immediately after merely seeing __pus/__puus
        // creates an unusable tab when UC requires web verification for its
        // first real directory request.
        let credentials = MemoryCredentialStore::default();
        let transport = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"code":0,"status":200,"message":"verify in official browser"}"#,
        ])));

        let error = super::finalize_uc_web_authorization(
            UcSession::cookie("__uid=browser-user; __pus=identity; __puus=session"),
            &credentials,
            &transport,
        )
        .unwrap_err();

        assert!(matches!(error, QuarkError::AuthorizationPending));
        assert!(credentials.load("uc").unwrap().is_none());
    }

    #[test]
    fn uc_web_authorization_does_not_replace_a_previous_session_when_root_verification_fails() {
        // Break caught: persisting a candidate browser cookie before the first
        // root read succeeds destroys a previously working UC authorization.
        let credentials = MemoryCredentialStore::default();
        credentials
            .save("uc", b"previous-verified-session")
            .unwrap();
        let transport = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"code":0,"status":200,"message":"risk verification required"}"#,
        ])));

        let error = super::finalize_uc_web_authorization(
            UcSession::cookie("__uid=new-user; __pus=new-identity; __puus=new-session"),
            &credentials,
            &transport,
        )
        .unwrap_err();

        assert!(matches!(error, QuarkError::AuthorizationPending));
        assert_eq!(
            credentials.load("uc").unwrap().as_deref(),
            Some(b"previous-verified-session".as_slice())
        );
    }

    #[test]
    fn uc_web_authorization_saves_a_candidate_only_after_root_verification_succeeds() {
        // Break caught: never persisting a verified candidate leaves a
        // successful official login unable to restore its UC workspace.
        let credentials = MemoryCredentialStore::default();
        credentials
            .save("uc", b"previous-verified-session")
            .unwrap();
        let transport = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"code":0,"status":200,"data":{"list":[]},"metadata":{"_total":0}}"#,
        ])));

        let account = super::finalize_uc_web_authorization(
            UcSession::cookie("__uid=new-user; __pus=new-identity; __puus=new-session"),
            &credentials,
            &transport,
        )
        .expect("a verified root response completes authorization");

        assert_eq!(account.id, "new-user");
        let saved: UcSession =
            serde_json::from_slice(&credentials.load("uc").unwrap().unwrap()).unwrap();
        assert_eq!(
            saved.cookie,
            "__uid=new-user; __pus=new-identity; __puus=new-session"
        );
    }

    #[test]
    fn tianyi_web_authorization_maps_root_authorization_errors_to_pending() {
        for error in [
            TianyiRootAuthorizationError::Expired,
            TianyiRootAuthorizationError::WebVerification,
            TianyiRootAuthorizationError::Captcha,
        ] {
            let credentials = MemoryCredentialStore::default();
            let result = super::finalize_tianyi_web_authorization(
                TianyiSession::cookie("SSON=candidate-session"),
                &credentials,
                &TianyiRootAuthorizationErrorTransport(error),
            );

            assert!(matches!(result, Err(QuarkError::AuthorizationPending)));
            assert!(credentials.load("tianyi").unwrap().is_none());
        }
    }

    #[test]
    fn tianyi_web_authorization_keeps_the_old_session_when_candidate_root_is_unreadable() {
        let credentials = MemoryCredentialStore::default();
        let existing = serde_json::to_vec(&TianyiSession::cookie("SSON=existing-session"))
            .expect("the test session serializes");
        credentials.save("tianyi", &existing).unwrap();

        let result = super::finalize_tianyi_web_authorization(
            TianyiSession::cookie("SSON=candidate-session"),
            &credentials,
            &TianyiRootRejectingTransport,
        );

        assert!(matches!(result, Err(QuarkError::Provider(_))));
        assert_eq!(credentials.load("tianyi").unwrap(), Some(existing));
    }

    #[test]
    fn tianyi_web_authorization_keeps_the_old_session_when_candidate_root_shape_is_malformed() {
        let credentials = MemoryCredentialStore::default();
        let existing = serde_json::to_vec(&TianyiSession::cookie("SSON=existing-session"))
            .expect("the test session serializes");
        credentials.save("tianyi", &existing).unwrap();

        let result = super::finalize_tianyi_web_authorization(
            TianyiSession::cookie("SSON=candidate-session"),
            &credentials,
            &TianyiMalformedRootTransport,
        );

        assert!(matches!(result, Err(QuarkError::InvalidResponse)));
        assert_eq!(credentials.load("tianyi").unwrap(), Some(existing));
    }

    #[test]
    fn xunlei_browser_session_does_not_require_a_native_client_secret() {
        let session = super::parse_xunlei_web_session(serde_json::json!({
            "access_token": "browser-access",
            "refresh_token": "browser-refresh",
            "user_id": "browser-user",
            "client_id": "official-web-client",
            "device_id": "web-device",
            "client_version": "1.0.0",
            "package_name": "pan.xunlei.com",
            "user_agent": "Mozilla/5.0",
        }))
        .unwrap();

        assert!(session.profile.client_secret.is_empty());
        assert_eq!(session.user_id, "browser-user");
    }

    #[test]
    fn xunlei_web_authorization_accepts_the_official_browser_user_state_without_native_identity() {
        // The current official web client keeps the freshly issued tokens in
        // Nuxt's users.curUser state.  It does not publish a native client
        // profile, so requiring that profile traps a completed web login in
        // its authorization window forever.
        let session = super::parse_xunlei_web_session(serde_json::json!({
            "access_token": "browser-access",
            "refresh_token": "browser-refresh",
            "user_id": "browser-user"
        }))
        .expect("official browser state needs no fabricated native profile");

        assert!(session.profile.client_id.is_empty());
        assert_eq!(session.user_id, "browser-user");
    }

    #[test]
    fn xunlei_web_capture_preserves_modern_security_context_for_root_validation() {
        // Dropping the bridge's official signature makes the first Rust root
        // request unable to initialize its operation captcha.
        let session = super::parse_xunlei_web_session(serde_json::json!({
            "access_token": "browser-access", "user_id": "browser-user",
            "client_id": "Xqp0kJBXWhwaTpB6", "package_name": "pan.xunlei.com",
            "web_security": {
                "captcha_sign": "official-signature", "timestamp": "1789036800000",
                "action": "GET:/drive/v1/files", "client_version": "1.92.98",
                "package_name": "pan.xunlei.com"
            }
        }))
        .unwrap();
        assert!(session.profile.is_modern_web());
        assert_eq!(session.web_security.captcha_sign, "official-signature");
        assert_eq!(session.web_security.timestamp, "1789036800000");
        assert_eq!(session.web_security.action, "GET:/drive/v1/files");
        assert!(session.web_security.captcha_token.is_empty());
        assert!(session.web_context.credit_key.is_empty());
    }

    #[test]
    fn xunlei_web_capture_rejects_malformed_security_before_saving() {
        let result = super::parse_xunlei_web_session(serde_json::json!({
            "access_token": "browser-access", "user_id": "browser-user",
            "web_security": {"captcha_sign": {"unexpected": "object"}}
        }));
        assert!(matches!(result, Err(QuarkError::AuthorizationPending)));
    }

    #[test]
    fn aliyun_browser_token_keeps_the_current_bearer_and_default_drive() {
        let session = super::parse_aliyun_web_session(serde_json::json!({
            "access_token": "browser-access",
            "refresh_token": "browser-refresh",
            "default_drive_id": "browser-drive",
        }))
        .unwrap();

        assert_eq!(session.access_token, "browser-access");
        assert_eq!(session.refresh_token, "browser-refresh");
        assert_eq!(session.drive_id, "browser-drive");
    }
}

#[tauri::command]
pub async fn cloud_upload_files(
    app: AppHandle,
    provider: CloudProvider,
    parent_id: String,
    paths: Option<Vec<String>>,
) -> Result<Vec<CloudFile>, QuarkError> {
    let picker = app.clone();
    let provider_label = match provider {
        CloudProvider::Quark => "夸克",
        CloudProvider::Pikpak => "PikPak",
        CloudProvider::Baidu => "百度网盘",
        CloudProvider::Tianyi => "天翼云盘",
        CloudProvider::Aliyun => "阿里云盘",
        CloudProvider::Xunlei => "迅雷云盘",
        CloudProvider::Uc => "UC网盘",
    };
    // The native picker may stay open indefinitely; it must hold no session lock.
    let paths =
        tauri::async_runtime::spawn_blocking(move || -> Result<Vec<PathBuf>, QuarkError> {
            let paths = match paths {
                Some(paths) => paths.into_iter().map(PathBuf::from).collect(),
                None => picker
                    .dialog()
                    .file()
                    .set_title(format!("选择上传到 {provider_label} 的文件"))
                    .blocking_pick_files()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|p| {
                        p.into_path()
                            .map_err(|_| QuarkError::InvalidInput("无法访问选中的文件".into()))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            };
            Ok(paths)
        })
        .await
        .map_err(|_| QuarkError::InvalidInput("文件选择中断".into()))??;
    tauri::async_runtime::spawn_blocking(move || {
        let s = app.state::<Arc<CloudState>>();
        let h = ReqwestTransport::new()?;
        let mut files = Vec::new();
        for path in paths {
            let file = match provider {
                CloudProvider::Quark => s.with_provider(CloudProvider::Quark, |store| {
                    run_cloud_work(
                        CloudProvider::Quark,
                        SessionWorkKind::Mutation,
                        store,
                        &h,
                        || QuarkClient::new(cookie(store)?, &h).upload_file(&parent_id, &path, &h),
                    )
                })?,
                // PikPak uploads hold provider state only around session refresh
                // and initialization; its OSS byte transfer remains outside that lock.
                // Do not hold that lock around the generic recovery callback,
                // otherwise its own session snapshot would self-deadlock.
                CloudProvider::Pikpak => run_with_session_recovery(
                    CloudProvider::Pikpak,
                    &s.store,
                    SessionWorkKind::Mutation,
                    || s.upload_pikpak_file(&h, &parent_id, &path),
                    |credential| {
                        s.with_provider(CloudProvider::Pikpak, |store| {
                            recover_provider_password_session(
                                CloudProvider::Pikpak,
                                store,
                                &h,
                                credential,
                            )
                        })
                    },
                )?,
                CloudProvider::Baidu => s.with_provider(CloudProvider::Baidu, |store| {
                    run_cloud_work(
                        CloudProvider::Baidu,
                        SessionWorkKind::Mutation,
                        store,
                        &h,
                        || {
                            let client = baidu::BaiduClient::new(
                                load_provider_session(CloudProvider::Baidu, store)?,
                                &h,
                            );
                            client.upload_file(&parent_id, &path, &h)
                        },
                    )
                })?,
                CloudProvider::Tianyi => s.with_provider(CloudProvider::Tianyi, |store| {
                    run_cloud_work(
                        CloudProvider::Tianyi,
                        SessionWorkKind::Mutation,
                        store,
                        &h,
                        || {
                            let client = tianyi::TianyiClient::new(
                                load_provider_session(CloudProvider::Tianyi, store)?,
                                &h,
                            );
                            client.upload_file(&parent_id, &path, &h)
                        },
                    )
                })?,
                CloudProvider::Aliyun => s.with_provider(CloudProvider::Aliyun, |store| {
                    run_cloud_work(
                        CloudProvider::Aliyun,
                        SessionWorkKind::Mutation,
                        store,
                        &h,
                        || aliyun::AliyunClient::new(store, &h).upload_file(&parent_id, &path),
                    )
                })?,
                CloudProvider::Xunlei => s.with_provider(CloudProvider::Xunlei, |store| {
                    run_cloud_work(
                        CloudProvider::Xunlei,
                        SessionWorkKind::Mutation,
                        store,
                        &h,
                        || {
                            let session = load_provider_session(CloudProvider::Xunlei, store)?;
                            let client = xunlei::XunleiClient::new(session, &h);
                            let file = client.upload_file(&parent_id, &path)?;
                            save_provider_session(CloudProvider::Xunlei, store, &client.session())?;
                            Ok(file)
                        },
                    )
                })?,
                CloudProvider::Uc => s.with_provider(CloudProvider::Uc, |store| {
                    run_cloud_work(
                        CloudProvider::Uc,
                        SessionWorkKind::Mutation,
                        store,
                        &h,
                        || {
                            uc::UcClient::new(load_provider_session(CloudProvider::Uc, store)?, &h)
                                .upload_file(&parent_id, &path, &h)
                        },
                    )
                })?,
            };
            files.push(file);
        }
        Ok(files)
    })
    .await
    .map_err(|_| QuarkError::Transport("上传任务中断".into()))?
}
#[tauri::command]
pub async fn cloud_logout(app: AppHandle, provider: CloudProvider) -> Result<(), QuarkError> {
    require_connected_provider(provider)?;
    let copy = app.clone();
    run_session_work(app, provider, SessionWorkKind::Logout, move |s, _| {
        if browser_login::supports_browser_password(provider) {
            let profile_window = WebviewWindowBuilder::new(
                &copy,
                format!("cloud-logout-{}", provider.key()),
                WebviewUrl::External(
                    "about:blank"
                        .parse()
                        .map_err(|_| QuarkError::InvalidResponse)?,
                ),
            )
            .visible(false)
            .data_directory(password_webview_data_directory(&copy, provider)?)
            .build()
            .map_err(|_| QuarkError::Storage)?;
            let cleared = profile_window
                .clear_all_browsing_data()
                .map_err(|_| QuarkError::Storage);
            let _ = profile_window.close();
            cleared?;
            s.password_recovery_attempts
                .lock()
                .map_err(|_| QuarkError::Storage)?
                .remove(provider.key());
        }
        logout_provider(provider, &s.store)?;
        if let Some(w) = copy.get_webview_window(&format!("cloud-auth-{}", provider.key())) {
            let _ = w.close();
        }
        Ok(())
    })
    .await
}
