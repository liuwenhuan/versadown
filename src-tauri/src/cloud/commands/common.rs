use super::*;
pub use crate::cloud::common::*;
pub(crate) const CLOUD_SHARE_PREPARE_FAILURE: &str = "网盘分享下载准备失败，请重试";

pub(crate) fn share_resolution_failure_message(error: &QuarkError) -> &'static str {
    match error {
        QuarkError::NotAuthorized => "尚未授权此网盘，请登录后使用",
        QuarkError::AuthorizationExpired => "网盘会话已过期，请重新登录授权",
        QuarkError::SessionRenewedRetryOperation => "登录已自动恢复，请重新执行本次操作",
        QuarkError::WebAuthorizationRequired => "此登录方式需要网页验证，请点击网页授权完成登录",
        QuarkError::CaptchaRequired => "网盘要求完成官方操作验证后重试",
        QuarkError::PikpakOperationVerificationRequired(_) => {
            "PikPak 需要官方操作验证，请在我的网盘中完成验证后重试"
        }
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
        QuarkError::PikpakOperationVerificationRequired(_) => {
            "pikpak_operation_verification_required"
        }
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

pub(crate) async fn run<T: Send + 'static>(
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

pub(crate) async fn run_when_initialized<T: Send + 'static>(
    lookup: impl FnMut() -> Option<Arc<CloudState>> + Send + 'static,
    provider: CloudProvider,
    work: impl FnOnce(&CloudState, &ReqwestTransport) -> Result<T, QuarkError> + Send + 'static,
    initialization_timeout: std::time::Duration,
) -> Result<T, QuarkError> {
    run_initialized(lookup, provider, work, initialization_timeout, true).await
}

async fn run_initialized<T: Send + 'static>(
    mut lookup: impl FnMut() -> Option<Arc<CloudState>> + Send + 'static,
    provider: CloudProvider,
    work: impl FnOnce(&CloudState, &ReqwestTransport) -> Result<T, QuarkError> + Send + 'static,
    initialization_timeout: std::time::Duration,
    exclusive: bool,
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
        if exclusive {
            state.with_provider(provider, |_| work(&state, &state.http))
        } else {
            work(&state, &state.http)
        }
    })
    .await
    .map_err(|_| QuarkError::Transport("网盘任务中断".into()))?
}

pub(crate) struct SessionWorkContext<'a> {
    pub store: crate::cloud::driver::StoreRef<'a>,
    state: &'a CloudState,
}
impl std::ops::Deref for SessionWorkContext<'_> {
    type Target = CloudState;
    fn deref(&self) -> &Self::Target {
        self.state
    }
}

pub(crate) async fn run_session_work<T: Send + 'static>(
    app: AppHandle,
    provider: CloudProvider,
    kind: SessionWorkKind,
    work: impl FnMut(&SessionWorkContext<'_>, &ReqwestTransport) -> Result<T, QuarkError>
        + Send
        + 'static,
) -> Result<T, QuarkError> {
    run_session_work_checked(app, provider, kind, work, || Ok(())).await
}

pub(crate) async fn run_session_work_checked<T: Send + 'static>(
    app: AppHandle,
    provider: CloudProvider,
    kind: SessionWorkKind,
    mut work: impl FnMut(&SessionWorkContext<'_>, &ReqwestTransport) -> Result<T, QuarkError>
        + Send
        + 'static,
    check: impl Fn() -> Result<(), QuarkError> + Send + 'static,
) -> Result<T, QuarkError> {
    let refresh_app = app.clone();
    run_initialized(
        move || {
            app.try_state::<Arc<CloudState>>()
                .map(|s| Arc::clone(s.inner()))
        },
        provider,
        move |state, http| {
            state.auth_supervisor.run_checked(
                &state.store,
                provider,
                kind,
                |snapshot| {
                    work(
                        &SessionWorkContext {
                            store: crate::cloud::driver::StoreRef(snapshot),
                            state,
                        },
                        http,
                    )
                },
                |genuine_auth_error| {
                    recover_authorization(&refresh_app, state, provider, http, genuine_auth_error)
                },
                check,
            )
        },
        std::time::Duration::from_secs(10),
        false,
    )
    .await
}

pub(crate) async fn run_share_session_read_work<T: Send + 'static>(
    app: AppHandle,
    provider: CloudProvider,
    work: impl FnMut(&SessionWorkContext<'_>, &ReqwestTransport) -> Result<T, QuarkError>
        + Send
        + 'static,
) -> Result<T, QuarkError> {
    run_session_work(app, provider, SessionWorkKind::Read, work).await
}

pub(crate) fn valid_name(name: &str) -> Result<(), QuarkError> {
    if name.trim().is_empty() || name.contains(['\r', '\n', '/', '\\']) {
        Err(QuarkError::InvalidInput("请输入有效的文件名称".into()))
    } else {
        Ok(())
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

pub(crate) fn is_windows_device_name(name: &str) -> bool {
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

/// Personal files lock their exact source. Share files lock the stable plan,
/// so new siblings, overwrite, refresh and cleanup cannot bypass each other
/// while the plan's entry set changes. The guard covers resolution through
/// aria2 rebinding, or cleanup claiming through the provider mutation.
#[derive(Default)]
pub struct CloudDownloadSourceGate {
    locks: Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>,
}

impl CloudDownloadSourceGate {
    /// Reclaim unowned source gates and return the number still in use.
    pub fn cleanup(&self) -> Result<usize, QuarkError> {
        let mut locks = self.locks.lock().map_err(|_| QuarkError::Storage)?;
        locks.retain(|_, lock| lock.strong_count() > 0);
        Ok(locks.len())
    }

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
            locks.retain(|_, lock| lock.strong_count() > 0);
            // Upgrade/create while holding the map lock. Both holders and
            // waiting acquires own an Arc, so cleanup cannot split one source
            // into two live mutexes. Never await the source under this lock.
            match locks.get(&key).and_then(std::sync::Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(tokio::sync::Mutex::new(()));
                    locks.insert(key, Arc::downgrade(&lock));
                    lock
                }
            }
        };
        Ok(lock.lock_owned().await)
    }
}

impl CloudState {
    /// All session consumers, including recovery and logout, share this provider boundary.
    pub fn with_provider<T>(
        &self,
        provider: CloudProvider,
        work: impl FnOnce(&DpapiCredentialStore) -> Result<T, QuarkError>,
    ) -> Result<T, QuarkError> {
        self.auth_supervisor
            .with_provider(provider, || work(&self.store))
    }
}
