//! Opt-in diagnosis for one user-selected recycle-bin fixture. Mutation is
//! blocked unless the exact fixture name is explicitly confirmed separately.
use alldownloader_lib::cloud::{
    CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CloudProvider, DpapiCredentialStore,
    PikPakClient, ProviderRegistry, QuarkError, ReqwestTransport,
};
use serde_json::Value;
use std::{path::PathBuf, sync::Mutex};

const TARGET: &str = "AllDownloader-IT-pikpak-cache-1788958358211-root";
struct Probe {
    http: ReqwestTransport,
    allowed_id: Mutex<Option<String>>,
}
impl CloudHttpTransport for Probe {
    fn send(&self, mut request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        let url = url::Url::parse(&request.url).map_err(|_| QuarkError::InvalidResponse)?;
        let mut body: Value = request
            .body
            .as_deref()
            .and_then(|b| serde_json::from_str(b).ok())
            .unwrap_or(Value::Null);
        if url.path() == "/v1/shield/captcha/init"
            && std::env::var("PIKPAK_PROBE_NORMALIZE_DEVICE").as_deref() == Ok("1")
        {
            let device = body["device_id"]
                .as_str()
                .unwrap()
                .rsplit('.')
                .next()
                .unwrap()[..32]
                .to_owned();
            body["device_id"] = serde_json::json!(device);
            body["meta"]["captcha_sign"] =
                serde_json::json!(alldownloader_lib::cloud::pikpak_captcha_sign(
                    body["client_id"].as_str().unwrap(),
                    &device,
                    body["meta"]["timestamp"].as_str().unwrap()
                ));
            request.body = Some(body.to_string());
        }
        let deletion = url.path() == "/drive/v1/files:batchDelete";
        if deletion {
            let id = self.allowed_id.lock().unwrap();
            let expected = id.as_ref().map(|id| serde_json::json!([id]));
            if std::env::var("PIKPAK_DELETE_CONFIRMED_NAME").as_deref() != Ok(TARGET)
                || expected.as_ref() != body.get("ids")
            {
                eprintln!(
                    "PikPak probe: deletion blocked before network; ids_count={}",
                    body["ids"].as_array().map_or(0, Vec::len)
                );
                return Err(QuarkError::InvalidInput("probe-deletion-blocked".into()));
            }
        } else if request.method != "GET"
            && !matches!(url.path(), "/v1/shield/captcha/init" | "/v1/auth/token")
        {
            return Err(QuarkError::InvalidInput(
                "probe-unexpected-mutation-blocked".into(),
            ));
        }
        let device = request
            .headers
            .get("X-Device-Id")
            .map(String::as_str)
            .unwrap_or("");
        eprintln!("PikPak probe request: {} {}; header_device_len={}; body_device_len={}; device_match={}; action={}",
            request.method, url.path(), device.len(), body["device_id"].as_str().unwrap_or("").len(),
            body["device_id"].as_str().map_or(true, |d| d == device), body["action"].as_str().unwrap_or("none"));
        let response = self.http.send(request)?;
        let value: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
        if response.status >= 400 {
            let details = format!(
                "{} {} {}",
                value["error_details"], value["details"], value["error_description"]
            )
            .to_ascii_lowercase();
            eprintln!(
                "PikPak error detail categories: {:?}",
                [
                    "device",
                    "captcha",
                    "ids",
                    "file",
                    "folder",
                    "parameter",
                    "client",
                    "signature",
                    "space",
                    "with_folders",
                    "empty",
                    "invalid",
                    "sign",
                    "token",
                    "expired",
                    "argument",
                    "length",
                    "version",
                    "format"
                ]
                .into_iter()
                .filter(|word| details.contains(word))
                .collect::<Vec<_>>()
            );
        }
        eprintln!("PikPak probe response: HTTP {}; code={:?}; error_code={:?}; error_keys={:?}; challenge={}",
            response.status, value["code"].as_i64(), value["error_code"].as_i64(),
            value.as_object().map(|o| o.keys().collect::<Vec<_>>()),
            value["url"].as_str().is_some_and(|s| !s.is_empty()));
        Ok(response)
    }
}

#[test]
#[ignore = "requires ALLDOWNLOADER_LIVE_CLOUD=1; deletion blocked unless exact target confirmed"]
fn live_pikpak_selected_recycle_delete() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        std::env::var("ALLDOWNLOADER_LIVE_CLOUD").as_deref(),
        Ok("1")
    );
    let store = DpapiCredentialStore::new(
        std::env::var_os("ALLDOWNLOADER_TEST_CLOUD_SESSIONS")
            .map(PathBuf::from)
            .unwrap_or(
                PathBuf::from(std::env::var_os("APPDATA").ok_or("APPDATA missing")?)
                    .join("com.alldownloader.desktop/cloud-sessions"),
            ),
    )?;
    let http = Probe {
        http: ReqwestTransport::new()?,
        allowed_id: Mutex::new(None),
    };
    let client = PikPakClient::new(&http, &store);
    let recycle = ProviderRegistry::global()
        .get(CloudProvider::Pikpak)
        .recycle_bin_directory_id;
    let items = client.list(recycle)?.items;
    let matches = items
        .iter()
        .filter(|f| f.name == TARGET && f.kind == "folder")
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err("expected exactly one matching test directory in recycle bin".into());
    }
    let id = matches[0].id.clone();
    *http.allowed_id.lock().unwrap() = Some(id.clone());
    ProviderRegistry::global()
        .driver(CloudProvider::Pikpak, &store, &http)
        .delete(&[id.clone()])?;
    assert!(
        !client.list(recycle)?.items.iter().any(|f| f.id == id),
        "deleted fixture still listed"
    );
    eprintln!("PikPak selected fixture permanently deleted and absence verified");
    Ok(())
}
