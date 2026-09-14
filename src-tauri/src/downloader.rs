use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Serialize;
use std::io::Read;
use thiserror::Error;
use uuid::Uuid;

use crate::parser::{extract_cloud_share_candidate, ShareInputError};
use crate::{
    duplicate_state, validate_download_input, CandidateKind, DownloadTask, DuplicateDownloadState,
    InputValidationError, NewTask, StoreError, TaskStatus, TaskStore,
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
    #[error(transparent)]
    ShareInput(#[from] ShareInputError),
    #[error(transparent)]
    Input(#[from] InputValidationError),
    #[error("分享链接必须先通过网盘分享解析流程处理")]
    CloudShareInput,
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
    "aria2-next-x86_64-pc-windows-msvc.exe"
}

fn decode_thunder_source(source: &str) -> Result<String, DownloadError> {
    crate::parser::decode_thunder_target(source).map_err(DownloadError::from)
}

pub fn prepare_download(input: &str, store: &TaskStore) -> Result<ParseOutcome, DownloadError> {
    if extract_cloud_share_candidate(input, None)?.is_some() {
        return Err(DownloadError::CloudShareInput);
    }
    let requested = validate_download_input(input)?;
    let candidate = if requested.kind == CandidateKind::Thunder {
        validate_download_input(&decode_thunder_source(&requested.source)?)?
    } else {
        requested
    };
    let mut new_task = NewTask::direct(candidate.source.clone());
    new_task.kind = match &candidate.kind {
        CandidateKind::Ed2k => "ed2k",
        CandidateKind::Ftp => "ftp",
        CandidateKind::Hls => "hls",
        CandidateKind::Dash => "dash",
        CandidateKind::Magnet => "magnet",
        CandidateKind::Torrent => "torrent",
        _ => "file",
    }
    .to_string();
    new_task.name = readable_task_name(&new_task.source, &new_task.kind)
        .unwrap_or_else(|| new_task.name.clone());
    if let Some(task) = store.find_by_source(&new_task.source)? {
        if task.status != TaskStatus::Recycled {
            return Ok(ParseOutcome {
                duplicate: Some(duplicate_state(store, &task)?),
                task,
                requires_confirmation: false,
                resources: Vec::new(),
            });
        }
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

fn readable_task_name(source: &str, kind: &str) -> Option<String> {
    let encoded = if kind == "ed2k" {
        source.split('|').nth(2)?.to_owned()
    } else if kind == "ftp" {
        return crate::parse_ftp_url(source)
            .ok()
            .map(|target| target.file_name().to_owned());
    } else {
        let url = url::Url::parse(source).ok()?;
        url.path_segments()?.next_back()?.to_owned()
    };
    percent_encoding::percent_decode_str(&encoded)
        .decode_utf8()
        .ok()
        .map(|name| name.into_owned())
        .filter(|name| !name.trim().is_empty())
}

pub fn confirm_download(id: Uuid, store: &TaskStore) -> Result<DownloadTask, DownloadError> {
    Ok(store.transition(id, TaskStatus::Queued)?)
}

#[cfg(test)]
mod tests {
    use base64::{engine::general_purpose::STANDARD, Engine as _};

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

    #[test]
    fn protocol_specific_tasks_receive_safe_readable_names_and_thunder_ftp_uses_ftp_engine() {
        let store = TaskStore::open_in_memory().unwrap();
        let hls = prepare_download(
            "https://media.test/live/master.m3u8?token=temporary",
            &store,
        )
        .unwrap();
        assert_eq!(hls.task.name, "master.m3u8");

        let ed2k = prepare_download(
            "ed2k://|file|Example%20Archive.zip|42|0123456789ABCDEF0123456789ABCDEF|/",
            &store,
        )
        .unwrap();
        assert_eq!(ed2k.task.name, "Example Archive.zip");

        let wrapped = STANDARD.encode(b"AAftp://example.test/pub/archive.zipZZ");
        let thunder = prepare_download(&format!("thunder://{wrapped}"), &store).unwrap();
        assert_eq!(thunder.task.kind, "ftp");
        assert_eq!(thunder.task.name, "archive.zip");
    }

    #[test]
    fn share_page_is_not_saved_as_a_cloud_task_source() {
        let store = TaskStore::open_in_memory().unwrap();
        let source = "https://pan.baidu.com/s/share-id?pwd=1234";

        assert!(matches!(
            prepare_download(source, &store),
            Err(DownloadError::CloudShareInput)
        ));
        assert!(store.find_by_source(source).unwrap().is_none());
    }

    #[test]
    fn malformed_magnet_creates_no_task() {
        let store = TaskStore::open_in_memory().unwrap();

        assert_eq!(
            prepare_download("magnet:?xt=urn:btih:short", &store)
                .unwrap_err()
                .to_string(),
            "磁链格式无效"
        );
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn malformed_ftp_source_creates_no_task() {
        let store = TaskStore::open_in_memory().unwrap();

        assert_eq!(
            prepare_download("ftp://example.test/", &store)
                .unwrap_err()
                .to_string(),
            "FTP 链接格式无效"
        );
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn protocol_specific_inputs_keep_a_stable_task_kind() {
        let store = TaskStore::open_in_memory().unwrap();
        for (source, expected) in [
            (
                "ed2k://|file|Example.zip|42|0123456789ABCDEF0123456789ABCDEF|/",
                "ed2k",
            ),
            ("ftp://example.test/pub/file.iso", "ftp"),
            ("https://media.example.test/master.m3u8", "hls"),
            ("https://media.example.test/manifest.mpd", "dash"),
        ] {
            let outcome = prepare_download(source, &store).unwrap();
            assert_eq!(outcome.task.kind, expected, "{source}");
            assert!(!outcome.requires_confirmation, "{source}");
        }
    }

    #[test]
    fn a_recycled_task_does_not_block_a_new_submission_for_the_same_source() {
        let store = TaskStore::open_in_memory().unwrap();
        let first = prepare_download("https://example.test/recycled.bin", &store)
            .unwrap()
            .task;
        let completed = store
            .apply_transfer_snapshot(DownloadTask {
                status: TaskStatus::Completed,
                ..first
            })
            .unwrap();
        store
            .transition(completed.id, TaskStatus::Recycled)
            .unwrap();

        let repeated = prepare_download("https://example.test/recycled.bin", &store).unwrap();

        assert_ne!(repeated.task.id, completed.id);
        assert_eq!(repeated.duplicate, None);
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
