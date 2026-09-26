use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use versadown_lib::{
    list_queue_tasks_for_scope, parse_ftp_url, Aria2GlobalStat, DownloadTask, EventSink,
    FtpDownloadEngine, FtpSegment, NewTask, TaskStatus, TaskStore, UriSubmissionOptions,
};
use tempfile::tempdir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

#[test]
fn ftp_url_keeps_connection_details_but_redacts_credentials_from_diagnostics() {
    let target = parse_ftp_url("ftp://alice:s%40cret@example.test:2121/releases/video%20one.mkv")
        .expect("valid FTP URL");

    assert_eq!(target.host(), "example.test");
    assert_eq!(target.port(), 2121);
    assert_eq!(target.username(), "alice");
    assert_eq!(target.password(), "s@cret");
    assert_eq!(target.remote_path(), "/releases/video one.mkv");
    assert_eq!(target.file_name(), "video one.mkv");
    assert_eq!(
        target.display_source(),
        "ftp://example.test:2121/releases/video%20one.mkv"
    );
    assert!(!format!("{target:?}").contains("s@cret"));
}

#[test]
fn ftp_url_defaults_to_anonymous_and_rejects_a_directory_only_target() {
    let target = parse_ftp_url("ftp://example.test/pub/archive.zip").expect("valid FTP URL");
    assert_eq!(target.port(), 21);
    assert_eq!(target.username(), "anonymous");
    assert_eq!(target.password(), "anonymous@versadown.local");

    let error = parse_ftp_url("ftp://example.test/pub/").expect_err("directory is not a file");
    assert!(error.to_string().contains("文件路径"));
}

#[test]
fn ftp_credentials_are_not_exposed_by_the_queue_projection() {
    let store = TaskStore::open_in_memory().unwrap();
    store
        .create(NewTask {
            source: "ftp://alice:top-secret@example.test/archive.zip".into(),
            name: "archive.zip".into(),
            kind: "ftp".into(),
            is_private: false,
        })
        .unwrap();

    let queue = list_queue_tasks_for_scope(&store, None).unwrap();
    assert_eq!(queue[0].source, "ftp://example.test/archive.zip");
    assert!(!serde_json::to_string(&queue)
        .unwrap()
        .contains("top-secret"));
}

#[test]
fn ftp_segments_are_ordered_updated_and_deleted() {
    let store = TaskStore::open_in_memory().unwrap();
    let task = store
        .create(NewTask {
            source: "ftp://example.test/a.bin".into(),
            name: "a.bin".into(),
            kind: "ftp".into(),
            is_private: false,
        })
        .unwrap();

    store
        .replace_ftp_segments(
            task.id,
            &[
                FtpSegment {
                    index: 1,
                    start_offset: 8,
                    end_offset: 16,
                    completed_length: 0,
                },
                FtpSegment {
                    index: 0,
                    start_offset: 0,
                    end_offset: 8,
                    completed_length: 3,
                },
            ],
        )
        .unwrap();
    store.update_ftp_segment(task.id, 1, 4).unwrap();

    let segments = store.ftp_segments(task.id).unwrap();
    assert_eq!(segments[0].index, 0);
    assert_eq!(segments[1].completed_length, 4);

    store.remove_task_record(task.id).unwrap();
    assert!(store.ftp_segments(task.id).unwrap().is_empty());
}

#[tokio::test]
async fn ftp_scheduler_keeps_later_task_queued_until_an_active_slot_is_available() {
    let payload = vec![0x5a; 3 * 1024 * 1024];
    let (source, _) = spawn_ftp_server(payload, Duration::from_millis(10)).await;
    let directory = tempdir().unwrap();
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let first = store
        .create(NewTask {
            source: source.clone(),
            name: "first.bin".into(),
            kind: "ftp".into(),
            is_private: false,
        })
        .unwrap();
    let second = store
        .create(NewTask {
            source: source.clone(),
            name: "second.bin".into(),
            kind: "ftp".into(),
            is_private: false,
        })
        .unwrap();
    let engine = Arc::new(FtpDownloadEngine::new(
        directory.path(),
        Arc::clone(&store),
        Arc::new(NoEvents),
    ));
    engine.set_limits(1, 4);

    engine
        .submit_uri(
            first.id,
            &source,
            UriSubmissionOptions {
                output_name: Some("first.bin".into()),
                ..UriSubmissionOptions::default()
            },
        )
        .await
        .unwrap();
    engine
        .submit_uri(
            second.id,
            &source,
            UriSubmissionOptions {
                output_name: Some("second.bin".into()),
                ..UriSubmissionOptions::default()
            },
        )
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if store.find(first.id).unwrap().status == TaskStatus::Downloading {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first FTP task becomes active");
    assert_eq!(store.find(second.id).unwrap().status, TaskStatus::Queued);

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                store.find(second.id).unwrap().status,
                TaskStatus::Downloading | TaskStatus::Completed
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "queued FTP task did not become active after its predecessor completed; first={:?}, second={:?}",
            store.find(first.id),
            store.find(second.id)
        )
    });
}

struct NoEvents;

impl EventSink for NoEvents {
    fn task_updated(&self, _: &DownloadTask) {}

    fn global_stat_updated(&self, _: &Aria2GlobalStat) {}
}

struct FtpServer {
    source: String,
    resumed_at: Arc<AtomicUsize>,
    rest_offsets: Arc<Mutex<Vec<usize>>>,
    max_active_data_connections: Arc<AtomicUsize>,
}

impl FtpServer {
    fn max_active_data_connections(&self) -> usize {
        self.max_active_data_connections.load(Ordering::SeqCst)
    }

    fn rest_offsets(&self) -> Vec<usize> {
        self.rest_offsets.lock().unwrap().clone()
    }
}

async fn spawn_ftp_server(
    payload: Vec<u8>,
    chunk_delay: Duration,
) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    let server = spawn_test_ftp_server(payload, chunk_delay, false, false, 8 * 1024, None).await;
    (server.source, server.resumed_at)
}

async fn spawn_observable_ftp_server(
    payload: Vec<u8>,
    chunk_delay: Duration,
    reject_nonzero_rest: bool,
) -> FtpServer {
    spawn_test_ftp_server(
        payload,
        chunk_delay,
        reject_nonzero_rest,
        false,
        64 * 1024,
        None,
    )
    .await
}

async fn spawn_no_size_ftp_server(payload: Vec<u8>, chunk_delay: Duration) -> FtpServer {
    spawn_test_ftp_server(payload, chunk_delay, false, true, 64 * 1024, None).await
}

async fn spawn_session_limited_ftp_server(
    payload: Vec<u8>,
    chunk_delay: Duration,
    maximum_control_sessions: usize,
) -> FtpServer {
    spawn_test_ftp_server(
        payload,
        chunk_delay,
        false,
        false,
        64 * 1024,
        Some(maximum_control_sessions),
    )
    .await
}

async fn spawn_test_ftp_server(
    payload: Vec<u8>,
    chunk_delay: Duration,
    reject_nonzero_rest: bool,
    reject_size: bool,
    chunk_size: usize,
    maximum_control_sessions: Option<usize>,
) -> FtpServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let resumed_at = Arc::new(AtomicUsize::new(0));
    let rest_offsets = Arc::new(Mutex::new(Vec::new()));
    let active_data_connections = Arc::new(AtomicUsize::new(0));
    let max_active_data_connections = Arc::new(AtomicUsize::new(0));
    let active_control_sessions = Arc::new(AtomicUsize::new(0));
    let observed_resume = Arc::clone(&resumed_at);
    let observed_offsets = Arc::clone(&rest_offsets);
    let observed_active = Arc::clone(&active_data_connections);
    let observed_max_active = Arc::clone(&max_active_data_connections);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let payload = payload.clone();
            let observed_resume = Arc::clone(&observed_resume);
            let observed_offsets = Arc::clone(&observed_offsets);
            let observed_active = Arc::clone(&observed_active);
            let observed_max_active = Arc::clone(&observed_max_active);
            let observed_control_sessions = Arc::clone(&active_control_sessions);
            tokio::spawn(async move {
                handle_ftp_client(
                    stream,
                    payload,
                    observed_resume,
                    observed_offsets,
                    observed_active,
                    observed_max_active,
                    observed_control_sessions,
                    reject_nonzero_rest,
                    reject_size,
                    chunk_size,
                    chunk_delay,
                    maximum_control_sessions,
                )
                .await;
            });
        }
    });
    FtpServer {
        source: format!("ftp://tester:secret@{address}/payload.bin"),
        resumed_at,
        rest_offsets,
        max_active_data_connections,
    }
}

async fn handle_ftp_client(
    stream: TcpStream,
    payload: Vec<u8>,
    resumed_at: Arc<AtomicUsize>,
    rest_offsets: Arc<Mutex<Vec<usize>>>,
    active_data_connections: Arc<AtomicUsize>,
    max_active_data_connections: Arc<AtomicUsize>,
    active_control_sessions: Arc<AtomicUsize>,
    reject_nonzero_rest: bool,
    reject_size: bool,
    chunk_size: usize,
    chunk_delay: Duration,
    maximum_control_sessions: Option<usize>,
) {
    let mut control = BufReader::new(stream);
    control
        .get_mut()
        .write_all(b"220 test FTP ready\r\n")
        .await
        .unwrap();
    let mut line = String::new();
    let mut passive = None;
    let mut offset = 0usize;
    let mut logged_in = false;
    'session: loop {
        line.clear();
        if control.read_line(&mut line).await.unwrap_or(0) == 0 {
            break;
        }
        let command = line.trim_end();
        let (verb, argument) = command.split_once(' ').unwrap_or((command, ""));
        match verb.to_ascii_uppercase().as_str() {
            "USER" => control
                .get_mut()
                .write_all(b"331 password required\r\n")
                .await
                .unwrap(),
            "PASS" => {
                if reserve_control_session(&active_control_sessions, maximum_control_sessions) {
                    logged_in = true;
                    control
                        .get_mut()
                        .write_all(b"230 logged in\r\n")
                        .await
                        .unwrap();
                } else {
                    control
                        .get_mut()
                        .write_all(
                            b"530 Sorry, the maximum number of clients from your host are already connected.\r\n",
                        )
                        .await
                        .unwrap();
                }
            }
            "TYPE" => control
                .get_mut()
                .write_all(b"200 binary mode\r\n")
                .await
                .unwrap(),
            "SIZE" => {
                let response = if reject_size {
                    "502 SIZE unsupported\r\n".to_owned()
                } else {
                    format!("213 {}\r\n", payload.len())
                };
                control
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
            }
            "REST" => {
                let requested_offset = argument.parse().unwrap();
                rest_offsets.lock().unwrap().push(requested_offset);
                if reject_nonzero_rest && requested_offset > 0 {
                    control
                        .get_mut()
                        .write_all(b"502 nonzero restart rejected\r\n")
                        .await
                        .unwrap();
                } else {
                    offset = requested_offset;
                    resumed_at.store(offset, Ordering::SeqCst);
                    control
                        .get_mut()
                        .write_all(b"350 restart accepted\r\n")
                        .await
                        .unwrap();
                }
            }
            "PASV" => {
                let data = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let port = data.local_addr().unwrap().port();
                passive = Some(data);
                control
                    .get_mut()
                    .write_all(
                        format!(
                            "227 Entering Passive Mode (127,0,0,1,{},{})\r\n",
                            port / 256,
                            port % 256
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
            "RETR" => {
                control
                    .get_mut()
                    .write_all(b"150 opening data\r\n")
                    .await
                    .unwrap();
                let (mut data, _) = passive.take().unwrap().accept().await.unwrap();
                let active = active_data_connections.fetch_add(1, Ordering::SeqCst) + 1;
                max_active_data_connections.fetch_max(active, Ordering::SeqCst);
                let mut write_failed = false;
                for chunk in payload[offset..].chunks(chunk_size) {
                    if data.write_all(chunk).await.is_err() {
                        write_failed = true;
                        break;
                    }
                    if !chunk_delay.is_zero() {
                        tokio::time::sleep(chunk_delay).await;
                    }
                }
                let _ = data.shutdown().await;
                active_data_connections.fetch_sub(1, Ordering::SeqCst);
                if write_failed {
                    break 'session;
                }
                control
                    .get_mut()
                    .write_all(b"226 transfer complete\r\n")
                    .await
                    .unwrap();
            }
            "QUIT" => {
                control.get_mut().write_all(b"221 bye\r\n").await.unwrap();
                break;
            }
            _ => control
                .get_mut()
                .write_all(b"502 unsupported\r\n")
                .await
                .unwrap(),
        }
    }
    if logged_in {
        active_control_sessions.fetch_sub(1, Ordering::SeqCst);
    }
}

fn reserve_control_session(
    active_control_sessions: &AtomicUsize,
    maximum_control_sessions: Option<usize>,
) -> bool {
    let Some(maximum) = maximum_control_sessions else {
        active_control_sessions.fetch_add(1, Ordering::SeqCst);
        return true;
    };

    let mut active = active_control_sessions.load(Ordering::SeqCst);
    loop {
        if active >= maximum {
            return false;
        }
        match active_control_sessions.compare_exchange(
            active,
            active + 1,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => return true,
            Err(current) => active = current,
        }
    }
}

fn deterministic_payload(length: usize) -> Vec<u8> {
    (0..length).map(|index| (index % 251) as u8).collect()
}

#[tokio::test]
async fn ftp_uses_disjoint_ranges_up_to_the_connection_limit() {
    let payload = deterministic_payload(12 * 1024 * 1024);
    let server =
        spawn_observable_ftp_server(payload.clone(), Duration::from_millis(2), false).await;
    let directory = tempdir().unwrap();
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask {
            source: server.source.clone(),
            name: "payload.bin".into(),
            kind: "ftp".into(),
            is_private: false,
        })
        .unwrap();
    let engine = Arc::new(FtpDownloadEngine::new(
        directory.path(),
        Arc::clone(&store),
        Arc::new(NoEvents),
    ));
    engine.set_limits(5, 3);
    engine
        .submit_uri(task.id, &server.source, UriSubmissionOptions::default())
        .await
        .unwrap();

    let completed = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let current = store.find(task.id).unwrap();
            if current.status == TaskStatus::Completed {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("parallel FTP transfer completes");

    assert_eq!(
        std::fs::read(directory.path().join("payload.bin")).unwrap(),
        payload
    );
    assert_eq!(server.max_active_data_connections(), 3);
    assert!(server.rest_offsets().iter().any(|offset| *offset > 0));
    assert_eq!(completed.connections, 0);
    assert!(store.ftp_segments(task.id).unwrap().is_empty());
}

#[tokio::test]
async fn ftp_requeues_segments_when_the_server_limits_control_sessions() {
    let payload = deterministic_payload(12 * 1024 * 1024);
    let server =
        spawn_session_limited_ftp_server(payload.clone(), Duration::from_millis(2), 2).await;
    let directory = tempdir().unwrap();
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask {
            source: server.source.clone(),
            name: "payload.bin".into(),
            kind: "ftp".into(),
            is_private: false,
        })
        .unwrap();
    let engine = Arc::new(FtpDownloadEngine::new(
        directory.path(),
        Arc::clone(&store),
        Arc::new(NoEvents),
    ));
    engine.set_limits(5, 3);
    engine
        .submit_uri(task.id, &server.source, UriSubmissionOptions::default())
        .await
        .unwrap();

    let completed = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let current = store.find(task.id).unwrap();
            if matches!(current.status, TaskStatus::Completed | TaskStatus::Failed) {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("FTP task reaches a terminal state");

    assert_eq!(completed.status, TaskStatus::Completed, "{completed:?}");
    assert_eq!(
        std::fs::read(directory.path().join("payload.bin")).unwrap(),
        payload
    );
    assert_eq!(server.max_active_data_connections(), 2);
}

#[tokio::test]
async fn rejected_nonzero_rest_falls_back_to_one_stream() {
    let payload = deterministic_payload(8 * 1024 * 1024);
    let server = spawn_observable_ftp_server(payload.clone(), Duration::from_millis(2), true).await;
    let directory = tempdir().unwrap();
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask {
            source: server.source.clone(),
            name: "payload.bin".into(),
            kind: "ftp".into(),
            is_private: false,
        })
        .unwrap();
    let engine = Arc::new(FtpDownloadEngine::new(
        directory.path(),
        Arc::clone(&store),
        Arc::new(NoEvents),
    ));
    engine.set_limits(5, 4);
    engine
        .submit_uri(task.id, &server.source, UriSubmissionOptions::default())
        .await
        .unwrap();

    let completed = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let current = store.find(task.id).unwrap();
            if current.status == TaskStatus::Completed {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("FTP fallback transfer completes");

    assert_eq!(
        std::fs::read(directory.path().join("payload.bin")).unwrap(),
        payload
    );
    assert_eq!(server.max_active_data_connections(), 1);
    assert!(server.rest_offsets().iter().any(|offset| *offset > 0));
    assert_eq!(completed.connections, 0);
    assert!(store.ftp_segments(task.id).unwrap().is_empty());
}

#[tokio::test]
async fn missing_size_falls_back_to_one_stream() {
    let payload = deterministic_payload(1024 * 1024);
    let server = spawn_no_size_ftp_server(payload.clone(), Duration::from_millis(2)).await;
    let directory = tempdir().unwrap();
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask {
            source: server.source.clone(),
            name: "payload.bin".into(),
            kind: "ftp".into(),
            is_private: false,
        })
        .unwrap();
    let engine = Arc::new(FtpDownloadEngine::new(
        directory.path(),
        Arc::clone(&store),
        Arc::new(NoEvents),
    ));
    engine.set_limits(5, 4);
    engine
        .submit_uri(task.id, &server.source, UriSubmissionOptions::default())
        .await
        .unwrap();

    let finished = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = store.find(task.id).unwrap();
            if matches!(current.status, TaskStatus::Completed | TaskStatus::Failed) {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("FTP task reaches a terminal state");

    assert_eq!(finished.status, TaskStatus::Completed);
    assert_eq!(
        std::fs::read(directory.path().join("payload.bin")).unwrap(),
        payload
    );
    assert_eq!(server.max_active_data_connections(), 1);
    assert!(store.ftp_segments(task.id).unwrap().is_empty());
}

#[tokio::test]
async fn paused_parallel_ftp_transfer_resumes_without_holes() {
    let payload = deterministic_payload(12 * 1024 * 1024);
    let server =
        spawn_observable_ftp_server(payload.clone(), Duration::from_millis(10), false).await;
    let directory = tempdir().unwrap();
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask {
            source: server.source.clone(),
            name: "payload.bin".into(),
            kind: "ftp".into(),
            is_private: false,
        })
        .unwrap();
    let engine = Arc::new(FtpDownloadEngine::new(
        directory.path(),
        Arc::clone(&store),
        Arc::new(NoEvents),
    ));
    engine.set_limits(2, 3);
    engine
        .submit_uri(task.id, &server.source, UriSubmissionOptions::default())
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let segments = store.ftp_segments(task.id).unwrap();
            if segments
                .iter()
                .filter(|segment| segment.completed_length > 0)
                .count()
                >= 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("multiple FTP ranges make durable progress");

    let paused = engine
        .control(task.id, versadown_lib::DownloadAction::Pause)
        .await
        .unwrap();
    assert_eq!(paused.status, TaskStatus::Paused);
    let saved = store.ftp_segments(task.id).unwrap();
    assert!(
        saved
            .iter()
            .filter(|segment| segment.completed_length > 0)
            .count()
            >= 2
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(store.ftp_segments(task.id).unwrap(), saved);

    engine
        .control(task.id, versadown_lib::DownloadAction::Resume)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if store.find(task.id).unwrap().status == TaskStatus::Completed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("paused parallel FTP task resumes");

    assert_eq!(
        std::fs::read(directory.path().join("payload.bin")).unwrap(),
        payload
    );
}

#[tokio::test]
async fn restarted_parallel_ftp_transfer_resumes_saved_ranges() {
    use std::io::{Seek, SeekFrom, Write};

    let payload = deterministic_payload(12 * 1024 * 1024);
    let server = spawn_observable_ftp_server(payload.clone(), Duration::ZERO, false).await;
    let directory = tempdir().unwrap();
    let output = directory.path().join("payload.bin");
    let segments = vec![
        FtpSegment {
            index: 0,
            start_offset: 0,
            end_offset: 4 * 1024 * 1024,
            completed_length: 1024 * 1024,
        },
        FtpSegment {
            index: 1,
            start_offset: 4 * 1024 * 1024,
            end_offset: 8 * 1024 * 1024,
            completed_length: 512 * 1024,
        },
        FtpSegment {
            index: 2,
            start_offset: 8 * 1024 * 1024,
            end_offset: 12 * 1024 * 1024,
            completed_length: 0,
        },
    ];
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&output)
        .unwrap();
    file.set_len(payload.len() as u64).unwrap();
    file.write_all(&payload[..1024 * 1024]).unwrap();
    file.seek(SeekFrom::Start(4 * 1024 * 1024)).unwrap();
    file.write_all(&payload[4 * 1024 * 1024..4 * 1024 * 1024 + 512 * 1024])
        .unwrap();
    drop(file);

    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let mut task = store
        .create(NewTask {
            source: server.source.clone(),
            name: "payload.bin".into(),
            kind: "ftp".into(),
            is_private: false,
        })
        .unwrap();
    task.status = TaskStatus::Downloading;
    task.total_length = Some(payload.len() as u64);
    task.completed_length = 1024 * 1024 + 512 * 1024;
    store
        .apply_transfer_snapshot_with_files(
            task.clone(),
            &[(
                1,
                output.to_string_lossy().into_owned(),
                payload.len() as u64,
                task.completed_length,
                true,
            )],
        )
        .unwrap();
    store.replace_ftp_segments(task.id, &segments).unwrap();

    let restarted = Arc::new(FtpDownloadEngine::new(
        directory.path(),
        Arc::clone(&store),
        Arc::new(NoEvents),
    ));
    restarted.set_limits(2, 3);
    restarted.recover().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if store.find(task.id).unwrap().status == TaskStatus::Completed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("restarted parallel FTP task completes");

    assert_eq!(std::fs::read(output).unwrap(), payload);
    assert!(server.rest_offsets().contains(&(1024 * 1024)));
    assert!(server
        .rest_offsets()
        .contains(&(4 * 1024 * 1024 + 512 * 1024)));
    assert!(server.rest_offsets().contains(&(8 * 1024 * 1024)));
}

#[tokio::test]
async fn ftp_engine_resumes_a_partial_file_and_completes_the_existing_task() {
    use std::sync::atomic::Ordering;

    let payload = b"0123456789abcdefghijklmnopqrstuvwxyz".to_vec();
    let (source, resumed_at) = spawn_ftp_server(payload.clone(), Duration::ZERO).await;
    let directory = tempdir().unwrap();
    let output = directory.path().join("payload.bin");
    std::fs::write(&output, &payload[..9]).unwrap();

    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask {
            source: source.clone(),
            name: "payload.bin".into(),
            kind: "ftp".into(),
            is_private: false,
        })
        .unwrap();
    store
        .upsert_files(
            task.id,
            &[(
                0,
                output.to_string_lossy().into_owned(),
                payload.len() as u64,
                9,
                true,
            )],
        )
        .unwrap();
    let engine = Arc::new(FtpDownloadEngine::new(
        directory.path(),
        Arc::clone(&store),
        Arc::new(NoEvents),
    ));

    engine
        .submit_uri(task.id, &source, UriSubmissionOptions::default())
        .await
        .unwrap();

    let completed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let task = store.find(task.id).unwrap();
            if task.status == TaskStatus::Completed {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("FTP transfer completes");

    assert_eq!(std::fs::read(output).unwrap(), payload);
    assert_eq!(resumed_at.load(Ordering::SeqCst), 9);
    assert_eq!(completed.progress, 100);
    assert_eq!(completed.total_length, Some(36));
    assert_eq!(completed.completed_length, 36);
    assert!(completed.aria_gid.is_none());
}

#[tokio::test]
async fn ftp_engine_pauses_and_resumes_the_same_durable_task() {
    use std::sync::atomic::Ordering;

    let payload = vec![0x5a; 512 * 1024];
    let (source, resumed_at) = spawn_ftp_server(payload.clone(), Duration::from_millis(10)).await;
    let directory = tempdir().unwrap();
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask {
            source: source.clone(),
            name: "payload.bin".into(),
            kind: "ftp".into(),
            is_private: false,
        })
        .unwrap();
    let engine = Arc::new(FtpDownloadEngine::new(
        directory.path(),
        Arc::clone(&store),
        Arc::new(NoEvents),
    ));
    engine
        .submit_uri(task.id, &source, UriSubmissionOptions::default())
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let current = store.find(task.id).unwrap();
            if current.completed_length >= 8 * 1024 && current.status == TaskStatus::Downloading {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let paused = engine
        .control(task.id, versadown_lib::DownloadAction::Pause)
        .await
        .unwrap();
    assert_eq!(paused.status, TaskStatus::Paused);
    let paused_size = std::fs::metadata(directory.path().join("payload.bin"))
        .unwrap()
        .len();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        std::fs::metadata(directory.path().join("payload.bin"))
            .unwrap()
            .len(),
        paused_size
    );

    engine
        .control(task.id, versadown_lib::DownloadAction::Resume)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if store.find(task.id).unwrap().status == TaskStatus::Completed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    assert_eq!(
        std::fs::read(directory.path().join("payload.bin")).unwrap(),
        payload
    );
    assert!(resumed_at.load(Ordering::SeqCst) >= paused_size as usize);
}

#[tokio::test]
async fn ftp_connection_failure_becomes_a_retryable_failed_task() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        client
            .write_all(b"530 service unavailable\r\n")
            .await
            .unwrap();
    });
    let source = format!("ftp://127.0.0.1:{port}/missing.bin");
    let directory = tempdir().unwrap();
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store
        .create(NewTask {
            source: source.clone(),
            name: "missing.bin".into(),
            kind: "ftp".into(),
            is_private: false,
        })
        .unwrap();
    let engine = Arc::new(FtpDownloadEngine::new(
        directory.path(),
        Arc::clone(&store),
        Arc::new(NoEvents),
    ));
    engine
        .submit_uri(task.id, &source, UriSubmissionOptions::default())
        .await
        .unwrap();

    let failed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let current = store.find(task.id).unwrap();
            if current.status == TaskStatus::Failed {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("connection failure left task at {:?}", store.find(task.id)));
    assert!(failed.error.unwrap().contains("FTP 服务器操作失败"));
}

#[tokio::test]
async fn ftp_engine_recovers_an_active_task_after_restart() {
    use std::sync::atomic::Ordering;

    let payload = b"restart-safe-ftp-payload".to_vec();
    let (source, resumed_at) = spawn_ftp_server(payload.clone(), Duration::ZERO).await;
    let directory = tempdir().unwrap();
    let output = directory.path().join("payload.bin");
    std::fs::write(&output, &payload[..7]).unwrap();
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let mut task = store
        .create(NewTask {
            source: source.clone(),
            name: "payload.bin".into(),
            kind: "ftp".into(),
            is_private: false,
        })
        .unwrap();
    task.status = TaskStatus::Downloading;
    task.total_length = Some(payload.len() as u64);
    task.completed_length = 7;
    store
        .apply_transfer_snapshot_with_files(
            task.clone(),
            &[(
                1,
                output.to_string_lossy().into_owned(),
                payload.len() as u64,
                7,
                true,
            )],
        )
        .unwrap();

    let restarted = Arc::new(FtpDownloadEngine::new(
        directory.path(),
        Arc::clone(&store),
        Arc::new(NoEvents),
    ));
    restarted.recover().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if store.find(task.id).unwrap().status == TaskStatus::Completed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    assert_eq!(std::fs::read(output).unwrap(), payload);
    assert_eq!(resumed_at.load(Ordering::SeqCst), 7);
}
