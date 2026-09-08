use serde::{Deserialize, Serialize};
use url::Url;
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
pub struct AppSettings {
    pub debug_reports: bool,
    pub download_directory: String,
    pub proxy: ProxySettings,
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
    Paused,
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
                TaskStatus::Failed | TaskStatus::Cancelled,
                TaskStatus::Queued
            )
            | (
                TaskStatus::Processing,
                TaskStatus::Completed | TaskStatus::Failed
            )
    )
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DownloadFileState {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub status: TaskStatus,
    pub progress: u8,
    pub size: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredDownloadFile {
    pub path: String,
    pub length: u64,
}

pub(crate) fn content_key_for(source: &str) -> String {
    let source = source.trim();
    if source.to_ascii_lowercase().starts_with("magnet:?") {
        if let Some(info_hash) = Url::parse(source).ok().and_then(|url| {
            url.query_pairs().find_map(|(key, value)| {
                if key.eq_ignore_ascii_case("xt") {
                    let value = value.as_ref();
                    value
                        .get(..9)
                        .filter(|prefix| prefix.eq_ignore_ascii_case("urn:btih:"))
                        .map(|_| value[9..].to_ascii_lowercase())
                } else {
                    None
                }
            })
        }) {
            return format!("magnet:btih:{info_hash}");
        }
    }

    Url::parse(source)
        .map(|mut url| {
            url.set_fragment(None);
            url.to_string()
        })
        .unwrap_or_else(|_| source.to_string())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DownloadTask {
    pub id: Uuid,
    pub source: String,
    pub name: String,
    pub kind: String,
    pub status: TaskStatus,
    pub progress: u8,
    pub speed: Option<String>,
    #[serde(rename = "speedBytes")]
    pub speed_bytes: u64,
    pub size: Option<String>,
    pub eta: Option<String>,
    pub connections: Option<u32>,
    #[serde(default, rename = "statusMessage")]
    pub status_message: Option<String>,
    pub children: Vec<DownloadFileState>,
    pub error: Option<String>,
    #[serde(skip_serializing)]
    pub aria2_gid: Option<String>,
    #[serde(skip_serializing)]
    pub directory: Option<String>,
    #[serde(skip_serializing)]
    pub content_key: String,
    #[serde(skip_serializing)]
    pub content_files: Vec<StoredDownloadFile>,
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
        let name = if source.to_ascii_lowercase().starts_with("magnet:?") {
            Url::parse(&source)
                .ok()
                .and_then(|url| {
                    url.query_pairs()
                        .find(|(key, value)| key == "dn" && !value.trim().is_empty())
                        .map(|(_, value)| value.into_owned())
                })
                .or_else(|| {
                    Url::parse(&source)
                        .ok()
                        .and_then(|url| {
                            url.query_pairs()
                                .find(|(key, _)| key == "xt")
                                .map(|(_, value)| value.into_owned())
                        })
                        .and_then(|exact_topic| exact_topic.rsplit(':').next().map(str::to_string))
                        .filter(|hash| !hash.is_empty())
                        .map(|hash| {
                            format!("磁力下载 {}", hash.chars().take(8).collect::<String>())
                        })
                })
                .unwrap_or_else(|| "磁力下载".to_string())
        } else {
            source
                .rsplit('/')
                .next()
                .filter(|item| !item.is_empty())
                .unwrap_or("download")
                .split(['?', '#'])
                .next()
                .unwrap_or("download")
                .to_string()
        };
        Self {
            source,
            name,
            kind: "file".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{content_key_for, NewTask};

    #[test]
    fn magnet_task_uses_its_display_name_instead_of_the_raw_uri() {
        let task = NewTask::direct("magnet:?xt=urn:btih:46A00FD699618BC0319C45F7075C067D564A0819&dn=Lanterns.S01E04.1080p%20WEBRip&tr=udp%3A%2F%2Ftracker.example");

        assert_eq!(task.name, "Lanterns.S01E04.1080p WEBRip");
    }

    #[test]
    fn unnamed_magnet_task_uses_a_short_hash_label() {
        let task = NewTask::direct("magnet:?xt=urn:btih:46A00FD699618BC0319C45F7075C067D564A0819");

        assert_eq!(task.name, "磁力下载 46A00FD6");
    }

    #[test]
    fn magnet_content_key_ignores_trackers_and_display_name() {
        let key = content_key_for(
            "magnet:?dn=fixture&xt=urn:btih:0123456789012345678901234567890123456789&tr=udp%3A%2F%2Ffirst",
        );

        assert_eq!(
            key,
            content_key_for(
                "magnet:?tr=udp%3A%2F%2Fsecond&xt=URN%3ABTIH%3A0123456789012345678901234567890123456789&dn=renamed",
            )
        );
    }
}
