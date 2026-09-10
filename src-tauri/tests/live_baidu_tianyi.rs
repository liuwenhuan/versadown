//! Explicit, opt-in live regressions for the browser-authorized Baidu and
//! Tianyi drives.
//!
//! The tests deliberately create only one freshly generated
//! `AllDownloader-IT-*` directory per provider.  It is the sole remote item
//! moved to the provider recycle bin during cleanup.  Session bytes, cookies,
//! signed URLs and account details are never printed.

use alldownloader_lib::cloud::{
    baidu::{BaiduClient, BaiduSession},
    tianyi::{TianyiClient, TianyiSession},
    CloudFile, CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudUploadTransport,
    CredentialStore, DpapiCredentialStore, QuarkError, ReqwestTransport, UploadHttpRequest,
    UploadHttpResponse,
};
use std::{
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type LiveResult<T> = Result<T, Box<dyn Error>>;

/// Delegates to the real HTTPS transport without logging a request URL or
/// headers.  Both can contain sensitive signed credentials.
struct LiveTransport {
    inner: ReqwestTransport,
    /// Diagnostic-only metadata.  It intentionally excludes URLs, header
    /// values, response bodies, account data and all credential material.
    observations: Mutex<Vec<String>>,
}

impl LiveTransport {
    fn new() -> Result<Self, QuarkError> {
        Ok(Self {
            inner: ReqwestTransport::new()?,
            observations: Mutex::new(Vec::new()),
        })
    }

    fn report(&self) {
        for observation in self.observations.lock().unwrap().iter() {
            eprintln!("live cloud request: {observation}");
        }
    }

    fn observe_request(request: &CloudHttpRequest) -> String {
        let host = url::Url::parse(&request.url)
            .ok()
            .and_then(|value| value.host_str().map(str::to_owned))
            .unwrap_or_else(|| "invalid-host".into());
        let presence = |name: &str| request.headers.contains_key(name);
        format!(
            "{} {host}; cookie={}, origin={}, referer={}, user_agent={}",
            request.method,
            presence("Cookie"),
            presence("Origin"),
            presence("Referer"),
            presence("User-Agent"),
        )
    }
}

impl CloudHttpTransport for LiveTransport {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        let request_description = Self::observe_request(&request);
        match self.inner.send(request) {
            Ok(response) => {
                let parsed = serde_json::from_str::<serde_json::Value>(&response.body).ok();
                let code = parsed.as_ref().and_then(|value| {
                    value
                        .get("errno")
                        .or_else(|| value.get("res_code"))
                        .or_else(|| value.get("result"))
                        .or_else(|| value.get("code"))
                        .and_then(serde_json::Value::as_i64)
                });
                let markup_code = (!response.body.trim_start().starts_with('{'))
                    .then(|| {
                        regex::Regex::new(r"<(?:errno|res_code|result|code)>\s*(-?\d+)")
                            .ok()
                            .and_then(|pattern| pattern.captures(&response.body))
                            .and_then(|capture| capture.get(1))
                            .map(|value| value.as_str().to_owned())
                    })
                    .flatten();
                let shape = match parsed.as_ref() {
                    Some(serde_json::Value::Object(object)) => {
                        let mut keys = object.keys().cloned().collect::<Vec<_>>();
                        keys.sort();
                        format!("json-object({})", keys.join(","))
                    }
                    Some(serde_json::Value::Array(_)) => "json-array".into(),
                    Some(_) => "json-scalar".into(),
                    None if response.body.trim_start().starts_with('<') => {
                        let root = regex::Regex::new(
                            r"<\??[A-Za-z0-9_.:-]+[^>]*>\s*<([A-Za-z][A-Za-z0-9_-]*)",
                        )
                        .ok()
                        .and_then(|pattern| pattern.captures(&response.body))
                        .and_then(|capture| capture.get(1))
                        .map(|value| value.as_str())
                        .unwrap_or("unknown");
                        let tags = regex::Regex::new(r"</?([A-Za-z][A-Za-z0-9_-]*)")
                            .ok()
                            .map(|pattern| {
                                let mut tags = pattern
                                    .captures_iter(&response.body)
                                    .filter_map(|capture| {
                                        capture.get(1).map(|value| value.as_str())
                                    })
                                    .collect::<Vec<_>>();
                                tags.sort_unstable();
                                tags.dedup();
                                tags.into_iter().take(8).collect::<Vec<_>>().join(",")
                            })
                            .unwrap_or_default();
                        format!("markup(root={root};{tags})")
                    }
                    None => "non-json".into(),
                };
                self.observations.lock().unwrap().push(format!(
                    "{request_description}; HTTP {}; provider_code={}; response={shape}",
                    response.status,
                    code.map(|value| value.to_string())
                        .or(markup_code)
                        .unwrap_or_else(|| "none".into())
                ));
                Ok(response)
            }
            Err(error) => {
                self.observations.lock().unwrap().push(format!(
                    "{request_description}; transport={}",
                    provider_error_kind(&error)
                ));
                Err(error)
            }
        }
    }
}

impl CloudUploadTransport for LiveTransport {
    fn send_upload(&self, request: UploadHttpRequest) -> Result<UploadHttpResponse, QuarkError> {
        self.inner.send_upload(request)
    }
}

fn failure(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::other(message.into()))
}

fn provider_error_kind(error: &QuarkError) -> &'static str {
    match error {
        QuarkError::NotAuthorized => "not-authorized",
        QuarkError::AuthorizationExpired => "authorization-expired",
        QuarkError::WebAuthorizationRequired => "web-authorization-required",
        QuarkError::CaptchaRequired => "captcha-required",
        QuarkError::PikpakRegionalMembershipRestricted => "pikpak-regional-membership-restricted",
        QuarkError::AuthorizationPending => "authorization-pending",
        QuarkError::SessionRenewedRetryOperation => "session-renewed-retry-operation",
        QuarkError::InvalidResponse => "invalid-response",
        QuarkError::Storage => "storage",
        QuarkError::Transport(_) => "transport",
        QuarkError::Provider(_) => "provider",
        QuarkError::InvalidInput(_) => "invalid-input",
        QuarkError::UploadUnavailable => "upload-unavailable",
        QuarkError::ProviderUnsupported => "provider-unsupported",
    }
}

fn provider_step<T>(
    provider: &str,
    operation: &str,
    action: impl FnOnce() -> Result<T, QuarkError>,
) -> LiveResult<T> {
    action().map_err(|error| {
        failure(format!(
            "{provider} {operation}: {}",
            provider_error_kind(&error)
        ))
    })
}

fn require_live_authorization() -> LiveResult<()> {
    if std::env::var("ALLDOWNLOADER_LIVE_CLOUD").as_deref() != Ok("1") {
        return Err(failure(
            "live cloud regression requires ALLDOWNLOADER_LIVE_CLOUD=1",
        ));
    }
    Ok(())
}

fn session_store() -> LiveResult<DpapiCredentialStore> {
    let app_data = std::env::var_os("APPDATA")
        .ok_or_else(|| failure("APPDATA is required for the encrypted session store"))?;
    Ok(DpapiCredentialStore::new(
        PathBuf::from(app_data)
            .join("com.alldownloader.desktop")
            .join("cloud-sessions"),
    )?)
}

fn test_name(provider: &str, suffix: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("AllDownloader-IT-{provider}-{millis}-{suffix}")
}

fn local_artifact(provider: &str) -> LiveResult<(PathBuf, PathBuf, Vec<u8>)> {
    let root = std::env::temp_dir().join(test_name(provider, "local"));
    fs::create_dir_all(&root)?;
    let contents = format!(
        "AllDownloader live integration regression for {provider}.\nNo user content is used.\n"
    )
    .into_bytes();
    let file = root.join("payload.txt");
    fs::write(&file, &contents)?;
    Ok((root, file, contents))
}

fn find_named(directory: &[CloudFile], name: &str, kind: &str) -> LiveResult<CloudFile> {
    directory
        .iter()
        .find(|item| item.name == name && item.kind == kind)
        .cloned()
        .ok_or_else(|| failure(format!("newly created {kind} {name:?} was not listed")))
}

fn download_bytes(
    url: &str,
    headers: &std::collections::BTreeMap<String, String>,
) -> LiveResult<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let mut request = client.get(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request.send()?;
    if !response.status().is_success() {
        return Err(failure(format!(
            "provider direct download returned HTTP {}",
            response.status()
        )));
    }
    Ok(response.bytes()?.to_vec())
}

fn remove_local(root: &Path) {
    let _ = fs::remove_dir_all(root);
}

fn cookie_names(cookie: &str) -> String {
    let mut names = cookie
        .split(';')
        .filter_map(|pair| pair.trim().split_once('=').map(|(name, _)| name.trim()))
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    names.join(",")
}

fn tianyi_unauthenticated_response_shape(cookie: &str) -> LiveResult<String> {
    // No redirects are followed: this records only a status and a sanitized
    // destination host/path, allowing us to distinguish an API-shape issue
    // from a server-side login redirect without retaining a URL query.
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()?;
    let response = client
        .get("https://cloud.189.cn/api/open/file/listFiles.action?folderId=-11&pageNum=1&pageSize=1&orderBy=1&order=ASC")
        .header("Cookie", cookie)
        .header("Origin", "https://cloud.189.cn")
        .header("Referer", "https://cloud.189.cn/")
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36")
        .send()?;
    let destination = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| url::Url::parse(value).ok())
        .map(|url| format!("{}{}", url.host_str().unwrap_or("invalid-host"), url.path()))
        .unwrap_or_else(|| "none".into());
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .unwrap_or("none");
    Ok(format!(
        "HTTP {}; location={destination}; content_type={content_type}",
        response.status()
    ))
}

/// This validates the current browser session against Baidu's official Pan
/// API.  Baidu is intentionally the only provider that uses its `netdisk`
/// client identity: the client implementation owns that distinction.
#[test]
#[ignore = "requires an explicitly authorized Baidu account and creates only AllDownloader-IT-* test data"]
fn live_baidu_directory_upload_rename_move_download_and_trash() -> LiveResult<()> {
    require_live_authorization()?;
    let store = session_store()?;
    let session: BaiduSession = serde_json::from_slice(
        &store
            .load("baidu")?
            .ok_or_else(|| failure("Baidu is not currently authorized"))?,
    )?;
    eprintln!(
        "Baidu live session: web_auth={}, cookie_present={}, cookie_names={}",
        matches!(
            session.auth_method,
            alldownloader_lib::cloud::CloudAuthMethod::Web
        ),
        !session.cookie.is_empty(),
        cookie_names(&session.cookie),
    );
    let http = LiveTransport::new()?;
    let client = BaiduClient::new(session, &http);
    let (local_root, payload, expected) = local_artifact("baidu")?;
    let base_name = test_name("baidu", "root");
    let target_name = test_name("baidu", "move-target");
    let renamed_name = test_name("baidu", "renamed.txt");

    let initial_root = match provider_step("Baidu", "list root", || client.directory("/")) {
        Ok(directory) => directory,
        Err(error) => {
            http.report();
            return Err(error);
        }
    };
    if initial_root.items.iter().any(|item| item.name == base_name) {
        remove_local(&local_root);
        return Err(failure("unexpected Baidu test-directory name collision"));
    }
    if let Err(error) = provider_step("Baidu", "mkdir root", || client.mkdir("/", &base_name)) {
        http.report();
        remove_local(&local_root);
        return Err(error);
    }
    let base = find_named(
        &provider_step("Baidu", "list after mkdir", || client.directory("/"))?.items,
        &base_name,
        "folder",
    )?;
    let result = (|| -> LiveResult<()> {
        provider_step("Baidu", "mkdir move target", || {
            client.mkdir(&base.id, &target_name)
        })?;
        let target = find_named(
            &provider_step("Baidu", "list move target", || client.directory(&base.id))?.items,
            &target_name,
            "folder",
        )?;
        let uploaded = provider_step("Baidu", "upload", || {
            client.upload_file(&base.id, &payload, &http)
        })?;
        provider_step("Baidu", "rename", || {
            client.rename(&uploaded.id, &renamed_name)
        })?;
        let renamed = find_named(
            &provider_step("Baidu", "list after rename", || client.directory(&base.id))?.items,
            &renamed_name,
            "file",
        )?;
        provider_step("Baidu", "move", || {
            client.move_files(&[renamed.id.clone()], &target.id)
        })?;
        let moved = find_named(
            &provider_step("Baidu", "list after move", || client.directory(&target.id))?.items,
            &renamed_name,
            "file",
        )?;
        let download = provider_step("Baidu", "resolve download", || client.download(&moved.id))?;
        if download.name != renamed_name {
            return Err(failure(
                "Baidu download metadata did not preserve renamed file",
            ));
        }
        if download_bytes(&download.url, &download.headers)? != expected {
            return Err(failure(
                "Baidu downloaded bytes differ from uploaded test file",
            ));
        }
        Ok(())
    })();

    // The id was produced by listing the unique directory we just created;
    // Baidu's delete operation moves it to the recycle bin.
    let cleanup = client.trash(&[base.id.clone()]);
    remove_local(&local_root);
    http.report();
    match (result, cleanup) {
        (Ok(()), Ok(())) => {
            if client
                .directory("/")?
                .items
                .iter()
                .any(|item| item.id == base.id)
            {
                Err(failure("Baidu test directory remained visible after trash"))
            } else {
                Ok(())
            }
        }
        (Err(test), Ok(())) => Err(test),
        (Ok(()), Err(cleanup)) => Err(Box::new(cleanup)),
        (Err(test), Err(cleanup)) => Err(failure(format!(
            "test failed ({test}); cleanup of only its test directory also failed ({cleanup})"
        ))),
    }
}

/// Tianyi uses its own authenticated browser profile (Cookie, Origin,
/// Referer and a normal Chrome UA).  This regression verifies that exact
/// configured profile end-to-end; it deliberately never substitutes Baidu's
/// `netdisk` identity into a different provider's API.
#[test]
#[ignore = "requires an explicitly authorized Tianyi account and creates only AllDownloader-IT-* test data"]
fn live_tianyi_directory_upload_rename_move_download_and_trash() -> LiveResult<()> {
    require_live_authorization()?;
    let store = session_store()?;
    let session: TianyiSession = serde_json::from_slice(
        &store
            .load("tianyi")?
            .ok_or_else(|| failure("Tianyi is not currently authorized"))?,
    )?;
    eprintln!(
        "Tianyi live session: web_auth={}, cookie_present={}, cookie_names={}",
        matches!(
            session.auth_method,
            alldownloader_lib::cloud::CloudAuthMethod::Web
        ),
        !session.cookie.is_empty(),
        cookie_names(&session.cookie),
    );
    let http = LiveTransport::new()?;
    let client = TianyiClient::new(session.clone(), &http);
    let (local_root, payload, expected) = local_artifact("tianyi")?;
    let base_name = test_name("tianyi", "root");
    let target_name = test_name("tianyi", "move-target");
    let renamed_name = test_name("tianyi", "renamed.txt");

    let initial_root = match provider_step("Tianyi", "list root", || client.directory("-11")) {
        Ok(directory) => directory,
        Err(error) => {
            http.report();
            eprintln!(
                "Tianyi no-redirect probe: {}",
                tianyi_unauthenticated_response_shape(&session.cookie)
                    .unwrap_or_else(|_| "transport-failed".into())
            );
            return Err(error);
        }
    };
    if initial_root.items.iter().any(|item| item.name == base_name) {
        remove_local(&local_root);
        return Err(failure("unexpected Tianyi test-directory name collision"));
    }
    if let Err(error) = provider_step("Tianyi", "mkdir root", || client.mkdir("-11", &base_name)) {
        http.report();
        remove_local(&local_root);
        return Err(error);
    }
    let base = find_named(
        &provider_step("Tianyi", "list after mkdir", || client.directory("-11"))?.items,
        &base_name,
        "folder",
    )?;
    let result = (|| -> LiveResult<()> {
        provider_step("Tianyi", "mkdir move target", || {
            client.mkdir(&base.id, &target_name)
        })?;
        let target = find_named(
            &provider_step("Tianyi", "list move target", || client.directory(&base.id))?.items,
            &target_name,
            "folder",
        )?;
        let uploaded = provider_step("Tianyi", "upload", || {
            client.upload_file(&base.id, &payload, &http)
        })?;
        provider_step("Tianyi", "rename", || {
            client.rename(&uploaded.id, &renamed_name)
        })?;
        let renamed = find_named(
            &provider_step("Tianyi", "list after rename", || client.directory(&base.id))?.items,
            &renamed_name,
            "file",
        )?;
        provider_step("Tianyi", "move", || {
            client.move_files(&[renamed.id.clone()], &target.id)
        })?;
        let moved = find_named(
            &provider_step("Tianyi", "list after move", || client.directory(&target.id))?.items,
            &renamed_name,
            "file",
        )?;
        let download = provider_step("Tianyi", "resolve download", || client.download(&moved.id))?;
        if download.name != renamed_name {
            return Err(failure(
                "Tianyi download metadata did not preserve renamed file",
            ));
        }
        if download_bytes(&download.url, &download.headers)? != expected {
            return Err(failure(
                "Tianyi downloaded bytes differ from uploaded test file",
            ));
        }
        Ok(())
    })();

    // The id was produced by listing the unique directory we just created;
    // Tianyi's delete operation is explicitly requested with `toRecycle`.
    let cleanup = client.trash(&[base.id.clone()]);
    remove_local(&local_root);
    http.report();
    match (result, cleanup) {
        (Ok(()), Ok(())) => {
            if client
                .directory("-11")?
                .items
                .iter()
                .any(|item| item.id == base.id)
            {
                Err(failure(
                    "Tianyi test directory remained visible after trash",
                ))
            } else {
                Ok(())
            }
        }
        (Err(test), Ok(())) => Err(test),
        (Ok(()), Err(cleanup)) => Err(Box::new(cleanup)),
        (Err(test), Err(cleanup)) => Err(failure(format!(
            "test failed ({test}); cleanup of only its test directory also failed ({cleanup})"
        ))),
    }
}

fn wait_for_named(
    mut directory: impl FnMut() -> Result<alldownloader_lib::cloud::CloudDirectory, QuarkError>,
    name: &str,
    kind: &str,
) -> LiveResult<CloudFile> {
    let mut last_error = None;
    for _ in 0..10 {
        match directory() {
            Ok(listing) => match find_named(&listing.items, name, kind) {
                Ok(item) => return Ok(item),
                Err(error) => last_error = Some(error),
            },
            Err(error) => last_error = Some(Box::new(error)),
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err(last_error.unwrap_or_else(|| failure("provider did not expose the expected item")))
}

#[test]
#[ignore = "requires an explicitly authorized Tianyi account and creates only AllDownloader-IT-* test data"]
fn live_tianyi_folder_rename_move_and_trash() -> LiveResult<()> {
    require_live_authorization()?;
    let store = session_store()?;
    let session: TianyiSession = serde_json::from_slice(
        &store
            .load("tianyi")?
            .ok_or_else(|| failure("Tianyi is not currently authorized"))?,
    )?;
    let http = LiveTransport::new()?;
    let client = TianyiClient::new(session, &http);
    let base_name = test_name("tianyi", "mutations-root");
    let source_name = test_name("tianyi", "rename-source");
    let renamed_name = test_name("tianyi", "renamed-folder");
    let destination_name = test_name("tianyi", "move-target");

    if let Err(error) = client.mkdir("-11", &base_name) {
        http.report();
        return Err(Box::new(error));
    }
    let base = find_named(&client.directory("-11")?.items, &base_name, "folder")?;
    let result = (|| -> LiveResult<()> {
        client.mkdir(&base.id, &source_name)?;
        client.mkdir(&base.id, &destination_name)?;
        let children = client.directory(&base.id)?;
        let source = find_named(&children.items, &source_name, "folder")?;
        let destination = find_named(&children.items, &destination_name, "folder")?;
        client.rename(&source.id, &renamed_name)?;
        let renamed = wait_for_named(|| client.directory(&base.id), &renamed_name, "folder")?;
        client.move_files(&[renamed.id.clone()], &destination.id)?;
        wait_for_named(
            || client.directory(&destination.id),
            &renamed_name,
            "folder",
        )?;
        Ok(())
    })();
    let cleanup = client.trash(&[base.id.clone()]);
    match (result, cleanup) {
        (Ok(()), Ok(())) => {
            // The batch delete operation is asynchronous.  A successful task
            // id may take a few seconds before the listing hides the folder.
            for _ in 0..10 {
                if !client
                    .directory("-11")?
                    .items
                    .iter()
                    .any(|item| item.id == base.id)
                {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(failure(
                "Tianyi mutation test root remained visible after trash",
            ))
        }
        (Err(test), Ok(())) => Err(test),
        (Ok(()), Err(cleanup)) => Err(Box::new(cleanup)),
        (Err(test), Err(cleanup)) => Err(failure(format!(
            "test failed ({test}); cleanup of only its test directory also failed ({cleanup})"
        ))),
    }
}
