use super::{CloudProvider, QuarkError};
use zeroize::Zeroizing;

#[derive(Default)]
pub(crate) struct CloudDownloadRefreshBudget(
    std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
);

impl CloudDownloadRefreshBudget {
    pub(crate) fn claim(&self, source: &str, error: Option<&str>) -> bool {
        // aria2 HTTP transfer failure. A temporary cloud URL may have expired,
        // or its previous validator may no longer match after it is re-signed.
        // Re-resolve every valid personal-cloud source once, but do not retry
        // unrelated network, disk, or permission errors.
        if error != Some("22") {
            return false;
        }
        if super::commands::parse_stable_cloud_source(source).is_err() {
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
    fn personal_cloud_download_refresh_is_provider_scoped_and_bounded() {
        let budget = CloudDownloadRefreshBudget::default();
        assert!(!budget.claim("cloud://baidu/file", Some("9")));
        assert!(!budget.claim("https://example.org/file", Some("22")));
        for provider in [
            "quark", "pikpak", "baidu", "tianyi", "aliyun", "xunlei", "uc",
        ] {
            let source = format!("cloud://{provider}/file");
            assert!(
                budget.claim(&source, Some("22")),
                "{provider} should re-sign once"
            );
            assert!(
                !budget.claim(&source, Some("22")),
                "{provider} retry must stay bounded"
            );
        }
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
