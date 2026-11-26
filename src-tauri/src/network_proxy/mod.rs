//! One proxy decision for every outbound connection of the application.
//!
//! The settings offer three modes: follow the environment/system proxy
//! (default), a manual proxy, or a direct connection that ignores every
//! inherited proxy. A task may additionally pin its own [`TaskNetwork`].

mod discovery;
#[cfg(test)]
mod tests;

use std::{fmt, sync::RwLock};

use serde::{Deserialize, Serialize};

use crate::ProxySettings;
use discovery::Proxies;

/// The proxy one outbound connection must use.
#[derive(Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "url", rename_all = "camelCase")]
pub enum ProxyRoute {
    /// Whatever the environment (and, for yt-dlp, the OS) configures.
    #[default]
    System,
    /// The validated manual proxy URL from the settings.
    Manual(String),
    /// No proxy at all, even when the environment or OS configures one.
    Direct,
}

impl fmt::Debug for ProxyRoute {
    // The manual URL may carry the proxy password.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::System => "System",
            Self::Manual(_) => "Manual(<redacted>)",
            Self::Direct => "Direct",
        })
    }
}

impl ProxyRoute {
    pub fn manual(&self) -> Option<&str> {
        match self {
            Self::Manual(url) => Some(url),
            _ => None,
        }
    }
}

/// A task's own network choice. `Follow` defers to the global settings.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskNetwork {
    #[default]
    Follow,
    Direct,
    Proxy,
}

impl TaskNetwork {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Follow => "follow",
            Self::Direct => "direct",
            Self::Proxy => "proxy",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "follow" => Some(Self::Follow),
            "direct" => Some(Self::Direct),
            "proxy" => Some(Self::Proxy),
            _ => None,
        }
    }
}

const INVALID_MANUAL_PROXY: &str = "代理服务器地址或端口无效";

/// The manual proxy URL configured in the settings, whether or not the
/// manual mode is active. `None` when no address has been entered.
pub(crate) fn manual_proxy_url(proxy: &ProxySettings) -> Result<Option<String>, String> {
    if proxy.host.trim().is_empty() {
        return Ok(None);
    }
    if !matches!(proxy.protocol.as_str(), "http" | "https") {
        return Err("aria2 仅支持 HTTP/HTTPS 代理".into());
    }
    if proxy.host.contains("://") || proxy.port == 0 {
        return Err(INVALID_MANUAL_PROXY.into());
    }
    let host = proxy.host.trim();
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let mut value = url::Url::parse(&format!("{}://{}:{}", proxy.protocol, host, proxy.port))
        .map_err(|_| "代理服务器地址无效".to_string())?;
    if !proxy.username.is_empty() {
        value
            .set_username(&proxy.username)
            .map_err(|_| "代理用户名无效".to_string())?;
    }
    if !proxy.password.is_empty() {
        value
            .set_password(Some(&proxy.password))
            .map_err(|_| "代理密码无效".to_string())?;
    }
    Ok(Some(value.to_string()))
}

/// The route of the global settings: direct, manual or follow-system.
pub(crate) fn global_route(proxy: &ProxySettings) -> Result<ProxyRoute, String> {
    task_route(proxy, TaskNetwork::Follow)
}

/// The route one task uses, combining its own choice with the settings.
pub(crate) fn task_route(proxy: &ProxySettings, network: TaskNetwork) -> Result<ProxyRoute, String> {
    let manual = || {
        manual_proxy_url(proxy)?
            .map(ProxyRoute::Manual)
            .ok_or_else(|| INVALID_MANUAL_PROXY.to_string())
    };
    match network {
        TaskNetwork::Direct => Ok(ProxyRoute::Direct),
        TaskNetwork::Proxy => manual(),
        TaskNetwork::Follow if proxy.direct => Ok(ProxyRoute::Direct),
        TaskNetwork::Follow if proxy.enabled => manual(),
        TaskNetwork::Follow => Ok(ProxyRoute::System),
    }
}

static PUBLISHED: RwLock<Option<ProxySettings>> = RwLock::new(None);

/// Publish the saved settings to consumers that do not hold the settings
/// themselves: shared HTTP clients and the download engine's resumes.
pub(crate) fn publish_settings(proxy: &ProxySettings) {
    *PUBLISHED.write().unwrap_or_else(|error| error.into_inner()) = Some(proxy.clone());
}

fn published() -> ProxySettings {
    PUBLISHED
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .unwrap_or_default()
}

/// Whether the global settings demand a direct connection.
pub(crate) fn global_direct() -> bool {
    published().direct
}

/// The route a task uses under the currently published settings.
pub(crate) fn published_task_route(network: TaskNetwork) -> Result<ProxyRoute, String> {
    task_route(&published(), network)
}

/// Apply the global direct mode to an application HTTP client. Other modes
/// keep the client's existing proxy behaviour.
pub(crate) fn client_builder(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    if global_direct() {
        builder.no_proxy()
    } else {
        builder
    }
}

/// Blocking counterpart of [`client_builder`].
pub(crate) fn blocking_client_builder(
    builder: reqwest::blocking::ClientBuilder,
) -> reqwest::blocking::ClientBuilder {
    if global_direct() {
        builder.no_proxy()
    } else {
        builder
    }
}

/// The proxies a child process actually received for one run.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AppliedProxy {
    proxies: Proxies,
}

impl AppliedProxy {
    /// Whether a request to `url` went through a proxy, honouring `no_proxy`.
    pub fn proxied_for(&self, url: &str) -> bool {
        let Ok(url) = url::Url::parse(url) else {
            return false;
        };
        let Some(host) = url.host_str() else {
            return false;
        };
        let scheme = url.scheme().to_ascii_lowercase();
        if !self.proxies.contains_key(&scheme) && !self.proxies.contains_key("all") {
            return false;
        }
        !self
            .proxies
            .get("no")
            .is_some_and(|rules| bypasses(rules, &host.to_ascii_lowercase()))
    }

    /// A server rejection seen through a proxy gets the proxy-specific hint.
    pub fn diagnose(&self, key: &str, url: &str) -> String {
        if key == "video.error.accessDenied" && self.proxied_for(url) {
            "video.error.accessDeniedProxy".into()
        } else {
            key.into()
        }
    }
}

fn bypasses(rules: &str, host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    rules.split(',').map(str::trim).any(|rule| {
        if rule == "*" {
            return true;
        }
        let rule = rule.trim_start_matches("*.").trim_start_matches('.');
        // `host:port` rules; an IPv6 literal keeps its colons.
        let rule = match rule.rsplit_once(':') {
            Some((name, port)) if !name.contains(':') && port.parse::<u16>().is_ok() => name,
            _ => rule,
        };
        let rule = rule.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
        !rule.is_empty() && (host == rule || host.ends_with(&format!(".{rule}")))
    })
}

/// Configure the proxy environment of a yt-dlp process (and its aria2 and
/// ffmpeg children, which inherit it) for `route`.
pub(crate) fn apply_to_command(
    cmd: &mut tokio::process::Command,
    route: &ProxyRoute,
) -> Result<AppliedProxy, String> {
    configure_command(
        cmd,
        route,
        discovery::environment_keys(),
        discovery::process_environment(),
        discovery::system_proxies,
    )
}

fn configure_command(
    cmd: &mut tokio::process::Command,
    route: &ProxyRoute,
    inherited_keys: Vec<std::ffi::OsString>,
    environment: Proxies,
    system: impl FnOnce() -> Proxies,
) -> Result<AppliedProxy, String> {
    let applied = child_proxies(route, environment, system)?;
    for key in inherited_keys {
        cmd.env_remove(key);
    }
    // --proxy would discard yt-dlp's no_proxy rules and collapse per-protocol
    // settings, so routes are expressed through the environment. Only direct
    // needs the flag: without it urllib falls back to the OS proxy settings.
    if *route == ProxyRoute::Direct {
        cmd.args(["--proxy", ""]);
    }
    for (scheme, proxy) in &applied.proxies {
        cmd.env(format!("{scheme}_proxy"), proxy);
    }
    Ok(applied)
}

fn child_proxies(
    route: &ProxyRoute,
    environment: Proxies,
    system: impl FnOnce() -> Proxies,
) -> Result<AppliedProxy, String> {
    let proxies = match route {
        ProxyRoute::Direct => Proxies::new(),
        ProxyRoute::Manual(url) => discovery::resolve(Some(url.as_str()), environment, system)?,
        ProxyRoute::System => discovery::resolve(None, environment, system)?,
    };
    Ok(AppliedProxy { proxies })
}

/// The download engine never inherits proxy variables: aria2 gives them
/// precedence over an empty per-download `all-proxy`, so a direct download
/// would otherwise still use them. Each download states its route instead.
pub(crate) fn strip_inherited(cmd: &mut std::process::Command) {
    strip_keys(cmd, discovery::environment_keys());
}

fn strip_keys(cmd: &mut std::process::Command, keys: Vec<std::ffi::OsString>) {
    for key in keys {
        cmd.env_remove(key);
    }
}

/// Per-download aria2 options expressing `route`.
pub(crate) fn aria2_options(route: &ProxyRoute) -> Vec<(&'static str, String)> {
    aria2_options_from(route, discovery::process_environment())
}

/// Every proxy option is stated, empty when unused: `changeOption` keeps
/// values from an earlier submission, and a per-protocol proxy would
/// override `all-proxy`. aria2-next has no `ftp-proxy` (`changeOption`
/// rejects it); its FTP transfers use `all-proxy`.
fn aria2_options_from(route: &ProxyRoute, environment: Proxies) -> Vec<(&'static str, String)> {
    const OPTIONS: [(&str, &str); 4] = [
        ("http", "http-proxy"),
        ("https", "https-proxy"),
        ("all", "all-proxy"),
        ("no", "no-proxy"),
    ];
    OPTIONS
        .into_iter()
        .map(|(scheme, option)| {
            let value = match route {
                ProxyRoute::Direct => None,
                ProxyRoute::Manual(url) => (scheme == "all").then(|| url.clone()),
                // What aria2 itself read from its environment before it was stripped.
                ProxyRoute::System => environment.get(scheme).cloned(),
            };
            (option, value.unwrap_or_default())
        })
        .collect()
}
