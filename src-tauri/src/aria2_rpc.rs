use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use serde::Deserialize;
use serde_json::{json, Map, Value};
use thiserror::Error;
use uuid::Uuid;

use crate::ProxySettings;

#[derive(Debug, Error)]
pub enum Aria2RpcError {
    #[error("aria2 RPC I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("aria2 RPC JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("aria2 RPC error: {0}")]
    Rpc(String),
    #[error("aria2 RPC returned an invalid response")]
    InvalidResponse,
    #[error("aria2 RPC did not start in time")]
    StartupTimeout,
}

#[derive(Clone)]
pub struct Aria2Client {
    address: SocketAddr,
    secret: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Aria2File {
    pub index: String,
    pub path: String,
    pub length: String,
    pub completed_length: String,
    pub selected: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Aria2TorrentInfo {
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Aria2BitTorrent {
    pub info: Option<Aria2TorrentInfo>,
    pub announce_list: Vec<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Aria2Status {
    pub gid: String,
    pub status: String,
    pub total_length: String,
    pub completed_length: String,
    pub download_speed: String,
    pub error_code: String,
    pub error_message: String,
    pub followed_by: Vec<String>,
    pub connections: String,
    pub files: Vec<Aria2File>,
    pub bittorrent: Option<Aria2BitTorrent>,
}

impl Aria2Client {
    pub fn new(address: SocketAddr, secret: String) -> Self {
        Self { address, secret }
    }

    fn rpc(&self, method: &str, params: Vec<Value>) -> Result<Value, Aria2RpcError> {
        let mut authenticated = vec![Value::String(format!("token:{}", self.secret))];
        authenticated.extend(params);
        let body = serde_json::to_vec(
            &json!({ "jsonrpc": "2.0", "id": Uuid::new_v4().to_string(), "method": method, "params": authenticated }),
        )?;
        let mut stream = TcpStream::connect_timeout(&self.address, Duration::from_secs(2))?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;
        write!(stream, "POST /jsonrpc HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", self.address, body.len())?;
        stream.write_all(&body)?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response)?;
        let body_start = response
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .ok_or(Aria2RpcError::InvalidResponse)?
            + 4;
        let response: Value = serde_json::from_slice(&response[body_start..])?;
        if let Some(error) = response.get("error") {
            return Err(Aria2RpcError::Rpc(
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown RPC error")
                    .to_string(),
            ));
        }
        response
            .get("result")
            .cloned()
            .ok_or(Aria2RpcError::InvalidResponse)
    }

    pub fn ping(&self) -> Result<(), Aria2RpcError> {
        self.rpc("aria2.getVersion", Vec::new()).map(|_| ())
    }

    pub fn add_uri(
        &self,
        uri: &str,
        directory: &Path,
        proxy: &ProxySettings,
        gid: Option<&str>,
    ) -> Result<String, Aria2RpcError> {
        self.add_uri_with_options(uri, directory, proxy, gid, &[], None)
    }

    pub fn add_uri_with_options(
        &self,
        uri: &str,
        directory: &Path,
        proxy: &ProxySettings,
        gid: Option<&str>,
        headers: &[(String, String)],
        filename: Option<&str>,
    ) -> Result<String, Aria2RpcError> {
        self.add_uri_with_options_and_overwrite(
            uri, directory, proxy, gid, headers, filename, false,
        )
    }

    pub fn add_uri_with_options_and_overwrite(
        &self,
        uri: &str,
        directory: &Path,
        proxy: &ProxySettings,
        gid: Option<&str>,
        headers: &[(String, String)],
        filename: Option<&str>,
        allow_overwrite: bool,
    ) -> Result<String, Aria2RpcError> {
        let mut options = Map::new();
        let mut request_headers = Vec::new();
        for (name, value) in headers {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
                || value.contains(['\r', '\n'])
            {
                return Err(Aria2RpcError::Rpc("invalid download header".into()));
            }
            request_headers.push(Value::String(format!("{name}: {value}")));
        }
        if !request_headers.is_empty() {
            options.insert("header".into(), Value::Array(request_headers));
        }
        if let Some(filename) = filename {
            if filename.is_empty()
                || matches!(filename, "." | "..")
                || filename.contains(['/', '\\', ':'])
            {
                return Err(Aria2RpcError::Rpc(
                    "filename must not contain a path".into(),
                ));
            }
            options.insert("out".into(), json!(filename));
        }
        options.insert(
            "dir".into(),
            Value::String(directory.to_string_lossy().into_owned()),
        );
        options.insert("continue".into(), Value::String("true".into()));
        options.insert(
            "max-connection-per-server".into(),
            Value::String("8".into()),
        );
        options.insert("split".into(), Value::String("8".into()));
        options.insert("follow-torrent".into(), Value::String("true".into()));
        if allow_overwrite {
            options.insert("allow-overwrite".into(), Value::String("true".into()));
        }
        if let Some(gid) = gid {
            options.insert("gid".into(), Value::String(gid.to_string()));
        }
        for (key, value) in proxy_options(proxy) {
            options.insert(key, Value::String(value));
        }
        self.rpc("aria2.addUri", vec![json!([uri]), Value::Object(options)])?
            .as_str()
            .map(str::to_string)
            .ok_or(Aria2RpcError::InvalidResponse)
    }

    pub fn tell_status(&self, gid: &str) -> Result<Aria2Status, Aria2RpcError> {
        let keys = [
            "gid",
            "status",
            "totalLength",
            "completedLength",
            "downloadSpeed",
            "errorCode",
            "errorMessage",
            "followedBy",
            "connections",
            "files",
            "bittorrent",
        ];
        Ok(serde_json::from_value(
            self.rpc("aria2.tellStatus", vec![json!(gid), json!(keys)])?,
        )?)
    }

    pub fn pause(&self, gid: &str) -> Result<(), Aria2RpcError> {
        self.rpc("aria2.pause", vec![json!(gid)]).map(|_| ())
    }
    pub fn unpause(&self, gid: &str) -> Result<(), Aria2RpcError> {
        self.rpc("aria2.unpause", vec![json!(gid)]).map(|_| ())
    }
    pub fn remove(&self, gid: &str) -> Result<(), Aria2RpcError> {
        self.rpc("aria2.forceRemove", vec![json!(gid)]).map(|_| ())
    }
    pub fn shutdown(&self) {
        let _ = self.rpc("aria2.forceShutdown", Vec::new());
    }
}

fn proxy_options(proxy: &ProxySettings) -> HashMap<String, String> {
    if !proxy.enabled {
        return HashMap::new();
    }
    let host = if proxy.host.contains(':') && !proxy.host.starts_with('[') {
        format!("[{}]", proxy.host)
    } else {
        proxy.host.clone()
    };
    let mut options = HashMap::from([(
        "all-proxy".into(),
        format!("{}://{}:{}", proxy.protocol, host, proxy.port),
    )]);
    if !proxy.username.is_empty() {
        options.insert("all-proxy-user".into(), proxy.username.clone());
    }
    if !proxy.password.is_empty() {
        options.insert("all-proxy-passwd".into(), proxy.password.clone());
    }
    options
}

const METADATA_TRACKER_STALL: Duration = Duration::from_secs(45);

struct MetadataTrackerAttempt {
    since: Instant,
    trackers: Vec<String>,
    next_index: usize,
}

#[derive(Default)]
struct MetadataRecovery {
    attempts: HashMap<String, MetadataTrackerAttempt>,
    sources: HashMap<String, String>,
}

impl MetadataRecovery {
    fn next_trackers(&mut self, status: &Aria2Status, now: Instant) -> Option<Vec<String>> {
        if status.status == "waiting" {
            if let Some(attempt) = self.attempts.get_mut(&status.gid) {
                attempt.since = now;
            }
            return None;
        }
        let metadata = status
            .bittorrent
            .as_ref()
            .filter(|torrent| torrent.info.is_none());
        if status.status != "active"
            || status.total_length != "0"
            || status.completed_length != "0"
            || status.download_speed != "0"
            || !status.followed_by.is_empty()
            || metadata.is_none()
        {
            self.attempts.remove(&status.gid);
            if !matches!(status.status.as_str(), "active" | "waiting" | "paused")
                || !status.followed_by.is_empty()
            {
                self.sources.remove(&status.gid);
            }
            return None;
        }
        let mut trackers = Vec::new();
        for tracker in metadata?.announce_list.iter().flatten() {
            if !trackers.contains(tracker) {
                trackers.push(tracker.clone());
            }
        }
        if trackers.len() < 2 {
            return None;
        }
        let attempt = self
            .attempts
            .entry(status.gid.clone())
            .or_insert(MetadataTrackerAttempt {
                since: now,
                trackers,
                next_index: 1,
            });
        if status.connections != "0"
            || now.saturating_duration_since(attempt.since) < METADATA_TRACKER_STALL
            || attempt.next_index >= attempt.trackers.len()
        {
            return None;
        }
        let rotated = attempt.trackers[attempt.next_index..]
            .iter()
            .chain(attempt.trackers[..attempt.next_index].iter())
            .cloned()
            .collect();
        attempt.next_index += 1;
        attempt.since = now;
        Some(rotated)
    }
}

pub struct Aria2Engine {
    client: Aria2Client,
    child: Mutex<Option<Child>>,
    metadata_recovery: Mutex<MetadataRecovery>,
}

impl Aria2Engine {
    pub fn start(binary: PathBuf, data_dir: &Path) -> Result<Self, Aria2RpcError> {
        // Older builds asked aria2 to serialize live task options. That file can contain
        // short-lived cloud URLs and credential headers in plaintext; TaskStore now owns
        // recovery and resolves fresh provider URLs instead.
        let legacy_session = data_dir.join("aria2.session");
        match std::fs::remove_file(legacy_session) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            listener.local_addr()?.port()
        };
        let secret = Uuid::new_v4().to_string();
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let mut command = Command::new(binary);
        command
            .args([
                "--enable-rpc=true",
                "--rpc-listen-all=false",
                &format!("--rpc-listen-port={port}"),
                &format!("--rpc-secret={secret}"),
                &format!("--stop-with-process={}", std::process::id()),
                "--console-log-level=warn",
                "--summary-interval=0",
                "--allow-overwrite=false",
                "--auto-file-renaming=true",
            ])
            // A parent shell may export HTTP(S)_PROXY.  aria2 otherwise inherits it and can
            // silently send tracker traffic to a stale/unreachable proxy even when the app's
            // download proxy setting is disabled. Per-task proxy options below are authoritative.
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            // aria2 has no default bootstrap host; a fresh routing table cannot discover peers on its own.
            .arg("--dht-entry-point=router.bittorrent.com:6881")
            .arg(format!(
                "--dht-file-path={}",
                data_dir.join("dht.dat").to_string_lossy()
            ))
            .arg(format!(
                "--dht-file-path6={}",
                data_dir.join("dht6.dat").to_string_lossy()
            ))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command.spawn()?;
        let client = Aria2Client::new(address, secret);
        for _ in 0..50 {
            if client.ping().is_ok() {
                return Ok(Self {
                    client,
                    child: Mutex::new(Some(child)),
                    metadata_recovery: Mutex::new(MetadataRecovery::default()),
                });
            }
            if child.try_wait()?.is_some() {
                return Err(Aria2RpcError::StartupTimeout);
            }
            thread::sleep(Duration::from_millis(40));
        }
        let _ = child.kill();
        Err(Aria2RpcError::StartupTimeout)
    }

    pub fn add_uri(
        &self,
        uri: &str,
        directory: &Path,
        proxy: &ProxySettings,
        gid: Option<&str>,
    ) -> Result<String, Aria2RpcError> {
        self.add_uri_with_options(uri, directory, proxy, gid, &[], None)
    }
    pub fn add_uri_with_options(
        &self,
        uri: &str,
        directory: &Path,
        proxy: &ProxySettings,
        gid: Option<&str>,
        headers: &[(String, String)],
        filename: Option<&str>,
    ) -> Result<String, Aria2RpcError> {
        self.add_uri_with_options_and_overwrite(
            uri, directory, proxy, gid, headers, filename, false,
        )
    }

    pub fn add_uri_with_options_and_overwrite(
        &self,
        uri: &str,
        directory: &Path,
        proxy: &ProxySettings,
        gid: Option<&str>,
        headers: &[(String, String)],
        filename: Option<&str>,
        allow_overwrite: bool,
    ) -> Result<String, Aria2RpcError> {
        let mut recovery = self
            .metadata_recovery
            .lock()
            .map_err(|_| Aria2RpcError::Rpc("metadata recovery lock unavailable".into()))?;
        let gid = self
            .client
            .add_uri_with_options_and_overwrite(
                uri,
                directory,
                proxy,
                gid,
                headers,
                filename,
                allow_overwrite,
            )?;
        if uri.to_ascii_lowercase().starts_with("magnet:?") {
            recovery.sources.insert(gid.clone(), uri.to_string());
        }
        Ok(gid)
    }
    pub fn status(&self, gid: &str) -> Result<Aria2Status, Aria2RpcError> {
        let mut recovery = self
            .metadata_recovery
            .lock()
            .map_err(|_| Aria2RpcError::Rpc("metadata recovery lock unavailable".into()))?;
        let status = self.client.tell_status(gid)?;
        if let Some(trackers) = recovery.next_trackers(&status, Instant::now()) {
            if let Some(source) = recovery.sources.get(gid) {
                self.restart_metadata(gid, source, &trackers)?;
                return self.client.tell_status(gid);
            }
        }
        Ok(status)
    }

    fn restart_metadata(
        &self,
        gid: &str,
        source: &str,
        trackers: &[String],
    ) -> Result<(), Aria2RpcError> {
        let mut magnet = url::Url::parse(source).map_err(|_| Aria2RpcError::InvalidResponse)?;
        let parameters = magnet
            .query_pairs()
            .filter(|(key, _)| key != "tr")
            .map(|pair| (pair.0.into_owned(), pair.1.into_owned()))
            .collect::<Vec<_>>();
        magnet.set_query(None);
        magnet
            .query_pairs_mut()
            .extend_pairs(parameters)
            .extend_pairs(trackers.iter().map(|tracker| ("tr", tracker)));
        // changeOption restarts aria2 but does not rebuild an existing magnet's announce list.
        // Recreate only the still-empty metadata request, preserving its options, source fields and GID.
        let mut options = self.client.rpc("aria2.getOption", vec![json!(gid)])?;
        options
            .as_object_mut()
            .ok_or(Aria2RpcError::InvalidResponse)?
            .insert("gid".into(), json!(gid));
        self.client.remove(gid)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.client.tell_status(gid)?.status != "removed" {
            if Instant::now() >= deadline {
                return Err(Aria2RpcError::Rpc(
                    "metadata tracker retry could not stop its old request".into(),
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
        self.client
            .rpc("aria2.removeDownloadResult", vec![json!(gid)])?;
        self.client
            .rpc("aria2.addUri", vec![json!([magnet.as_str()]), options])?;
        Ok(())
    }

    pub fn pause(&self, gid: &str) -> Result<(), Aria2RpcError> {
        let mut recovery = self
            .metadata_recovery
            .lock()
            .map_err(|_| Aria2RpcError::Rpc("metadata recovery lock unavailable".into()))?;
        self.client.pause(gid)?;
        recovery.attempts.remove(gid);
        Ok(())
    }
    pub fn unpause(&self, gid: &str) -> Result<(), Aria2RpcError> {
        let _recovery = self
            .metadata_recovery
            .lock()
            .map_err(|_| Aria2RpcError::Rpc("metadata recovery lock unavailable".into()))?;
        self.client.unpause(gid)
    }
    pub fn remove(&self, gid: &str) -> Result<(), Aria2RpcError> {
        let mut recovery = self
            .metadata_recovery
            .lock()
            .map_err(|_| Aria2RpcError::Rpc("metadata recovery lock unavailable".into()))?;
        self.client.remove(gid)?;
        recovery.attempts.remove(gid);
        recovery.sources.remove(gid);
        Ok(())
    }
}

impl Drop for Aria2Engine {
    fn drop(&mut self) {
        self.client.shutdown();
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting_metadata() -> Aria2Status {
        serde_json::from_value(json!({
            "gid": "metadata", "status": "active", "totalLength": "0", "completedLength": "0",
            "downloadSpeed": "0", "connections": "0", "bittorrent": {
                "announceList": [["http://first/announce"], ["http://second/announce"], ["http://first/announce"], ["udp://third:80/announce"]]
            }
        })).unwrap()
    }

    #[test]
    fn tracker_recovery_waits_then_tries_each_alternative_once_without_dropping_uris() {
        let mut recovery = MetadataRecovery::default();
        let status = waiting_metadata();
        let start = Instant::now();
        assert!(recovery.next_trackers(&status, start).is_none());
        assert!(recovery
            .next_trackers(&status, start + Duration::from_secs(44))
            .is_none());
        assert_eq!(
            recovery
                .next_trackers(&status, start + Duration::from_secs(45))
                .unwrap(),
            [
                "http://second/announce",
                "udp://third:80/announce",
                "http://first/announce"
            ]
        );
        assert!(recovery
            .next_trackers(&status, start + Duration::from_secs(46))
            .is_none());
        assert_eq!(
            recovery
                .next_trackers(&status, start + Duration::from_secs(90))
                .unwrap(),
            [
                "udp://third:80/announce",
                "http://first/announce",
                "http://second/announce"
            ]
        );
        assert!(recovery
            .next_trackers(&status, start + Duration::from_secs(135))
            .is_none());
    }

    #[test]
    fn tracker_recovery_does_not_restart_connected_paused_or_payload_transfers() {
        let start = Instant::now();
        for (field, value) in [
            ("connections", "1"),
            ("status", "paused"),
            ("totalLength", "4096"),
            ("completedLength", "1024"),
            ("downloadSpeed", "256"),
        ] {
            let mut recovery = MetadataRecovery::default();
            recovery.next_trackers(&waiting_metadata(), start);
            let mut status = waiting_metadata();
            match field {
                "connections" => status.connections = value.into(),
                "status" => status.status = value.into(),
                "totalLength" => status.total_length = value.into(),
                "completedLength" => status.completed_length = value.into(),
                _ => status.download_speed = value.into(),
            }
            assert!(
                recovery
                    .next_trackers(&status, start + Duration::from_secs(60))
                    .is_none(),
                "restarted {field}"
            );
        }
        let mut recovery = MetadataRecovery::default();
        recovery.next_trackers(&waiting_metadata(), start);
        let mut followed = waiting_metadata();
        followed.followed_by = vec!["payload-gid".into()];
        assert!(recovery
            .next_trackers(&followed, start + Duration::from_secs(60))
            .is_none());
    }

    #[test]
    fn tracker_recovery_keeps_its_retry_budget_across_the_recreated_tasks_queue_phase() {
        let mut recovery = MetadataRecovery::default();
        let status = waiting_metadata();
        let start = Instant::now();
        recovery.next_trackers(&status, start);
        assert!(recovery
            .next_trackers(&status, start + Duration::from_secs(45))
            .is_some());
        let mut queued = status.clone();
        queued.status = "waiting".into();
        assert!(recovery
            .next_trackers(&queued, start + Duration::from_secs(46))
            .is_none());
        assert_eq!(
            recovery
                .next_trackers(&status, start + Duration::from_secs(91))
                .unwrap(),
            [
                "udp://third:80/announce",
                "http://first/announce",
                "http://second/announce"
            ]
        );
        assert!(recovery
            .next_trackers(&status, start + Duration::from_secs(136))
            .is_none());
    }

    #[test]
    #[ignore = "live metadata-only probe; set ALLDOWNLOADER_BT_PROBE_MAGNET to an authorized magnet"]
    fn live_magnet_metadata_probe_uses_production_tracker_recovery() {
        let magnet = std::env::var("ALLDOWNLOADER_BT_PROBE_MAGNET")
            .expect("set ALLDOWNLOADER_BT_PROBE_MAGNET");
        assert!(magnet.starts_with("magnet:?"));
        let root =
            std::env::temp_dir().join(format!("alldownloader-live-metadata-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(crate::aria2_binary_name());
        let engine = Aria2Engine::start(binary, &root).unwrap();
        engine
            .client
            .rpc(
                "aria2.changeGlobalOption",
                vec![json!({"bt-metadata-only": "true", "bt-save-metadata": "false"})],
            )
            .unwrap();
        let gid = engine
            .add_uri(&magnet, &root, &ProxySettings::default(), None)
            .unwrap();
        let started = Instant::now();
        let mut resolved = false;
        let mut previous = String::new();
        while started.elapsed() < Duration::from_secs(110) {
            let status = engine.status(&gid).unwrap();
            let snapshot = format!(
                "status={} total={} received={} speed={} connections={}",
                status.status,
                status.total_length,
                status.completed_length,
                status.download_speed,
                status.connections
            );
            if snapshot != previous {
                println!("{}s {snapshot}", started.elapsed().as_secs());
                previous = snapshot;
            }
            if status.status == "complete"
                && status.completed_length.parse::<u64>().unwrap_or(0) > 0
            {
                resolved = true;
                break;
            }
            if status.status == "error" {
                panic!(
                    "metadata probe error {}: {}",
                    status.error_code, status.error_message
                );
            }
            thread::sleep(Duration::from_millis(250));
        }
        drop(engine);
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            resolved,
            "metadata did not arrive within the bounded live probe"
        );
    }

    #[test]
    #[ignore = "bounded 52-second real aria2 tracker failover regression"]
    fn stalled_metadata_tries_next_tracker_without_replacing_the_gid() {
        use std::sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        };
        use std::time::Instant;
        let tracker = TcpListener::bind("127.0.0.1:0").unwrap();
        tracker.set_nonblocking(true).unwrap();
        let address = tracker.local_addr().unwrap();
        let first_hits = Arc::new(AtomicUsize::new(0));
        let next_hits = Arc::new(AtomicUsize::new(0));
        let running = Arc::new(AtomicBool::new(true));
        let counts = (
            Arc::clone(&first_hits),
            Arc::clone(&next_hits),
            Arc::clone(&running),
        );
        let server = thread::spawn(move || {
            while counts.2.load(Ordering::Relaxed) {
                match tracker.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut request = [0; 8192];
                        let count = stream.read(&mut request).unwrap_or(0);
                        let request = String::from_utf8_lossy(&request[..count]);
                        if request.starts_with("GET /first?") {
                            counts.0.fetch_add(1, Ordering::Relaxed);
                        }
                        if request.starts_with("GET /next?") {
                            counts.1.fetch_add(1, Ordering::Relaxed);
                        }
                        let body = "d8:intervali1800e5:peers0:e";
                        let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        let root =
            std::env::temp_dir().join(format!("alldownloader-tracker-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(crate::aria2_binary_name());
        let engine = Aria2Engine::start(binary, &root).unwrap();
        let mut magnet =
            url::Url::parse("magnet:?xt=urn:btih:0123456789012345678901234567890123456789")
                .unwrap();
        magnet
            .query_pairs_mut()
            .append_pair("tr", &format!("http://{address}/first"))
            .append_pair("tr", &format!("http://{address}/next"));
        let gid = engine
            .add_uri(magnet.as_str(), &root, &ProxySettings::default(), None)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(52);
        while Instant::now() < deadline && next_hits.load(Ordering::Relaxed) == 0 {
            assert_eq!(engine.status(&gid).unwrap().gid, gid);
            thread::sleep(Duration::from_millis(100));
        }
        drop(engine);
        running.store(false, Ordering::Relaxed);
        server.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(first_hits.load(Ordering::Relaxed) > 0);
        assert!(
            next_hits.load(Ordering::Relaxed) > 0,
            "successful but peerless tracker stranded metadata for its 1800-second interval"
        );
    }

    #[test]
    fn cold_start_bootstraps_dht_without_a_previous_routing_table() {
        let root = std::env::temp_dir().join(format!("alldownloader-dht-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("aria2.session"), b"header=Cookie: legacy-secret").unwrap();
        let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(crate::aria2_binary_name());
        let engine = Aria2Engine::start(binary, &root).unwrap();
        let options = engine
            .client
            .rpc("aria2.getGlobalOption", Vec::new())
            .unwrap();
        let bootstrap = options["dht-entry-point"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let cache = options["dht-file-path"].as_str().unwrap_or("").to_string();
        let saved_session = options["save-session"].as_str().unwrap_or("").to_string();
        let inherited_proxy = options["all-proxy"].as_str().unwrap_or("").to_string();
        assert!(
            !root.join("aria2.session").exists(),
            "legacy plaintext aria2 session must be removed before startup"
        );
        drop(engine);
        assert!(
            !bootstrap.is_empty(),
            "cold DHT has no routing table and no bootstrap entry point"
        );
        assert_eq!(
            PathBuf::from(cache),
            root.join("dht.dat"),
            "DHT cache must belong to this application instance"
        );
        assert!(
            saved_session.is_empty(),
            "aria2 must not persist signed URLs or credential headers; TaskStore owns recovery"
        );
        assert!(
            inherited_proxy.is_empty(),
            "aria2 must not inherit a proxy when the application proxy setting is disabled"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
