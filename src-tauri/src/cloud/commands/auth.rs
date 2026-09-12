use super::*;
use crate::cloud::driver::{NativeLoginOutcome, ProviderAuth};

/// The host owns browser fallback; the callback keeps this boundary testable without a WebView.
pub(crate) fn password_login_with_browser(
    provider: CloudProvider,
    store: &impl CredentialStore,
    auth: &mut dyn ProviderAuth,
    username: &str,
    password: &str,
    mut browser: impl FnMut(bool) -> Result<CloudAccount, QuarkError>,
) -> Result<CloudAccount, QuarkError> {
    match auth.login_password(username, password) {
        Ok(NativeLoginOutcome::Authenticated(account)) => Ok(account),
        Ok(NativeLoginOutcome::NeedBrowserFallback(error))
            if browser_login::supports_browser_password(provider) =>
        {
            if ProviderRegistry::global().get(provider).browser_profile_owner {
                set_browser_password_login_preferred(store, provider, true)?;
            }
            browser(matches!(error, QuarkError::CaptchaRequired))
        }
        Ok(NativeLoginOutcome::NeedBrowserFallback(error)) | Err(error) => Err(error),
    }
}

pub(crate) fn validate_browser_password_session(
    provider: CloudProvider,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
    cookies: &[String],
    browser_context: Option<&serde_json::Value>,
    expected_account: Option<&str>,
    mut report_stage: impl FnMut(&'static str),
) -> Result<CloudAccount, QuarkError> {
    report_stage("读取登录凭据");
    let (account, snapshot) = ProviderRegistry::global()
        .driver(provider, store, http)
        .auth()
        .validate_browser_password(cookies, browser_context, &mut report_stage)?;
    if account.id.is_empty() || expected_account.is_some_and(|expected| expected != account.id) {
        return Err(QuarkError::InvalidInput(
            "登录账号与原任务账号不一致，已保留原凭据，请使用原账号登录".into(),
        ));
    }
    report_stage("保存登录凭据");
    store.save(provider.key(), &snapshot)?;
    Ok(account)
}

#[tauri::command]
pub fn cloud_provider_capabilities(provider: CloudProvider) -> CloudAuthorizationCapabilities {
    provider_capabilities(provider)
}

#[tauri::command]
pub fn cloud_provider_catalog() -> Vec<crate::cloud::ProviderDescriptorDto> {
    crate::cloud::ProviderRegistry::global().catalog()
}

#[tauri::command]
pub async fn cloud_saved_password_credentials(
    app: AppHandle,
    provider: CloudProvider,
) -> Result<Option<SavedCloudCredentialInfo>, QuarkError> {
    if !provider_capabilities(provider).password {
        return Ok(None);
    }
    run(app, provider, move |state, _| {
        saved_cloud_credential_info(&state.store, provider)
    })
    .await
}

pub(crate) fn tianyi_session_safe_shape(session: &tianyi::TianyiSession) -> serde_json::Value {
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

pub(crate) fn require_connected_provider(provider: CloudProvider) -> Result<(), QuarkError> {
    let _ = ProviderRegistry::global().get(provider);
    Ok(())
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
        let url = crate::cloud::ProviderRegistry::global()
            .get(provider)
            .authorization_url;
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
                if ProviderRegistry::global()
                    .get(provider)
                    .browser_profile_owner
                    && is_baidu_authorization_page(payload.url())
                {
                    let _ = window.eval(BAIDU_AUTO_OPEN_LOGIN_JS);
                }
            }
        });
        if browser_login::supports_browser_password(provider) {
            builder = builder.data_directory(password_webview_data_directory(&app, provider)?);
        }
        if ProviderRegistry::global().get(provider).browser_context {
            builder = builder.initialization_script(include_str!("../tianyi-browser-context.js"));
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

pub(crate) fn cloud_provider_from_download_source(
    source: &str,
) -> Result<CloudProvider, QuarkError> {
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

pub(crate) fn queue_provider_authorization_failures(
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

pub(crate) async fn resume_pending_authorization_retries(
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

// The browser page is the only place where these renewable credentials are
// acquired.  The scripts return a narrowly selected object; they never return
// a storage dump, DOM content or passwords to Rust.

#[tauri::command]
pub async fn cloud_complete_authorization(
    app: AppHandle,
    provider: CloudProvider,
) -> Result<CloudAccount, QuarkError> {
    let window = app
        .get_webview_window(&format!("cloud-auth-{}", provider.key()))
        .ok_or(QuarkError::WebAuthorizationRequired)?;
    let account = run(app.clone(), provider, move |state, http| {
        let mut driver = ProviderRegistry::global().driver(provider, &state.store, http);
        let recipe = driver.auth().web_authorization_recipe();
        let input = crate::cloud::driver::WebAuthorizationInput {
            cookies: browser_cookies(&window, web_session_cookie_urls(provider))?,
            storage: match recipe.token_script {
                Some(script) if recipe.asynchronous_tokens => {
                    asynchronous_web_tokens(&window, script)?
                }
                Some(script) => web_storage_tokens(&window, script)?,
                None => serde_json::Value::Null,
            },
            browser_context: if recipe.browser_context {
                Some(tianyi_browser_context(&window)?)
            } else {
                None
            },
        };
        let account = if recipe.browser_context {
            let validation_http = TianyiBrowserValidationTransport {
                window: &window,
                native: http,
            };
            let account = ProviderRegistry::global()
                .driver(provider, &state.store, &validation_http)
                .auth()
                .complete_web(&input)?;
            account
        } else {
            driver.auth().complete_web(&input)?
        };
        let _ = window.close();
        if ProviderRegistry::global()
            .get(provider)
            .browser_profile_owner
        {
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
    password: Option<String>,
    use_saved_password: Option<bool>,
) -> Result<CloudAccount, QuarkError> {
    let request = PasswordLoginRequest {
        provider,
        username,
        password,
        use_saved_password: use_saved_password.unwrap_or(false),
    };
    if !provider_capabilities(provider).password {
        return Err(QuarkError::WebAuthorizationRequired);
    }
    let browser_app = app.clone();
    let account = run(app.clone(), provider, move |state, http| {
        let (username, password) = resolve_password_login(&state.store, request)?;
        let reset_baidu_profile = ProviderRegistry::global()
            .get(provider)
            .browser_profile_owner
            && prepare_baidu_account_login(&state.store, &username)?;
        let reset_password_profile =
            prepare_password_account_login(&state.store, provider, &username)?;
        reset_password_profile_if_required(
            &state.store,
            provider,
            reset_baidu_profile || reset_password_profile,
            || clear_password_webview_profile(&browser_app, provider),
        )?;
        if reset_baidu_profile || reset_password_profile {
            if ProviderRegistry::global()
                .get(provider)
                .browser_profile_owner
            {
                set_baidu_browser_profile_username(&state.store, None)?;
            }
            set_browser_password_login_preferred(&state.store, provider, false)?;
        }
        // Remember the latest submitted input before contacting the provider,
        // so a rejected or interrupted attempt does not force re-entry.
        save_password_login_draft(&state.store, provider, &username, &password)?;
        let account = password_login_with_browser(
            provider,
            &state.store,
            ProviderRegistry::global()
                .driver(provider, &state.store, http)
                .auth(),
            &username,
            &password,
            |captcha| {
                browser_password_login(
                    &browser_app,
                    provider,
                    &state.store,
                    http,
                    &username,
                    &password,
                    None,
                    captcha,
                )
            },
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
        Ok(account)
    })
    .await?;
    queue_provider_authorization_failures(&app, provider)?;
    resume_pending_authorization_retries(app, provider).await?;
    Ok(account)
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
