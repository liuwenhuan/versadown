use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fmt,
    io::{Read, Write},
};

pub(super) const PROTOCOL_VERSION: u8 = 1;
pub(super) const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProtocolError {
    Io,
    FrameTooLarge,
    Malformed,
}

pub(super) fn write_frame<T: Serialize>(
    writer: &mut impl Write,
    value: &T,
) -> Result<(), ProtocolError> {
    let payload = serde_json::to_vec(value).map_err(|_| ProtocolError::Malformed)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    writer
        .write_all(&(payload.len() as u32).to_be_bytes())
        .and_then(|_| writer.write_all(&payload))
        .and_then(|_| writer.flush())
        .map_err(|_| ProtocolError::Io)
}

pub(super) fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> Result<T, ProtocolError> {
    let mut length = [0_u8; 4];
    reader
        .read_exact(&mut length)
        .map_err(|_| ProtocolError::Io)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    let mut payload = vec![0_u8; length];
    reader
        .read_exact(&mut payload)
        .map_err(|_| ProtocolError::Io)?;
    serde_json::from_slice(&payload).map_err(|_| ProtocolError::Malformed)
}

/// Authentication arrives from the worker only after it has read the nonce
/// from inherited stdin. It never appears in the worker command line.
#[derive(Serialize, Deserialize)]
pub(crate) struct WorkerHello {
    pub(super) protocol_version: u8,
    nonce: String,
}

impl WorkerHello {
    pub(super) fn new(nonce: String) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            nonce,
        }
    }

    pub(super) fn nonce(&self) -> &str {
        &self.nonce
    }
}

impl fmt::Debug for WorkerHello {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerHello")
            .field("protocol_version", &self.protocol_version)
            .field("nonce", &"<redacted>")
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct WorkerInitialization {
    pub(super) user_id: String,
    pub(super) peer_id: String,
}

impl fmt::Debug for WorkerInitialization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WorkerInitialization { credentials: <redacted> }")
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct WorkerSubmission {
    pub(super) url: String,
    pub(super) headers: Vec<(String, String)>,
    pub(super) output_path: String,
    pub(super) output_name: String,
    pub(super) acceleration_token: Option<String>,
    pub(super) cid: Option<String>,
    pub(super) gcid: Option<String>,
    pub(super) total_length: Option<u64>,
}

impl fmt::Debug for WorkerSubmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WorkerSubmission { signed_request: <redacted> }")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkerAction {
    Pause,
    Resume,
    Cancel,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", content = "payload", rename_all = "snake_case")]
pub(crate) enum WorkerRequest {
    Initialize(WorkerInitialization),
    Submit(WorkerSubmission),
    Poll { task_id: u32 },
    Control { task_id: u32, action: WorkerAction },
    Release { task_id: u32 },
    Shutdown,
}

impl fmt::Debug for WorkerRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let command = match self {
            Self::Initialize(_) => "initialize",
            Self::Submit(_) => "submit",
            Self::Poll { .. } => "poll",
            Self::Control { .. } => "control",
            Self::Release { .. } => "release",
            Self::Shutdown => "shutdown",
        };
        formatter
            .debug_struct("WorkerRequest")
            .field("command", &command)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub(crate) struct WorkerProgress {
    pub(super) completed: u64,
    pub(super) total: u64,
    pub(super) status: i32,
    pub(super) error_code: i32,
    pub(super) connections: u32,
    pub(super) received: u64,
    pub(super) peer_received: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkerErrorCode {
    Incompatible,
    TaskUnknown,
    OperationFailed,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub(crate) enum WorkerResponse {
    Acknowledged,
    Submitted { task_id: u32 },
    Progress(WorkerProgress),
    Error { code: WorkerErrorCode },
}

#[cfg(test)]
mod tests {
    use super::{read_frame, write_frame, WorkerInitialization, WorkerRequest, MAX_FRAME_BYTES};
    use std::io::Cursor;

    #[test]
    fn length_framing_rejects_truncated_and_oversized_messages() {
        let mut truncated = Cursor::new(vec![0, 0, 0, 8, b'{', b'}']);
        assert!(read_frame::<WorkerRequest>(&mut truncated).is_err());

        let mut oversized = Cursor::new((MAX_FRAME_BYTES as u32 + 1).to_be_bytes().to_vec());
        assert!(read_frame::<WorkerRequest>(&mut oversized).is_err());
    }

    #[test]
    fn request_debug_output_redacts_credential_values_while_wire_payload_keeps_them() {
        let request = WorkerRequest::Initialize(WorkerInitialization {
            user_id: "user-secret".into(),
            peer_id: "peer-secret".into(),
        });
        let debug = format!("{request:?}");
        assert!(!debug.contains("user-secret"));
        assert!(!debug.contains("peer-secret"));

        let mut payload = Vec::new();
        write_frame(&mut payload, &request).unwrap();
        assert!(String::from_utf8_lossy(&payload).contains("user-secret"));
        assert!(String::from_utf8_lossy(&payload).contains("peer-secret"));
    }
}
