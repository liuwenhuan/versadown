use super::{CloudAuthorizationCapabilities, CloudProvider, PasswordLoginMode};
use serde::Serialize;
use std::sync::OnceLock;
use url::Url;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub list: bool,
    pub create_directory: bool,
    pub rename: bool,
    pub r#move: bool,
    pub copy: bool,
    pub trash: bool,
    pub delete: bool,
    pub restore: bool,
    pub download: bool,
    pub upload: bool,
    pub share_inspect: bool,
    pub share_transfer: bool,
    pub recorded_transfer_resolve: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDescriptor {
    pub provider: CloudProvider,
    pub display_name: &'static str,
    pub share_display_name: &'static str,
    pub share_directory_name: &'static str,
    pub root_directory_id: &'static str,
    pub recycle_bin_directory_id: &'static str,
    pub authorization: CloudAuthorizationCapabilities,
    pub authorization_url: &'static str,
    pub password_login_hosts: &'static [&'static str],
    pub capabilities: ProviderCapabilities,
    /// Exact hosts: suffix and query-string matches are never trusted.
    pub share_hosts: &'static [&'static str],
    /// URL path prefixes understood by the existing provider share protocols.
    pub share_patterns: &'static [&'static str],
    /// Legacy candidate classification does not authorize share adapter requests.
    #[serde(skip)]
    candidate_aliases: &'static [(&'static str, &'static str)],
    pub max_upload_concurrency: usize,
}

pub type ProviderDescriptorDto = ProviderDescriptor;

impl ProviderDescriptor {
    pub fn accepts_password_login_url(&self, url: &Url) -> bool {
        url.scheme() == "https"
            && url.port_or_known_default() == Some(443)
            && url
                .host_str()
                .is_some_and(|host| self.password_login_hosts.contains(&host))
    }

    pub fn accepts_share_url(&self, url: &Url) -> bool {
        matches!(url.scheme(), "http" | "https")
            && url
                .host_str()
                .is_some_and(|host| self.share_hosts.contains(&host))
            && self
                .share_patterns
                .iter()
                .any(|prefix| url.path().starts_with(prefix))
    }
}

pub struct ProviderRegistry {
    descriptors: Vec<ProviderDescriptor>,
}

impl ProviderRegistry {
    pub fn global() -> &'static Self {
        static REGISTRY: OnceLock<ProviderRegistry> = OnceLock::new();
        REGISTRY.get_or_init(|| Self {
            descriptors: CloudProvider::ALL.into_iter().map(descriptor).collect(),
        })
    }

    pub fn all(&self) -> &[ProviderDescriptor] {
        &self.descriptors
    }

    pub fn get(&self, provider: CloudProvider) -> &ProviderDescriptor {
        self.descriptors
            .iter()
            .find(|d| d.provider == provider)
            .expect("every CloudProvider has a descriptor")
    }

    pub fn catalog(&self) -> Vec<ProviderDescriptorDto> {
        self.descriptors.clone()
    }

    pub fn for_share_url(&self, url: &Url) -> Option<&ProviderDescriptor> {
        self.descriptors.iter().find(|d| d.accepts_share_url(url))
    }

    pub fn for_candidate_url(&self, url: &Url) -> Option<&ProviderDescriptor> {
        self.for_share_url(url).or_else(|| {
            if !matches!(url.scheme(), "https" | "http") {
                return None;
            }
            self.descriptors.iter().find(|d| {
                d.candidate_aliases.iter().any(|(host, prefix)| {
                    url.host_str() == Some(*host) && url.path().starts_with(prefix)
                })
            })
        })
    }

    pub fn by_key(&self, key: &str) -> Option<&ProviderDescriptor> {
        self.descriptors.iter().find(|d| d.provider.key() == key)
    }
}

fn descriptor(provider: CloudProvider) -> ProviderDescriptor {
    use CloudProvider::*;
    use PasswordLoginMode::*;
    let (
        display_name,
        root_directory_id,
        recycle_bin_directory_id,
        password_login_mode,
        authorization_url,
        share_hosts,
        share_patterns,
    ): (
        _,
        _,
        _,
        _,
        _,
        &'static [&'static str],
        &'static [&'static str],
    ) = match provider {
        Quark => (
            "夸克",
            "0",
            super::QUARK_RECYCLE_BIN_ID,
            None,
            "https://pan.quark.cn",
            &["pan.quark.cn"],
            &["/s/"],
        ),
        Pikpak => (
            "PikPak",
            "0",
            super::PIKPAK_RECYCLE_BIN_ID,
            RustDirect,
            "https://mypikpak.com/drive",
            &["mypikpak.com", "www.mypikpak.com"],
            &["/s/"],
        ),
        Baidu => (
            "百度",
            "/",
            super::baidu::BAIDU_RECYCLE_BIN_ID,
            RustDirect,
            "https://pan.baidu.com",
            &["pan.baidu.com"],
            &["/s/"],
        ),
        Tianyi => (
            "天翼",
            "-11",
            super::tianyi::TIANYI_RECYCLE_BIN_ID,
            RustDirect,
            "https://cloud.189.cn",
            &["cloud.189.cn"],
            &["/t/"],
        ),
        Aliyun => (
            "阿里",
            "root",
            super::aliyun::ALIYUN_RECYCLE_BIN_ID,
            OfficialWebView,
            "https://www.aliyundrive.com/drive",
            &[
                "alipan.com",
                "www.alipan.com",
                "aliyundrive.com",
                "www.aliyundrive.com",
            ],
            &["/s/"],
        ),
        Xunlei => (
            "迅雷",
            "0",
            super::xunlei::XUNLEI_RECYCLE_BIN_ID,
            OfficialWebView,
            "https://pan.xunlei.com",
            &["pan.xunlei.com"],
            &["/s/"],
        ),
        Uc => (
            "UC",
            "0",
            super::uc::UC_RECYCLE_BIN_ID,
            OfficialWebView,
            "https://drive.uc.cn",
            &["drive.uc.cn"],
            &["/s/"],
        ),
    };
    ProviderDescriptor {
        provider,
        display_name,
        root_directory_id,
        recycle_bin_directory_id,
        authorization_url,
        share_display_name: match provider {
            Quark => "夸克网盘",
            Pikpak => "PikPak",
            Baidu => "百度网盘",
            Tianyi => "天翼云盘",
            Aliyun => "阿里云盘",
            Xunlei => "迅雷网盘",
            Uc => "UC 网盘",
        },
        share_directory_name: match provider {
            Quark => "夸克分享目录",
            Pikpak => "PikPak分享目录",
            Baidu => "百度网盘分享目录",
            Tianyi => "天翼云盘分享目录",
            Aliyun => "阿里云盘分享目录",
            Xunlei => "迅雷云盘分享目录",
            Uc => "UC网盘分享目录",
        },
        password_login_hosts: match provider {
            Baidu => &["pan.baidu.com", "passport.baidu.com"],
            Tianyi => &["cloud.189.cn", "open.e.189.cn"],
            _ => &[],
        },
        candidate_aliases: match provider {
            Quark => &[("pan.quark.cn", "/"), ("quark.cn", "/s/")],
            Pikpak => &[
                ("mypikpak.com", "/"),
                ("www.mypikpak.com", "/"),
                ("pikpak.me", "/"),
            ],
            _ => &[],
        },
        authorization: CloudAuthorizationCapabilities {
            provider,
            web: true,
            password: password_login_mode == RustDirect,
            password_login_mode,
        },
        capabilities: ProviderCapabilities {
            list: true,
            create_directory: true,
            rename: true,
            r#move: true,
            copy: true,
            trash: true,
            delete: true,
            restore: true,
            download: true,
            upload: true,
            share_inspect: true,
            share_transfer: matches!(provider, Quark | Baidu | Uc | Xunlei),
            recorded_transfer_resolve: matches!(provider, Quark | Baidu | Uc | Xunlei),
        },
        share_hosts,
        share_patterns,
        max_upload_concurrency: 2,
    }
}
