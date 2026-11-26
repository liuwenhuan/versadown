//! Discovery of the proxies a child process would inherit: environment
//! variables (with urllib precedence) and, on macOS/Windows, the OS settings.

use std::collections::BTreeMap;

/// Scheme (`http`, `https`, `all`, `no`, ...) to proxy value.
pub(super) type Proxies = BTreeMap<String, String>;

/// Names of every proxy variable in this process, in their original case.
pub(super) fn environment_keys() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| is_proxy_variable(&key.to_string_lossy()))
        .collect()
}

pub(super) fn is_proxy_variable(key: &str) -> bool {
    key.to_ascii_lowercase().ends_with("_proxy")
}

/// The proxy variables of this process, keyed by lower-case scheme.
pub(super) fn process_environment() -> Proxies {
    environment_proxies(
        std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?))),
    )
}

pub(super) fn resolve(
    manual: Option<&str>,
    mut proxies: Proxies,
    system: impl FnOnce() -> Proxies,
) -> Result<Proxies, String> {
    if let Some(proxy) = manual {
        proxies.insert("http".into(), proxy.into());
        proxies.insert("https".into(), proxy.into());
        proxies.remove("all");
    }
    // Match urllib/yt-dlp: any environment proxy settings take precedence
    // over the OS configuration; HTTP is also the default HTTPS proxy.
    if proxies.is_empty() {
        proxies = system();
    }
    if !proxies.contains_key("https") {
        if let Some(http) = proxies.get("http").cloned() {
            proxies.insert("https".into(), http);
        }
    }
    for scheme in ["http", "https", "all"] {
        if let Some(value) = proxies.get_mut(scheme) {
            *value = normalize(value)?;
        }
    }
    Ok(proxies)
}

fn normalize(proxy: &str) -> Result<String, String> {
    let proxy = proxy.trim();
    if proxy == "__noproxy__" {
        return Err("video.error.proxy".into());
    }
    let proxy = if proxy.contains("://") {
        proxy.to_owned()
    } else {
        format!("http://{proxy}")
    };
    let url = url::Url::parse(&proxy).map_err(|_| "video.error.proxy")?;
    // The bundled aria2-next supports HTTP(S) and SOCKS5, but not SOCKS4.
    if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h") || url.host_str().is_none()
    {
        return Err("video.error.proxy".into());
    }
    Ok(proxy)
}

pub(super) fn environment_proxies(environment: impl IntoIterator<Item = (String, String)>) -> Proxies {
    let environment: Vec<_> = environment.into_iter().collect();
    let mut proxies = Proxies::new();
    for (key, value) in &environment {
        if let Some(scheme) = key.to_ascii_lowercase().strip_suffix("_proxy") {
            if !value.is_empty() {
                proxies.insert(scheme.to_owned(), value.to_owned());
            }
        }
    }
    if environment.iter().any(|(key, _)| key == "REQUEST_METHOD") {
        proxies.remove("http");
    }
    // urllib gives lowercase variables priority, including an empty value
    // that clears an uppercase definition.
    for (key, value) in &environment {
        if let Some(scheme) = key.strip_suffix("_proxy") {
            let scheme = scheme.to_ascii_lowercase();
            if value.is_empty() {
                proxies.remove(&scheme);
            } else {
                proxies.insert(scheme, value.to_owned());
            }
        }
    }
    proxies
}

#[cfg(target_os = "macos")]
pub(super) fn system_proxies() -> Proxies {
    std::process::Command::new("/usr/sbin/scutil")
        .arg("--proxy")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| macos_proxies(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default()
}

#[cfg(windows)]
pub(super) fn system_proxies() -> Proxies {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};

    let Ok(settings) = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
    else {
        return Proxies::new();
    };
    if settings.get_value::<u32, _>("ProxyEnable").unwrap_or(0) == 0 {
        return Proxies::new();
    }
    settings
        .get_value::<String, _>("ProxyServer")
        .map(|server| windows_proxies(&server))
        .unwrap_or_default()
}

#[cfg(not(any(target_os = "macos", windows)))]
pub(super) fn system_proxies() -> Proxies {
    Proxies::new()
}

#[cfg(any(target_os = "macos", test))]
fn macos_proxies(output: &str) -> Proxies {
    let mut settings = BTreeMap::new();
    let mut depth = 0;
    for line in output.lines() {
        if depth == 1 {
            if let Some((key, value)) = line.trim().split_once(" : ") {
                settings.insert(key, value);
            }
        }
        depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
    }
    let mut proxies = Proxies::new();
    for (scheme, prefix) in [("http", "HTTP"), ("https", "HTTPS")] {
        if settings.get(format!("{prefix}Enable").as_str()) != Some(&"1") {
            continue;
        }
        let host = settings.get(format!("{prefix}Proxy").as_str());
        let port = settings
            .get(format!("{prefix}Port").as_str())
            .and_then(|port| port.parse::<u16>().ok());
        if let (Some(host), Some(port)) = (host, port.filter(|port| *port > 0)) {
            let host = if host.contains(':') && !host.starts_with('[') {
                format!("[{host}]")
            } else {
                host.to_string()
            };
            proxies.insert(scheme.into(), format!("http://{host}:{port}"));
        }
    }
    proxies
}

#[cfg(any(windows, test))]
fn windows_proxies(server: &str) -> Proxies {
    let mut proxies = Proxies::new();
    if !server.contains(['=', ';']) {
        if !server.trim().is_empty() {
            proxies.insert("http".into(), server.trim().into());
            proxies.insert("https".into(), server.trim().into());
        }
        return proxies;
    }
    for entry in server.split(';') {
        if let Some((scheme, address)) = entry.split_once('=') {
            if !address.trim().is_empty() {
                proxies.insert(scheme.trim().to_ascii_lowercase(), address.trim().into());
            }
        }
    }
    if let Some(socks) = proxies.get("socks") {
        let socks = if socks.contains("://") {
            socks.replacen("socks://", "socks4://", 1)
        } else {
            format!("socks4://{socks}")
        };
        proxies.entry("http".into()).or_insert(socks.clone());
        proxies.entry("https".into()).or_insert(socks);
    }
    proxies
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(entries: &[(&str, &str)]) -> Proxies {
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn lowercase_environment_and_empty_values_override_uppercase() {
        let proxies = environment_proxies(pairs(&[
            ("HTTP_PROXY", "http://upper.invalid:80"),
            ("http_proxy", "http://lower.invalid:80"),
            ("HTTPS_PROXY", "http://upper.invalid:80"),
            ("https_proxy", ""),
            ("NO_PROXY", "localhost,.example.org"),
        ]));
        assert_eq!(proxies.get("http").unwrap(), "http://lower.invalid:80");
        assert!(!proxies.contains_key("https"));
        assert_eq!(proxies.get("no").unwrap(), "localhost,.example.org");
    }

    #[test]
    fn manual_proxy_preserves_bypass_rules_and_overrides_environment() {
        let proxies = resolve(
            Some("http://manual.invalid:80"),
            pairs(&[
                ("https", "http://environment.invalid:80"),
                ("all", "socks4://environment.invalid:1080"),
                ("no", "localhost"),
            ]),
            || panic!("manual settings must not read system proxies"),
        )
        .unwrap();
        assert_eq!(proxies.get("https").unwrap(), "http://manual.invalid:80");
        assert_eq!(proxies.get("no").unwrap(), "localhost");
        assert!(!proxies.contains_key("all"));
    }

    #[test]
    fn environment_retains_distinct_protocols_and_suppresses_system_settings() {
        let proxies = resolve(
            None,
            pairs(&[
                ("http", "http://plain.invalid:80"),
                ("https", "http://secure.invalid:80"),
            ]),
            || panic!("environment settings must not read system proxies"),
        )
        .unwrap();
        assert_ne!(proxies.get("http"), proxies.get("https"));
        let bypass_only = resolve(None, pairs(&[("no", "*")]), || {
            panic!("no_proxy alone suppresses system proxies in urllib")
        })
        .unwrap();
        assert_eq!(bypass_only.len(), 1);
    }

    #[test]
    fn system_http_proxy_is_also_used_for_https() {
        let proxies = resolve(None, Proxies::new(), || {
            pairs(&[("http", "proxy.invalid:8080")])
        })
        .unwrap();
        assert_eq!(proxies.get("https").unwrap(), "http://proxy.invalid:8080");
        assert_eq!(proxies.get("http"), proxies.get("https"));
    }

    #[test]
    fn macos_reads_enabled_global_settings_only() {
        let proxies = macos_proxies("<dictionary> {\n  HTTPEnable : 1\n  HTTPProxy : 127.0.0.1\n  HTTPPort : 8080\n  HTTPSEnable : 0\n  HTTPSProxy : disabled.invalid\n  HTTPSPort : 8081\n  __SCOPED__ : <dictionary> {\n    HTTPProxy : ignored.invalid\n  }\n}");
        assert_eq!(proxies, pairs(&[("http", "http://127.0.0.1:8080")]));
    }

    #[test]
    fn windows_supports_shared_and_per_protocol_proxy_servers() {
        assert_eq!(
            windows_proxies("proxy.invalid:8080"),
            pairs(&[
                ("http", "proxy.invalid:8080"),
                ("https", "proxy.invalid:8080")
            ])
        );
        assert_eq!(
            windows_proxies("http=plain.invalid:80;https=secure.invalid:81"),
            pairs(&[("http", "plain.invalid:80"), ("https", "secure.invalid:81")])
        );
    }

    #[test]
    fn unsupported_socks4_is_reported_without_exposing_proxy_details() {
        assert_eq!(
            resolve(None, windows_proxies("socks=localhost:1080"), Proxies::new).unwrap_err(),
            "video.error.proxy"
        );
        assert!(normalize("socks5://localhost:1080").is_ok());
        assert!(normalize("socks5h://localhost:1080").is_ok());
        assert_eq!(normalize("__noproxy__").unwrap_err(), "video.error.proxy");
    }
}
