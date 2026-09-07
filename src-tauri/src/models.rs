use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Draft,
    Parsing,
    AwaitingUser,
    Queued,
    Downloading,
    Processing,
    Completed,
    Failed,
    Cancelled,
    Paused,
}

impl TaskStatus {
    pub fn as_db(self) -> &'static str {
        match self {
            Self::Draft => "draft", Self::Parsing => "parsing", Self::AwaitingUser => "awaiting_user",
            Self::Queued => "queued", Self::Downloading => "downloading", Self::Processing => "processing",
            Self::Completed => "completed", Self::Failed => "failed", Self::Cancelled => "cancelled", Self::Paused => "paused",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        Some(match value {
            "draft" => Self::Draft, "parsing" => Self::Parsing, "awaiting_user" => Self::AwaitingUser,
            "queued" => Self::Queued, "downloading" => Self::Downloading, "processing" => Self::Processing,
            "completed" => Self::Completed, "failed" => Self::Failed, "cancelled" => Self::Cancelled, "paused" => Self::Paused,
            _ => return None,
        })
    }
}

pub fn may_transition(from: TaskStatus, to: TaskStatus) -> bool {
    matches!((from, to),
        (TaskStatus::Draft, TaskStatus::Parsing)
        | (TaskStatus::Parsing, TaskStatus::Queued | TaskStatus::AwaitingUser | TaskStatus::Failed)
        | (TaskStatus::AwaitingUser, TaskStatus::Queued | TaskStatus::Cancelled)
        | (TaskStatus::Queued, TaskStatus::Downloading | TaskStatus::Paused | TaskStatus::Cancelled)
        | (TaskStatus::Downloading, TaskStatus::Processing | TaskStatus::Paused | TaskStatus::Failed | TaskStatus::Cancelled)
        | (TaskStatus::Paused, TaskStatus::Queued | TaskStatus::Downloading | TaskStatus::Cancelled)
        | (TaskStatus::Processing, TaskStatus::Completed | TaskStatus::Failed))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DownloadTask {
    pub id: Uuid,
    pub source: String,
    pub name: String,
    pub kind: String,
    pub status: TaskStatus,
    pub progress: u8,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NewTask {
    pub source: String,
    pub name: String,
    pub kind: String,
}

impl NewTask {
    pub fn direct(source: impl Into<String>) -> Self {
        let source = source.into();
        let name = source.rsplit('/').next().filter(|item| !item.is_empty()).unwrap_or("download").to_string();
        Self { source, name, kind: "file".to_string() }
    }
}
