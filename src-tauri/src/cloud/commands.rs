use super::{http::response_json, types::required, *};
use crate::{
    commands::{task_action_outcome, AppState, TaskActionOutcome},
    duplicate_state, DownloadTask, NewTask, TaskStatus, TaskStore, UriSubmissionOptions,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};
use tauri::{
    webview::{NewWindowResponse, PageLoadEvent},
    AppHandle, Manager, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_dialog::DialogExt;

/// Personal files lock their exact source. Share files lock the stable plan,
/// so new siblings, overwrite, refresh and cleanup cannot bypass each other
/// while the plan's entry set changes. The guard covers resolution through
/// aria2 rebinding, or cleanup claiming through the provider mutation.
#[derive(Default)]
pub struct CloudDownloadSourceGate {
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
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
    run(app, provider, move |state, http| {
        run_cloud_work(provider, kind, &state.store, http, || work(state, http))
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

fn recover_provider_password_session(
    provider: CloudProvider,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
    credential: &CloudPasswordCredential,
) -> Result<(), QuarkError> {
    if credential.provider() != provider {
        return Err(QuarkError::Storage);
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
            let session = baidu::BaiduClient::login_password(
                credential.username(),
                credential.password(),
                http,
            )?;
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
        // Keep the encrypted credential. A provider can return a different
        // account after a risk challenge or an account switch; selecting that
        // account prevents an unsafe automatic login without losing the prior
        // account's saved password.
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
        WebviewWindowBuilder::new(
            &app,
            &label,
            WebviewUrl::External(url.parse().map_err(|_| QuarkError::InvalidResponse)?),
        )
        .title(format!("{} 网页授权", provider.key()))
        .inner_size(1024.0, 760.0)
        .incognito(true)
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
        })
        .build()
        .map_err(|_| QuarkError::WebAuthorizationRequired)?;
    }
    Ok(CloudAuthorization {
        provider,
        window_label: label,
        instructions: "请在弹出的官方网盘窗口完成登录；验证成功后授权窗口会自动关闭。".into(),
    })
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
    run(app, provider, move |state, http| {
        let account = match provider {
            CloudProvider::Quark => {
                // Must stay on the blocking worker, never the Windows UI thread (WebView2 deadlock).
                let cookie = browser_cookies(
                    &window,
                    &["https://pan.quark.cn", "https://drive-pc.quark.cn"],
                )?
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
                    &["https://pan.baidu.com", "https://passport.baidu.com"],
                )?)?;
                let account = finalize_baidu_web_authorization(session, &state.store, http)?;
                let _ = window.close();
                Ok(account)
            }
            CloudProvider::Tianyi => {
                let session = tianyi::parse_tianyi_web_session(&browser_cookies(
                    &window,
                    &["https://cloud.189.cn", "https://open.e.189.cn"],
                )?)?;
                let account = finalize_tianyi_web_authorization(session, &state.store, http)?;
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
                    &["https://drive.uc.cn", "https://pc-api.uc.cn"],
                )?)?;
                let account = finalize_uc_web_authorization(session, &state.store, http)?;
                let _ = window.close();
                Ok(account)
            }
        }?;
        if cloud_session_policy(provider).supports_password_fallback {
            select_password_credential_account(&state.store, provider, &account.id)?;
        }
        Ok(account)
    })
    .await
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
    run(app, provider, move |state, http| {
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
                let session = baidu::BaiduClient::login_password(&username, &password, http)?;
                let client = baidu::BaiduClient::new(session.clone(), http);
                let account = client.account()?;
                client.directory("/")?;
                save_provider_session(CloudProvider::Baidu, &state.store, &session)?;
                account
            }
            CloudProvider::Tianyi => {
                let session = tianyi::TianyiClient::login_password(&username, &password, http)?;
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
    .await
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
    run_session_work(
        app,
        provider,
        SessionWorkKind::Mutation,
        move |s, h| match provider {
            // The UI only exposes this operation while browsing PikPak's virtual
            // recycle-bin directory. Other providers keep their non-destructive
            // `cloud_trash` behaviour.
            CloudProvider::Pikpak => PikPakClient::new(h, &s.store).delete(&file_ids),
            _ => Err(QuarkError::InvalidInput(
                "只有 PikPak 回收站中的项目可以永久删除".into(),
            )),
        },
    )
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
    // Share challenges must not trigger the generic account/password recovery.
    run(app, candidate.provider, move |state, http| {
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
    run(app, provider, move |state, http| {
        let adapter = share::provider_share_adapter(provider, &state.store, http)?;
        share::resolve_current_share_download(&state.share_plans, adapter.as_ref(), &source)
    })
    .await
}

pub async fn submit_cloud_share_file(
    app: AppHandle,
    state: &AppState,
    source: String,
    mut configured: UriSubmissionOptions,
) -> Result<TaskActionOutcome, QuarkError> {
    share::parse_stable_cloud_share_source(&source)?;
    let _source_submission = state.cloud_download_sources.acquire(&source).await?;
    if let Some(existing) = state
        .store
        .find_by_source(&source)
        .map_err(|_| QuarkError::Storage)?
        .filter(|task| task.status != TaskStatus::Recycled)
    {
        let duplicate =
            duplicate_state(&state.store, &existing).map_err(|_| QuarkError::Storage)?;
        return task_action_outcome(&state.store, existing, Some(duplicate))
            .map_err(QuarkError::Transport);
    }
    let cloud = app
        .try_state::<Arc<CloudState>>()
        .ok_or(QuarkError::Storage)?;
    let entry = cloud.share_plans.resolve_entry(&source)?;
    cloud
        .share_plans
        .link_local_sources(std::slice::from_ref(&source))?;
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
    let resolved = resolve_stable_cloud_share_download(app.clone(), &source).await?;
    let service = state
        .runtime
        .service()
        .await
        .map_err(|_| QuarkError::Transport("下载引擎不可用".into()))?;
    let task =
        share::submit_current_share_download(&service, &source, resolved, configured).await?;
    cloud.share_plans.link_local_task(&source, task.id)?;
    if let Ok(latest) = state.store.find(task.id) {
        schedule_cloud_share_cleanup(app.clone(), &latest);
    }
    task_action_outcome(&state.store, task, None).map_err(QuarkError::Transport)
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
            if let Ok(payload) = crate::commands::task_payload(&state.store, task.clone()) {
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
        crate::engine_runtime::cleanup_completed_share(
            &cloud.share_plans,
            &tasks,
            &task,
            enabled,
            retry,
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
        if let Ok(payload) = crate::commands::task_payload(&state.store, updated.clone()) {
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
    pub queued_count: usize,
    pub tasks: Vec<TaskActionOutcome>,
    pub failures: Vec<CloudDirectoryDownloadFailure>,
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
    let (code, message) = match error {
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
    };
    CloudDirectoryDownloadFailure {
        relative_path: planned_relative_path(file),
        code,
        message,
    }
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
    match submit_cloud_file(app, &state, provider, file_id, configured).await? {
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
    let plan = run_session_work(
        app.clone(),
        provider,
        SessionWorkKind::Read,
        move |state, http| {
            plan_cloud_directory_download(provider, &directory_id, &directory_name, |id| {
                list_cloud_directory(provider, &state.store, http, id)
            })
        },
    )
    .await?;

    let state = app.state::<AppState>();
    let configured =
        crate::commands::download_submission_options(&state).map_err(QuarkError::Transport)?;
    let base_directory = configured
        .directory
        .clone()
        .ok_or_else(|| QuarkError::InvalidInput("未配置下载目录".into()))?;
    let mut outcome = CloudDirectoryDownloadOutcome {
        queued_count: 0,
        tasks: Vec::new(),
        failures: Vec::new(),
    };

    for file in plan.files {
        let relative_path = planned_relative_path(&file);
        let output_directory =
            match directory_download_output_directory(&base_directory, &file.relative_directory) {
                Ok(directory) => directory,
                Err(error) => {
                    outcome
                        .failures
                        .push(directory_download_failure(&file, &error));
                    continue;
                }
            };
        let mut options = configured.clone();
        options.directory = Some(output_directory);
        options.output_name = Some(file.output_name.clone());

        match submit_cloud_file(app.clone(), &state, provider, file.file_id.clone(), options).await
        {
            Ok(CloudFileSubmission::Queued(task)) => {
                outcome.queued_count += 1;
                outcome.tasks.push(task);
            }
            Ok(CloudFileSubmission::Duplicate(_)) => {
                outcome.failures.push(CloudDirectoryDownloadFailure {
                    relative_path,
                    code: "duplicate",
                    message: "已存在下载任务，未重复入队",
                });
            }
            Err(error) => outcome
                .failures
                .push(directory_download_failure(&file, &error)),
        }
    }

    Ok(outcome)
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
    use std::{cell::Cell, collections::VecDeque, sync::Mutex};

    use super::{create_cloud_task, resolve_download};
    use crate::cloud::{
        baidu::{parse_baidu_web_session, BaiduSession},
        save_password_credential,
        tianyi::TianyiSession,
        uc::UcSession,
        xunlei::{XunleiClientProfile, XunleiSession, XunleiWebContext},
        CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudPasswordCredential,
        CloudProvider, CredentialStore, MemoryCredentialStore, QuarkError, SessionWorkKind,
        QUARK_DESKTOP_USER_AGENT,
    };
    use crate::TaskStore;

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
            r#"{"name":"fixture.txt","web_content_link":"https://cdn.example.test/fixture"}"#,
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
            Self(Mutex::new(VecDeque::from([
                CloudHttpResponse {
                    status: 200,
                    body: r#"{"err_no":0}"#.into(),
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
        assert!(root.items.is_empty());
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
    if matches!(provider, CloudProvider::Quark | CloudProvider::Uc) {
        return Err(QuarkError::UploadUnavailable);
    }
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
                CloudProvider::Quark | CloudProvider::Uc => {
                    return Err(QuarkError::UploadUnavailable)
                }
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
        logout_provider(provider, &s.store)?;
        if let Some(w) = copy.get_webview_window(&format!("cloud-auth-{}", provider.key())) {
            let _ = w.close();
        }
        Ok(())
    })
    .await
}
