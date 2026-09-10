//! Explicitly enabled live regressions for the two browser-authorized drives.
//!
//! They only ever create one uniquely named `AllDownloader-IT-*` directory,
//! operate on descendants of that directory, and finally move that directory
//! to the provider's recycle bin.  They are ignored by default because they
//! use the current Windows user's encrypted provider sessions.

use alldownloader_lib::cloud::{
    xunlei::{XunleiClient, XunleiSession},
    CloudFile, CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudUploadTransport,
    CredentialStore, DpapiCredentialStore, PikPakClient, PikPakSession, QuarkError,
    ReqwestTransport, UploadHttpRequest, UploadHttpResponse,
};
use std::{
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type LiveResult<T> = Result<T, Box<dyn Error>>;

/// A test-only transport that records no secrets: it reports only the
/// provider hostname, HTTP method, and result class for a failed object-store
/// transfer.  That is enough to diagnose a transfer boundary without ever
/// printing the signed URL or STS credentials.
struct LiveTransport(ReqwestTransport);

impl LiveTransport {
    fn new() -> Result<Self, QuarkError> {
        Ok(Self(ReqwestTransport::new()?))
    }
}

fn safe_host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .unwrap_or_else(|| "invalid-host".into())
}

fn error_kind(error: &QuarkError) -> &'static str {
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

#[test]
fn provider_failure_message_redacts_server_echoes() {
    let error = QuarkError::Provider(
        "HTTP 403 https://signed.example/file?token=credential&captcha=context".into(),
    );

    let message = safe_provider_failure("PikPak CACHE resolve download", &error);

    assert_eq!(message, "PikPak CACHE resolve download: provider");
    assert!(!message.contains("signed.example"));
    assert!(!message.contains("credential"));
    assert!(!message.contains("captcha"));
}

fn provider_step<T>(
    name: &str,
    operation: impl FnOnce() -> Result<T, QuarkError>,
) -> LiveResult<T> {
    operation().map_err(|error| failure(safe_provider_failure(name, &error)))
}

fn safe_provider_failure(name: &str, error: &QuarkError) -> String {
    // Server responses and transport errors can echo signed URLs, bearer
    // material, or challenge context. The enum class is sufficient for a
    // live-regression diagnosis and is the only safe failure detail to log.
    format!("{name}: {}", error_kind(error))
}

impl CloudHttpTransport for LiveTransport {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        let cache_probe = request.url.contains("usage=CACHE");
        let xunlei_method =
            (safe_host(&request.url) == "api-pan.xunlei.com").then_some(request.method);
        let xunlei_detail = request.method == "GET"
            && url::Url::parse(&request.url).is_ok_and(|url| {
                matches!(
                    url.host_str(),
                    Some("api-pan.xunlei.com" | "x-api-pan.xunlei.com")
                ) && url.path().starts_with("/drive/v1/files/")
            });
        let response = self.0.send(request)?;
        if xunlei_detail {
            let value: serde_json::Value = serde_json::from_str(&response.body).unwrap_or_default();
            let file = value.get("data").unwrap_or(&value);
            let phase = match file["phase"].as_str() {
                Some("PHASE_TYPE_COMPLETE") => "complete",
                Some("PHASE_TYPE_PENDING") => "pending",
                Some("PHASE_TYPE_RUNNING") => "running",
                Some("PHASE_TYPE_ERROR") => "error",
                _ => "other",
            };
            let link_state = |key: &str| match file[key].as_str() {
                None => "missing",
                Some("") => "empty",
                Some(raw) => match url::Url::parse(raw).ok().as_ref().map(url::Url::scheme) {
                    Some("https") => "https",
                    Some("http") => "http",
                    _ => "other",
                },
            };
            eprintln!(
                "Xunlei detail: phase={phase}, web_link={}, download_link={}",
                link_state("web_content_link"),
                link_state("download_url")
            );
        }
        if let Some(method) = xunlei_method {
            let v: serde_json::Value = serde_json::from_str(&response.body).unwrap_or_default();
            let code = v["error_code"]
                .as_i64()
                .or_else(|| v["code"].as_i64())
                .unwrap_or_default();
            if response.status >= 400 || code != 0 {
                eprintln!(
                    "Xunlei API failure: method={method}, HTTP={}, code={code}, error={:?}",
                    response.status,
                    v["error"]
                        .as_str()
                        .filter(|s| s.len() < 80
                            && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
                );
            }
        }
        if cache_probe {
            let cache_details = serde_json::from_str::<serde_json::Value>(&response.body)
                .ok()
                .map(|value| {
                    let fields = value
                        .as_object()
                        .map(|object| object.keys().cloned().collect::<Vec<_>>().join(","))
                        .unwrap_or_else(|| "non-object".into());
                    let link = match value["web_content_link"].as_str() {
                        Some(raw) => url::Url::parse(raw)
                            .ok()
                            .map(|url| {
                                format!("{}://{}", url.scheme(), url.host_str().unwrap_or(""))
                            })
                            .unwrap_or_else(|| format!("non-url(len={})", raw.len())),
                        None => "missing".into(),
                    };
                    let media_link = value["medias"]
                        .as_array()
                        .and_then(|media| media.first())
                        .and_then(|media| media.pointer("/link/url"))
                        .and_then(serde_json::Value::as_str)
                        .and_then(|raw| url::Url::parse(raw).ok())
                        .map(|url| format!("{}://{}", url.scheme(), url.host_str().unwrap_or("")))
                        .unwrap_or_else(|| "missing-or-invalid".into());
                    format!("keys={fields}; web_content_link={link}; first_media={media_link}")
                })
                .unwrap_or_else(|| "non-json".into());
            // Keys only: the response can contain a signed link, which must
            // never appear in a test log.
            eprintln!(
                "PikPak CACHE response: HTTP {}; {cache_details}",
                response.status
            );
        }
        Ok(response)
    }
}

impl CloudUploadTransport for LiveTransport {
    fn send_upload(&self, request: UploadHttpRequest) -> Result<UploadHttpResponse, QuarkError> {
        let method = request.method;
        match self.0.send_upload(request) {
            Ok(response) => {
                eprintln!("live object transfer: {method} -> HTTP {}", response.status);
                Ok(response)
            }
            Err(error) => {
                eprintln!("live object transfer: {method} -> {}", error_kind(&error));
                Err(error)
            }
        }
    }
}

fn failure(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::other(message.into()))
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
    // An isolated test app can authorize without replacing the user's normal
    // account store. This is an explicit opt-in test-data path, not APPDATA.
    if let Some(data_dir) = std::env::var_os("ALLDOWNLOADER_TEST_DATA_DIR") {
        return Ok(DpapiCredentialStore::new(
            PathBuf::from(data_dir).join("cloud-sessions"),
        )?);
    }
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

fn contains_id(items: &[CloudFile], id: &str) -> bool {
    items.iter().any(|item| item.id == id)
}

fn find_named(directory: &[CloudFile], name: &str, kind: &str) -> LiveResult<CloudFile> {
    directory
        .iter()
        .find(|item| item.name == name && item.kind == kind)
        .cloned()
        .ok_or_else(|| failure(format!("newly created {kind} {name:?} was not listed")))
}

fn download_bytes(url: &str) -> LiveResult<Vec<u8>> {
    download_bytes_with_headers(url, &[])
}

fn download_bytes_with_headers(url: &str, headers: &[(&str, &str)]) -> LiveResult<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let mut request = client.get(url);
    for (key, value) in headers {
        request = request.header(*key, *value);
    }
    let response = request
        .send()
        .map_err(|_| failure("provider direct download transport failed"))?;
    if !response.status().is_success() {
        return Err(failure(format!(
            "provider direct download returned HTTP {}",
            response.status()
        )));
    }
    Ok(response
        .bytes()
        .map_err(|_| failure("provider direct download body read failed"))?
        .to_vec())
}

fn remove_local(root: &Path) {
    let _ = fs::remove_dir_all(root);
}

fn xunlei_browser_root_request(session: &XunleiSession, web_headers: bool) -> CloudHttpRequest {
    let mut url = url::Url::parse("https://x-api-pan.xunlei.com/drive/v1/files")
        .expect("fixed Xunlei browser endpoint is valid");
    url.query_pairs_mut()
        .append_pair("parent_id", "")
        .append_pair("page_token", "")
        .append_pair("space", "")
        .append_pair("filters", r#"{"trashed":{"eq":false}}"#)
        .append_pair("with", "url")
        .append_pair("with_audit", "true")
        .append_pair("thumbnail_size", "SIZE_LARGE");
    let mut headers = std::collections::BTreeMap::from([
        ("Authorization".into(), format!("Bearer {}", session.access_token)),
        ("Accept".into(), "application/json;charset=UTF-8".into()),
        (
            "User-Agent".into(),
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36".into(),
        ),
    ]);
    if web_headers {
        headers.insert("Origin".into(), "https://pan.xunlei.com".into());
        headers.insert("Referer".into(), "https://pan.xunlei.com/".into());
    }
    CloudHttpRequest {
        method: "GET",
        url: url.into(),
        headers,
        body: None,
    }
}

fn xunlei_browser_root_probe(
    http: &impl CloudHttpTransport,
    session: &XunleiSession,
    web_headers: bool,
) -> LiveResult<()> {
    let response = http.send(xunlei_browser_root_request(session, web_headers))?;
    let value: serde_json::Value = serde_json::from_str(&response.body)?;
    let code = value
        .get("error_code")
        .or_else(|| value.get("code"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    if !(200..300).contains(&response.status) || code != 0 {
        return Err(failure(format!(
            "Xunlei browser endpoint probe with web_headers={web_headers} returned HTTP {} / code {code}",
            response.status
        )));
    }
    let count = value["files"].as_array().map_or(0, Vec::len);
    eprintln!(
        "Xunlei browser endpoint probe with web_headers={web_headers} succeeded: {count} root entries"
    );
    Ok(())
}

#[test]
#[ignore = "requires an explicitly authorized PikPak account and creates only AllDownloader-IT-* test data"]
fn live_pikpak_directory_upload_rename_move_download_and_trash() -> LiveResult<()> {
    require_live_authorization()?;
    let store = session_store()?;
    let session: PikPakSession = serde_json::from_slice(
        &store
            .load("pikpak")?
            .ok_or_else(|| failure("PikPak is not currently authorized"))?,
    )?;
    eprintln!(
        "PikPak live session: auth={:?}, access_expired={}, browser_client={}, client_is_android={}, device_present={}, captcha_present={}",
        session.auth_method,
        session.expires_at <= SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
        session.client_id.is_some(),
        session.client_id.as_deref() == Some("YNxT9w7GMdWvEOKa"),
        !session.device_id.is_empty(),
        !session.captcha_token.is_empty(),
    );
    let http = LiveTransport::new()?;
    let client = PikPakClient::new(&http, &store);
    let (local_root, payload, expected) = local_artifact("pikpak")?;
    let base_name = test_name("pikpak", "root");
    let target_name = test_name("pikpak", "move-target");
    let renamed_name = test_name("pikpak", "renamed.txt");

    // Root listing is deliberately performed before any mutation.
    let initial_root = client.list("0")?;
    if initial_root.items.iter().any(|item| item.name == base_name) {
        remove_local(&local_root);
        return Err(failure("unexpected test-directory name collision"));
    }
    let base = client.mkdir("0", &base_name)?;
    let result = (|| -> LiveResult<()> {
        let listed = provider_step("PikPak list after mkdir", || client.list("0"))?;
        if !contains_id(&listed.items, &base.id) {
            return Err(failure(
                "PikPak did not list the newly created test directory",
            ));
        }
        let target = provider_step("PikPak mkdir move target", || {
            client.mkdir(&base.id, &target_name)
        })?;
        let uploaded = provider_step("PikPak upload", || client.upload_file(&base.id, &payload))?;
        provider_step("PikPak rename", || {
            client.rename(&uploaded.id, &renamed_name)
        })?;
        let after_rename = provider_step("PikPak list after rename", || client.list(&base.id))?;
        let renamed = find_named(&after_rename.items, &renamed_name, "file")?;
        provider_step("PikPak move", || {
            client.move_files(&[renamed.id.clone()], &target.id)
        })?;
        let moved = provider_step("PikPak list after move", || client.list(&target.id))?;
        let moved_file = find_named(&moved.items, &renamed_name, "file")?;
        let (url, download_name) = provider_step("PikPak resolve download", || {
            client.download(&moved_file.id)
        })?;
        if download_name != renamed_name {
            return Err(failure(
                "PikPak download metadata did not preserve the renamed file",
            ));
        }
        if download_bytes(&url)? != expected {
            return Err(failure(
                "PikPak downloaded bytes differ from the uploaded test file",
            ));
        }
        Ok(())
    })();

    // Only the directory created above is sent to the recycle bin, never a
    // pre-existing root item.  This also removes every test descendant.
    let cleanup = client.trash(&[base.id.clone()]);
    remove_local(&local_root);
    match (result, cleanup) {
        (Ok(()), Ok(())) => {
            if contains_id(&client.list("0")?.items, &base.id) {
                Err(failure(
                    "PikPak test directory remained visible after trash",
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

#[test]
#[ignore = "requires an explicitly authorized PikPak account and creates only AllDownloader-IT-* test data"]
fn live_pikpak_cache_downloads_uploaded_bytes_and_trashes_only_its_directory() -> LiveResult<()> {
    require_live_authorization()?;
    let store = session_store()?;
    let http = LiveTransport::new()?;
    let client = PikPakClient::new(&http, &store);
    let (local_root, payload, expected) = local_artifact("pikpak-cache")?;
    let base_name = test_name("pikpak-cache", "root");

    let initial_root = client.list("0")?;
    if initial_root.items.iter().any(|item| item.name == base_name) {
        remove_local(&local_root);
        return Err(failure("unexpected test-directory name collision"));
    }
    let base = client.mkdir("0", &base_name)?;
    let result = (|| -> LiveResult<()> {
        let uploaded = provider_step("PikPak cache-test upload", || {
            client.upload_file(&base.id, &payload)
        })?;
        let (url, download_name) = provider_step("PikPak CACHE resolve download", || {
            client.download(&uploaded.id)
        })?;
        if download_name != "payload.txt" {
            return Err(failure(
                "PikPak CACHE download metadata did not preserve payload name",
            ));
        }
        if download_bytes(&url)? != expected {
            return Err(failure(
                "PikPak CACHE download bytes differ from uploaded payload",
            ));
        }
        Ok(())
    })();
    let cleanup = client.trash(&[base.id.clone()]);
    remove_local(&local_root);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(test), Ok(())) => Err(test),
        (Ok(()), Err(cleanup)) => Err(Box::new(cleanup)),
        (Err(test), Err(cleanup)) => Err(failure(format!(
            "test failed ({test}); cleanup of only its test directory also failed ({cleanup})"
        ))),
    }
}

#[test]
#[ignore = "requires an explicitly authorized Xunlei account and creates only AllDownloader-IT-* test data"]
fn live_xunlei_directory_upload_rename_move_download_and_trash() -> LiveResult<()> {
    require_live_authorization()?;
    let store = session_store()?;
    let session: XunleiSession = serde_json::from_slice(
        &store
            .load("xunlei")?
            .ok_or_else(|| failure("Xunlei is not currently authorized"))?,
    )?;
    eprintln!(
        "Xunlei live session: web_auth={}, user_id_present={}, browser_client={}, browser_device={}, browser_version={}, browser_package={}, browser_user_agent={}, captcha_present={}",
        matches!(session.auth_method, alldownloader_lib::cloud::CloudAuthMethod::Web),
        !session.user_id.is_empty(),
        !session.profile.client_id.is_empty(),
        !session.profile.device_id.is_empty(),
        !session.profile.client_version.is_empty(),
        !session.profile.package_name.is_empty(),
        !session.profile.user_agent.is_empty(),
        !session.security_token.is_empty() || !session.web_security.captcha_token.is_empty(),
    );
    let http = LiveTransport::new()?;
    let client = XunleiClient::new(session, &http);
    let (local_root, payload, mut expected) = local_artifact("xunlei")?;
    // Avoid reusing a previously indexed GCID when investigating readiness
    // immediately after a genuinely new object-store upload.
    expected.resize(32768, 0);
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut expected);
    fs::write(&payload, &expected)?;
    let base_name = test_name("xunlei", "root");
    let target_name = test_name("xunlei", "move-target");
    let renamed_name = test_name("xunlei", "renamed.txt");

    // This first request is intentionally mutation-free and is also the
    // real-world probe for headers carried by browser-bearer authorization.
    let initial_root = provider_step("Xunlei root", || client.directory("0"))?;
    if initial_root.items.iter().any(|item| item.name == base_name) {
        remove_local(&local_root);
        return Err(failure("unexpected test-directory name collision"));
    }
    provider_step("Xunlei create test root", || client.mkdir("0", &base_name))?;
    let base = find_named(
        &provider_step("Xunlei locate test root", || client.directory("0"))?.items,
        &base_name,
        "folder",
    )?;
    let result = (|| -> LiveResult<()> {
        provider_step("Xunlei create destination", || {
            client.mkdir(&base.id, &target_name)
        })?;
        let target = find_named(
            &provider_step("Xunlei locate destination", || client.directory(&base.id))?.items,
            &target_name,
            "folder",
        )?;
        let uploaded = provider_step("Xunlei upload", || client.upload_file(&base.id, &payload))?;
        // OSS transfer completion precedes the drive's asynchronous file
        // finalization; renaming a pending object returns file_rename_uncompleted.
        let mut upload_ready = false;
        for _ in 0..30 {
            if provider_step("Xunlei wait upload completion", || {
                client.directory(&base.id)
            })?
            .items
            .iter()
            .any(|file| file.id == uploaded.id)
            {
                upload_ready = true;
                break;
            }
            thread::sleep(Duration::from_secs(1));
        }
        if !upload_ready {
            return Err(failure(
                "Xunlei uploaded object was not finalized within 30 seconds",
            ));
        }
        provider_step("Xunlei rename", || {
            client.rename(&uploaded.id, &renamed_name)
        })?;

        // Xunlei hides non-complete files from ordinary directory results;
        // wait for the provider's asynchronous completion before moving it.
        let mut renamed = None;
        for _ in 0..30 {
            let files = provider_step("Xunlei verify rename", || client.directory(&base.id))?;
            if let Some(file) = files
                .items
                .into_iter()
                .find(|item| item.id == uploaded.id && item.name == renamed_name)
            {
                renamed = Some(file);
                break;
            }
            thread::sleep(Duration::from_secs(1));
        }
        let renamed =
            renamed.ok_or_else(|| failure("Xunlei upload did not reach complete state"))?;
        provider_step("Xunlei move", || {
            client.move_files(&[renamed.id.clone()], &target.id)
        })?;
        let moved = find_named(
            &provider_step("Xunlei verify move", || client.directory(&target.id))?.items,
            &renamed_name,
            "file",
        )?;
        provider_step("Xunlei copy", || {
            client.copy_files(&[moved.id.clone()], &base.id)
        })?;
        let copied = find_named(
            &provider_step("Xunlei verify copy", || client.directory(&base.id))?.items,
            &renamed_name,
            "file",
        )?;
        provider_step("Xunlei resolve immediate copied download", || {
            client.download(&copied.id)
        })?;
        let (url, download_name) =
            provider_step("Xunlei resolve download", || client.download(&moved.id))?;
        if download_name != renamed_name {
            return Err(failure(
                "Xunlei download metadata did not preserve the renamed file",
            ));
        }
        let session = client.session();
        if download_bytes_with_headers(
            &url,
            &[
                ("User-Agent", session.profile.user_agent.as_str()),
                ("Referer", "https://pan.xunlei.com/"),
            ],
        )? != expected
        {
            return Err(failure(
                "Xunlei downloaded bytes differ from the uploaded test file",
            ));
        }
        if copied.id == moved.id {
            return Err(failure("Xunlei copy returned the source object"));
        }
        for _ in 0..2 {
            provider_step("Xunlei resolve newly copied download", || {
                client.download(&copied.id)
            })?;
            provider_step("Xunlei resolve moved download again", || {
                client.download(&moved.id)
            })?;
        }
        provider_step("Xunlei trash copied fixture", || {
            client.trash(&[copied.id.clone()])
        })?;
        Ok(())
    })();

    // `trash` is a recycle-bin move.  The id was obtained by listing the
    // just-created uniquely named directory, so no user file can be targeted.
    let cleanup = client.trash(&[base.id.clone()]);
    if let Ok(()) = cleanup {
        // A token refresh is persisted only after all operations and cleanup.
        // Browser-only sessions simply write the equivalent current session.
        let session = client.session();
        store.save("xunlei", &serde_json::to_vec(&session)?)?;
    }
    remove_local(&local_root);
    match (result, cleanup) {
        (Ok(()), Ok(())) => {
            if contains_id(
                &provider_step("Xunlei verify trash", || client.directory("0"))?.items,
                &base.id,
            ) {
                Err(failure(
                    "Xunlei test directory remained visible after trash",
                ))
            } else {
                Ok(())
            }
        }
        (Err(test), Ok(())) => Err(test),
        (Ok(()), Err(cleanup)) => Err(failure(safe_provider_failure("Xunlei cleanup", &cleanup))),
        (Err(test), Err(cleanup)) => Err(failure(format!(
            "test failed ({test}); {}",
            safe_provider_failure("Xunlei cleanup of its test directory", &cleanup)
        ))),
    }
}

#[test]
#[ignore = "requires an explicitly authorized Xunlei account; performs only two read-only browser-contract probes"]
fn live_xunlei_browser_contract_probes_origin_and_referer_separately() -> LiveResult<()> {
    require_live_authorization()?;
    let store = session_store()?;
    let session: XunleiSession = serde_json::from_slice(
        &store
            .load("xunlei")?
            .ok_or_else(|| failure("Xunlei is not currently authorized"))?,
    )?;
    eprintln!(
        "Xunlei browser probe session: client={}, device={}, version={}, package={}, user_agent={}, captcha={}",
        !session.profile.client_id.is_empty(),
        !session.profile.device_id.is_empty(),
        !session.profile.client_version.is_empty(),
        !session.profile.package_name.is_empty(),
        !session.profile.user_agent.is_empty(),
        !session.security_token.is_empty(),
    );
    let http = LiveTransport::new()?;

    // Same endpoint and request shape; Origin/Referer are the only difference
    // between the two probes. This identifies whether these browser headers
    // are actually required without guessing or exposing credentials.
    let without_web_headers = xunlei_browser_root_probe(&http, &session, false);
    let with_web_headers = xunlei_browser_root_probe(&http, &session, true);
    match (without_web_headers, with_web_headers) {
        (Ok(()), Ok(())) => Ok(()),
        (without, with) => Err(failure(format!(
            "Xunlei browser probe outcomes: no Origin/Referer={}; with Origin/Referer={}",
            without.err().map_or("ok".into(), |error| error.to_string()),
            with.err().map_or("ok".into(), |error| error.to_string()),
        ))),
    }
}
