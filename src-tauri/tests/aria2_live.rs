#![cfg(windows)]

use alldownloader_lib::aria2::{
    Aria2Api, Aria2LaunchConfig, Aria2Process, Aria2RpcClient, Aria2Status,
};
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::Response,
    routing::get,
    Router,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    convert::Infallible,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::time::{sleep, timeout_at, Instant};

const PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(50);

#[derive(Clone)]
struct HttpFixture {
    payload: Arc<Vec<u8>>,
    resumed_requests: Arc<AtomicUsize>,
}

// The only origin is loopback; streaming keeps pause observable without network timing.
async fn serve_payload(State(state): State<HttpFixture>, headers: HeaderMap) -> Response {
    let length = state.payload.len();
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let (start, end, partial) = match range {
        Some(range) => {
            let parsed = range
                .strip_prefix("bytes=")
                .and_then(|v| v.split_once('-'))
                .and_then(|(start, end)| {
                    Some((
                        start.parse::<usize>().ok()?,
                        if end.is_empty() {
                            length - 1
                        } else {
                            end.parse::<usize>().ok()?.min(length - 1)
                        },
                    ))
                });
            match parsed {
                Some((start, end)) if start <= end && start < length => (start, end, true),
                _ => {
                    return Response::builder()
                        .status(StatusCode::RANGE_NOT_SATISFIABLE)
                        .header(header::CONTENT_RANGE, format!("bytes */{length}"))
                        .body(Body::empty())
                        .unwrap()
                }
            }
        }
        None => (0, length - 1, false),
    };
    if start > 0 {
        state.resumed_requests.fetch_add(1, Ordering::SeqCst);
    }
    let stream = futures_util::stream::unfold(
        (state.payload, start),
        move |(payload, offset)| async move {
            if offset > end {
                return None;
            }
            sleep(Duration::from_millis(20)).await;
            let next = (offset + 32 * 1024).min(end + 1);
            let bytes = Bytes::copy_from_slice(&payload[offset..next]);
            Some((Ok::<_, Infallible>(bytes), (payload, next)))
        },
    );
    let mut response = Response::builder()
        .status(if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, end - start + 1)
        .header(header::CONNECTION, "close");
    if partial {
        response = response.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{length}"),
        );
    }
    response.body(Body::from_stream(stream)).unwrap()
}

struct LiveAria2Fixture {
    process: Option<Aria2Process>,
    client: Aria2RpcClient,
    config: Aria2LaunchConfig,
    server: tokio::task::JoinHandle<()>,
    origin: String,
    http: HttpFixture,
    _root: tempfile::TempDir,
}

impl LiveAria2Fixture {
    async fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let http = HttpFixture {
            payload: Arc::new((0..PAYLOAD_BYTES).map(|i| (i % 251) as u8).collect()),
            resumed_requests: Arc::new(AtomicUsize::new(0)),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/payload.bin", get(serve_payload))
            .with_state(http.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let config = Aria2LaunchConfig {
            binary: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("binaries/aria2c-x86_64-pc-windows-msvc.exe"),
            data_dir: root.path().join("data"),
            download_dir: root.path().join("downloads"),
            parent_pid: std::process::id(),
            peer_discovery: false,
        };
        let (process, client) = Aria2Process::start(config.clone()).await.unwrap();
        assert_eq!(client.get_version().await.unwrap().version, "1.37.0");
        Self {
            process: Some(process),
            client,
            config,
            server,
            origin,
            http,
            _root: root,
        }
    }

    async fn add_slow_download(&self, name: &str) -> String {
        let options = BTreeMap::from([
            ("out".into(), name.into()),
            ("split".into(), "1".into()),
            ("max-connection-per-server".into(), "1".into()),
            ("min-split-size".into(), "1M".into()),
            ("file-allocation".into(), "none".into()),
            ("all-proxy".into(), "".into()),
        ]);
        self.client
            .add_uri(vec![format!("{}/payload.bin", self.origin)], options)
            .await
            .unwrap()
    }

    async fn wait_status(
        &self,
        gid: &str,
        duration: Duration,
        predicate: impl Fn(&Aria2Status) -> bool,
    ) -> Aria2Status {
        let deadline = Instant::now() + duration;
        loop {
            let status = timeout_at(deadline, self.client.tell_status(gid))
                .await
                .expect("tellStatus exceeded live-test deadline")
                .unwrap();
            assert_ne!(
                status.status, "error",
                "aria2 download failed: {:?}",
                status.error_code
            );
            if predicate(&status) {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "live-test deadline: status={}, completedLength={}",
                status.status,
                status.completed_length
            );
            tokio::time::sleep_until((Instant::now() + POLL).min(deadline)).await;
        }
    }

    async fn wait_progress(&self, gid: &str) -> Aria2Status {
        self.wait_status(gid, Duration::from_secs(5), |status| {
            status.status == "active"
                && status.completed_length.parse::<u64>().unwrap() >= 1024 * 1024
        })
        .await
    }

    async fn pause(&self, gid: &str) -> Aria2Status {
        assert_eq!(self.client.pause(gid).await.unwrap(), gid);
        self.wait_status(gid, Duration::from_secs(3), |status| {
            status.status == "paused"
        })
        .await
    }

    async fn wait_complete(&self, gid: &str) {
        let complete = self
            .wait_status(gid, Duration::from_secs(20), |status| {
                status.status == "complete"
            })
            .await;
        assert_eq!(
            complete.completed_length.parse::<usize>().unwrap(),
            PAYLOAD_BYTES
        );
        assert_eq!(
            complete.total_length.parse::<usize>().unwrap(),
            PAYLOAD_BYTES
        );
    }

    fn assert_download_sha256(&self, name: &str) {
        let downloaded = std::fs::read(self.config.download_dir.join(name)).unwrap();
        assert_eq!(downloaded.len(), PAYLOAD_BYTES);
        assert_eq!(
            Sha256::digest(&downloaded),
            Sha256::digest(self.http.payload.as_slice())
        );
    }

    async fn save_and_shutdown(&mut self) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        assert_eq!(
            timeout_at(deadline, self.client.save_session())
                .await
                .unwrap()
                .unwrap(),
            "OK"
        );
        let session = std::fs::read_to_string(self.config.data_dir.join("aria2.session")).unwrap();
        assert!(!session.contains("rpc-secret") && !session.contains("token:"));
        assert_eq!(
            timeout_at(deadline, self.client.shutdown())
                .await
                .unwrap()
                .unwrap(),
            "OK"
        );
        let process = self.process.as_mut().unwrap();
        assert!(
            process
                .wait_for_exit(deadline.saturating_duration_since(Instant::now()))
                .await
                .unwrap(),
            "owned aria2 child did not exit after saveSession/shutdown"
        );
        assert!(!process.is_running().unwrap());
        session
    }

    async fn restart(&mut self) {
        assert!(!self.process.as_mut().unwrap().is_running().unwrap());
        let (process, client) = Aria2Process::start(self.config.clone()).await.unwrap();
        self.process = Some(process);
        self.client = client;
    }
}

impl Drop for LiveAria2Fixture {
    fn drop(&mut self) {
        // Failure cleanup touches only this fixture's owned child and temporary data.
        if let Some(process) = &mut self.process {
            let _ = process.terminate_fallback();
        }
        self.server.abort();
    }
}

#[tokio::test]
async fn bundled_aria2_downloads_pauses_resumes_and_stops() {
    // Catches missing RPC control, synthetic progress, corrupt range output, and child leaks.
    let mut fixture = LiveAria2Fixture::start().await;
    let gid = fixture.add_slow_download("complete.bin").await;
    let progress = fixture.wait_progress(&gid).await;
    assert!(progress.completed_length.parse::<usize>().unwrap() < PAYLOAD_BYTES);
    assert!(progress.download_speed.parse::<u64>().unwrap() > 0);
    let paused = fixture.pause(&gid).await;
    let deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < deadline {
        let status = fixture.client.tell_status(&gid).await.unwrap();
        assert_eq!(status.status, "paused");
        assert_eq!(status.completed_length, paused.completed_length);
        sleep(POLL).await;
    }
    assert_eq!(fixture.client.unpause(&gid).await.unwrap(), gid);
    fixture.wait_complete(&gid).await;
    fixture.assert_download_sha256("complete.bin");
    assert!(
        fixture.http.resumed_requests.load(Ordering::SeqCst) > 0,
        "unpause must exercise an actual HTTP Range continuation"
    );
    fixture.save_and_shutdown().await;
    println!("aria2 1.37.0: real progress, pause, Range resume, SHA-256, session save and owned-child shutdown passed");
}

#[tokio::test]
async fn bundled_aria2_restores_session_and_cancel_preserves_partial_files() {
    // Catches session options/GID loss across restart and destructive cancellation.
    let mut fixture = LiveAria2Fixture::start().await;
    let gid = fixture.add_slow_download("recovered.bin").await;
    fixture.wait_progress(&gid).await;
    fixture.pause(&gid).await;
    let session = fixture.save_and_shutdown().await;
    assert!(
        session.contains(&format!("gid={gid}")),
        "unfinished GID missing from saved session"
    );
    assert!(session.contains("/payload.bin"));
    fixture.restart().await;
    let restored = fixture.client.tell_status(&gid).await.unwrap();
    if restored.status == "paused" {
        fixture.client.unpause(&gid).await.unwrap();
    }
    fixture.wait_complete(&gid).await;
    fixture.assert_download_sha256("recovered.bin");
    assert!(fixture.http.resumed_requests.load(Ordering::SeqCst) > 0);

    let cancelled = fixture.add_slow_download("cancelled.bin").await;
    fixture.wait_progress(&cancelled).await;
    fixture.pause(&cancelled).await;
    let path = fixture.config.download_dir.join("cancelled.bin");
    let partial = std::fs::read(&path).unwrap();
    assert!(!partial.is_empty());
    assert!(fixture
        .config
        .download_dir
        .join("cancelled.bin.aria2")
        .is_file());
    assert_eq!(fixture.client.remove(&cancelled).await.unwrap(), cancelled);
    // aria2 drops paused GIDs immediately on remove; they need not appear in tellStopped.
    timeout_at(Instant::now() + Duration::from_secs(3), async {
        loop {
            let mut pending = fixture.client.tell_active().await.unwrap();
            pending.extend(fixture.client.tell_waiting(0, 100).await.unwrap());
            if pending.iter().all(|status| status.gid != cancelled) {
                break;
            }
            sleep(POLL).await;
        }
        for status in fixture.client.tell_stopped(0, 100).await.unwrap() {
            if status.gid == cancelled {
                assert_eq!(status.status, "removed");
            }
        }
    })
    .await
    .expect("cancelled GID remained in the engine queue");
    assert_eq!(std::fs::read(path).unwrap(), partial);
    assert!(fixture
        .config
        .download_dir
        .join("cancelled.bin.aria2")
        .is_file());
    fixture.save_and_shutdown().await;
    println!("aria2 1.37.0: saved GID recovery, Range resume, SHA-256 and cancellation preserving partial/control files passed");
}
