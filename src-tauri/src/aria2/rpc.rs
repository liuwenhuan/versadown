use super::types::{
    Aria2Error, Aria2GlobalStat, Aria2Peer, Aria2Status, Aria2Version, RpcFault, RpcSecret,
};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;

pub type Aria2Options = BTreeMap<String, String>;

// The logger is intentionally bounded. A large queue performs many status
// polls per minute, and an unbounded debug file can itself become an I/O
// problem while investigating an engine failure.
const MAX_DEBUG_LOG_BYTES: u64 = 10 * 1024 * 1024;

/// A local JSON-lines diagnostic log.  It deliberately records RPC metadata
/// rather than request parameters, which can contain credentials or signed
/// download URLs.
#[derive(Clone, Debug)]
pub struct Aria2DebugLogger {
    writer: Arc<Mutex<File>>,
    task_ids: Arc<Mutex<HashMap<String, String>>>,
}

impl Aria2DebugLogger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Aria2Error> {
        let writer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(Aria2Error::Io)?;
        Ok(Self {
            writer: Arc::new(Mutex::new(writer)),
            task_ids: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn bind_task(&self, task_id: &str, gid: &str) {
        if let Ok(mut task_ids) = self.task_ids.lock() {
            task_ids.insert(gid.to_owned(), task_id.to_owned());
        }
        self.record("task_bound", Some(task_id), Some(gid), "ok", None);
    }

    pub fn record_task_operation(
        &self,
        operation: &str,
        task_id: &str,
        gid: Option<&str>,
        error: Option<&Aria2Error>,
    ) {
        if error.is_none() && is_high_frequency_poll(operation) {
            return;
        }
        if let Some(gid) = gid {
            if let Ok(mut task_ids) = self.task_ids.lock() {
                task_ids.insert(gid.to_owned(), task_id.to_owned());
            }
        }
        self.record(
            operation,
            Some(task_id),
            gid,
            error.map_or("ok", aria2_error_kind),
            error.and_then(rpc_fault_code),
        );
    }

    fn record_rpc(&self, operation: &str, gid: Option<&str>, error: Option<&Aria2Error>) {
        if error.is_none() && is_high_frequency_poll(operation) {
            return;
        }
        let task_id = gid.and_then(|gid| {
            self.task_ids
                .lock()
                .ok()
                .and_then(|task_ids| task_ids.get(gid).cloned())
        });
        self.record(
            operation,
            task_id.as_deref(),
            gid,
            error.map_or("ok", aria2_error_kind),
            error.and_then(rpc_fault_code),
        );
    }

    fn record(
        &self,
        operation: &str,
        task_id: Option<&str>,
        gid: Option<&str>,
        outcome: &str,
        fault_code: Option<i64>,
    ) {
        let timestamp_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let entry = json!({
            "timestamp_unix_ms": timestamp_unix_ms,
            "component": "aria2-rpc",
            "operation": operation,
            "task_id": task_id,
            "gid": gid,
            "outcome": outcome,
            "fault_code": fault_code,
        });
        if let Ok(mut writer) = self.writer.lock() {
            let should_truncate = writer
                .metadata()
                .map(|metadata| metadata.len() >= MAX_DEBUG_LOG_BYTES)
                .unwrap_or(false);
            if should_truncate {
                let _ = writer.set_len(0);
                let _ = writer.seek(SeekFrom::Start(0));
            }
            if serde_json::to_writer(&mut *writer, &entry).is_ok() {
                let _ = writeln!(writer);
            }
        }
    }
}

fn is_high_frequency_poll(operation: &str) -> bool {
    matches!(
        operation,
        "aria2.tellStatus"
            | "aria2.tellActive"
            | "aria2.tellWaiting"
            | "aria2.tellStopped"
            | "aria2.getGlobalStat"
            | "system.multicall"
    )
}

fn aria2_error_kind(error: &Aria2Error) -> &'static str {
    match error {
        Aria2Error::Rpc(_) => "rpc_fault",
        Aria2Error::Timeout => "timeout",
        Aria2Error::Transport => "transport",
        Aria2Error::HttpStatus(_) => "http_status",
        Aria2Error::MismatchedResponseId => "mismatched_response_id",
        Aria2Error::MalformedResponse => "malformed_response",
        Aria2Error::InvalidEndpoint => "invalid_endpoint",
        Aria2Error::InvalidField { .. } => "invalid_field",
        Aria2Error::UnknownStatus(_) => "unknown_status",
        Aria2Error::Io(_) => "io",
        Aria2Error::RpcPortInUse(_) => "rpc_port_in_use",
        Aria2Error::ReadinessTimeout => "readiness_timeout",
        Aria2Error::ProcessExited => "process_exited",
    }
}

fn rpc_fault_code(error: &Aria2Error) -> Option<i64> {
    match error {
        Aria2Error::Rpc(fault) => Some(fault.code),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct RpcCall {
    pub method: String,
    pub params: Vec<Value>,
}
impl RpcCall {
    pub fn new(method: impl Into<String>, params: Vec<Value>) -> Self {
        Self {
            method: method.into(),
            params,
        }
    }
}

#[async_trait]
pub trait Aria2Api: Send + Sync {
    /// Records application task context alongside an RPC operation when this
    /// implementation has diagnostic logging enabled.
    fn record_task_operation(
        &self,
        _operation: &str,
        _task_id: &str,
        _gid: Option<&str>,
        _error: Option<&Aria2Error>,
    ) {
    }
    async fn add_uri(&self, uris: Vec<String>, options: Aria2Options)
        -> Result<String, Aria2Error>;
    async fn add_torrent(
        &self,
        torrent: String,
        uris: Vec<String>,
        options: Aria2Options,
    ) -> Result<String, Aria2Error>;
    async fn tell_status(&self, gid: &str) -> Result<Aria2Status, Aria2Error>;
    async fn tell_active(&self) -> Result<Vec<Aria2Status>, Aria2Error>;
    async fn tell_waiting(&self, offset: i64, count: u32) -> Result<Vec<Aria2Status>, Aria2Error>;
    async fn tell_stopped(&self, offset: i64, count: u32) -> Result<Vec<Aria2Status>, Aria2Error>;
    async fn get_global_stat(&self) -> Result<Aria2GlobalStat, Aria2Error>;
    async fn get_peers(&self, _gid: &str) -> Result<Vec<Aria2Peer>, Aria2Error> {
        Ok(vec![])
    }
    async fn multicall(
        &self,
        calls: Vec<RpcCall>,
    ) -> Result<Vec<Result<Value, RpcFault>>, Aria2Error>;
    async fn pause(&self, gid: &str) -> Result<String, Aria2Error>;
    async fn force_pause(&self, gid: &str) -> Result<String, Aria2Error> {
        self.pause(gid).await
    }
    async fn unpause(&self, gid: &str) -> Result<String, Aria2Error>;
    async fn remove(&self, gid: &str) -> Result<String, Aria2Error>;
    async fn force_remove(&self, gid: &str) -> Result<String, Aria2Error>;
    async fn remove_download_result(&self, gid: &str) -> Result<String, Aria2Error> {
        self.force_remove(gid).await
    }
    async fn change_global_option(&self, _options: Aria2Options) -> Result<String, Aria2Error> {
        Err(Aria2Error::Transport)
    }
    async fn change_option(&self, gid: &str, options: Aria2Options) -> Result<String, Aria2Error>;
    async fn get_option(&self, gid: &str) -> Result<Aria2Options, Aria2Error>;
    async fn save_session(&self) -> Result<String, Aria2Error>;
    async fn shutdown(&self) -> Result<String, Aria2Error>;
}

#[derive(Clone, Debug)]
pub struct Aria2RpcClient {
    endpoint: Url,
    secret: RpcSecret,
    client: reqwest::Client,
    ids: Arc<AtomicU64>,
    debug_logger: Option<Aria2DebugLogger>,
}

impl Aria2RpcClient {
    pub fn new(endpoint: impl AsRef<str>, secret: RpcSecret) -> Result<Self, Aria2Error> {
        Self::with_timeout(endpoint, secret, Duration::from_secs(3))
    }
    pub fn with_timeout(
        endpoint: impl AsRef<str>,
        secret: RpcSecret,
        timeout: Duration,
    ) -> Result<Self, Aria2Error> {
        let endpoint = Url::parse(endpoint.as_ref()).map_err(|_| Aria2Error::InvalidEndpoint)?;
        if endpoint.scheme() != "http"
            || endpoint.host_str() != Some("127.0.0.1")
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.path() != "/jsonrpc"
        {
            return Err(Aria2Error::InvalidEndpoint);
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map_err(|_| Aria2Error::Transport)?;
        Ok(Self {
            endpoint,
            secret,
            client,
            ids: Arc::new(AtomicU64::new(1)),
            debug_logger: None,
        })
    }

    pub fn with_debug_logger(mut self, debug_logger: Option<Aria2DebugLogger>) -> Self {
        self.debug_logger = debug_logger;
        self
    }
    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }
    pub fn websocket_endpoint(&self) -> Url {
        let mut endpoint = self.endpoint.clone();
        endpoint
            .set_scheme("ws")
            .expect("HTTP URL accepts WS scheme");
        endpoint
    }
    pub fn authorization_param(&self) -> String {
        self.secret.authorization_param()
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Vec<Value>,
    ) -> Result<T, Aria2Error> {
        let gid = rpc_gid(method, &params);
        let result = self.request_unlogged(method, params).await;
        if let Some(logger) = &self.debug_logger {
            logger.record_rpc(method, gid.as_deref(), result.as_ref().err());
        }
        result
    }

    async fn request_unlogged<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Vec<Value>,
    ) -> Result<T, Aria2Error> {
        let id = self.ids.fetch_add(1, Ordering::Relaxed).to_string();
        let response = self
            .client
            .post(self.endpoint.clone())
            .json(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
            .send()
            .await
            .map_err(transport_error)?;
        if !response.status().is_success() {
            return Err(Aria2Error::HttpStatus(response.status().as_u16()));
        }
        let body = response.text().await.map_err(transport_error)?;
        let mut envelope: Value =
            serde_json::from_str(&body).map_err(|_| Aria2Error::MalformedResponse)?;
        // Decode first: Unicode escapes must not hide an echoed secret from redaction.
        // Sanitize keys too, before any typed result or fault can escape this boundary.
        redact_response(&self.secret, &mut envelope);
        if envelope.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(Aria2Error::MalformedResponse);
        }
        if envelope.get("id").and_then(Value::as_str) != Some(id.as_str()) {
            return Err(Aria2Error::MismatchedResponseId);
        }
        match (envelope.get("result"), envelope.get("error")) {
            (Some(result), None) => {
                serde_json::from_value(result.clone()).map_err(|_| Aria2Error::MalformedResponse)
            }
            (None, Some(error)) => Err(Aria2Error::Rpc(
                serde_json::from_value(error.clone()).map_err(|_| Aria2Error::MalformedResponse)?,
            )),
            _ => Err(Aria2Error::MalformedResponse),
        }
    }
    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        mut params: Vec<Value>,
    ) -> Result<T, Aria2Error> {
        params.insert(0, json!(self.secret.authorization_param()));
        self.request(method, params).await
    }
    pub async fn get_version(&self) -> Result<Aria2Version, Aria2Error> {
        self.call("aria2.getVersion", vec![]).await
    }
}
fn redact_response(secret: &RpcSecret, value: &mut Value) {
    match value {
        Value::String(text) => *text = secret.redact(text),
        Value::Array(values) => {
            for value in values {
                redact_response(secret, value);
            }
        }
        Value::Object(values) => {
            let original = std::mem::take(values);
            for (key, mut value) in original {
                redact_response(secret, &mut value);
                values.insert(secret.redact(&key), value);
            }
        }
        _ => {}
    }
}
fn transport_error(error: reqwest::Error) -> Aria2Error {
    if error.is_timeout() {
        Aria2Error::Timeout
    } else {
        Aria2Error::Transport
    }
}

fn rpc_gid(method: &str, params: &[Value]) -> Option<String> {
    // `call` inserts the RPC token at index zero. addUri/addTorrent do not
    // have a GID yet, while system.multicall contains multiple nested calls.
    if matches!(
        method,
        "aria2.addUri" | "aria2.addTorrent" | "system.multicall"
    ) {
        return None;
    }
    params.get(1).and_then(Value::as_str).map(str::to_owned)
}

#[async_trait]
impl Aria2Api for Aria2RpcClient {
    fn record_task_operation(
        &self,
        operation: &str,
        task_id: &str,
        gid: Option<&str>,
        error: Option<&Aria2Error>,
    ) {
        if let Some(logger) = &self.debug_logger {
            logger.record_task_operation(operation, task_id, gid, error);
        }
    }

    async fn add_uri(
        &self,
        uris: Vec<String>,
        options: Aria2Options,
    ) -> Result<String, Aria2Error> {
        self.call("aria2.addUri", vec![json!(uris), json!(options)])
            .await
    }
    async fn add_torrent(
        &self,
        torrent: String,
        uris: Vec<String>,
        options: Aria2Options,
    ) -> Result<String, Aria2Error> {
        self.call(
            "aria2.addTorrent",
            vec![json!(torrent), json!(uris), json!(options)],
        )
        .await
    }
    async fn tell_status(&self, gid: &str) -> Result<Aria2Status, Aria2Error> {
        self.call("aria2.tellStatus", vec![json!(gid)]).await
    }
    async fn tell_active(&self) -> Result<Vec<Aria2Status>, Aria2Error> {
        self.call("aria2.tellActive", vec![]).await
    }
    async fn tell_waiting(&self, offset: i64, count: u32) -> Result<Vec<Aria2Status>, Aria2Error> {
        self.call("aria2.tellWaiting", vec![json!(offset), json!(count)])
            .await
    }
    async fn tell_stopped(&self, offset: i64, count: u32) -> Result<Vec<Aria2Status>, Aria2Error> {
        self.call("aria2.tellStopped", vec![json!(offset), json!(count)])
            .await
    }
    async fn get_global_stat(&self) -> Result<Aria2GlobalStat, Aria2Error> {
        self.call("aria2.getGlobalStat", vec![]).await
    }
    async fn get_peers(&self, gid: &str) -> Result<Vec<Aria2Peer>, Aria2Error> {
        self.call("aria2.getPeers", vec![json!(gid)]).await
    }
    async fn pause(&self, gid: &str) -> Result<String, Aria2Error> {
        self.call("aria2.pause", vec![json!(gid)]).await
    }
    async fn force_pause(&self, gid: &str) -> Result<String, Aria2Error> {
        self.call("aria2.forcePause", vec![json!(gid)]).await
    }
    async fn unpause(&self, gid: &str) -> Result<String, Aria2Error> {
        self.call("aria2.unpause", vec![json!(gid)]).await
    }
    async fn remove(&self, gid: &str) -> Result<String, Aria2Error> {
        self.call("aria2.remove", vec![json!(gid)]).await
    }
    async fn force_remove(&self, gid: &str) -> Result<String, Aria2Error> {
        self.call("aria2.forceRemove", vec![json!(gid)]).await
    }
    async fn remove_download_result(&self, gid: &str) -> Result<String, Aria2Error> {
        self.call("aria2.removeDownloadResult", vec![json!(gid)])
            .await
    }
    async fn change_global_option(&self, options: Aria2Options) -> Result<String, Aria2Error> {
        self.call("aria2.changeGlobalOption", vec![json!(options)])
            .await
    }
    async fn change_option(&self, gid: &str, options: Aria2Options) -> Result<String, Aria2Error> {
        self.call("aria2.changeOption", vec![json!(gid), json!(options)])
            .await
    }
    async fn get_option(&self, gid: &str) -> Result<Aria2Options, Aria2Error> {
        self.call("aria2.getOption", vec![json!(gid)]).await
    }
    async fn save_session(&self) -> Result<String, Aria2Error> {
        self.call("aria2.saveSession", vec![]).await
    }
    async fn shutdown(&self) -> Result<String, Aria2Error> {
        self.call("aria2.shutdown", vec![]).await
    }
    async fn multicall(
        &self,
        calls: Vec<RpcCall>,
    ) -> Result<Vec<Result<Value, RpcFault>>, Aria2Error> {
        let count = calls.len();
        let mut authenticated = Vec::with_capacity(count);
        for mut call in calls {
            if !call.method.starts_with("aria2.") {
                return Err(Aria2Error::MalformedResponse);
            }
            call.params
                .insert(0, json!(self.secret.authorization_param()));
            authenticated.push(json!({"methodName":call.method, "params":call.params}));
        }
        // system.multicall itself takes only the call array, and each protected nested method needs its own token.
        let results: Vec<Value> = self
            .request("system.multicall", vec![json!(authenticated)])
            .await?;
        if results.len() != count {
            return Err(Aria2Error::MalformedResponse);
        }
        results
            .into_iter()
            .map(|result| {
                if let Some(values) = result.as_array() {
                    if values.len() != 1 {
                        return Err(Aria2Error::MalformedResponse);
                    }
                    Ok(Ok(values[0].clone()))
                } else {
                    Ok(Err(serde_json::from_value(result)
                        .map_err(|_| Aria2Error::MalformedResponse)?))
                }
            })
            .collect()
    }
}
