//! Opt-in, read-only download diagnostics. Never prints signed URLs or credentials.
use rusqlite::{Connection, OpenFlags};
use std::{io::Read, path::PathBuf, time::Duration};
use versadown_lib::cloud::{
    commands::parse_stable_cloud_source, CloudHttpRequest, CloudHttpResponse, CloudHttpTransport,
    PikPakClient, QuarkError, ReqwestTransport, SystemCredentialStore,
};

struct UsageTransport {
    inner: ReqwestTransport,
    fetch: bool,
}
impl CloudHttpTransport for UsageTransport {
    fn send(&self, mut request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        if self.fetch {
            request.url = request.url.replace("usage=CACHE", "usage=FETCH");
        }
        let detail = request.url.contains("/drive/v1/files/");
        let response = self.inner.send(request)?;
        if detail {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&response.body) {
                println!(
                    "{}",
                    serde_json::json!({"file_fields":value.as_object().map(|v| v.keys().collect::<Vec<_>>()),
                    "phase":value["phase"],"size":value["size"],
                    "link_types":value["links"].as_object().map(|v|v.keys().collect::<Vec<_>>())})
                );
                if let Some(links) = value["links"].as_object() {
                    for (index, link) in links.values().take(3).enumerate() {
                        if let Some(uri) =
                            link["url"].as_str().filter(|u| u.starts_with("https://"))
                        {
                            probe(&format!("provider-link-{index}"), uri);
                        }
                    }
                }
            }
        }
        Ok(response)
    }
}
fn probe(label: &str, uri: &str) {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    match client
        .get(uri)
        .header("Range", "bytes=0-1023")
        .header("User-Agent", "aria2-next/3.0.9")
        .send()
    {
        Ok(mut response) => {
            let status = response.status().as_u16();
            let mut bytes = Vec::new();
            let complete = (&mut response).take(2048).read_to_end(&mut bytes).is_ok();
            println!(
                "{}",
                serde_json::json!({"probe":label,"http":status,"bytes":bytes.len(),"complete":complete})
            );
        }
        Err(_) => println!(
            "{}",
            serde_json::json!({"probe":label,"request_failed":true})
        ),
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("VERSADOWN_LIVE_CLOUD").as_deref() != Ok("1") {
        return Err("Explicit live-test opt-in required".into());
    }
    let task_id = std::env::args().nth(1).ok_or("Task ID required")?;
    let root = PathBuf::from(std::env::var("APPDATA")?).join("com.versadown.desktop");
    let db =
        Connection::open_with_flags(root.join("tasks.sqlite"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let (source, gid): (String, String) = db.query_row(
        "SELECT source,aria_gid FROM tasks WHERE id=?1",
        [&task_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (provider, file_id) = parse_stable_cloud_source(&source)?;
    if provider.key() != "pikpak" {
        return Err("Not a PikPak task".into());
    }
    let checkpoints = Connection::open_with_flags(
        root.join("aria2-next-state/stream/state.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let old: String =
        checkpoints.query_row("SELECT uri FROM downloads WHERE gid=?1", [&gid], |r| {
            r.get(0)
        })?;
    probe("old", &old);
    let store = SystemCredentialStore::new(root.join("cloud-sessions"))?;
    for fetch in [false, true] {
        let transport = UsageTransport {
            inner: ReqwestTransport::new()?,
            fetch,
        };
        let label = if fetch { "fresh-fetch" } else { "fresh-cache" };
        match PikPakClient::new(&transport, &store).download(&file_id) {
            Ok((uri, _)) => {
                println!(
                    "{}",
                    serde_json::json!({"probe":label,"same_as_old":uri==old})
                );
                probe(label, &uri);
            }
            Err(error) => {
                let value = serde_json::to_value(error)?;
                println!(
                    "{}",
                    serde_json::json!({"probe":label,"api_error_code":value["code"]})
                );
            }
        }
    }
    Ok(())
}
