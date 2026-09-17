use super::super::browser::password_login_url;
use super::super::password_login_with_browser;
use super::*;
use crate::cloud::{
    browser_login, load_saved_password_login, prepare_password_account_login,
    reset_password_profile_if_required, PasswordLoginMode,
};

#[test]
fn xunlei_advertises_official_webview_password_login_on_exact_official_hosts() {
    let descriptor = crate::cloud::ProviderRegistry::global().get(CloudProvider::Xunlei);
    assert!(descriptor.authorization.password);
    assert_eq!(
        descriptor.authorization.password_login_mode,
        PasswordLoginMode::OfficialWebView
    );
    for url in [
        "https://pan.xunlei.com/",
        "https://i.xunlei.com/xluser/login.html",
    ] {
        assert!(browser_login::trusted_login_url(
            CloudProvider::Xunlei,
            &url.parse().unwrap()
        ));
    }
    for url in [
        "http://i.xunlei.com/xluser/login.html",
        "https://evil.i.xunlei.com/xluser/login.html",
        "https://i.xunlei.com.evil.test/xluser/login.html",
    ] {
        assert!(!browser_login::trusted_login_url(
            CloudProvider::Xunlei,
            &url.parse().unwrap()
        ));
    }
}

#[test]
fn xunlei_password_login_starts_at_the_cloud_parent_for_its_official_sso_callback() {
    assert_eq!(
        password_login_url(CloudProvider::Xunlei),
        "https://pan.xunlei.com"
    );
}

#[test]
fn xunlei_password_login_enters_the_official_browser_flow() {
    let store = MemoryCredentialStore::default();
    let http = XunleiRootAcceptingTransport(Mutex::new(Vec::new()));
    let browser_calls = Cell::new(0);

    let error = password_login_with_browser(
        CloudProvider::Xunlei,
        &store,
        crate::cloud::ProviderRegistry::global()
            .driver(CloudProvider::Xunlei, &store, &http)
            .auth(),
        "fixture-user",
        "fixture-password",
        |_| {
            browser_calls.set(browser_calls.get() + 1);
            Err(QuarkError::Storage)
        },
    )
    .unwrap_err();

    assert!(matches!(error, QuarkError::Storage));
    assert_eq!(browser_calls.get(), 1);
}

#[test]
fn xunlei_password_credentials_and_webview_profile_are_provider_isolated() {
    let store = MemoryCredentialStore::default();
    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Xunlei, "xl-1", "alice", "secret"),
    )
    .unwrap();
    assert_eq!(
        load_saved_password_login(&store, CloudProvider::Xunlei)
            .unwrap()
            .unwrap()
            .username(),
        "alice"
    );
    let root = std::path::Path::new("C:/profiles");
    assert_eq!(
        password_webview_data_directory_from_root(root, CloudProvider::Xunlei).unwrap(),
        root.join("web-auth").join("xunlei")
    );
}

#[test]
fn password_command_encrypts_the_submitted_draft_before_opening_the_browser() {
    let source = include_str!("auth.rs")
        .split("pub async fn cloud_login_password")
        .nth(1)
        .expect("password command should exist");
    let draft = source
        .find("save_password_login_draft(&state.store, provider")
        .expect("password input should be encrypted before browser validation");
    let browser = source
        .find("password_login_with_browser(")
        .expect("password command should enter the provider login flow");
    assert!(draft < browser);
}

struct PostLoginFailure {
    stage: &'static str,
    error: Option<QuarkError>,
    stages: Vec<&'static str>,
}

impl crate::cloud::driver::ProviderAuth for PostLoginFailure {
    fn login_password(
        &mut self,
        _username: &str,
        _password: &str,
    ) -> Result<crate::cloud::driver::NativeLoginOutcome, QuarkError> {
        self.stages.push("native-login-success");
        crate::cloud::driver::complete_native_password_login(Ok(()), |_| {
            self.stages.push("account");
            if self.stage == "root" {
                self.stages.push("root");
            }
            Err(self.error.take().unwrap())
        })
    }
}

fn assert_post_login_failure_does_not_open_browser(provider: CloudProvider, stage: &'static str) {
    for error in [
        QuarkError::InvalidResponse,
        QuarkError::Provider("post-login-validation".into()),
        QuarkError::CaptchaRequired,
    ] {
        let store = MemoryCredentialStore::default();
        set_browser_password_login_preferred(&store, provider, false).unwrap();
        let expected_error = serde_json::to_value(&error).unwrap();
        let expected_code = cloud_failure_code(&error);
        let mut auth = PostLoginFailure {
            stage,
            error: Some(error),
            stages: vec![],
        };
        let browser_calls = Cell::new(0);
        let result = password_login_with_browser(
            provider,
            &store,
            &mut auth,
            "fixture-user",
            "fixture-password",
            |_| {
                browser_calls.set(browser_calls.get() + 1);
                Err(QuarkError::Storage)
            },
        );
        assert_eq!(auth.stages.first(), Some(&"native-login-success"));
        assert_eq!(auth.stages.last(), Some(&stage));
        let error = result.unwrap_err();
        assert_eq!(
            browser_calls.get(),
            0,
            "{provider:?}/{stage}: browser host was called"
        );
        assert_eq!(cloud_failure_code(&error), expected_code);
        assert_eq!(serde_json::to_value(error).unwrap(), expected_error);
        assert!(!browser_password_login_preferred(&store, provider).unwrap());
    }
}

#[test]
fn baidu_post_login_account_failure_does_not_open_browser() {
    assert_post_login_failure_does_not_open_browser(CloudProvider::Baidu, "account");
}

#[test]
fn baidu_post_login_root_failure_does_not_open_browser() {
    assert_post_login_failure_does_not_open_browser(CloudProvider::Baidu, "root");
}

#[test]
fn tianyi_post_login_account_failure_does_not_open_browser() {
    assert_post_login_failure_does_not_open_browser(CloudProvider::Tianyi, "account");
}

#[test]
fn tianyi_post_login_root_failure_does_not_open_browser() {
    assert_post_login_failure_does_not_open_browser(CloudProvider::Tianyi, "root");
}

struct NativeLoginFailure(Option<QuarkError>);

#[test]
fn failed_browser_account_switch_forces_reset_before_reusing_saved_login() {
    for provider in [CloudProvider::Baidu] {
        let store = MemoryCredentialStore::default();
        save_password_credential(
            &store,
            CloudPasswordCredential::new(provider, "a", "alice", "secret-a"),
        )
        .unwrap();
        let reset = prepare_password_account_login(&store, provider, "bob").unwrap();
        reset_password_profile_if_required(&store, provider, reset, || Ok(())).unwrap();
        let result = password_login_with_browser(
            provider,
            &store,
            &mut NativeLoginFailure(Some(QuarkError::WebAuthorizationRequired)),
            "bob",
            "secret-b",
            |_| {
                Err(QuarkError::Transport(
                    "post-cookie validation failed".into(),
                ))
            },
        );
        assert!(result.is_err());
        assert_eq!(
            load_saved_password_login(&store, provider)
                .unwrap()
                .unwrap()
                .username(),
            "alice"
        );
        assert!(
            prepare_password_account_login(&store, provider, "alice").unwrap(),
            "a failed browser attempt may have installed Bob's cookies"
        );
    }
}

impl crate::cloud::driver::ProviderAuth for NativeLoginFailure {
    fn login_password(
        &mut self,
        _username: &str,
        _password: &str,
    ) -> Result<crate::cloud::driver::NativeLoginOutcome, QuarkError> {
        crate::cloud::driver::complete_native_password_login::<()>(
            Err(self.0.take().unwrap()),
            |_| panic!("a failed native login must not reach account/root validation"),
        )
    }
}

#[test]
fn native_login_recoverable_errors_open_browser_once() {
    for provider in [CloudProvider::Baidu, CloudProvider::Tianyi] {
        for error in [
            QuarkError::WebAuthorizationRequired,
            QuarkError::AuthorizationExpired,
            QuarkError::NotAuthorized,
            QuarkError::InvalidResponse,
            QuarkError::Provider("native-login".into()),
            QuarkError::CaptchaRequired,
        ] {
            let captcha = matches!(error, QuarkError::CaptchaRequired);
            let store = MemoryCredentialStore::default();
            set_browser_password_login_preferred(&store, provider, false).unwrap();
            let mut auth = NativeLoginFailure(Some(error));
            let browser_calls = Cell::new(0);
            let account = password_login_with_browser(
                provider,
                &store,
                &mut auth,
                "user",
                "password",
                |actual_captcha| {
                    browser_calls.set(browser_calls.get() + 1);
                    assert_eq!(actual_captcha, captcha);
                    Ok(crate::cloud::CloudAccount {
                        provider,
                        id: "browser-account".into(),
                        display_name: "fixture".into(),
                        auth_method: crate::cloud::CloudAuthMethod::Password,
                    })
                },
            )
            .unwrap();
            assert_eq!(account.id, "browser-account");
            assert_eq!(browser_calls.get(), 1);
            assert_eq!(
                browser_password_login_preferred(&store, provider).unwrap(),
                provider == CloudProvider::Baidu,
            );
        }
    }
}

#[test]
fn native_login_nonrecoverable_errors_do_not_open_browser() {
    for provider in [CloudProvider::Baidu, CloudProvider::Tianyi] {
        for error in [
            QuarkError::Storage,
            QuarkError::Transport("native-transport".into()),
            QuarkError::InvalidInput("native-input".into()),
        ] {
            let store = MemoryCredentialStore::default();
            let expected = serde_json::to_value(&error).unwrap();
            let mut auth = NativeLoginFailure(Some(error));
            let error = password_login_with_browser(
                provider,
                &store,
                &mut auth,
                "user",
                "password",
                |_| panic!("nonrecoverable native error opened browser"),
            )
            .unwrap_err();
            assert_eq!(serde_json::to_value(error).unwrap(), expected);
            assert!(!browser_password_login_preferred(&store, provider).unwrap());
        }
    }
}

struct FailPostLoginTransport {
    native: SessionRecoveryTransport,
    successful_requests: usize,
    calls: Mutex<usize>,
    failure: Mutex<Option<QuarkError>>,
}

#[derive(Default)]
struct LoginStore {
    inner: MemoryCredentialStore,
    mutations: Mutex<Vec<String>>,
}

impl CredentialStore for LoginStore {
    fn save(&self, key: &str, bytes: &[u8]) -> Result<(), QuarkError> {
        self.mutations.lock().unwrap().push(format!("save:{key}"));
        self.inner.save(key, bytes)
    }
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, QuarkError> {
        self.inner.load(key)
    }
    fn remove(&self, key: &str) -> Result<(), QuarkError> {
        self.mutations.lock().unwrap().push(format!("remove:{key}"));
        self.inner.remove(key)
    }
}

struct SessionSaveFailureStore {
    store: LoginStore,
    provider: CloudProvider,
    error: Mutex<Option<QuarkError>>,
}

impl CredentialStore for SessionSaveFailureStore {
    fn save(&self, key: &str, bytes: &[u8]) -> Result<(), QuarkError> {
        if key == self.provider.key() {
            self.store
                .mutations
                .lock()
                .unwrap()
                .push(format!("save:{key}"));
            return Err(self
                .error
                .lock()
                .unwrap()
                .take()
                .expect("session save retried"));
        }
        self.store.save(key, bytes)
    }
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, QuarkError> {
        self.store.load(key)
    }
    fn remove(&self, key: &str) -> Result<(), QuarkError> {
        self.store.remove(key)
    }
}

fn assert_native_driver_session_save_failure(
    provider: CloudProvider,
    native: impl Fn() -> SessionRecoveryTransport,
    root_body: &str,
) {
    for error in [
        QuarkError::Storage,
        QuarkError::Provider("session-save-fixture".into()),
        QuarkError::CaptchaRequired,
    ] {
        let expected = serde_json::to_value(&error).unwrap();
        let store = SessionSaveFailureStore {
            store: LoginStore::default(),
            provider,
            error: Mutex::new(Some(error)),
        };
        store
            .store
            .inner
            .save(provider.key(), b"previous-session")
            .unwrap();
        let http = native();
        http.0.lock().unwrap().push_back(CloudHttpResponse {
            status: 200,
            body: root_body.into(),
            ..Default::default()
        });
        let browser_calls = Cell::new(0);
        let error = password_login_with_browser(
            provider,
            &store,
            crate::cloud::ProviderRegistry::global()
                .driver(provider, &store, &http)
                .auth(),
            "alice@fixture.test",
            "fixture-password",
            |_| {
                browser_calls.set(browser_calls.get() + 1);
                Err(QuarkError::InvalidResponse)
            },
        )
        .unwrap_err();
        assert_eq!(
            http.remaining(),
            0,
            "login/account/root must complete before save failure"
        );
        assert_eq!(browser_calls.get(), 0);
        assert_eq!(serde_json::to_value(error).unwrap(), expected);
        assert_eq!(
            store.load(provider.key()).unwrap().as_deref(),
            Some(b"previous-session".as_slice()),
        );
        assert!(!browser_password_login_preferred(&store, provider).unwrap());
        assert_eq!(
            *store.store.mutations.lock().unwrap(),
            [format!("save:{}", provider.key())],
            "failed session save must not write or remove browser preference",
        );
    }
}

#[test]
fn baidu_native_driver_session_save_failure_has_no_preference_side_effect() {
    assert_native_driver_session_save_failure(
        CloudProvider::Baidu,
        || SessionRecoveryTransport::baidu_password_relogin("account-1"),
        r#"{"errno":0,"list":[]}"#,
    );
}

#[test]
fn tianyi_native_driver_session_save_failure_has_no_preference_side_effect() {
    assert_native_driver_session_save_failure(
        CloudProvider::Tianyi,
        tianyi_native_login_fixture,
        r#"<listFiles><fileList></fileList></listFiles>"#,
    );
}

impl CloudHttpTransport for FailPostLoginTransport {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        let mut calls = self.calls.lock().unwrap();
        *calls += 1;
        if *calls == self.successful_requests + 1 {
            return Err(self.failure.lock().unwrap().take().unwrap());
        }
        self.native.send(request)
    }
}

fn assert_native_driver_post_login_error(
    provider: CloudProvider,
    successful_requests: usize,
    native: impl Fn() -> SessionRecoveryTransport,
) {
    for error in [
        QuarkError::InvalidResponse,
        QuarkError::Provider("driver-post-validation".into()),
        QuarkError::CaptchaRequired,
    ] {
        let store = LoginStore::default();
        store.save(provider.key(), b"previous-session").unwrap();
        set_browser_password_login_preferred(&store, provider, false).unwrap();
        store.mutations.lock().unwrap().clear();
        let expected = serde_json::to_value(&error).unwrap();
        let http = FailPostLoginTransport {
            native: native(),
            successful_requests,
            calls: Mutex::new(0),
            failure: Mutex::new(Some(error)),
        };
        let browser_calls = Cell::new(0);
        let error = password_login_with_browser(
            provider,
            &store,
            crate::cloud::ProviderRegistry::global()
                .driver(provider, &store, &http)
                .auth(),
            "alice@fixture.test",
            "fixture-password",
            |_| {
                browser_calls.set(browser_calls.get() + 1);
                Err(QuarkError::Storage)
            },
        )
        .unwrap_err();
        assert_eq!(*http.calls.lock().unwrap(), successful_requests + 1);
        assert_eq!(browser_calls.get(), 0);
        assert_eq!(serde_json::to_value(error).unwrap(), expected);
        assert!(!browser_password_login_preferred(&store, provider).unwrap());
        assert_eq!(
            store.load(provider.key()).unwrap().as_deref(),
            Some(b"previous-session".as_slice())
        );
        assert!(
            store.mutations.lock().unwrap().is_empty(),
            "validation failure wrote credentials or browser preference"
        );
    }
}

#[test]
fn baidu_native_driver_post_login_account_error_preserves_session() {
    assert_native_driver_post_login_error(CloudProvider::Baidu, 9, || {
        SessionRecoveryTransport::baidu_password_relogin("account-1")
    });
}

#[test]
fn baidu_native_driver_post_login_root_error_preserves_session() {
    assert_native_driver_post_login_error(CloudProvider::Baidu, 10, || {
        SessionRecoveryTransport::baidu_password_relogin("account-1")
    });
}

#[test]
fn tianyi_native_driver_post_login_root_error_preserves_session() {
    assert_native_driver_post_login_error(CloudProvider::Tianyi, 5, tianyi_native_login_fixture);
}

fn tianyi_native_login_fixture() -> SessionRecoveryTransport {
    const RSA_KEY: &str = concat!(
        "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDY7mpaUysvgQkbp0iIn2ezoUyh",
        "i1zPFn0HCXloLFWT7uoNkqtrphpQ/63LEcPz1VYzmDuDIf3iGxQKzeoHTiVMSmW6",
        "FlhDeqVOG094hFJvZeK4OzA6HVwzwnEW5vIZ7d+u61RV1bsFxmB68+8JXs3ycGcE",
        "4anY+YzZJcyOcEGKVQIDAQAB",
    );
    let bodies = [
            format!(r#"<input id="captchaToken" value="captcha-token">
                <script>var lt = "login-ticket"; var returnUrl = 'https://m.cloud.189.cn/return'; var paramId = "param-id"; var reqId = "request-id";</script>
                <input id="j_rsaKey" value="{RSA_KEY}">"#),
            r#"{"result":0,"toUrl":"https://open.e.189.cn/callback?ticket=opaque"}"#.into(),
            r#"{"res_code":0,"loginName":"alice@fixture.test","sessionKey":"app-session-key","sessionSecret":"app-session-secret","accessToken":"access-token"}"#.into(),
            String::new(),
            r#"<listFiles><fileList></fileList></listFiles>"#.into(),
        ];
    SessionRecoveryTransport(Mutex::new(
        bodies
            .into_iter()
            .map(|body| CloudHttpResponse {
                status: 200,
                body,
                ..Default::default()
            })
            .collect(),
    ))
}

#[test]
fn native_driver_login_success_validates_saves_and_skips_browser() {
    for (provider, http, root_body) in [
        (
            CloudProvider::Baidu,
            SessionRecoveryTransport::baidu_password_relogin("account-1"),
            r#"{"errno":0,"list":[]}"#,
        ),
        (
            CloudProvider::Tianyi,
            tianyi_native_login_fixture(),
            r#"<listFiles><fileList></fileList></listFiles>"#,
        ),
    ] {
        http.0.lock().unwrap().push_back(CloudHttpResponse {
            status: 200,
            body: root_body.into(),
            ..Default::default()
        });
        let store = MemoryCredentialStore::default();
        let account = password_login_with_browser(
            provider,
            &store,
            crate::cloud::ProviderRegistry::global()
                .driver(provider, &store, &http)
                .auth(),
            "alice@fixture.test",
            "fixture-password",
            |_| panic!("successful native validation must not open browser"),
        )
        .unwrap();
        assert_eq!(account.provider, provider);
        assert_eq!(account.auth_method, crate::cloud::CloudAuthMethod::Password);
        assert!(store.load(provider.key()).unwrap().is_some());
        assert!(!browser_password_login_preferred(&store, provider).unwrap());
        assert_eq!(http.remaining(), 0);
    }
}
