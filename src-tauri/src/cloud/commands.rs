use super::{http::response_json, types::required, *};
use crate::{
    commands::{task_action_outcome, AppState, TaskActionOutcome}, duplicate_state,
    DownloadTask, NewTask, TaskStatus, TaskStore, UriSubmissionOptions,
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::DialogExt;

pub struct CloudState {
    pub store: DpapiCredentialStore,
    quark_operation: Mutex<()>,
    pikpak_operation: Mutex<()>,
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
            quark_operation: Mutex::new(()),
            pikpak_operation: Mutex::new(()),
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
        state.with_provider(provider, |_| work(&state, &ReqwestTransport::new()?))
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
    }
}
fn valid_name(name: &str) -> Result<(), QuarkError> {
    if name.trim().is_empty() || name.contains(['\r', '\n', '/', '\\']) {
        Err(QuarkError::InvalidInput("请输入有效的文件名称".into()))
    } else {
        Ok(())
    }
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
        };
        WebviewWindowBuilder::new(
            &app,
            &label,
            WebviewUrl::External(url.parse().map_err(|_| QuarkError::InvalidResponse)?),
        )
        .title(format!("{} 网页授权", provider.key()))
        .inner_size(1024.0, 760.0)
        .incognito(true)
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
                let mut values = std::collections::BTreeMap::new();
                for u in ["https://pan.quark.cn", "https://drive-pc.quark.cn"] {
                    for c in window
                        .cookies_for_url(u.parse().map_err(|_| QuarkError::InvalidResponse)?)
                        .map_err(|_| QuarkError::WebAuthorizationRequired)?
                    {
                        values.insert(c.name().to_owned(), c.value().to_owned());
                    }
                }
                let cookie = values
                    .into_iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
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
                let (tx, rx) = std::sync::mpsc::channel();
                window
                    .eval_with_callback(WEB_TOKENS_JS, move |s| {
                        let _ = tx.send(s);
                    })
                    .map_err(|_| QuarkError::WebAuthorizationRequired)?;
                let raw = rx
                    .recv_timeout(std::time::Duration::from_secs(15))
                    .map_err(|_| QuarkError::WebAuthorizationRequired)?;
                let session = parse_web_tokens(&raw)?;
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
    let password = zeroize::Zeroizing::new(password);
    run(app, provider, move |state, http| {
        if provider == CloudProvider::Quark {
            return Err(QuarkError::WebAuthorizationRequired);
        }
        let temporary = MemoryCredentialStore::default();
        let client = PikPakClient::new(http, &temporary);
        client.login(&username, &password)?;
        let account = client.account()?;
        state.store.save(
            "pikpak",
            &temporary.load("pikpak")?.ok_or(QuarkError::Storage)?,
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
            source: format!("cloud://{}/{}", provider.key(), super::types::encoded(file_id)),
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
    if id.trim().is_empty() || id.chars().any(|character| character.is_control() || character == '\u{fffd}') {
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
    let stable_source = format!("cloud://{}/{}", provider.key(), super::types::encoded(&file_id));
    let state = app.state::<AppState>();
    if let Some(existing) = state
        .store
        .find_by_source(&stable_source)
        .map_err(|_| QuarkError::Storage)?
    {
        let duplicate = duplicate_state(&state.store, &existing).map_err(|_| QuarkError::Storage)?;
        return task_action_outcome(&state.store, existing, Some(duplicate))
            .map_err(QuarkError::Transport);
    }
    let resolved = resolve_stable_cloud_download(app.clone(), &stable_source).await?;
    let task = create_cloud_task(&state.store, provider, &file_id, &resolved.name)?;
    let signed_url = resolved.url.clone();
    let mut options = resolved.into_uri_submission_options();
    let configured = crate::commands::download_submission_options(&state)
        .map_err(QuarkError::Transport)?;
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
            Ok(ResolvedCloudDownload{url:parse_quark_download_url(&r.body)?,name:v.pointer("/data/0/file_name").and_then(serde_json::Value::as_str).unwrap_or(file_id).into(),headers:vec![("Cookie".into(),cookie_for_download(&cookie, &r.set_cookies)),("Referer".into(),"https://pan.quark.cn/".into()),("User-Agent".into(),QUARK_DESKTOP_USER_AGENT.into())]})
        }
        CloudProvider::Pikpak => {
            let (url, name) = PikPakClient::new(http, store).download(file_id)?;
            Ok(ResolvedCloudDownload {
                url,
                name,
                headers: Vec::new(),
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
    use std::sync::Mutex;

    use super::{create_cloud_task, resolve_download};
    use crate::cloud::{
        CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudProvider, CredentialStore,
        MemoryCredentialStore, QuarkError, QUARK_DESKTOP_USER_AGENT,
    };
    use crate::TaskStore;

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
            assert!(super::parse_stable_cloud_source(source).is_err(), "{source}");
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

        let download = resolve_download(CloudProvider::Quark, "fixture", &credentials, &transport)
            .unwrap();
        let api_request = transport.0.lock().unwrap().clone().unwrap();
        let aria2_user_agent = download
            .headers
            .iter()
            .find(|(name, _)| name == "User-Agent")
            .map(|(_, value)| value);

        assert!(
            api_request
                .headers
                .get("User-Agent")
                .is_some_and(|value| value.contains("quark-cloud-drive/"))
        );
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
                ("Cookie".into(), "sid=web-session; __puus=fresh-download-session".into()),
                ("Referer".into(), "https://pan.quark.cn/".into()),
                ("User-Agent".into(), QUARK_DESKTOP_USER_AGENT.into()),
            ]
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
}

#[tauri::command]
pub async fn cloud_upload_files(
    app: AppHandle,
    provider: CloudProvider,
    parent_id: String,
    paths: Option<Vec<String>>,
) -> Result<Vec<CloudFile>, QuarkError> {
    if provider == CloudProvider::Quark {
        return Err(QuarkError::UploadUnavailable);
    }
    let picker = app.clone();
    // The native picker may stay open indefinitely; it must hold no session lock.
    let paths =
        tauri::async_runtime::spawn_blocking(move || -> Result<Vec<PathBuf>, QuarkError> {
            let paths = match paths {
                Some(paths) => paths.into_iter().map(PathBuf::from).collect(),
                None => picker
                    .dialog()
                    .file()
                    .set_title("选择上传到 PikPak 的文件")
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
            files.push(s.upload_pikpak_file(&h, &parent_id, &path)?)
        }
        Ok(files)
    })
    .await
    .map_err(|_| QuarkError::Transport("上传任务中断".into()))?
}
#[tauri::command]
pub async fn cloud_logout(app: AppHandle, provider: CloudProvider) -> Result<(), QuarkError> {
    let copy = app.clone();
    run(app, provider, move |s, _| {
        s.store.remove(provider.key())?;
        if let Some(w) = copy.get_webview_window(&format!("cloud-auth-{}", provider.key())) {
            let _ = w.close();
        }
        Ok(())
    })
    .await
}
