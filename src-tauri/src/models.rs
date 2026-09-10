use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProxySettings {
    pub enabled: bool,
    pub protocol: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

impl Default for ProxySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            protocol: "http".into(),
            host: String::new(),
            port: 7890,
            username: String::new(),
            password: String::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AiSettings {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub api_key_configured: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MagnetMetadataSettings {
    pub enabled: bool,
    pub base_url: String,
}

impl Default for MagnetMetadataSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            base_url: "http://peer.aigutta.com:8080".into(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AiSettingsInput {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LauncherVisibility {
    #[default]
    Always,
    Downloading,
    Hidden,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    pub debug_reports: bool,
    pub download_directory: String,
    pub launcher_visibility: LauncherVisibility,
    pub proxy: ProxySettings,
    pub ai: AiSettings,
    pub magnet_metadata: MagnetMetadataSettings,
    #[serde(default = "default_delete_transferred_share_files_after_download")]
    pub delete_transferred_share_files_after_download: bool,
}

const fn default_delete_transferred_share_files_after_download() -> bool {
    true
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            debug_reports: false,
            download_directory: String::new(),
            launcher_visibility: LauncherVisibility::default(),
            proxy: ProxySettings::default(),
            ai: AiSettings::default(),
            magnet_metadata: MagnetMetadataSettings::default(),
            delete_transferred_share_files_after_download: true,
        }
    }
}

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
    Recycled,
    Paused,
}

/// The explicit user decision required before a task can be submitted again.
///
/// The state is derived from the durable task record and its output files; it
/// deliberately does not depend on a stale aria2 session entry.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateDownloadState {
    ExistingActive,
    ResumeAvailable,
    CompletedFilesPresent,
    CompletedFilesMissing,
    OverwriteRequired,
}

impl TaskStatus {
    pub fn as_db(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Parsing => "parsing",
            Self::AwaitingUser => "awaiting_user",
            Self::Queued => "queued",
            Self::Downloading => "downloading",
            Self::Processing => "processing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Recycled => "recycled",
            Self::Paused => "paused",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        Some(match value {
            "draft" => Self::Draft,
            "parsing" => Self::Parsing,
            "awaiting_user" => Self::AwaitingUser,
            "queued" => Self::Queued,
            "downloading" => Self::Downloading,
            "processing" => Self::Processing,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "recycled" => Self::Recycled,
            "paused" => Self::Paused,
            _ => return None,
        })
    }
}

pub fn may_transition(from: TaskStatus, to: TaskStatus) -> bool {
    matches!(
        (from, to),
        (TaskStatus::Draft, TaskStatus::Parsing)
            | (
                TaskStatus::Parsing,
                TaskStatus::Queued | TaskStatus::AwaitingUser | TaskStatus::Failed
            )
            | (
                TaskStatus::AwaitingUser,
                TaskStatus::Queued | TaskStatus::Cancelled
            )
            | (
                TaskStatus::Queued,
                TaskStatus::Downloading | TaskStatus::Paused | TaskStatus::Cancelled
            )
            | (
                TaskStatus::Downloading,
                TaskStatus::Processing
                    | TaskStatus::Paused
                    | TaskStatus::Failed
                    | TaskStatus::Cancelled
            )
            | (
                TaskStatus::Paused,
                TaskStatus::Queued | TaskStatus::Downloading | TaskStatus::Cancelled
            )
            | (
                TaskStatus::Processing,
                TaskStatus::Completed | TaskStatus::Failed
            )
            | (
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled,
                TaskStatus::Recycled
            )
    )
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
    #[serde(default)]
    pub aria_gid: Option<String>,
    #[serde(default)]
    pub followed_by_gid: Option<String>,
    /// Stable aria2 metadata task GID for magnet downloads. The download child
    /// GID can be regenerated when an aria2 session is loaded.
    #[serde(default)]
    pub metadata_gid: Option<String>,
    #[serde(default)]
    pub total_length: Option<u64>,
    #[serde(default)]
    pub completed_length: u64,
    #[serde(default)]
    pub download_speed: u64,
    #[serde(default)]
    pub upload_speed: u64,
    #[serde(default)]
    pub connections: u32,
    #[serde(default)]
    pub eta_seconds: Option<u64>,
    #[serde(default)]
    pub aria_error_code: Option<String>,
    #[serde(default)]
    pub updated_at: i64,
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
        let name = source
            .rsplit('/')
            .next()
            .filter(|item| !item.is_empty())
            .unwrap_or("download")
            .to_string();
        Self {
            source,
            name,
            kind: "file".to_string(),
        }
    }
}
