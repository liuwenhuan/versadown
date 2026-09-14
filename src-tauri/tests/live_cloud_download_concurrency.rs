//! Explicit, opt-in, read-only probe for provider direct-download Range support.
//!
//! The probe never prints cloud item metadata, resolved URLs, request headers,
//! session material, or proxy configuration.

use alldownloader_lib::{
    cloud::{
        commands::CloudState, CloudError, CloudFile, CloudFileKind, CloudProvider, CredentialStore,
        ProviderRegistry, ReqwestTransport, ResolvedCloudDownload,
    },
    AppSettings, ProxySettings,
};
use std::{
    collections::{HashSet, VecDeque},
    fs,
    path::PathBuf,
    thread,
    time::Duration,
};

const RANGE_BYTES: u64 = 4 * 1024;
const RANGE_COUNT: u64 = 4;
const MIN_FILE_BYTES: u64 = RANGE_BYTES * RANGE_COUNT;
const MAX_DIRECTORIES: usize = 32;

fn app_data_root() -> Result<PathBuf, &'static str> {
    let app_data = std::env::var_os("APPDATA").ok_or("app-data-unavailable")?;
    Ok(PathBuf::from(app_data).join("com.alldownloader.desktop"))
}

fn configured_proxy_client(root: &PathBuf) -> Result<reqwest::blocking::Client, &'static str> {
    let bytes = fs::read(root.join("settings.json")).map_err(|_| "settings-unavailable")?;
    let settings: AppSettings = serde_json::from_slice(&bytes).map_err(|_| "settings-invalid")?;
    let proxy_url = proxy_url(&settings.proxy)?;
    let proxy = reqwest::Proxy::all(proxy_url.as_str()).map_err(|_| "proxy-invalid")?;
    reqwest::blocking::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|_| "proxy-client-unavailable")
}

fn proxy_url(proxy: &ProxySettings) -> Result<url::Url, &'static str> {
    if !proxy.enabled {
        return Err("proxy-disabled");
    }
    if !matches!(proxy.protocol.as_str(), "http" | "https")
        || proxy.host.trim().is_empty()
        || proxy.host.contains("://")
        || proxy.port == 0
    {
        return Err("proxy-invalid");
    }
    let host = proxy.host.trim();
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let mut value = url::Url::parse(&format!("{}://{}:{}", proxy.protocol, host, proxy.port))
        .map_err(|_| "proxy-invalid")?;
    if !proxy.username.is_empty() {
        value
            .set_username(&proxy.username)
            .map_err(|_| "proxy-invalid")?;
    }
    if !proxy.password.is_empty() {
        value
            .set_password(Some(&proxy.password))
            .map_err(|_| "proxy-invalid")?;
    }
    Ok(value)
}

fn cloud_error_kind(error: &CloudError) -> &'static str {
    match error {
        CloudError::NotAuthorized => "session-missing",
        CloudError::AuthorizationExpired => "session-expired",
        CloudError::SessionRenewedRetryOperation => "session-renewed-retry-needed",
        CloudError::WebAuthorizationRequired => "web-authorization-required",
        CloudError::CaptchaRequired => "verification-required",
        CloudError::PikpakOperationVerificationRequired(_) => "verification-required",
        CloudError::PikpakRegionalMembershipRestricted => "regional-membership-restricted",
        CloudError::AuthorizationPending => "authorization-pending",
        CloudError::InvalidResponse => "provider-response-invalid",
        CloudError::Storage => "session-store-unavailable",
        CloudError::Transport(_) => "provider-transport-error",
        CloudError::Provider(_) => "provider-error",
        CloudError::InvalidInput(_) => "provider-input-invalid",
        CloudError::UploadUnavailable | CloudError::ProviderUnsupported => "provider-unsupported",
    }
}

fn find_large_file(
    state: &CloudState,
    http: &ReqwestTransport,
    provider: CloudProvider,
) -> Result<Option<CloudFile>, CloudError> {
    let mut directories = VecDeque::from([ProviderRegistry::global()
        .get(provider)
        .root_directory_id
        .to_owned()]);
    let mut visited = HashSet::new();
    for _ in 0..MAX_DIRECTORIES {
        let Some(parent) = directories.pop_front() else {
            break;
        };
        if !visited.insert(parent.clone()) {
            continue;
        }
        let listing = ProviderRegistry::global()
            .driver(provider, &state.store, http)
            .list_directory(&parent)?;
        for item in listing.items {
            match item.kind {
                CloudFileKind::File if item.size.is_some_and(|size| size >= MIN_FILE_BYTES) => {
                    return Ok(Some(item));
                }
                CloudFileKind::Folder if visited.len() + directories.len() < MAX_DIRECTORIES => {
                    directories.push_back(item.id);
                }
                _ => {}
            }
        }
    }
    Ok(None)
}

fn resolve_download(
    state: &CloudState,
    http: &ReqwestTransport,
    provider: CloudProvider,
    file: &CloudFile,
) -> Result<ResolvedCloudDownload, CloudError> {
    ProviderRegistry::global()
        .driver(provider, &state.store, http)
        .resolve_download(&file.id)
}

enum RangeResult {
    Pass,
    Transport,
    Status(u16),
    ContentRange,
    ByteCount,
    Worker,
}

fn request_range(
    client: &reqwest::blocking::Client,
    download: &ResolvedCloudDownload,
    start: u64,
    end: u64,
    total: u64,
) -> RangeResult {
    let mut request = client
        .get(&download.url)
        .header(reqwest::header::RANGE, format!("bytes={start}-{end}"));
    for (name, value) in &download.headers {
        if !name.eq_ignore_ascii_case("range") {
            request = request.header(name, value);
        }
    }
    let response = match request.send() {
        Ok(response) => response,
        Err(_) => return RangeResult::Transport,
    };
    let status = response.status().as_u16();
    if status != 206 {
        return RangeResult::Status(status);
    }
    let expected = format!("bytes {start}-{end}/{total}");
    if response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        != Some(expected.as_str())
    {
        return RangeResult::ContentRange;
    }
    match response.bytes() {
        Ok(bytes) if bytes.len() as u64 == end - start + 1 => RangeResult::Pass,
        Ok(_) => RangeResult::ByteCount,
        Err(_) => RangeResult::Transport,
    }
}

fn probe_ranges(
    client: &reqwest::blocking::Client,
    download: &ResolvedCloudDownload,
    total: u64,
) -> Vec<RangeResult> {
    thread::scope(|scope| {
        let handles = (0..RANGE_COUNT)
            .map(|index| {
                let start = index * RANGE_BYTES;
                let end = start + RANGE_BYTES - 1;
                scope.spawn(move || request_range(client, download, start, end, total))
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or(RangeResult::Worker))
            .collect()
    })
}

fn print_skip(provider: CloudProvider, reason: &str) {
    eprintln!("{} SKIP {reason}", provider.key());
}

fn print_fail(provider: CloudProvider, reason: &str) {
    eprintln!("{} FAIL {reason}", provider.key());
}

#[test]
#[ignore = "requires saved cloud sessions and ALLDOWNLOADER_LIVE_CLOUD_CONCURRENCY=1"]
fn enabled_providers_support_four_concurrent_disjoint_ranges_through_configured_proxy() {
    let providers = CloudProvider::ALL
        .into_iter()
        .filter(|provider| provider.is_enabled())
        .collect::<Vec<_>>();
    if std::env::var("ALLDOWNLOADER_LIVE_CLOUD_CONCURRENCY").as_deref() != Ok("1") {
        for provider in providers {
            print_skip(provider, "opt-in-disabled");
        }
        return;
    }

    let root = match app_data_root() {
        Ok(root) => root,
        Err(reason) => {
            for provider in providers {
                print_skip(provider, reason);
            }
            return;
        }
    };
    let client = match configured_proxy_client(&root) {
        Ok(client) => client,
        Err(reason) => {
            for provider in providers {
                print_skip(provider, reason);
            }
            return;
        }
    };
    let state = match CloudState::new(root.join("cloud-sessions")) {
        Ok(state) => state,
        Err(error) => {
            let reason = cloud_error_kind(&error);
            for provider in providers {
                print_skip(provider, reason);
            }
            return;
        }
    };
    let http = match ReqwestTransport::new() {
        Ok(http) => http,
        Err(error) => {
            let reason = cloud_error_kind(&error);
            for provider in providers {
                print_skip(provider, reason);
            }
            return;
        }
    };

    let mut failures = 0usize;
    for provider in providers {
        match state.store.load(provider.key()) {
            Ok(Some(_)) => {}
            Ok(None) => {
                print_skip(provider, "session-missing");
                continue;
            }
            Err(error) => {
                print_skip(provider, cloud_error_kind(&error));
                continue;
            }
        }
        if let Err(error) = ProviderRegistry::global()
            .driver(provider, &state.store, &http)
            .account()
        {
            print_skip(provider, cloud_error_kind(&error));
            continue;
        }
        let file = match find_large_file(&state, &http, provider) {
            Ok(Some(file)) => file,
            Ok(None) => {
                print_skip(provider, "no-suitable-file");
                continue;
            }
            Err(error) => {
                print_skip(provider, cloud_error_kind(&error));
                continue;
            }
        };
        let total = file.size.expect("large-file selection requires a size");
        let download = match resolve_download(&state, &http, provider, &file) {
            Ok(download) => download,
            Err(error) => {
                print_skip(provider, cloud_error_kind(&error));
                continue;
            }
        };
        let results = probe_ranges(&client, &download, total);
        if results
            .iter()
            .all(|result| matches!(result, RangeResult::Pass))
        {
            eprintln!(
                "{} PASS HTTP 206 ranges 0-4095,4096-8191,8192-12287,12288-16383 validated",
                provider.key()
            );
            continue;
        }
        failures += 1;
        let (index, result) = results
            .iter()
            .enumerate()
            .find(|(_, result)| !matches!(result, RangeResult::Pass))
            .expect("a failed result must be present");
        match result {
            RangeResult::Transport => {
                print_fail(provider, &format!("range-{index} transport-error"))
            }
            RangeResult::Status(status) => {
                print_fail(provider, &format!("range-{index} HTTP-{status}"))
            }
            RangeResult::ContentRange => print_fail(
                provider,
                &format!("range-{index} HTTP-206 content-range-mismatch"),
            ),
            RangeResult::ByteCount => print_fail(
                provider,
                &format!("range-{index} HTTP-206 byte-count-mismatch"),
            ),
            RangeResult::Worker => print_fail(provider, &format!("range-{index} worker-error")),
            RangeResult::Pass => unreachable!(),
        }
    }
    assert_eq!(failures, 0, "one or more providers failed Range validation");
}
