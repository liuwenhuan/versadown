use serde::{Deserialize, Serialize};
use thiserror::Error;
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CloudProvider {
    Quark,
    Pikpak,
    Baidu,
    Tianyi,
    Aliyun,
    Xunlei,
    Uc,
}
impl CloudProvider {
    pub fn key(self) -> &'static str {
        match self {
            Self::Quark => "quark",
            Self::Pikpak => "pikpak",
            Self::Baidu => "baidu",
            Self::Tianyi => "tianyi",
            Self::Aliyun => "aliyun",
            Self::Xunlei => "xunlei",
            Self::Uc => "uc",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CloudAuthorizationCapabilities {
    pub provider: CloudProvider,
    pub web: bool,
    pub password: bool,
    pub password_login_mode: PasswordLoginMode,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PasswordLoginMode {
    None,
    RustDirect,
    OfficialWebView,
}

pub fn provider_capabilities(provider: CloudProvider) -> CloudAuthorizationCapabilities {
    let password_login_mode = match provider {
        CloudProvider::Pikpak | CloudProvider::Baidu | CloudProvider::Tianyi => {
            PasswordLoginMode::RustDirect
        }
        CloudProvider::Aliyun | CloudProvider::Xunlei | CloudProvider::Uc => {
            PasswordLoginMode::OfficialWebView
        }
        CloudProvider::Quark => PasswordLoginMode::None,
    };
    CloudAuthorizationCapabilities {
        provider,
        web: true,
        password: password_login_mode == PasswordLoginMode::RustDirect,
        password_login_mode,
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CloudAuthMethod {
    #[default]
    Web,
    Password,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudAccount {
    pub provider: CloudProvider,
    pub id: String,
    pub display_name: String,
    pub auth_method: CloudAuthMethod,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudFile {
    pub id: String,
    pub parent_id: String,
    pub name: String,
    pub kind: String,
    pub size: Option<u64>,
    pub modified_at: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CloudDirectory {
    pub id: String,
    pub items: Vec<CloudFile>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudAuthorization {
    pub provider: CloudProvider,
    pub window_label: String,
    pub instructions: String,
}
#[derive(Debug, Error)]
pub enum QuarkError {
    #[error("尚未授权此网盘，请登录后使用")]
    NotAuthorized,
    #[error("网盘会话已过期，请重新登录授权")]
    AuthorizationExpired,
    #[error("登录已自动恢复，请重新执行本次操作")]
    SessionRenewedRetryOperation,
    #[error("此登录方式需要网页验证，请点击网页授权完成登录")]
    WebAuthorizationRequired,
    #[error("网盘要求完成官方操作验证后重试")]
    CaptchaRequired,
    #[error("区域会员在当前 IP 地区下载受限；请切换到有权限 IP 后重新登录重试，或升级全球会员")]
    PikpakRegionalMembershipRestricted,
    #[error("正在等待网页登录，请在授权窗口完成登录")]
    AuthorizationPending,
    #[error("云盘响应无效，请刷新后重试")]
    InvalidResponse,
    #[error("安全凭据存储不可用")]
    Storage,
    #[error("网络请求失败：{0}")]
    Transport(String),
    #[error("网盘请求失败：{0}")]
    Provider(String),
    #[error("{0}")]
    InvalidInput(String),
    #[error("当前上传协议尚未接通，请使用网盘官方页面上传")]
    UploadUnavailable,
    #[error("当前网盘服务尚未接通，请稍后重试")]
    ProviderUnsupported,
}
impl Serialize for QuarkError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let code = match self {
            Self::NotAuthorized => "not_authorized",
            Self::AuthorizationExpired => "credentials_expired",
            Self::SessionRenewedRetryOperation => "session_renewed_retry_operation",
            Self::WebAuthorizationRequired => "web_auth_required",
            Self::CaptchaRequired => "web_auth_required",
            Self::PikpakRegionalMembershipRestricted => "pikpak_regional_membership_restricted",
            Self::AuthorizationPending => "authorization_pending",
            Self::Storage => "credential_storage_error",
            Self::InvalidResponse => "invalid_response",
            Self::Transport(_) => "network_error",
            Self::Provider(_) => "provider_error",
            Self::InvalidInput(_) => "invalid_input",
            Self::UploadUnavailable => "provider_unsupported",
            Self::ProviderUnsupported => "provider_unsupported",
        };
        serde_json::json!({"code":code,"message":self.to_string()}).serialize(s)
    }
}
pub(crate) fn required(v: &serde_json::Value, key: &str) -> Result<String, QuarkError> {
    v.get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or(QuarkError::InvalidResponse)
}
pub(crate) fn encoded(v: &str) -> String {
    url::form_urlencoded::byte_serialize(v.as_bytes()).collect()
}
