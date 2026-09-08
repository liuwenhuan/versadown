use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use alldownloader_lib::{Aria2Engine, ProxySettings};
use uuid::Uuid;

fn start_slow_server() -> (String, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let running = Arc::new(AtomicBool::new(true));
    let server_running = Arc::clone(&running);
    thread::spawn(move || {
        while server_running.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    thread::spawn(move || {
                        let mut request = [0u8; 4096];
                        let count = stream.read(&mut request).unwrap_or(0);
                        let request = String::from_utf8_lossy(&request[..count]);
                        const CHUNK: &[u8] = &[0x5a; 32 * 1024];
                        const CHUNKS: usize = 96;
                        let length = CHUNK.len() * CHUNKS;
                        let range_start = request
                            .lines()
                            .find_map(|line| line.strip_prefix("Range: bytes="))
                            .and_then(|range| range.split('-').next())
                            .and_then(|start| start.parse::<usize>().ok())
                            .unwrap_or(0);
                        let remaining = length.saturating_sub(range_start);
                        if range_start > 0 {
                            let _ = write!(stream, "HTTP/1.1 206 Partial Content\r\nContent-Length: {remaining}\r\nContent-Range: bytes {range_start}-{}/{length}\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n", length - 1);
                        } else {
                            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n");
                        }
                        let mut sent = 0;
                        while sent < remaining {
                            let count = CHUNK.len().min(remaining - sent);
                            if stream.write_all(&CHUNK[..count]).is_err() {
                                break;
                            }
                            sent += count;
                            let _ = stream.flush();
                            thread::sleep(Duration::from_millis(15));
                        }
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(_) => break,
            }
        }
    });
    (format!("http://{address}/large-test.bin"), running)
}

#[test]
fn real_aria2_engine_reports_progress_and_obeys_pause_resume() {
    let root = std::env::temp_dir().join(format!("alldownloader-rpc-test-{}", Uuid::new_v4()));
    let downloads = root.join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let binary = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join("aria2c-x86_64-pc-windows-msvc.exe");
    let (url, server_running) = start_slow_server();
    let engine = Aria2Engine::start(binary, &root).unwrap();
    let gid = engine
        .add_uri(&url, &downloads, &ProxySettings::default(), None)
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut saw_intermediate_progress = false;
    while Instant::now() < deadline {
        let status = engine.status(&gid).unwrap();
        let total: u64 = status.total_length.parse().unwrap_or(0);
        let completed: u64 = status.completed_length.parse().unwrap_or(0);
        if total > 0 && completed > 0 && completed < total {
            saw_intermediate_progress = true;
            engine.pause(&gid).unwrap();
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        saw_intermediate_progress,
        "aria2 never exposed an intermediate byte count"
    );

    let pause_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < pause_deadline && engine.status(&gid).unwrap().status != "paused" {
        thread::sleep(Duration::from_millis(40));
    }
    assert_eq!(engine.status(&gid).unwrap().status, "paused");
    engine.unpause(&gid).unwrap();

    let complete_deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < complete_deadline && engine.status(&gid).unwrap().status != "complete" {
        thread::sleep(Duration::from_millis(50));
    }
    let final_status = engine.status(&gid).unwrap();
    assert_eq!(
        final_status.status, "complete",
        "aria2 error {}: {}",
        final_status.error_code, final_status.error_message
    );

    let cancelled_gid = engine
        .add_uri(
            &format!("{url}?cancel"),
            &downloads,
            &ProxySettings::default(),
            None,
        )
        .unwrap();
    engine.remove(&cancelled_gid).unwrap();
    let cancel_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < cancel_deadline
        && engine.status(&cancelled_gid).unwrap().status != "removed"
    {
        thread::sleep(Duration::from_millis(40));
    }
    assert_eq!(engine.status(&cancelled_gid).unwrap().status, "removed");

    drop(engine);
    server_running.store(false, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn unfinished_download_resumes_with_the_same_gid_after_engine_restart() {
    let root = std::env::temp_dir().join(format!("alldownloader-resume-test-{}", Uuid::new_v4()));
    let downloads = root.join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let binary = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join("aria2c-x86_64-pc-windows-msvc.exe");
    let (url, server_running) = start_slow_server();

    let first_engine = Aria2Engine::start(binary.clone(), &root).unwrap();
    let gid = first_engine
        .add_uri(
            &format!("{url}?resume"),
            &downloads,
            &ProxySettings::default(),
            None,
        )
        .unwrap();
    let progress_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < progress_deadline {
        let status = first_engine.status(&gid).unwrap();
        if status.completed_length.parse::<u64>().unwrap_or(0) > 0 {
            break;
        }
        thread::sleep(Duration::from_millis(40));
    }
    assert!(
        first_engine
            .status(&gid)
            .unwrap()
            .completed_length
            .parse::<u64>()
            .unwrap_or(0)
            > 0
    );
    drop(first_engine);

    let second_engine = Aria2Engine::start(binary, &root).unwrap();
    let resumed_gid = second_engine
        .add_uri(
            &format!("{url}?resume"),
            &downloads,
            &ProxySettings::default(),
            Some(&gid),
        )
        .unwrap();
    assert_eq!(resumed_gid, gid);
    let complete_deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < complete_deadline
        && second_engine.status(&gid).unwrap().status != "complete"
    {
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(second_engine.status(&gid).unwrap().status, "complete");

    drop(second_engine);
    server_running.store(false, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(root);
}
