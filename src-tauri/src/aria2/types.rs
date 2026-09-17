use crate::models::TaskStatus;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone)]
pub struct RpcSecret(String);

impl RpcSecret {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }
    pub fn from_token(token: impl AsRef<str>) -> Result<Self, Aria2Error> {
        let token = token.as_ref();
        if token.is_empty()
            || token
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err(Aria2Error::InvalidField {
                field: "rpcSecret",
                value: "[REDACTED]".into(),
            });
        }
        Ok(Self(token.into()))
    }
    pub fn authorization_param(&self) -> String {
        format!("token:{}", self.hex())
    }
    pub(super) fn hex(&self) -> String {
        self.0.clone()
    }
    pub(super) fn redact(&self, text: &str) -> String {
        text.replace(&self.hex(), "[REDACTED]")
    }
}
impl fmt::Debug for RpcSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RpcSecret([REDACTED])")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RpcFault {
    pub code: i64,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Aria2Error {
    #[error("Invalid aria2 field {field}")]
    InvalidField { field: &'static str, value: String },
    #[error("Unknown aria2 status")]
    UnknownStatus(String),
    #[error("RPC endpoint must be an HTTP loopback address without credentials or query")]
    InvalidEndpoint,
    #[error("aria2 RPC transport failed")]
    Transport,
    #[error("aria2 RPC timed out")]
    Timeout,
    #[error("aria2 RPC HTTP status {0}")]
    HttpStatus(u16),
    #[error("aria2 RPC response id did not match")]
    MismatchedResponseId,
    #[error("Malformed aria2 RPC response")]
    MalformedResponse,
    #[error("aria2 RPC error {0:?}")]
    Rpc(RpcFault),
    #[error("aria2 process I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("aria2 RPC port {0} is already in use")]
    RpcPortInUse(u16),
    #[error("aria2 RPC readiness timed out")]
    ReadinessTimeout,
    #[error("aria2 process exited before becoming ready")]
    ProcessExited,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Aria2File {
    pub index: String,
    pub path: String,
    pub length: String,
    pub completed_length: String,
    pub selected: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Aria2Peer {
    pub ip: String,
    pub port: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Aria2Status {
    pub gid: String,
    pub status: String,
    pub total_length: String,
    pub completed_length: String,
    pub download_speed: String,
    pub upload_speed: String,
    pub connections: String,
    #[serde(default)]
    pub files: Vec<Aria2File>,
    #[serde(default)]
    pub bittorrent: Option<Aria2BitTorrent>,
    #[serde(default)]
    pub followed_by: Vec<String>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Aria2BitTorrent {
    #[serde(default)]
    pub info: Option<Aria2TorrentInfo>,
    /// Aria2 Next keeps a magnet on its original GID while awaiting a
    /// file-selection decision. Legacy aria2 instead exposes a followed child
    /// GID, so this field is optional for compatibility with both engines.
    #[serde(default, rename = "fileSelectionState")]
    pub file_selection_state: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Aria2TorrentInfo {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TransferFile {
    pub index: u32,
    pub path: String,
    pub length: u64,
    pub completed_length: u64,
    pub selected: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TransferSnapshot {
    pub gid: String,
    pub status: TaskStatus,
    pub total_length: Option<u64>,
    pub completed_length: u64,
    pub download_speed: u64,
    pub upload_speed: u64,
    pub connections: u32,
    pub progress_percent: Option<u8>,
    pub eta_seconds: Option<u64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub followed_by: Vec<String>,
    pub files: Vec<TransferFile>,
}

pub fn parse_u64(field: &'static str, value: &str) -> Result<u64, Aria2Error> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Aria2Error::InvalidField {
            field,
            value: value.into(),
        });
    }
    value.parse().map_err(|_| Aria2Error::InvalidField {
        field,
        value: value.into(),
    })
}

fn parse_u32(field: &'static str, value: &str) -> Result<u32, Aria2Error> {
    u32::try_from(parse_u64(field, value)?).map_err(|_| Aria2Error::InvalidField {
        field,
        value: value.into(),
    })
}

pub fn map_aria2_status(value: &str) -> Result<TaskStatus, Aria2Error> {
    Ok(match value {
        "waiting" => TaskStatus::Queued,
        "active" => TaskStatus::Downloading,
        "paused" => TaskStatus::Paused,
        "complete" => TaskStatus::Processing,
        "error" => TaskStatus::Failed,
        "removed" => TaskStatus::Cancelled,
        other => return Err(Aria2Error::UnknownStatus(other.into())),
    })
}

impl TryFrom<Aria2File> for TransferFile {
    type Error = Aria2Error;
    fn try_from(file: Aria2File) -> Result<Self, Self::Error> {
        let selected = match file.selected.as_str() {
            "true" => true,
            "false" => false,
            _ => {
                return Err(Aria2Error::InvalidField {
                    field: "selected",
                    value: file.selected,
                })
            }
        };
        let index = parse_u32("index", &file.index)?;
        if index == 0 {
            return Err(Aria2Error::InvalidField {
                field: "index",
                value: file.index,
            });
        }
        Ok(Self {
            index,
            path: file.path,
            length: parse_u64("length", &file.length)?,
            completed_length: parse_u64("completedLength", &file.completed_length)?,
            selected,
        })
    }
}

impl TryFrom<Aria2Status> for TransferSnapshot {
    type Error = Aria2Error;
    fn try_from(value: Aria2Status) -> Result<Self, Self::Error> {
        let total = parse_u64("totalLength", &value.total_length)?;
        let total_length = (total > 0).then_some(total);
        let completed_length = parse_u64("completedLength", &value.completed_length)?;
        let download_speed = parse_u64("downloadSpeed", &value.download_speed)?;
        let progress_percent = total_length
            .map(|total| ((u128::from(completed_length) * 100 / u128::from(total)).min(100)) as u8);
        let eta_seconds = total_length
            .filter(|_| download_speed > 0)
            .map(|total| total.saturating_sub(completed_length) / download_speed);
        if let Some(code) = &value.error_code {
            parse_u64("errorCode", code)?;
        }
        Ok(Self {
            gid: value.gid,
            status: map_aria2_status(&value.status)?,
            total_length,
            completed_length,
            download_speed,
            upload_speed: parse_u64("uploadSpeed", &value.upload_speed)?,
            connections: parse_u32("connections", &value.connections)?,
            progress_percent,
            eta_seconds,
            error_code: value.error_code,
            error_message: value.error_message,
            followed_by: value.followed_by,
            files: value
                .files
                .into_iter()
                .map(TransferFile::try_from)
                .collect::<Result<_, _>>()?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(try_from = "WireGlobalStat")]
pub struct Aria2GlobalStat {
    pub download_speed: u64,
    pub upload_speed: u64,
    pub num_active: u64,
    pub num_waiting: u64,
    pub num_stopped: u64,
    pub num_stopped_total: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireGlobalStat {
    download_speed: String,
    upload_speed: String,
    num_active: String,
    num_waiting: String,
    num_stopped: String,
    num_stopped_total: String,
}
impl TryFrom<WireGlobalStat> for Aria2GlobalStat {
    type Error = Aria2Error;
    fn try_from(value: WireGlobalStat) -> Result<Self, Self::Error> {
        Ok(Self {
            download_speed: parse_u64("downloadSpeed", &value.download_speed)?,
            upload_speed: parse_u64("uploadSpeed", &value.upload_speed)?,
            num_active: parse_u64("numActive", &value.num_active)?,
            num_waiting: parse_u64("numWaiting", &value.num_waiting)?,
            num_stopped: parse_u64("numStopped", &value.num_stopped)?,
            num_stopped_total: parse_u64("numStoppedTotal", &value.num_stopped_total)?,
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Aria2Version {
    pub version: String,
    pub enabled_features: Vec<String>,
}
