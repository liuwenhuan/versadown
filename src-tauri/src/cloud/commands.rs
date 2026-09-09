use super::{http::response_json, types::required, *};
use crate::{
    commands::{task_action_outcome, AppState, TaskActionOutcome},
    duplicate_state, DownloadTask, NewTask, TaskStatus, TaskStore, UriSubmissionOptions,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tauri::{
    webview::{NewWindowResponse, PageLoadEvent},
    AppHandle, Manager, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_dialog::DialogExt;

pub struct CloudState {
    pub store: DpapiCredentialStore,
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
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<Arc<CloudState>>();
        state.with_provider(provider, |_| work(&state, &state.http))
    })
    .await
    .map_err(|_| QuarkError::Transport("网盘任务中断".into()))?
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
        CloudProvider::Xunlei => {
            let session = load_provider_session(CloudProvider::Xunlei, store)?;
            let client = xunlei::XunleiClient::new(session, http);
            if client.session().profile.client_secret.is_empty() {
                return client.web_account();
            }
            let account = client.account()?;
            save_provider_session(CloudProvider::Xunlei, store, &client.session())?;
            Ok(account)
        }
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
    store.remove(provider.key())
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

const XUNLEI_WEB_TOKENS_JS: &str = r#"(() => {
 if (location.protocol !== 'https:' || !(location.hostname === 'xunlei.com' || location.hostname.endsWith('.xunlei.com'))) return null;
 const out = {}; const seen = new Set();
 const fields = {
  access_token: ['access_token','accessToken'], refresh_token: ['refresh_token','refreshToken'],
  user_id: ['user_id','userId'], security_token: ['security_token','securityToken','captcha_token','captchaToken'],
  client_id: ['client_id','clientId'], client_secret: ['client_secret','clientSecret'],
  device_id: ['device_id','deviceId'], client_version: ['client_version','clientVersion'],
  package_name: ['package_name','packageName'], user_agent: ['user_agent','userAgent'],
  expires_at: ['expires_at','expiresAt']
 };
 function inspect(v, depth) {
  if (!v || depth > 6) return;
  if (typeof v === 'string') { try { inspect(JSON.parse(v), depth + 1); } catch (_) {} return; }
  if (typeof v !== 'object' || seen.has(v)) return; seen.add(v);
  for (const [k, x] of Object.entries(v)) {
   const target = Object.entries(fields).find(([, names]) => names.includes(k));
   if (target && (typeof x === 'string' || (target[0] === 'expires_at' && Number.isFinite(x)))) out[target[0]] = x;
   else if (!/password/i.test(k)) inspect(x, depth + 1);
  }
 }
 for (const storage of [localStorage, sessionStorage]) for (let i = 0; i < storage.length; i++) inspect({[storage.key(i)]: storage.getItem(storage.key(i))}, 0);
 // Xunlei's current official web client keeps its completed login in Nuxt's
 // in-memory user state, rather than in Web Storage.  Only copy the selected
 // renewable fields; this never exposes the rest of the account object.
 const currentUser = window.$nuxt?.$store?.state?.users?.curUser;
 if (currentUser && typeof currentUser === 'object') inspect(currentUser, 0);
 // Device and client identity can live in a sibling Vuex module. `inspect`
 // still returns only its narrow token/profile allowlist, never the state
 // object or arbitrary account data.
 inspect(window.$nuxt?.$store?.state, 0);
 if (!out.user_agent && typeof navigator?.userAgent === 'string') out.user_agent = navigator.userAgent;
 // The web client deliberately does not expose a native client profile or
 // client secret.  A current browser bearer and stable user id are sufficient
 // to complete the browser handshake; the Rust adapter must not fabricate
 // missing native-only fields.
 return out.access_token && out.refresh_token && out.user_id ? out : null;
})()"#;

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

fn with_xunlei_client<T, H: CloudHttpTransport>(
    store: &impl CredentialStore,
    http: &H,
    work: impl FnOnce(&xunlei::XunleiClient<'_, H>) -> Result<T, QuarkError>,
) -> Result<T, QuarkError> {
    let session = load_provider_session(CloudProvider::Xunlei, store)?;
    let client = xunlei::XunleiClient::new(session, http);
    let result = work(&client)?;
    save_provider_session(CloudProvider::Xunlei, store, &client.session())?;
    Ok(result)
}

fn parse_xunlei_web_session(value: serde_json::Value) -> Result<xunlei::XunleiSession, QuarkError> {
    let field =
        |key| super::types::required(&value, key).map_err(|_| QuarkError::AuthorizationPending);
    let optional = |key| value[key].as_str().unwrap_or_default().to_owned();
    xunlei::XunleiSession::from_web_authorization(
        value["access_token"].as_str().unwrap_or_default(),
        field("refresh_token")?,
        field("user_id")?,
        optional("security_token"),
        xunlei::XunleiClientProfile {
            client_id: optional("client_id"),
            client_secret: optional("client_secret"),
            device_id: optional("device_id"),
            client_version: optional("client_version"),
            package_name: optional("package_name"),
            user_agent: optional("user_agent"),
        },
        value["expires_at"].as_u64().unwrap_or(0),
    )
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
    // Browser login is complete once the account endpoint accepts the newly
    // captured session.  A root-list probe is a separate file-management
    // concern: making it part of this handshake kept a successfully scanned
    // QR login trapped in its authorization window whenever that optional
    // request was delayed or rejected.
    save_provider_session(CloudProvider::Baidu, store, &session)?;
    Ok(account)
}

fn finalize_tianyi_web_authorization(
    session: tianyi::TianyiSession,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
) -> Result<CloudAccount, QuarkError> {
    let client = tianyi::TianyiClient::new(session.clone(), http);
    let account = client.web_account()?;
    // The official browser session is the authorization boundary. Keep both
    // private account and root-directory requests in normal file-management
    // flow: either can fail independently despite a completed browser login.
    save_provider_session(CloudProvider::Tianyi, store, &session)?;
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
        match provider {
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
                let session =
                    parse_xunlei_web_session(web_storage_tokens(&window, XUNLEI_WEB_TOKENS_JS)?)?;
                let client = xunlei::XunleiClient::new(session, http);
                // The browser does not expose Xunlei's native client secret.
                // Persist the valid web session and create its workspace
                // without issuing a native refresh or an extra API probe.
                let account = client.web_account()?;
                save_provider_session(CloudProvider::Xunlei, &state.store, &client.session())?;
                let _ = window.close();
                Ok(account)
            }
            CloudProvider::Uc => {
                let session = uc::parse_uc_web_session(&browser_cookies(
                    &window,
                    &["https://drive.uc.cn", "https://pc-api.uc.cn"],
                )?)?;
                let client = uc::UcClient::new(session.clone(), http);
                // Once the official browser window has yielded the complete
                // session cookie pair, persist it and close immediately.
                // Validating with UC's undocumented `member` endpoint here
                // made a successful browser login appear to hang whenever
                // that optional probe was rejected.
                let account = client.web_account()?;
                save_provider_session(CloudProvider::Uc, &state.store, &session)?;
                let _ = window.close();
                Ok(account)
            }
        }
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
    let password = zeroize::Zeroizing::new(password);
    run(app, provider, move |state, http| match provider {
        CloudProvider::Pikpak => {
            let temporary = MemoryCredentialStore::default();
            let client = PikPakClient::new(http, &temporary);
            client.login(&username, &password)?;
            let account = client.account()?;
            state.store.save(
                "pikpak",
                &temporary.load("pikpak")?.ok_or(QuarkError::Storage)?,
            )?;
            Ok(account)
        }
        CloudProvider::Baidu => {
            let session = baidu::BaiduClient::login_password(&username, &password, http)?;
            let client = baidu::BaiduClient::new(session.clone(), http);
            let account = client.account()?;
            client.directory("/")?;
            save_provider_session(CloudProvider::Baidu, &state.store, &session)?;
            Ok(account)
        }
        CloudProvider::Tianyi => {
            let session = tianyi::TianyiClient::login_password(&username, &password, http)?;
            let client = tianyi::TianyiClient::new(session.clone(), http);
            let account = client.account()?;
            client.directory("-11")?;
            save_provider_session(CloudProvider::Tianyi, &state.store, &session)?;
            Ok(account)
        }
        CloudProvider::Quark
        | CloudProvider::Aliyun
        | CloudProvider::Xunlei
        | CloudProvider::Uc => Err(QuarkError::WebAuthorizationRequired),
    })
    .await
}
#[tauri::command]
pub async fn cloud_account(
    app: AppHandle,
    provider: CloudProvider,
) -> Result<CloudAccount, QuarkError> {
    run(app, provider, move |s, h| {
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
    run(app, provider, move |s, h| match provider {
        CloudProvider::Quark => QuarkClient::new(cookie(&s.store)?, h).directory(&parent_id),
        CloudProvider::Pikpak => PikPakClient::new(h, &s.store).list(&parent_id),
        CloudProvider::Baidu => {
            baidu::BaiduClient::new(load_provider_session(CloudProvider::Baidu, &s.store)?, h)
                .directory(&parent_id)
        }
        CloudProvider::Tianyi => {
            tianyi::TianyiClient::new(load_provider_session(CloudProvider::Tianyi, &s.store)?, h)
                .directory(&parent_id)
        }
        CloudProvider::Aliyun => aliyun::AliyunClient::new(&s.store, h).directory(&parent_id),
        CloudProvider::Xunlei => {
            with_xunlei_client(&s.store, h, |client| client.directory(&parent_id))
        }
        CloudProvider::Uc => {
            uc::UcClient::new(load_provider_session(CloudProvider::Uc, &s.store)?, h)
                .directory(&parent_id)
        }
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
    run(app, provider, move |s, h| match provider {
        CloudProvider::Quark => {
            let v =
                response_json(&QuarkClient::new(cookie(&s.store)?, h).mkdir(&parent_id, &name)?)?;
            Ok(CloudFile {
                id: required(&v["data"], "fid")?,
                parent_id,
                name,
                kind: "folder".into(),
                size: None,
                modified_at: None,
            })
        }
        CloudProvider::Pikpak => PikPakClient::new(h, &s.store).mkdir(&parent_id, &name),
        CloudProvider::Baidu => {
            let client =
                baidu::BaiduClient::new(load_provider_session(CloudProvider::Baidu, &s.store)?, h);
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
        CloudProvider::Aliyun => aliyun::AliyunClient::new(&s.store, h).mkdir(&parent_id, &name),
        CloudProvider::Xunlei => with_xunlei_client(&s.store, h, |client| {
            client.mkdir(&parent_id, &name)?;
            find_created_directory(client.directory(&parent_id)?, &name)
        }),
        CloudProvider::Uc => {
            let client = uc::UcClient::new(load_provider_session(CloudProvider::Uc, &s.store)?, h);
            client.mkdir(&parent_id, &name)?;
            find_created_directory(client.directory(&parent_id)?, &name)
        }
    })
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
    run(app, provider, move |s, h| match provider {
        CloudProvider::Quark => QuarkClient::new(cookie(&s.store)?, h)
            .rename(&file_id, &name)
            .map(|_| ()),
        CloudProvider::Pikpak => PikPakClient::new(h, &s.store).rename(&file_id, &name),
        CloudProvider::Baidu => {
            baidu::BaiduClient::new(load_provider_session(CloudProvider::Baidu, &s.store)?, h)
                .rename(&file_id, &name)
        }
        CloudProvider::Tianyi => {
            tianyi::TianyiClient::new(load_provider_session(CloudProvider::Tianyi, &s.store)?, h)
                .rename(&file_id, &name)
        }
        CloudProvider::Aliyun => aliyun::AliyunClient::new(&s.store, h).rename(&file_id, &name),
        CloudProvider::Xunlei => {
            with_xunlei_client(&s.store, h, |client| client.rename(&file_id, &name))
        }
        CloudProvider::Uc => {
            uc::UcClient::new(load_provider_session(CloudProvider::Uc, &s.store)?, h)
                .rename(&file_id, &name)
                .map(|_| ())
        }
    })
    .await
}
#[tauri::command]
pub async fn cloud_move(
    app: AppHandle,
    provider: CloudProvider,
    file_ids: Vec<String>,
    target_parent_id: String,
) -> Result<(), QuarkError> {
    run(app, provider, move |s, h| match provider {
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
        CloudProvider::Tianyi => {
            tianyi::TianyiClient::new(load_provider_session(CloudProvider::Tianyi, &s.store)?, h)
                .move_files(&file_ids, &target_parent_id)
        }
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
    })
    .await
}
#[tauri::command]
pub async fn cloud_copy(
    app: AppHandle,
    provider: CloudProvider,
    file_ids: Vec<String>,
    target_parent_id: String,
) -> Result<(), QuarkError> {
    run(app, provider, move |s, h| match provider {
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
        CloudProvider::Tianyi => {
            tianyi::TianyiClient::new(load_provider_session(CloudProvider::Tianyi, &s.store)?, h)
                .copy_files(&file_ids, &target_parent_id)
        }
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
    })
    .await
}
#[tauri::command]
pub async fn cloud_trash(
    app: AppHandle,
    provider: CloudProvider,
    file_ids: Vec<String>,
) -> Result<(), QuarkError> {
    run(app, provider, move |s, h| match provider {
        CloudProvider::Quark => QuarkClient::new(cookie(&s.store)?, h)
            .trash(&file_ids)
            .map(|_| ()),
        CloudProvider::Pikpak => PikPakClient::new(h, &s.store).trash(&file_ids),
        CloudProvider::Baidu => {
            baidu::BaiduClient::new(load_provider_session(CloudProvider::Baidu, &s.store)?, h)
                .trash(&file_ids)
        }
        CloudProvider::Tianyi => {
            tianyi::TianyiClient::new(load_provider_session(CloudProvider::Tianyi, &s.store)?, h)
                .trash(&file_ids)
        }
        CloudProvider::Aliyun => aliyun::AliyunClient::new(&s.store, h).trash(&file_ids),
        CloudProvider::Xunlei => with_xunlei_client(&s.store, h, |client| client.trash(&file_ids)),
        CloudProvider::Uc => {
            uc::UcClient::new(load_provider_session(CloudProvider::Uc, &s.store)?, h)
                .trash(&file_ids)
                .map(|_| ())
        }
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
    run(app, provider, move |state, http| {
        resolve_download(provider, &file_id, &state.store, http)
    })
    .await
}

#[tauri::command]
pub async fn cloud_download_url(
    app: AppHandle,
    provider: CloudProvider,
    file_id: String,
) -> Result<TaskActionOutcome, QuarkError> {
    let stable_source = format!(
        "cloud://{}/{}",
        provider.key(),
        super::types::encoded(&file_id)
    );
    let state = app.state::<AppState>();
    if let Some(existing) = state
        .store
        .find_by_source(&stable_source)
        .map_err(|_| QuarkError::Storage)?
        .filter(|task| task.status != TaskStatus::Recycled)
    {
        let duplicate =
            duplicate_state(&state.store, &existing).map_err(|_| QuarkError::Storage)?;
        return task_action_outcome(&state.store, existing, Some(duplicate))
            .map_err(QuarkError::Transport);
    }
    let resolved = resolve_stable_cloud_download(app.clone(), &stable_source).await?;
    let task = create_cloud_task(&state.store, provider, &file_id, &resolved.name)?;
    let signed_url = resolved.url.clone();
    let mut options = resolved.into_uri_submission_options();
    let configured =
        crate::commands::download_submission_options(&state).map_err(QuarkError::Transport)?;
    options.directory = configured.directory;
    options.proxy = configured.proxy;
    let service = state
        .runtime
        .service()
        .await
        .map_err(|error| QuarkError::Transport(error.to_string()))?;
    let submitted = service
        .submit_uri_with_options(task.id, vec![signed_url], options)
        .await;
    let task = match submitted {
        Ok(task) => task,
        Err(error) => {
            let message = format!("aria2 未能接受网盘下载任务：{error}");
            let _ = state.store.fail(task.id, &message);
            return Err(QuarkError::Transport(message));
        }
    };
    task_action_outcome(&state.store, task, None).map_err(QuarkError::Transport)
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
            Ok(ResolvedCloudDownload {
                url,
                name,
                headers: Vec::new(),
            })
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
    } else {
        result.into()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use super::{create_cloud_task, resolve_download};
    use crate::cloud::{
        baidu::BaiduSession, tianyi::TianyiSession, CloudHttpRequest, CloudHttpResponse,
        CloudHttpTransport, CloudProvider, CredentialStore, MemoryCredentialStore, QuarkError,
        QUARK_DESKTOP_USER_AGENT,
    };
    use crate::TaskStore;

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

    struct OfflineTransport;

    impl CloudHttpTransport for OfflineTransport {
        fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            Err(QuarkError::Transport(
                "private account endpoint unavailable".into(),
            ))
        }
    }

    #[test]
    fn baidu_web_authorization_persists_the_account_without_waiting_for_root_listing() {
        let credentials = MemoryCredentialStore::default();
        let transport = SequenceTransport(Mutex::new(VecDeque::from([
            r#"{"errno":0,"user":{"uk":"b1","netdisk_name":"baidu"}}"#,
        ])));

        let account = super::finalize_baidu_web_authorization(
            BaiduSession::cookie("BDUSS=browser-session"),
            &credentials,
            &transport,
        )
        .unwrap();

        assert_eq!(account.id, "b1");
        assert!(credentials.load("baidu").unwrap().is_some());
    }

    #[test]
    fn tianyi_web_authorization_persists_the_browser_session_without_a_private_probe() {
        // The official WebView already proved the user completed login. A
        // separately failing private account endpoint must not keep that
        // authorization window open or prevent its workspace tab.
        let credentials = MemoryCredentialStore::default();

        let account = super::finalize_tianyi_web_authorization(
            TianyiSession::cookie("SSON=official-browser-session"),
            &credentials,
            &OfflineTransport,
        )
        .expect("an official Tianyi browser session can complete authorization offline");

        assert_eq!(account.id, "tianyi-web");
        assert_eq!(account.provider, CloudProvider::Tianyi);
        assert!(credentials.load("tianyi").unwrap().is_some());
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
        let source = include_str!("commands.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source precedes its tests");
        assert!(source.contains("window.$nuxt?.$store?.state?.users?.curUser"));

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
    fn xunlei_web_capture_reads_only_selected_client_context_from_nuxt_state() {
        // The current web login keeps the bearer in users.curUser but can
        // keep the matching browser device/profile in a sibling Vuex module.
        // The capture script must inspect that state without returning the
        // state object itself: `inspect` copies only its explicit allowlist.
        let source = include_str!("commands.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source precedes its tests");

        assert!(source.contains("inspect(window.$nuxt?.$store?.state, 0)"));
        assert!(source.contains("out.user_agent = navigator.userAgent"));
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
                CloudProvider::Pikpak => s.upload_pikpak_file(&h, &parent_id, &path)?,
                CloudProvider::Baidu => s.with_provider(CloudProvider::Baidu, |store| {
                    let client = baidu::BaiduClient::new(
                        load_provider_session(CloudProvider::Baidu, store)?,
                        &h,
                    );
                    client.upload_file(&parent_id, &path, &h)
                })?,
                CloudProvider::Tianyi => s.with_provider(CloudProvider::Tianyi, |store| {
                    let client = tianyi::TianyiClient::new(
                        load_provider_session(CloudProvider::Tianyi, store)?,
                        &h,
                    );
                    client.upload_file(&parent_id, &path, &h)
                })?,
                CloudProvider::Aliyun => s.with_provider(CloudProvider::Aliyun, |store| {
                    aliyun::AliyunClient::new(store, &h).upload_file(&parent_id, &path)
                })?,
                CloudProvider::Xunlei => s.with_provider(CloudProvider::Xunlei, |store| {
                    let session = load_provider_session(CloudProvider::Xunlei, store)?;
                    let client = xunlei::XunleiClient::new(session, &h);
                    let file = client.upload_file(&parent_id, &path)?;
                    save_provider_session(CloudProvider::Xunlei, store, &client.session())?;
                    Ok(file)
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
    run(app, provider, move |s, _| {
        logout_provider(provider, &s.store)?;
        if let Some(w) = copy.get_webview_window(&format!("cloud-auth-{}", provider.key())) {
            let _ = w.close();
        }
        Ok(())
    })
    .await
}
