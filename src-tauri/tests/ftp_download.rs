use std::{sync::Arc, time::Duration};

use alldownloader_lib::{
    list_queue_tasks_for_scope, parse_ftp_url, Aria2GlobalStat, DownloadTask, EventSink,
    FtpDownloadEngine, NewTask, TaskStatus, TaskStore, UriSubmissionOptions,
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
    assert_eq!(target.password(), "anonymous@alldownloader.local");

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
        })
        .unwrap();

    let queue = list_queue_tasks_for_scope(&store, None).unwrap();
    assert_eq!(queue[0].source, "ftp://example.test/archive.zip");
    assert!(!serde_json::to_string(&queue)
        .unwrap()
        .contains("top-secret"));
}

struct NoEvents;

impl EventSink for NoEvents {
    fn task_updated(&self, _: &DownloadTask) {}

    fn global_stat_updated(&self, _: &Aria2GlobalStat) {}
}

async fn spawn_ftp_server(
    payload: Vec<u8>,
    chunk_delay: Duration,
) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let resumed_at = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_resume = Arc::clone(&resumed_at);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let payload = payload.clone();
            let observed_resume = Arc::clone(&observed_resume);
            tokio::spawn(async move {
                handle_ftp_client(stream, payload, observed_resume, chunk_delay).await;
            });
        }
    });
    (
        format!("ftp://tester:secret@{address}/payload.bin"),
        resumed_at,
    )
}

async fn handle_ftp_client(
    stream: TcpStream,
    payload: Vec<u8>,
    resumed_at: Arc<std::sync::atomic::AtomicUsize>,
    chunk_delay: Duration,
) {
    use std::sync::atomic::Ordering;

    let mut control = BufReader::new(stream);
    control
        .get_mut()
        .write_all(b"220 test FTP ready\r\n")
        .await
        .unwrap();
    let mut line = String::new();
    let mut passive = None;
    let mut offset = 0usize;
    loop {
        line.clear();
        if control.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let command = line.trim_end();
        let (verb, argument) = command.split_once(' ').unwrap_or((command, ""));
        match verb.to_ascii_uppercase().as_str() {
            "USER" => control
                .get_mut()
                .write_all(b"331 password required\r\n")
                .await
                .unwrap(),
            "PASS" => control
                .get_mut()
                .write_all(b"230 logged in\r\n")
                .await
                .unwrap(),
            "TYPE" => control
                .get_mut()
                .write_all(b"200 binary mode\r\n")
                .await
                .unwrap(),
            "SIZE" => {
                control
                    .get_mut()
                    .write_all(format!("213 {}\r\n", payload.len()).as_bytes())
                    .await
                    .unwrap();
            }
            "REST" => {
                offset = argument.parse().unwrap();
                resumed_at.store(offset, Ordering::SeqCst);
                control
                    .get_mut()
                    .write_all(b"350 restart accepted\r\n")
                    .await
                    .unwrap();
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
                for chunk in payload[offset..].chunks(8 * 1024) {
                    if data.write_all(chunk).await.is_err() {
                        return;
                    }
                    if !chunk_delay.is_zero() {
                        tokio::time::sleep(chunk_delay).await;
                    }
                }
                data.shutdown().await.unwrap();
                control
                    .get_mut()
                    .write_all(b"226 transfer complete\r\n")
                    .await
                    .unwrap();
            }
            "QUIT" => {
                control.get_mut().write_all(b"221 bye\r\n").await.unwrap();
                return;
            }
            _ => control
                .get_mut()
                .write_all(b"502 unsupported\r\n")
                .await
                .unwrap(),
        }
    }
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
        .control(task.id, alldownloader_lib::DownloadAction::Pause)
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
        .control(task.id, alldownloader_lib::DownloadAction::Resume)
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
