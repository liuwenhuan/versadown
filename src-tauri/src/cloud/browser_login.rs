use super::{CloudProvider, QuarkError};
use zeroize::Zeroizing;

#[derive(Default)]
pub(crate) struct PasswordDownloadRefreshBudget(
    std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
);

impl PasswordDownloadRefreshBudget {
    pub(crate) fn claim(&self, source: &str, error: Option<&str>) -> bool {
        // aria2 HTTP authorization failure. Do not repeatedly retry unrelated
        // network, disk, or permission errors.
        if error != Some("22") {
            return false;
        }
        let Ok((provider, _)) = super::commands::parse_stable_cloud_source(source) else {
            return false;
        };
        if !supports_browser_password(provider) {
            return false;
        }
        let Ok(mut attempts) = self.0.lock() else {
            return false;
        };
        attempts.retain(|_, at| at.elapsed() < std::time::Duration::from_secs(300));
        if attempts.contains_key(source) {
            return false;
        }
        attempts.insert(source.to_owned(), std::time::Instant::now());
        true
    }
}

pub(super) fn supports_browser_password(provider: CloudProvider) -> bool {
    !super::ProviderRegistry::global()
        .get(provider)
        .password_login_hosts
        .is_empty()
}

pub(super) fn trusted_login_url(provider: CloudProvider, url: &url::Url) -> bool {
    super::ProviderRegistry::global()
        .get(provider)
        .accepts_password_login_url(url)
}

pub(super) fn password_script(
    provider: CloudProvider,
    username: &str,
    password: &str,
) -> Result<Zeroizing<String>, QuarkError> {
    if !supports_browser_password(provider) || username.trim().is_empty() || password.is_empty() {
        return Err(QuarkError::InvalidInput("请输入账号和密码".into()));
    }
    let args = Zeroizing::new(
        serde_json::to_string(&(
            provider.key(),
            username,
            password,
            uuid::Uuid::new_v4().to_string(),
        ))
        .map_err(|_| QuarkError::Storage)?,
    );
    Ok(Zeroizing::new(format!(
        "{}(...{});",
        include_str!("password-login.js"),
        args.as_str()
    )))
}

pub(super) fn can_fallback_to_browser(error: &QuarkError) -> bool {
    matches!(
        error,
        QuarkError::WebAuthorizationRequired
            | QuarkError::AuthorizationExpired
            | QuarkError::NotAuthorized
            | QuarkError::InvalidResponse
            | QuarkError::Provider(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_download_refresh_is_provider_scoped_and_bounded() {
        let budget = PasswordDownloadRefreshBudget::default();
        assert!(!budget.claim("cloud://baidu/file", Some("9")));
        assert!(!budget.claim("cloud://quark/file", Some("22")));
        assert!(!budget.claim("https://example.org/file", Some("22")));
        assert!(budget.claim("cloud://baidu/file", Some("22")));
        assert!(!budget.claim("cloud://baidu/file", Some("22")));
        assert!(budget.claim("cloud://tianyi/file", Some("22")));
        budget.0.lock().unwrap().insert(
            "cloud://baidu/file".into(),
            std::time::Instant::now() - std::time::Duration::from_secs(301),
        );
        assert!(budget.claim("cloud://baidu/file", Some("22")));
    }

    #[test]
    fn credentials_only_enter_the_exact_official_https_origins() {
        for (provider, host) in [
            (CloudProvider::Baidu, "passport.baidu.com"),
            (CloudProvider::Tianyi, "open.e.189.cn"),
        ] {
            assert!(trusted_login_url(
                provider,
                &format!("https://{host}/login").parse().unwrap()
            ));
            assert!(!trusted_login_url(
                provider,
                &format!("http://{host}/login").parse().unwrap()
            ));
            assert!(!trusted_login_url(
                provider,
                &format!("https://{host}.example.org/login").parse().unwrap()
            ));
        }
        assert!(!trusted_login_url(
            CloudProvider::Baidu,
            &"https://open.e.189.cn".parse().unwrap()
        ));
    }

    #[test]
    fn browser_fallback_does_not_retry_a_captcha_or_storage_failure() {
        assert!(can_fallback_to_browser(
            &QuarkError::WebAuthorizationRequired
        ));
        assert!(!can_fallback_to_browser(&QuarkError::CaptchaRequired));
        assert!(!can_fallback_to_browser(&QuarkError::Storage));
    }
}
