use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Serialize;
use std::io::Read;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    classify_input, duplicate_state, CandidateKind, DownloadTask, DuplicateDownloadState, NewTask,
    StoreError, TaskStatus, TaskStore,
};

#[derive(Clone, Debug, Serialize)]
pub struct ParseResource {
    pub id: String,
    pub index: u32,
    pub name: String,
    pub size: String,
    pub selected: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ParseOutcome {
    pub task: DownloadTask,
    pub requires_confirmation: bool,
    pub resources: Vec<ParseResource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate: Option<DuplicateDownloadState>,
}

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{0}")]
    TorrentInput(String),
    #[error("{0}")]
    ThunderInput(String),
}

const MAX_TORRENT_BYTES: usize = 16 * 1024 * 1024;
const TORRENT_INPUT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Reads a user-selected torrent from disk or HTTP(S), enforcing a small metadata
/// size ceiling before encoding it for aria2's `addTorrent` RPC.
pub async fn load_torrent_base64(source: &str) -> Result<String, DownloadError> {
    load_torrent_base64_with_timeout(source, TORRENT_INPUT_TIMEOUT).await
}

async fn load_torrent_base64_with_timeout(
    source: &str,
    timeout: std::time::Duration,
) -> Result<String, DownloadError> {
    let parsed_url = url::Url::parse(source).ok();
    let remote_url = parsed_url
        .as_ref()
        .filter(|url| matches!(url.scheme(), "http" | "https"));
    let bytes = if let Some(remote_url) = remote_url {
        let client = reqwest::Client::builder()
            .connect_timeout(timeout.min(std::time::Duration::from_secs(10)))
            .read_timeout(timeout.min(std::time::Duration::from_secs(10)))
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|error| {
                DownloadError::TorrentInput(format!("无法创建种子下载请求：{error}"))
            })?;
        let mut response = client
            .get(remote_url.clone())
            .send()
            .await
            .map_err(|error| DownloadError::TorrentInput(format!("无法获取种子文件：{error}")))?;
        if !response.status().is_success() {
            return Err(DownloadError::TorrentInput(format!(
                "种子文件请求失败：HTTP {}",
                response.status().as_u16()
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_TORRENT_BYTES as u64)
        {
            return Err(DownloadError::TorrentInput(
                "种子文件超过 16 MiB 安全限制".into(),
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            DownloadError::TorrentInput(format!("读取种子文件响应失败：{error}"))
        })? {
            if bytes.len().saturating_add(chunk.len()) > MAX_TORRENT_BYTES {
                return Err(DownloadError::TorrentInput(
                    "种子文件超过 16 MiB 安全限制".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        bytes
    } else {
        let path = if parsed_url
            .as_ref()
            .is_some_and(|url| url.scheme() == "file")
        {
            parsed_url
                .expect("file URL was checked above")
                .to_file_path()
                .map_err(|_| DownloadError::TorrentInput("本地种子文件路径无效".into()))?
        } else if parsed_url.is_some() && source.contains("://") {
            return Err(DownloadError::TorrentInput(
                "种子文件仅支持本地路径、file:、HTTP 或 HTTPS 地址".into(),
            ));
        } else {
            std::path::PathBuf::from(source)
        };
        tokio::time::timeout(
            timeout,
            tokio::task::spawn_blocking(move || {
                let metadata = std::fs::metadata(&path).map_err(|error| {
                    DownloadError::TorrentInput(format!("无法读取本地种子文件：{error}"))
                })?;
                if metadata.len() > MAX_TORRENT_BYTES as u64 {
                    return Err(DownloadError::TorrentInput(
                        "种子文件超过 16 MiB 安全限制".into(),
                    ));
                }
                let file = std::fs::File::open(&path).map_err(|error| {
                    DownloadError::TorrentInput(format!("无法读取本地种子文件：{error}"))
                })?;
                let mut bytes = Vec::with_capacity(metadata.len() as usize);
                file.take(MAX_TORRENT_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| {
                        DownloadError::TorrentInput(format!("无法读取本地种子文件：{error}"))
                    })?;
                if bytes.len() > MAX_TORRENT_BYTES {
                    return Err(DownloadError::TorrentInput(
                        "种子文件超过 16 MiB 安全限制".into(),
                    ));
                }
                Ok(bytes)
            }),
        )
        .await
        .map_err(|_| DownloadError::TorrentInput("读取本地种子文件超时".into()))?
        .map_err(|error| DownloadError::TorrentInput(format!("读取种子文件失败：{error}")))??
    };
    if bytes.is_empty() {
        return Err(DownloadError::TorrentInput("种子文件为空".into()));
    }
    Ok(STANDARD.encode(bytes))
}

pub fn aria2_binary_name() -> &'static str {
    "aria2c-x86_64-pc-windows-msvc.exe"
}

fn decode_thunder_source(source: &str) -> Result<String, DownloadError> {
    let encoded = source
        .get("thunder://".len()..)
        .ok_or_else(|| DownloadError::ThunderInput("迅雷链接格式无效".into()))?;
    let decoded = STANDARD
        .decode(encoded)
        .map_err(|_| DownloadError::ThunderInput("迅雷链接编码无效".into()))?;
    let decoded = String::from_utf8(decoded)
        .map_err(|_| DownloadError::ThunderInput("迅雷链接内容不是有效文本".into()))?;
    let target = decoded
        .strip_prefix("AA")
        .and_then(|value| value.strip_suffix("ZZ"))
        .ok_or_else(|| DownloadError::ThunderInput("迅雷链接缺少标准 AA/ZZ 包装".into()))?;
    let scheme = url::Url::parse(target)
        .map_err(|_| DownloadError::ThunderInput("迅雷链接中的下载地址无效".into()))?
        .scheme()
        .to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https" | "ftp") {
        return Err(DownloadError::ThunderInput(
            "该迅雷链接不是 aria2 可下载的 HTTP/HTTPS/FTP 地址".into(),
        ));
    }
    Ok(target.to_string())
}

pub fn prepare_download(input: &str, store: &TaskStore) -> Result<ParseOutcome, DownloadError> {
    let candidate = classify_input(input);
    let source = if candidate.kind == CandidateKind::Thunder {
        decode_thunder_source(&candidate.uri)?
    } else {
        candidate.uri.clone()
    };
    let mut new_task = NewTask::direct(source);
    new_task.kind = match candidate.kind {
        CandidateKind::Magnet => "magnet",
        CandidateKind::Torrent => "torrent",
        CandidateKind::QuarkShare | CandidateKind::PikPakShare => "cloud",
        _ => "file",
    }
    .to_string();
    if let Some(task) = store.find_by_source(&new_task.source)? {
        return Ok(ParseOutcome {
            duplicate: Some(duplicate_state(store, &task)?),
            task,
            requires_confirmation: false,
            resources: Vec::new(),
        });
    }
    let initial = store.create(new_task)?;
    if candidate.auto_start {
        return Ok(ParseOutcome {
            task: initial,
            requires_confirmation: false,
            resources: Vec::new(),
            duplicate: None,
        });
    }
    if candidate.kind == CandidateKind::Torrent {
        // Loading and validating the torrent is asynchronous. Do not expose an
        // empty confirmation while that work is still pending.
        return Ok(ParseOutcome {
            task: initial,
            requires_confirmation: true,
            resources: Vec::new(),
            duplicate: None,
        });
    }
    Ok(ParseOutcome {
        task: store.transition(initial.id, TaskStatus::AwaitingUser)?,
        requires_confirmation: true,
        resources: Vec::new(),
        duplicate: None,
    })
}

pub fn confirm_download(id: Uuid, store: &TaskStore) -> Result<DownloadTask, DownloadError> {
    Ok(store.transition(id, TaskStatus::Queued)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_direct_source_returns_the_existing_task_for_duplicate_resolution() {
        let store = TaskStore::open_in_memory().unwrap();
        let first = prepare_download("https://example.test/fixture.bin", &store)
            .unwrap()
            .task;
        let failed = store.fail(first.id, "previous transfer stopped").unwrap();

        let repeated = prepare_download("https://example.test/fixture.bin", &store).unwrap();

        assert_eq!(repeated.task.id, failed.id);
        assert_eq!(
            repeated.duplicate,
            Some(crate::DuplicateDownloadState::OverwriteRequired)
        );
    }

    #[tokio::test]
    async fn loads_local_torrent_bytes_as_base64() {
        let directory = tempfile::tempdir().unwrap();
        let torrent = directory.path().join("game.torrent");
        std::fs::write(&torrent, b"torrent-bytes").unwrap();

        assert_eq!(
            load_torrent_base64(torrent.to_str().unwrap())
                .await
                .unwrap(),
            "dG9ycmVudC1ieXRlcw=="
        );
        let file_url = url::Url::from_file_path(&torrent)
            .unwrap()
            .to_string()
            .replacen("file:", "FILE:", 1);
        assert_eq!(
            load_torrent_base64(&file_url).await.unwrap(),
            "dG9ycmVudC1ieXRlcw=="
        );
        let error = load_torrent_base64(directory.path().join("missing.torrent").to_str().unwrap())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("无法读取本地种子文件"), "{error}");
    }

    #[tokio::test]
    async fn loads_http_torrent_with_query_and_rejects_http_errors() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new()
            .route(
                "/game.torrent",
                axum::routing::get(|| async { b"torrent-bytes" }),
            )
            .route(
                "/missing.torrent",
                axum::routing::get(|| async { axum::http::StatusCode::NOT_FOUND }),
            )
            .route(
                "/slow.torrent",
                axum::routing::get(|| async {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    b"late-torrent" as &'static [u8]
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        assert_eq!(
            load_torrent_base64(&format!(
                "HTTP://{address}/game.torrent?token=temporary#download"
            ))
            .await
            .unwrap(),
            "dG9ycmVudC1ieXRlcw=="
        );
        let error = load_torrent_base64(&format!("http://{address}/missing.torrent"))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("HTTP 404"), "{error}");
        assert!(load_torrent_base64("ftp://example.test/game.torrent")
            .await
            .unwrap_err()
            .to_string()
            .contains("仅支持"));
        let began = std::time::Instant::now();
        let error = load_torrent_base64_with_timeout(
            &format!("http://{address}/slow.torrent"),
            std::time::Duration::from_millis(100),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("无法获取种子文件"), "{error}");
        assert!(began.elapsed() < std::time::Duration::from_millis(500));
        server.abort();
    }
}
