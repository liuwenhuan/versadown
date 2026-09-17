//! Explicit, opt-in end-to-end regression for the production Aliyun and UC
//! provider drivers.
//!
//! Every mutation is contained below one freshly generated
//! `AllDownloader-E2E-*` directory. Existing user items are never selected or
//! mutated. Credential bytes, cookies and signed download URLs are never
//! printed.

use alldownloader_lib::cloud::{
    commands::CloudState, CloudDirectory, CloudError, CloudFile, CloudFileKind,
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudProvider, CloudShareOptions,
    CloudUploadTransport, CredentialStore, DpapiCredentialStore, ProviderRegistry,
    ReqwestTransport, ResolvedCloudDownload, UploadHttpRequest, UploadHttpResponse,
};
use std::{
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type LiveResult<T> = Result<T, Box<dyn Error>>;

struct LiveTransport {
    inner: ReqwestTransport,
    observations: Mutex<Vec<String>>,
}

impl LiveTransport {
    fn new() -> Result<Self, CloudError> {
        Ok(Self {
            inner: ReqwestTransport::new()?,
            observations: Mutex::new(Vec::new()),
        })
    }

    fn report(&self) {
        for observation in self.observations.lock().unwrap().iter() {
            eprintln!("LIVE-E2E request: {observation}");
        }
    }
}

impl CloudHttpTransport for LiveTransport {
    fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, CloudError> {
        let method = request.method;
        let endpoint = url::Url::parse(&request.url)
            .ok()
            .map(|url| format!("{}{}", url.host_str().unwrap_or("invalid-host"), url.path()))
            .unwrap_or_else(|| "invalid-endpoint".into());
        match self.inner.send(request) {
            Ok(response) => {
                let value = serde_json::from_str::<serde_json::Value>(&response.body).ok();
                let keys = value
                    .as_ref()
                    .and_then(serde_json::Value::as_object)
                    .map(|object| {
                        let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
                        keys.sort_unstable();
                        keys.join(",")
                    })
                    .unwrap_or_else(|| "non-object".into());
                let code = value
                    .as_ref()
                    .and_then(|value| value.get("code"))
                    .map(|value| match value {
                        serde_json::Value::Number(value) => value.to_string(),
                        serde_json::Value::String(value)
                            if value.len() <= 64
                                && value.bytes().all(|byte| {
                                    byte.is_ascii_alphanumeric() || b"._-".contains(&byte)
                                }) =>
                        {
                            value.clone()
                        }
                        _ => "redacted".into(),
                    })
                    .unwrap_or_else(|| "absent".into());
                let status = value
                    .as_ref()
                    .and_then(|value| value.get("status"))
                    .and_then(serde_json::Value::as_i64)
                    .map_or_else(|| "absent".into(), |value| value.to_string());
                let message = value
                    .as_ref()
                    .and_then(|value| value.get("message"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|_| code != "0" || status != "200")
                    .filter(|value| {
                        value.len() <= 256
                            && !value.contains(['\r', '\n'])
                            && !value.chars().any(char::is_control)
                    })
                    .unwrap_or("redacted");
                let data_shape = value
                    .as_ref()
                    .and_then(|value| value.get("data"))
                    .map(|value| match value {
                        serde_json::Value::Null => "null",
                        serde_json::Value::Bool(_) => "bool",
                        serde_json::Value::Number(_) => "number",
                        serde_json::Value::String(_) => "string",
                        serde_json::Value::Array(_) => "array",
                        serde_json::Value::Object(_) => "object",
                    })
                    .unwrap_or("absent");
                let data_keys = value
                    .as_ref()
                    .and_then(|value| value.get("data"))
                    .and_then(serde_json::Value::as_object)
                    .map(|object| {
                        let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
                        keys.sort_unstable();
                        keys.join(",")
                    })
                    .unwrap_or_else(|| "none".into());
                let upload_contract = if endpoint.ends_with("/file/upload/pre") {
                    value
                        .as_ref()
                        .and_then(|value| value.get("data"))
                        .and_then(serde_json::Value::as_object)
                        .map(|data| {
                            let string_field = |name: &str| {
                                data.get(name)
                                    .and_then(serde_json::Value::as_str)
                                    .map(|value| {
                                        if name == "upload_url" {
                                            url::Url::parse(value)
                                                .ok()
                                                .map(|url| {
                                                    format!(
                                                        "{}://{}{}",
                                                        url.scheme(),
                                                        url.host_str().unwrap_or("invalid-host"),
                                                        url.path()
                                                    )
                                                })
                                                .unwrap_or_else(|| "invalid-url".into())
                                        } else {
                                            format!("string(len={})", value.len())
                                        }
                                    })
                                    .unwrap_or_else(|| {
                                        data.get(name)
                                            .map(|value| format!("{}", value_type(value)))
                                            .unwrap_or_else(|| "absent".into())
                                    })
                            };
                            format!(
                                "; upload-contract bucket={:?}, upload_url={}, obj_key={}, auth_info={}, upload_id={}, callback={}",
                                data.get("bucket").and_then(serde_json::Value::as_str),
                                string_field("upload_url"),
                                string_field("obj_key"),
                                string_field("auth_info"),
                                string_field("upload_id"),
                                data.get("callback")
                                    .map(value_type)
                                    .unwrap_or("absent")
                            )
                        })
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let share_list_contract = if endpoint.ends_with("/share/mypage/detail") {
                    let entries = value
                        .as_ref()
                        .and_then(|value| value.pointer("/data/list"))
                        .and_then(serde_json::Value::as_array);
                    let count = entries.map_or(0, Vec::len);
                    let fields = entries
                        .into_iter()
                        .flatten()
                        .filter_map(serde_json::Value::as_object)
                        .flat_map(|entry| entry.keys().map(String::as_str))
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(",");
                    format!("; share-list count={count}; item-fields={fields}")
                } else {
                    String::new()
                };
                self.observations.lock().unwrap().push(format!(
                    "{method} {endpoint}; HTTP {}; code={code}; status={status}; message={message:?}; data={data_shape}({data_keys}); keys={keys}{upload_contract}{share_list_contract}",
                    response.status,
                ));
                Ok(response)
            }
            Err(error) => {
                self.observations.lock().unwrap().push(format!(
                    "{method} {endpoint}; transport={}",
                    error_kind(&error)
                ));
                Err(error)
            }
        }
    }
}

fn value_type(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

impl CloudUploadTransport for LiveTransport {
    fn send_upload(&self, request: UploadHttpRequest) -> Result<UploadHttpResponse, CloudError> {
        self.inner.send_upload(request)
    }
}

fn failure(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::other(message.into()))
}

fn error_kind(error: &CloudError) -> &'static str {
    match error {
        CloudError::NotAuthorized => "not-authorized",
        CloudError::AuthorizationExpired => "authorization-expired",
        CloudError::SessionRenewedRetryOperation => "session-renewed-retry-operation",
        CloudError::WebAuthorizationRequired => "web-authorization-required",
        CloudError::CaptchaRequired => "captcha-required",
        CloudError::PikpakRegionalMembershipRestricted => "regional-membership-restricted",
        CloudError::PikpakOperationVerificationRequired(_) => "operation-verification-required",
        CloudError::AuthorizationPending => "authorization-pending",
        CloudError::InvalidResponse => "invalid-response",
        CloudError::Storage => "storage",
        CloudError::Transport(_) => "transport",
        CloudError::Provider(message) if message.contains("UC 网盘账号未实名") => {
            "uc-real-name-verification-required"
        }
        CloudError::Provider(_) => "provider",
        CloudError::InvalidInput(_) => "invalid-input",
        CloudError::UploadUnavailable => "upload-unavailable",
        CloudError::ProviderUnsupported => "provider-unsupported",
    }
}

fn require_live_authorization() -> LiveResult<()> {
    if std::env::var("ALLDOWNLOADER_LIVE_CLOUD").as_deref() != Ok("1") {
        return Err(failure(
            "live cloud regression requires ALLDOWNLOADER_LIVE_CLOUD=1",
        ));
    }
    Ok(())
}

fn session_root() -> LiveResult<PathBuf> {
    let app_data = std::env::var_os("APPDATA")
        .ok_or_else(|| failure("APPDATA is required for the encrypted session store"))?;
    Ok(PathBuf::from(app_data)
        .join("com.alldownloader.desktop")
        .join("cloud-sessions"))
}

fn test_root_name(provider: CloudProvider) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!(
        "AllDownloader-E2E-{}-{millis}-{}",
        provider.key(),
        uuid::Uuid::new_v4().simple()
    )
}

fn step<T>(
    provider: CloudProvider,
    operation: &str,
    action: impl FnOnce() -> Result<T, CloudError>,
) -> LiveResult<T> {
    match action() {
        Ok(value) => {
            eprintln!("LIVE-E2E {} {operation}: PASS", provider.key());
            Ok(value)
        }
        Err(error) => {
            eprintln!(
                "LIVE-E2E {} {operation}: FAIL ({})",
                provider.key(),
                error_kind(&error)
            );
            Err(failure(format!(
                "{} {operation} failed: {}",
                provider.key(),
                error_kind(&error)
            )))
        }
    }
}

fn driver_list(
    state: &CloudState,
    http: &LiveTransport,
    provider: CloudProvider,
    parent: &str,
) -> Result<CloudDirectory, CloudError> {
    ProviderRegistry::global()
        .driver(provider, &state.store, http)
        .list_directory(parent)
}

fn create_directory(
    state: &CloudState,
    http: &LiveTransport,
    provider: CloudProvider,
    parent: &str,
    name: &str,
) -> Result<CloudFile, CloudError> {
    ProviderRegistry::global()
        .driver(provider, &state.store, http)
        .create_directory(parent, name)
}

fn find_named(items: &[CloudFile], name: &str, kind: CloudFileKind) -> LiveResult<CloudFile> {
    items
        .iter()
        .find(|item| item.name == name && item.kind == kind)
        .cloned()
        .ok_or_else(|| {
            failure(format!(
                "expected isolated {kind:?} {name:?} was not listed"
            ))
        })
}

fn wait_for_named(
    state: &CloudState,
    http: &LiveTransport,
    provider: CloudProvider,
    parent: &str,
    name: &str,
    kind: CloudFileKind,
) -> LiveResult<CloudFile> {
    let mut last = None;
    for _ in 0..20 {
        match driver_list(state, http, provider, parent) {
            Ok(directory) => match find_named(&directory.items, name, kind) {
                Ok(file) => return Ok(file),
                Err(error) => last = Some(error),
            },
            Err(error) => {
                last = Some(failure(format!(
                    "{} listing failed while waiting: {}",
                    provider.key(),
                    error_kind(&error)
                )))
            }
        }
        thread::sleep(Duration::from_secs(1));
    }
    Err(last.unwrap_or_else(|| failure("provider did not expose the expected item")))
}

fn wait_until_absent(
    state: &CloudState,
    http: &LiveTransport,
    provider: CloudProvider,
    parent: &str,
    id: &str,
) -> LiveResult<()> {
    for _ in 0..20 {
        let directory = driver_list(state, http, provider, parent).map_err(|error| {
            failure(format!(
                "{} listing failed while waiting for deletion: {}",
                provider.key(),
                error_kind(&error)
            ))
        })?;
        if !directory.items.iter().any(|item| item.id == id) {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(1));
    }
    Err(failure("isolated item remained visible after deletion"))
}

fn cleanup_abandoned_uc_e2e_shares(
    state: &CloudState,
    http: &LiveTransport,
) -> LiveResult<()> {
    let provider = CloudProvider::Uc;
    let shares = ProviderRegistry::global()
        .driver(provider, &state.store, http)
        .list_shares()
        .map_err(|error| {
            failure(format!(
                "UC test-share cleanup listing failed: {}",
                error_kind(&error)
            ))
        })?;
    let ids = shares
        .into_iter()
        .filter(|share| share.name.starts_with("AllDownloader-E2E-uc-"))
        .map(|share| share.id)
        .collect::<Vec<_>>();
    for (index, share_id) in ids.into_iter().enumerate() {
        ProviderRegistry::global()
            .driver(provider, &state.store, http)
            .cancel_shares(std::slice::from_ref(&share_id))
            .map_err(|error| {
                failure(format!(
                    "UC abandoned test-share cleanup item #{index} failed: {error}",
                ))
            })?;
    }
    Ok(())
}

fn upload_file(
    state: &CloudState,
    http: &LiveTransport,
    provider: CloudProvider,
    parent: &str,
    path: &Path,
) -> Result<CloudFile, CloudError> {
    ProviderRegistry::global()
        .driver(provider, &state.store, http)
        .upload(state, http, parent, path)
}

fn resolve_download(
    state: &CloudState,
    http: &LiveTransport,
    provider: CloudProvider,
    file: &str,
) -> Result<ResolvedCloudDownload, CloudError> {
    ProviderRegistry::global()
        .driver(provider, &state.store, http)
        .resolve_download(file)
}

fn download_bytes(download: ResolvedCloudDownload) -> LiveResult<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let mut request = client.get(&download.url);
    for (name, value) in download.headers {
        request = request.header(name, value);
    }
    let response = request.send()?;
    if !response.status().is_success() {
        return Err(failure(format!(
            "direct download returned HTTP {}",
            response.status()
        )));
    }
    Ok(response.bytes()?.to_vec())
}

fn rename(
    state: &CloudState,
    http: &LiveTransport,
    provider: CloudProvider,
    id: &str,
    name: &str,
) -> Result<(), CloudError> {
    ProviderRegistry::global()
        .driver(provider, &state.store, http)
        .rename(id, name)
}

fn move_item(
    state: &CloudState,
    http: &LiveTransport,
    provider: CloudProvider,
    id: &str,
    parent: &str,
) -> Result<(), CloudError> {
    ProviderRegistry::global()
        .driver(provider, &state.store, http)
        .move_files(&[id.to_owned()], parent)
}

fn trash(
    state: &CloudState,
    http: &LiveTransport,
    provider: CloudProvider,
    id: &str,
) -> Result<(), CloudError> {
    ProviderRegistry::global()
        .driver(provider, &state.store, http)
        .trash(&[id.to_owned()])
}

fn purge_named_from_recycle(
    state: &CloudState,
    http: &LiveTransport,
    provider: CloudProvider,
    names: &[&str],
) -> LiveResult<()> {
    let recycle_id = ProviderRegistry::global()
        .get(provider)
        .recycle_bin_directory_id;
    let required_root_name = names
        .first()
        .ok_or_else(|| failure("cleanup requires the isolated root name"))?;
    let mut found_root = false;
    for attempt in 0..20 {
        let recycle = driver_list(state, http, provider, recycle_id).map_err(|error| {
            failure(format!(
                "{} recycle listing failed during cleanup: {}",
                provider.key(),
                error_kind(&error)
            ))
        })?;
        if recycle
            .items
            .iter()
            .any(|item| item.name == *required_root_name)
        {
            found_root = true;
            let ids = recycle
                .items
                .into_iter()
                .filter(|item| names.contains(&item.name.as_str()))
                .map(|item| item.id)
                .collect::<Vec<_>>();
            ProviderRegistry::global()
                .driver(provider, &state.store, http)
                .delete(&ids)
                .map_err(|error| {
                    failure(format!(
                        "{} permanent cleanup failed: {}",
                        provider.key(),
                        error_kind(&error)
                    ))
                })?;
            break;
        }
        if attempt < 19 {
            thread::sleep(Duration::from_millis(250));
        }
    }
    if !found_root {
        return Err(failure("isolated root did not appear in the recycle bin"));
    }
    for attempt in 0..20 {
        let recycle = driver_list(state, http, provider, recycle_id)?;
        if !recycle
            .items
            .iter()
            .any(|item| names.contains(&item.name.as_str()))
        {
            return Ok(());
        }
        if attempt < 19 {
            thread::sleep(Duration::from_millis(250));
        }
    }
    Err(failure(
        "isolated entries remained in recycle after cleanup",
    ))
}

fn write_local_fixture(
    provider: CloudProvider,
) -> LiveResult<(tempfile::TempDir, PathBuf, Vec<u8>, PathBuf, Vec<u8>)> {
    let root = tempfile::Builder::new()
        .prefix(&format!("AllDownloader-E2E-{}-", provider.key()))
        .tempdir()?;
    let single_bytes = format!(
        "AllDownloader {} single-file live fixture\n",
        provider.key()
    )
    .into_bytes();
    let single = root.path().join("single.txt");
    fs::write(&single, &single_bytes)?;
    let nested_dir = root.path().join("upload-directory").join("child");
    fs::create_dir_all(&nested_dir)?;
    let nested_bytes =
        format!("AllDownloader {} nested live fixture\n", provider.key()).into_bytes();
    let nested = nested_dir.join("nested.txt");
    fs::write(&nested, &nested_bytes)?;
    Ok((root, single, single_bytes, nested, nested_bytes))
}

fn run_provider(provider: CloudProvider) -> LiveResult<()> {
    require_live_authorization()?;
    let root_id = ProviderRegistry::global().get(provider).root_directory_id;
    let credentials_root = session_root()?;
    let direct_store = DpapiCredentialStore::new(credentials_root.clone())?;
    direct_store.load(provider.key()).map_err(|error| {
        failure(format!(
            "{} credential decrypt failed: {}",
            provider.key(),
            error_kind(&error)
        ))
    })?;
    eprintln!("LIVE-E2E {} credential decrypt: PASS", provider.key());
    let state = CloudState::new(credentials_root).map_err(|error| {
        failure(format!(
            "{} CloudState initialization failed: {}",
            provider.key(),
            error_kind(&error)
        ))
    })?;
    if state.store.load(provider.key())?.is_none() {
        return Err(failure(format!(
            "{} is not currently authorized",
            provider.key()
        )));
    }
    let http = LiveTransport::new()?;
    let (_local, single_path, single_bytes, nested_path, nested_bytes) =
        write_local_fixture(provider)?;
    let test_name = test_root_name(provider);
    let delete_file_name = "delete-me.txt";
    let delete_directory_name = "delete-directory";

    step(provider, "account", || {
        ProviderRegistry::global()
            .driver(provider, &state.store, &http)
            .account()
    })?;
    let initial = step(provider, "list root", || {
        driver_list(&state, &http, provider, root_id)
    })?;
    if initial.items.iter().any(|item| item.name == test_name) {
        return Err(failure("unexpected isolated test-root name collision"));
    }

    let test_root = match step(provider, "create directory", || {
        create_directory(&state, &http, provider, root_id, &test_name)
    }) {
        Ok(root) => root,
        Err(error) => {
            http.report();
            if let Ok(created) = wait_for_named(
                &state,
                &http,
                provider,
                root_id,
                &test_name,
                CloudFileKind::Folder,
            ) {
                let _ = trash(&state, &http, provider, &created.id);
                let _ = purge_named_from_recycle(&state, &http, provider, &[&test_name]);
            }
            return Err(error);
        }
    };
    let result = (|| -> LiveResult<()> {
        let move_target = step(provider, "create move target", || {
            create_directory(&state, &http, provider, &test_root.id, "move-target")
        })?;

        let uploaded = step(provider, "upload single file", || {
            upload_file(&state, &http, provider, &test_root.id, &single_path)
        })?;
        let renamed_file_name = "single-renamed.txt";
        step(provider, "rename file", || {
            rename(&state, &http, provider, &uploaded.id, renamed_file_name)
        })?;
        let renamed_file = wait_for_named(
            &state,
            &http,
            provider,
            &test_root.id,
            renamed_file_name,
            CloudFileKind::File,
        )?;
        step(provider, "move file", || {
            move_item(&state, &http, provider, &renamed_file.id, &move_target.id)
        })?;
        let moved_file = wait_for_named(
            &state,
            &http,
            provider,
            &move_target.id,
            renamed_file_name,
            CloudFileKind::File,
        )?;
        let single_download = step(provider, "resolve single-file download", || {
            resolve_download(&state, &http, provider, &moved_file.id)
        })?;
        if download_bytes(single_download)? != single_bytes {
            return Err(failure("single-file download bytes differ from upload"));
        }
        eprintln!("LIVE-E2E {} download single file: PASS", provider.key());

        let upload_directory = step(provider, "create upload directory", || {
            create_directory(&state, &http, provider, &test_root.id, "uploaded-directory")
        })?;
        let upload_child = step(provider, "create upload subdirectory", || {
            create_directory(&state, &http, provider, &upload_directory.id, "child")
        })?;
        let nested_file = step(provider, "upload nested directory file", || {
            upload_file(&state, &http, provider, &upload_child.id, &nested_path)
        })?;
        eprintln!("LIVE-E2E {} upload directory: PASS", provider.key());
        let nested_listing = step(provider, "list downloaded directory recursively", || {
            driver_list(&state, &http, provider, &upload_child.id)
        })?;
        let nested_file = nested_listing
            .items
            .into_iter()
            .find(|item| item.id == nested_file.id)
            .ok_or_else(|| failure("nested uploaded file was not listed"))?;
        let nested_download = step(provider, "resolve nested directory download", || {
            resolve_download(&state, &http, provider, &nested_file.id)
        })?;
        if download_bytes(nested_download)? != nested_bytes {
            return Err(failure("directory download bytes differ from upload"));
        }
        eprintln!("LIVE-E2E {} download directory: PASS", provider.key());

        let rename_folder = step(provider, "create folder to rename", || {
            create_directory(
                &state,
                &http,
                provider,
                &test_root.id,
                "folder-before-rename",
            )
        })?;
        step(provider, "rename folder", || {
            rename(
                &state,
                &http,
                provider,
                &rename_folder.id,
                "folder-after-rename",
            )
        })?;
        let renamed_folder = wait_for_named(
            &state,
            &http,
            provider,
            &test_root.id,
            "folder-after-rename",
            CloudFileKind::Folder,
        )?;
        step(provider, "move directory", || {
            move_item(&state, &http, provider, &renamed_folder.id, &move_target.id)
        })?;
        wait_for_named(
            &state,
            &http,
            provider,
            &move_target.id,
            "folder-after-rename",
            CloudFileKind::Folder,
        )?;

        let delete_file = step(provider, "upload file for deletion", || {
            upload_file(&state, &http, provider, &test_root.id, &single_path)
        })?;
        step(provider, "rename deletion fixture", || {
            rename(&state, &http, provider, &delete_file.id, delete_file_name)
        })?;
        let delete_file = wait_for_named(
            &state,
            &http,
            provider,
            &test_root.id,
            delete_file_name,
            CloudFileKind::File,
        )?;
        step(provider, "delete file", || {
            trash(&state, &http, provider, &delete_file.id)
        })?;
        wait_until_absent(&state, &http, provider, &test_root.id, &delete_file.id)?;

        let delete_directory = step(provider, "create recursive-delete directory", || {
            create_directory(
                &state,
                &http,
                provider,
                &test_root.id,
                delete_directory_name,
            )
        })?;
        let delete_child = step(provider, "create recursive-delete child", || {
            create_directory(&state, &http, provider, &delete_directory.id, "child")
        })?;
        step(provider, "upload recursive-delete descendant", || {
            upload_file(&state, &http, provider, &delete_child.id, &nested_path)
        })?;
        step(provider, "delete directory recursively", || {
            trash(&state, &http, provider, &delete_directory.id)
        })?;
        wait_until_absent(&state, &http, provider, &test_root.id, &delete_directory.id)?;
        Ok(())
    })();

    let cleanup = trash(&state, &http, provider, &test_root.id)
        .map_err(|error| {
            failure(format!(
                "isolated root cleanup failed: {}",
                error_kind(&error)
            ))
        })
        .and_then(|()| wait_until_absent(&state, &http, provider, root_id, &test_root.id))
        .and_then(|()| {
            purge_named_from_recycle(
                &state,
                &http,
                provider,
                &[&test_name, delete_file_name, delete_directory_name],
            )
        });

    http.report();
    match (result, cleanup) {
        (Ok(()), Ok(())) => {
            eprintln!("LIVE-E2E {} cleanup: PASS", provider.key());
            Ok(())
        }
        (Err(test), Ok(())) => Err(test),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(test), Err(cleanup)) => Err(failure(format!(
            "test failed ({test}); cleanup also failed ({cleanup})"
        ))),
    }
}

fn run_uc_folder_matrix() -> LiveResult<()> {
    require_live_authorization()?;
    let provider = CloudProvider::Uc;
    let root_id = ProviderRegistry::global().get(provider).root_directory_id;
    let state = CloudState::new(session_root()?)?;
    let http = LiveTransport::new()?;
    let test_name = test_root_name(provider);
    let test_root = step(provider, "folder-matrix create root", || {
        create_directory(&state, &http, provider, root_id, &test_name)
    })?;
    let result = (|| -> LiveResult<()> {
        let target = step(provider, "folder-matrix create move target", || {
            create_directory(&state, &http, provider, &test_root.id, "move-target")
        })?;
        let source = step(provider, "folder-matrix create rename source", || {
            create_directory(
                &state,
                &http,
                provider,
                &test_root.id,
                "folder-before-rename",
            )
        })?;
        step(provider, "folder-matrix create nested child", || {
            create_directory(&state, &http, provider, &source.id, "nested-child")
        })?;
        step(provider, "folder-matrix rename folder", || {
            rename(&state, &http, provider, &source.id, "folder-after-rename")
        })?;
        let renamed = wait_for_named(
            &state,
            &http,
            provider,
            &test_root.id,
            "folder-after-rename",
            CloudFileKind::Folder,
        )?;
        step(provider, "folder-matrix move directory", || {
            move_item(&state, &http, provider, &renamed.id, &target.id)
        })?;
        let moved = wait_for_named(
            &state,
            &http,
            provider,
            &target.id,
            "folder-after-rename",
            CloudFileKind::Folder,
        )?;
        let nested = driver_list(&state, &http, provider, &moved.id)?;
        find_named(&nested.items, "nested-child", CloudFileKind::Folder)?;
        eprintln!("LIVE-E2E uc move directory with child: PASS");

        let delete_directory = step(
            provider,
            "folder-matrix create recursive-delete root",
            || create_directory(&state, &http, provider, &test_root.id, "delete-directory"),
        )?;
        step(
            provider,
            "folder-matrix create recursive-delete child",
            || create_directory(&state, &http, provider, &delete_directory.id, "child"),
        )?;
        step(
            provider,
            "folder-matrix delete directory recursively",
            || trash(&state, &http, provider, &delete_directory.id),
        )?;
        wait_until_absent(&state, &http, provider, &test_root.id, &delete_directory.id)?;
        Ok(())
    })();
    let cleanup = trash(&state, &http, provider, &test_root.id)
        .map_err(|error| {
            failure(format!(
                "UC folder root cleanup failed: {}",
                error_kind(&error)
            ))
        })
        .and_then(|()| wait_until_absent(&state, &http, provider, root_id, &test_root.id))
        .and_then(|()| {
            purge_named_from_recycle(&state, &http, provider, &[&test_name, "delete-directory"])
        });
    http.report();
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(test), Ok(())) => Err(test),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(test), Err(cleanup)) => Err(failure(format!(
            "folder test failed ({test}); cleanup also failed ({cleanup})"
        ))),
    }
}

fn run_uc_requested_upload() -> LiveResult<()> {
    require_live_authorization()?;
    let path = std::env::var_os("ALLDOWNLOADER_LIVE_UC_UPLOAD_PATH")
        .map(PathBuf::from)
        .ok_or_else(|| failure("ALLDOWNLOADER_LIVE_UC_UPLOAD_PATH is required"))?;
    let metadata = fs::metadata(&path)?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| failure("requested upload filename is invalid"))?;
    let provider = CloudProvider::Uc;
    let root_id = ProviderRegistry::global().get(provider).root_directory_id;
    let state = CloudState::new(session_root()?)?;
    let http = LiveTransport::new()?;
    let test_name = test_root_name(provider);
    let test_root = step(provider, "requested-upload create root", || {
        create_directory(&state, &http, provider, root_id, &test_name)
    })?;
    let result = (|| -> LiveResult<()> {
        let uploaded = step(provider, "requested file upload", || {
            upload_file(&state, &http, provider, &test_root.id, &path)
        })?;
        if uploaded.name != name || uploaded.size != Some(metadata.len()) {
            return Err(failure("requested upload returned mismatched metadata"));
        }
        let listed = wait_for_named(
            &state,
            &http,
            provider,
            &test_root.id,
            name,
            CloudFileKind::File,
        )?;
        if listed.size != Some(metadata.len()) {
            return Err(failure("requested upload listing returned mismatched size"));
        }
        Ok(())
    })();
    let cleanup = trash(&state, &http, provider, &test_root.id)
        .map_err(|error| {
            failure(format!(
                "requested upload cleanup failed: {}",
                error_kind(&error)
            ))
        })
        .and_then(|()| wait_until_absent(&state, &http, provider, root_id, &test_root.id))
        .and_then(|()| purge_named_from_recycle(&state, &http, provider, &[&test_name]));
    http.report();
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(test), Ok(())) => Err(test),
        (Ok(()), Err(cleanup)) => Err(cleanup),
        (Err(test), Err(cleanup)) => Err(failure(format!(
            "requested upload failed ({test}); cleanup also failed ({cleanup})"
        ))),
    }
}

fn run_uc_personal_share_lifecycle() -> LiveResult<()> {
    require_live_authorization()?;
    let provider = CloudProvider::Uc;
    let root_id = ProviderRegistry::global().get(provider).root_directory_id;
    let credentials_root = session_root()?;
    let direct_store = DpapiCredentialStore::new(credentials_root.clone())?;
    direct_store.load(provider.key()).map_err(|error| {
        failure(format!(
            "{} credential decrypt failed: {}",
            provider.key(),
            error_kind(&error)
        ))
    })?;
    eprintln!("LIVE-E2E {} credential decrypt: PASS", provider.key());
    let state = CloudState::new(credentials_root)?;
    if state.store.load(provider.key())?.is_none() {
        return Err(failure(format!(
            "{} is not currently authorized",
            provider.key()
        )));
    }
    let http = LiveTransport::new()?;
    if let Err(error) = cleanup_abandoned_uc_e2e_shares(&state, &http) {
        http.report();
        return Err(error);
    }
    let test_name = test_root_name(provider);
    let test_root = match step(provider, "personal-share create root", || {
        create_directory(&state, &http, provider, root_id, &test_name)
    }) {
        Ok(root) => root,
        Err(error) => {
            http.report();
            if let Ok(created) = wait_for_named(
                &state,
                &http,
                provider,
                root_id,
                &test_name,
                CloudFileKind::Folder,
            ) {
                let _ = trash(&state, &http, provider, &created.id);
                let _ = purge_named_from_recycle(&state, &http, provider, &[&test_name]);
            }
            return Err(error);
        }
    };
    let mut created_share_id = None;
    let result = (|| -> LiveResult<()> {
        step(provider, "personal-share create child", || {
            create_directory(&state, &http, provider, &test_root.id, "share-source")
        })?;
        let share = step(provider, "personal-share create", || {
            ProviderRegistry::global()
                .driver(provider, &state.store, &http)
                .create_share(
                    std::slice::from_ref(&test_root.id),
                    &CloudShareOptions {
                        name: test_name.clone(),
                        pass_code: None,
                        generate_pass_code: false,
                        expires_in_days: None,
                    },
                )
        })?;
        if share.id.is_empty() {
            return Err(failure("UC personal-share create returned an empty ID"));
        }
        let share_url = url::Url::parse(&share.url)
            .map_err(|_| failure("UC personal-share create returned an invalid URL"))?;
        if share_url.scheme() != "https" || share_url.host_str().is_none() {
            return Err(failure(
                "UC personal-share create did not return an HTTPS URL",
            ));
        }
        created_share_id = Some(share.id.clone());
        let listed = step(provider, "personal-share list", || {
            ProviderRegistry::global()
                .driver(provider, &state.store, &http)
                .list_shares()
        })?;
        if !listed.iter().any(|candidate| candidate.id == share.id) {
            return Err(failure("UC personal-share list did not contain the created share"));
        }
        step(provider, "personal-share cancel", || {
            ProviderRegistry::global()
                .driver(provider, &state.store, &http)
                .cancel_shares(std::slice::from_ref(&share.id))
        })?;
        let listed_after_cancellation = step(provider, "personal-share list after cancel", || {
            ProviderRegistry::global()
                .driver(provider, &state.store, &http)
                .list_shares()
        })?;
        if listed_after_cancellation
            .iter()
            .any(|candidate| candidate.id == share.id)
        {
            return Err(failure(
                "UC personal-share list still contained the cancelled share",
            ));
        }
        created_share_id = None;
        eprintln!("LIVE-E2E uc personal-share cancellation list confirmation: PASS");
        Ok(())
    })();
    let share_cleanup = match created_share_id.take() {
        Some(share_id) => ProviderRegistry::global()
            .driver(provider, &state.store, &http)
            .cancel_shares(std::slice::from_ref(&share_id))
            .map_err(|error| {
                failure(format!(
                    "UC personal-share cleanup failed: {}",
                    error_kind(&error)
                ))
            }),
        None => Ok(()),
    };
    let directory_cleanup = trash(&state, &http, provider, &test_root.id)
        .map_err(|error| {
            failure(format!(
                "UC personal-share root cleanup failed: {}",
                error_kind(&error)
            ))
        })
        .and_then(|()| wait_until_absent(&state, &http, provider, root_id, &test_root.id))
        .and_then(|()| purge_named_from_recycle(&state, &http, provider, &[&test_name]));
    http.report();
    match (result, share_cleanup, directory_cleanup) {
        (Ok(()), Ok(()), Ok(())) => {
            eprintln!("LIVE-E2E uc personal-share cleanup: PASS");
            Ok(())
        }
        (Err(test), Ok(()), Ok(())) => Err(test),
        (Ok(()), Err(share_cleanup), Ok(())) => Err(share_cleanup),
        (Ok(()), Ok(()), Err(directory_cleanup)) => Err(directory_cleanup),
        (Err(test), Err(share_cleanup), Ok(())) => Err(failure(format!(
            "personal-share test failed ({test}); share cleanup also failed ({share_cleanup})"
        ))),
        (Err(test), Ok(()), Err(directory_cleanup)) => Err(failure(format!(
            "personal-share test failed ({test}); directory cleanup also failed ({directory_cleanup})"
        ))),
        (Ok(()), Err(share_cleanup), Err(directory_cleanup)) => Err(failure(format!(
            "personal-share share cleanup failed ({share_cleanup}); directory cleanup also failed ({directory_cleanup})"
        ))),
        (Err(test), Err(share_cleanup), Err(directory_cleanup)) => Err(failure(format!(
            "personal-share test failed ({test}); share cleanup failed ({share_cleanup}); directory cleanup failed ({directory_cleanup})"
        ))),
    }
}

#[test]
#[ignore = "requires ALLDOWNLOADER_LIVE_CLOUD=1 and the current user's authorized Aliyun session"]
fn live_aliyun_complete_crud_and_transfer_matrix() -> LiveResult<()> {
    run_provider(CloudProvider::Aliyun)
}

#[test]
#[ignore = "requires ALLDOWNLOADER_LIVE_CLOUD=1 and the current user's authorized Aliyun session"]
fn live_aliyun_trash_already_absent_file_is_idempotent() -> LiveResult<()> {
    require_live_authorization()?;
    let state = CloudState::new(session_root()?)?;
    let http = LiveTransport::new()?;
    let result = trash(
        &state,
        &http,
        CloudProvider::Aliyun,
        "0000000000000000000000000000000000000000",
    );
    http.report();
    result.map_err(|error| {
        failure(format!(
            "Aliyun trash should be idempotent for an already absent file: {}",
            error_kind(&error)
        ))
    })
}

#[test]
#[ignore = "requires ALLDOWNLOADER_LIVE_CLOUD=1 and the current user's authorized UC session"]
fn live_uc_complete_crud_and_transfer_matrix() -> LiveResult<()> {
    run_provider(CloudProvider::Uc)
}

#[test]
#[ignore = "requires the current user's authorized UC session; covers folder operations when account policy blocks uploads"]
fn live_uc_folder_operations_matrix() -> LiveResult<()> {
    run_uc_folder_matrix()
}

#[test]
#[ignore = "requires ALLDOWNLOADER_LIVE_CLOUD=1, ALLDOWNLOADER_LIVE_UC_UPLOAD_PATH and an authorized UC session"]
fn live_uc_requested_file_upload() -> LiveResult<()> {
    run_uc_requested_upload()
}

#[test]
#[ignore = "requires ALLDOWNLOADER_LIVE_CLOUD=1 and the current user's authorized UC session; creates, cancels, and cleans up one isolated personal share"]
fn live_uc_personal_share_lifecycle() -> LiveResult<()> {
    run_uc_personal_share_lifecycle()
}

#[test]
#[ignore = "read-only cleanup audit for AllDownloader-E2E Aliyun and UC test roots"]
fn live_no_aliyun_or_uc_e2e_artifacts_remain() -> LiveResult<()> {
    require_live_authorization()?;
    let state = CloudState::new(session_root()?)?;
    let http = LiveTransport::new()?;
    let mut leftovers = Vec::new();
    for provider in [CloudProvider::Aliyun, CloudProvider::Uc] {
        let descriptor = ProviderRegistry::global().get(provider);
        for directory_id in [
            descriptor.root_directory_id,
            descriptor.recycle_bin_directory_id,
        ] {
            let directory = driver_list(&state, &http, provider, directory_id)?;
            leftovers.extend(
                directory
                    .items
                    .into_iter()
                    .filter(|item| {
                        item.name
                            .starts_with(&format!("AllDownloader-E2E-{}-", provider.key()))
                    })
                    .map(|item| format!("{}:{}", provider.key(), item.name)),
            );
        }
    }
    if leftovers.is_empty() {
        Ok(())
    } else {
        Err(failure(format!(
            "isolated E2E artifacts remain: {}",
            leftovers.join(", ")
        )))
    }
}
