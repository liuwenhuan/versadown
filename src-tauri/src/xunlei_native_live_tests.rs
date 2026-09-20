//! Opt-in real installed-DLL regression; never touches the user's tasks.
use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

const PAYLOAD_SIZE: usize = 16 * 1024 * 1024;

#[test]
#[ignore = "uses installed official SDK and a throttled loopback range server"]
fn installed_sdk_preserves_pause_and_process_restart() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/fixture.bin", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let stopped = Arc::new(AtomicBool::new(false));
    let restoring = Arc::new(AtomicBool::new(false));
    let ranges = Arc::new(Mutex::new(Vec::new()));
    let server = {
        let stopped = Arc::clone(&stopped);
        let restoring = Arc::clone(&restoring);
        let ranges = Arc::clone(&ranges);
        std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                let stopped = Arc::clone(&stopped);
                let restoring = Arc::clone(&restoring);
                let ranges = Arc::clone(&ranges);
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut request = Vec::new();
                    let mut byte = [0];
                    while request.len() < 16384 && !request.ends_with(b"\r\n\r\n") {
                        if stream.read_exact(&mut byte).is_err() {
                            return;
                        }
                        request.push(byte[0]);
                    }
                    let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
                    let signature = if restoring.load(Ordering::Relaxed) {
                        "signature=restore"
                    } else {
                        "signature=pause"
                    };
                    if !text.lines().next().unwrap_or("").contains(signature) {
                        let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        return;
                    }
                    let range = text
                        .lines()
                        .find_map(|line| line.strip_prefix("range: bytes="));
                    let (start, end) = range
                        .map(|range| {
                            let (start, end) = range.split_once('-').unwrap();
                            (
                                start.parse::<usize>().unwrap(),
                                end.parse::<usize>().unwrap_or(PAYLOAD_SIZE - 1),
                            )
                        })
                        .unwrap_or((0, PAYLOAD_SIZE - 1));
                    if start >= PAYLOAD_SIZE || end >= PAYLOAD_SIZE {
                        return;
                    }
                    let status = if range.is_some() {
                        "206 Partial Content"
                    } else {
                        "200 OK"
                    };
                    let header = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{PAYLOAD_SIZE}\r\nConnection: close\r\n\r\n", end - start + 1);
                    if stream.write_all(header.as_bytes()).is_err() || text.starts_with("head ") {
                        return;
                    }
                    ranges.lock().unwrap().push((start, end));
                    for pos in (start..=end).step_by(8192) {
                        if stopped.load(Ordering::Relaxed) {
                            return;
                        }
                        let chunk: Vec<u8> = (pos..(pos + 8192).min(end + 1))
                            .map(|n| (n % 251) as u8)
                            .collect();
                        if stream.write_all(&chunk).is_err() {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                });
            }
        })
    };
    let run = |phase| {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "xunlei_native::live_tests::sdk_resume_worker",
                "--ignored",
                "--nocapture",
            ])
            .env("XL_TEST_PHASE", phase)
            .env("XL_TEST_ROOT", root.path())
            // A cloud resolver returns a different signed URL after restart.
            // The bytes and output are unchanged, so the checkpoint must survive.
            .env("XL_TEST_URL", format!("{url}?signature={phase}"))
            .output()
            .unwrap()
    };
    let first = run("pause");
    let boundary = ranges.lock().unwrap().len();
    restoring.store(true, Ordering::Relaxed);
    let second = if first.status.success() {
        Some(run("restore"))
    } else {
        None
    };
    stopped.store(true, Ordering::Relaxed);
    server.join().unwrap();
    for result in [&first, second.as_ref().unwrap_or(&first)] {
        eprintln!(
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.status.success(), "real SDK subprocess failed");
    }
    let ranges = ranges.lock().unwrap();
    assert!(ranges.len() > boundary);
    assert!(
        // Redirect triggers the SDK's 20 KiB content-type probe at Range 0.
        // It is not a restart of the payload download. Larger data ranges
        // must still start beyond the saved prefix.
        ranges[boundary..]
            .iter()
            .all(|(start, end)| *start > 0 || *end < 20 * 1024),
        "restart redownloaded the file prefix"
    );
    assert!(ranges[boundary..]
        .iter()
        .any(|(start, end)| *start > 0 && end - start > 20 * 1024));
    let bytes = std::fs::read(root.path().join("fixture.bin")).unwrap();
    assert_eq!(bytes.len(), PAYLOAD_SIZE);
    assert!(bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| *byte == (index % 251) as u8));
    eprintln!("verified all {PAYLOAD_SIZE} bytes after pause/resume and a fresh SDK process");
}

fn wait_progress(
    api: &WindowsApi,
    id: u32,
    ready: impl Fn(NativeProgress) -> bool,
) -> NativeProgress {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let progress = api.progress(id).unwrap();
        assert_ne!(progress.status, NATIVE_ERROR, "SDK failure: {progress:?}");
        if ready(progress) {
            return progress;
        }
        assert!(Instant::now() < deadline, "SDK stalled: {progress:?}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
#[ignore = "child of installed_sdk_preserves_pause_and_process_restart"]
fn sdk_resume_worker() {
    let phase = std::env::var("XL_TEST_PHASE").expect("must run through parent test");
    let root = PathBuf::from(std::env::var_os("XL_TEST_ROOT").unwrap());
    let url = std::env::var("XL_TEST_URL").unwrap();
    let output = root.join("fixture.bin");
    let mut api = WindowsApi::load(Some(&root.join("plugin.json")), None)
        .unwrap()
        .unwrap();
    let create = |api: &mut WindowsApi| {
        api.create_p2sp_task(
            &url,
            &output,
            "fixture.bin",
            &root,
            "",
            PAYLOAD_SIZE as u64,
            "Mozilla/5.0",
            "",
            "1111111111111111111111111111111111111111",
        )
        .unwrap()
    };
    let id = create(&mut api);
    api.start(id).unwrap();
    if phase == "pause" {
        wait_progress(&api, id, |p| p.completed >= 512 * 1024);
        api.stop(id).unwrap();
        let stopped = wait_progress(&api, id, |p| p.status == NATIVE_STOPPED);
        // This is the production resume path, not a test-only DeleteTask call.
        let resumed = create(&mut api);
        api.start(resumed).unwrap();
        let first = wait_progress(&api, resumed, |p| p.total > 0);
        assert!(
            first.completed >= stopped.completed,
            "resume lost checkpoint: {stopped:?} -> {first:?}"
        );
        wait_progress(&api, resumed, |p| {
            p.completed >= stopped.completed + 512 * 1024
        });
        api.stop(resumed).unwrap();
        let stopped = wait_progress(&api, resumed, |p| p.status == NATIVE_STOPPED);
        api.release_output(&output).unwrap();
        std::fs::write(root.join("checkpoint-bytes"), stopped.completed.to_string()).unwrap();
        eprintln!(
            "checkpoint retained after in-process resume: {} bytes",
            stopped.completed
        );
    } else {
        let saved: u64 = std::fs::read_to_string(root.join("checkpoint-bytes"))
            .unwrap()
            .parse()
            .unwrap();
        let first = wait_progress(&api, id, |p| p.total > 0);
        assert!(
            first.completed >= saved,
            "process restart lost checkpoint: {saved} -> {first:?}"
        );
        let complete = wait_progress(&api, id, |p| p.status == NATIVE_COMPLETE);
        assert_eq!(complete.completed, PAYLOAD_SIZE as u64);
        assert!(output.is_file(), "SDK completion must include final rename");
        eprintln!(
            "fresh process restored {saved} bytes and completed with SDK state {}",
            complete.status
        );
        api.delete(id).unwrap();
    }
}
