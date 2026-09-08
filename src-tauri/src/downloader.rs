use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    classify_input, CandidateKind, DownloadTask, NewTask, StoreError, TaskStatus, TaskStore,
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
}

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{0}")]
    TorrentInput(String),
}

const MAX_TORRENT_BYTES: usize = 16 * 1024 * 1024;

/// Reads a user-selected torrent from disk or HTTP(S), enforcing a small metadata
/// size ceiling before encoding it for aria2's `addTorrent` RPC.
pub async fn load_torrent_base64(source: &str) -> Result<String, DownloadError> {
    let bytes = if source.starts_with("http://") || source.starts_with("https://") {
        let mut response = reqwest::get(source)
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
        let path = if source.starts_with("file:") {
            url::Url::parse(source)
                .map_err(|_| DownloadError::TorrentInput("本地种子文件 URL 无效".into()))?
                .to_file_path()
                .map_err(|_| DownloadError::TorrentInput("本地种子文件路径无效".into()))?
        } else {
            std::path::PathBuf::from(source)
        };
        tokio::task::spawn_blocking(move || {
            let metadata = std::fs::metadata(&path).map_err(|error| {
                DownloadError::TorrentInput(format!("无法读取本地种子文件：{error}"))
            })?;
            if metadata.len() > MAX_TORRENT_BYTES as u64 {
                return Err(DownloadError::TorrentInput(
                    "种子文件超过 16 MiB 安全限制".into(),
                ));
            }
            std::fs::read(&path).map_err(|error| {
                DownloadError::TorrentInput(format!("无法读取本地种子文件：{error}"))
            })
        })
        .await
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

pub fn prepare_download(input: &str, store: &TaskStore) -> Result<ParseOutcome, DownloadError> {
    let candidate = classify_input(input);
    let mut new_task = NewTask::direct(candidate.uri);
    new_task.kind = match candidate.kind {
        CandidateKind::Magnet => "magnet",
        CandidateKind::Torrent => "torrent",
        CandidateKind::QuarkShare | CandidateKind::PikPakShare => "cloud",
        _ => "file",
    }
    .to_string();
    let initial = store.create(new_task)?;
    if candidate.auto_start {
        return Ok(ParseOutcome {
            task: initial,
            requires_confirmation: false,
            resources: Vec::new(),
        });
    }
    if candidate.kind == CandidateKind::Torrent {
        // Loading and validating the torrent is asynchronous. Do not expose an
        // empty confirmation while that work is still pending.
        return Ok(ParseOutcome {
            task: initial,
            requires_confirmation: true,
            resources: Vec::new(),
        });
    }
    Ok(ParseOutcome {
        task: store.transition(initial.id, TaskStatus::AwaitingUser)?,
        requires_confirmation: true,
        resources: Vec::new(),
    })
}

pub fn confirm_download(id: Uuid, store: &TaskStore) -> Result<DownloadTask, DownloadError> {
    Ok(store.transition(id, TaskStatus::Queued)?)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        assert_eq!(
            load_torrent_base64(&format!(
                "http://{address}/game.torrent?token=temporary#download"
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
        server.abort();
    }
}
