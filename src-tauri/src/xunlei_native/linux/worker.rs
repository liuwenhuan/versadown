use super::{
    axdl::AxdlRuntime,
    peer_id::local_client_peer_id,
    protocol::{
        read_frame, write_frame, WorkerErrorCode, WorkerHello, WorkerInitialization, WorkerRequest,
        WorkerResponse, WorkerSubmission,
    },
};
use base64::Engine as _;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::Read,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
};

struct WorkerArguments {
    socket: PathBuf,
    // These immutable runtime descriptors are intentionally accepted as
    // command-line metadata. The worker never receives account/task secrets
    // by argv, environment variables, or files.
    _runtime_root: PathBuf,
    runtime_version: String,
    sdk: PathBuf,
    stat: PathBuf,
    node: PathBuf,
    addon: PathBuf,
    node_sha256: String,
    stat_sha256: String,
    sdk_sha256: String,
    addon_sha256: String,
}

extern "C" {
    fn _exit(status: i32) -> !;
}

pub(super) fn run_from_args() -> Result<(), ()> {
    let arguments = parse_arguments(std::env::args_os().skip(2))?;
    let nonce = read_nonce()?;
    let state_directory = worker_state_directory(&arguments.socket)?;
    write_private_phub_setting(&state_directory)?;
    let axdl = AxdlRuntime::load_verified(
        &arguments.stat,
        &arguments.sdk,
        &arguments.addon,
        &arguments.node,
        &arguments.node_sha256,
        &arguments.stat_sha256,
        &arguments.sdk_sha256,
        &arguments.addon_sha256,
    )
    .map_err(|_| ())?;
    let stream = UnixStream::connect(arguments.socket).map_err(|_| ())?;
    let result = run_stream_with_axdl(
        stream,
        nonce,
        Some(WorkerAxdlBridge {
            axdl,
            client_version: arguments.runtime_version,
            state_directory,
        }),
    );
    // The AXDL runtime owns native threads. This process is dedicated to one
    // bridge session, so it is always terminated by the parent after the
    // shutdown acknowledgement instead of attempting an in-process dlclose.
    let exit_code = if result.is_ok() { 0 } else { 1 };
    unsafe { _exit(exit_code) }
}

fn run_stream(stream: UnixStream, nonce: String) -> Result<(), ()> {
    run_stream_with_axdl(stream, nonce, None)
}

struct WorkerAxdlBridge {
    axdl: AxdlRuntime,
    client_version: String,
    state_directory: PathBuf,
}

struct WorkerTask {
    output_path: PathBuf,
    total_length: Option<u64>,
    paused: bool,
}

#[cfg(test)]
static LIVE_EXPECTED_GCID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

impl WorkerAxdlBridge {
    fn initialize(&mut self, initialization: &WorkerInitialization) -> Result<(), ()> {
        // The cloud web Peer-Id identifies the login session. The Linux SDK
        // uses a different, MAC-derived 16-character device Peer ID for P2P
        // resource queries; passing the web value yields no peer resources.
        let native_peer_id = local_client_peer_id().ok_or(())?;
        self.axdl
            .initialize(&self.client_version, &native_peer_id, &self.state_directory)
            .and_then(|()| self.axdl.set_user_identity(&initialization.user_id))
            .map_err(|_| ())
    }

    fn submit(&mut self, submission: WorkerSubmission) -> Result<u32, ()> {
        #[cfg(test)]
        if std::env::var_os("ALLDOWNLOADER_AXDL_REAL_VIP_TEST_LARGE").is_some() {
            if let Some(gcid) = submission.gcid.as_ref() {
                let _ = LIVE_EXPECTED_GCID.set(gcid.to_ascii_lowercase());
            }
        }
        let output_path = PathBuf::from(&submission.output_path);
        let output_directory = output_path.parent().ok_or(())?;
        let referer = submission
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("referer"))
            .map(|(_, value)| value.as_str())
            .unwrap_or("");
        self.axdl
            .create_and_start_task(
                &submission.url,
                referer,
                output_directory,
                &submission.output_name,
                submission.acceleration_token.as_deref(),
            )
            .map_err(|_| ())
    }

    fn stop(&self, task_id: u32) -> Result<(), ()> {
        self.axdl.stop_task(task_id).map_err(|_| ())
    }

    fn start(&self, task_id: u32) -> Result<(), ()> {
        self.axdl.start_task(task_id).map_err(|_| ())
    }

    fn delete(&mut self, task_id: u32) -> Result<(), ()> {
        self.axdl.delete_task(task_id).map_err(|_| ())
    }
}

fn run_stream_with_axdl(
    mut stream: UnixStream,
    nonce: String,
    mut axdl: Option<WorkerAxdlBridge>,
) -> Result<(), ()> {
    write_frame(&mut stream, &WorkerHello::new(nonce)).map_err(|_| ())?;
    let mut initialized = false;
    let mut tasks = BTreeMap::<u32, WorkerTask>::new();
    loop {
        let request = read_frame::<WorkerRequest>(&mut stream).map_err(|_| ())?;
        let should_exit = matches!(request, WorkerRequest::Shutdown);
        let response = match request {
            WorkerRequest::Initialize(initialization) => {
                let result = match axdl.as_mut() {
                    Some(bridge) => bridge.initialize(&initialization),
                    None => Ok(()),
                };
                if result.is_ok() {
                    initialized = true;
                    WorkerResponse::Acknowledged
                } else {
                    WorkerResponse::Error {
                        code: WorkerErrorCode::Incompatible,
                    }
                }
            }
            WorkerRequest::Submit(submission) if initialized => match axdl.as_mut() {
                Some(bridge) => {
                    let output_path = PathBuf::from(&submission.output_path);
                    let total_length = submission.total_length;
                    match bridge.submit(submission) {
                        Ok(task_id) => {
                            tasks.insert(
                                task_id,
                                WorkerTask {
                                    output_path,
                                    total_length,
                                    paused: false,
                                },
                            );
                            WorkerResponse::Submitted { task_id }
                        }
                        Err(()) => WorkerResponse::Error {
                            code: WorkerErrorCode::OperationFailed,
                        },
                    }
                }
                None => WorkerResponse::Error {
                    code: WorkerErrorCode::Incompatible,
                },
            },
            WorkerRequest::Submit(_) => WorkerResponse::Error {
                code: WorkerErrorCode::OperationFailed,
            },
            WorkerRequest::Poll { task_id } => match tasks.get(&task_id) {
                Some(task) => {
                    let info = axdl
                        .as_ref()
                        .and_then(|bridge| bridge.axdl.query_task_info(task_id));
                    #[cfg(test)]
                    if std::env::var_os("ALLDOWNLOADER_AXDL_REAL_VIP_TEST_LARGE").is_some() {
                        static PRINTED_INDEX_STATE: std::sync::atomic::AtomicBool =
                            std::sync::atomic::AtomicBool::new(false);
                        static PRINTED_CHANNEL_MIB: std::sync::atomic::AtomicU64 =
                            std::sync::atomic::AtomicU64::new(0);
                        if let Some((0, bytes)) = info.as_ref() {
                            let completed = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
                            let mib = completed / (1024 * 1024);
                            if mib > 0
                                && mib
                                    > PRINTED_CHANNEL_MIB
                                        .swap(mib, std::sync::atomic::Ordering::AcqRel)
                            {
                                let number = |offset| {
                                    u64::from_le_bytes(
                                        bytes[offset..offset + 8].try_into().unwrap(),
                                    )
                                };
                                eprintln!(
                                    "large AXDL channels: source={}, p2p={}, offline={}",
                                    number(104),
                                    number(120),
                                    number(136)
                                );
                            }
                            if completed >= 1024 * 1024
                                && !PRINTED_INDEX_STATE
                                    .swap(true, std::sync::atomic::Ordering::AcqRel)
                            {
                                eprintln!(
                                    "large AXDL index: cid_known={}, gcid_known={}, gcid_matches_token={}, native_completed={completed}",
                                    bytes[52..72].iter().any(|byte| *byte != 0),
                                    bytes[72..92].iter().any(|byte| *byte != 0),
                                    LIVE_EXPECTED_GCID.get().is_some_and(|expected| {
                                        let actual = bytes[72..92]
                                            .iter()
                                            .map(|byte| format!("{byte:02x}"))
                                            .collect::<String>();
                                        actual == *expected
                                    }),
                                );
                            }
                        }
                    }
                    WorkerResponse::Progress(progress_with_axdl_info(task, info.as_ref()))
                }
                None => WorkerResponse::Error {
                    code: WorkerErrorCode::TaskUnknown,
                },
            },
            WorkerRequest::Control { task_id, action } => match axdl.as_mut() {
                Some(bridge) => {
                    if !tasks.contains_key(&task_id) {
                        WorkerResponse::Error {
                            code: WorkerErrorCode::TaskUnknown,
                        }
                    } else {
                        let result = match action {
                            super::protocol::WorkerAction::Pause => bridge.stop(task_id),
                            super::protocol::WorkerAction::Resume => bridge.start(task_id),
                            super::protocol::WorkerAction::Cancel => bridge.delete(task_id),
                        };
                        match result {
                            Ok(()) => {
                                match action {
                                    super::protocol::WorkerAction::Pause => {
                                        tasks
                                            .get_mut(&task_id)
                                            .expect("task checked above")
                                            .paused = true;
                                    }
                                    super::protocol::WorkerAction::Resume => {
                                        tasks
                                            .get_mut(&task_id)
                                            .expect("task checked above")
                                            .paused = false;
                                    }
                                    super::protocol::WorkerAction::Cancel => {
                                        tasks.remove(&task_id);
                                    }
                                }
                                WorkerResponse::Acknowledged
                            }
                            Err(()) => WorkerResponse::Error {
                                code: WorkerErrorCode::TaskUnknown,
                            },
                        }
                    }
                }
                None => WorkerResponse::Error {
                    code: WorkerErrorCode::TaskUnknown,
                },
            },
            WorkerRequest::Release { task_id } => match axdl.as_mut() {
                Some(bridge) if tasks.contains_key(&task_id) => match bridge.delete(task_id) {
                    Ok(()) => {
                        tasks.remove(&task_id);
                        WorkerResponse::Acknowledged
                    }
                    Err(()) => WorkerResponse::Error {
                        code: WorkerErrorCode::TaskUnknown,
                    },
                },
                _ => WorkerResponse::Error {
                    code: WorkerErrorCode::TaskUnknown,
                },
            },
            WorkerRequest::Shutdown => WorkerResponse::Acknowledged,
        };
        write_frame(&mut stream, &response).map_err(|_| ())?;
        if should_exit {
            return Ok(());
        }
    }
}

fn progress_from_output(task: &WorkerTask) -> super::protocol::WorkerProgress {
    let completed = fs::metadata(&task.output_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let complete = task
        .total_length
        .is_some_and(|total| total > 0 && completed >= total);
    super::protocol::WorkerProgress {
        completed,
        total: task.total_length.unwrap_or(0),
        status: if complete {
            7
        } else if task.paused {
            5
        } else {
            3
        },
        received: completed,
        ..Default::default()
    }
}

fn progress_with_axdl_info(
    task: &WorkerTask,
    info: Option<&(i32, [u8; 200])>,
) -> super::protocol::WorkerProgress {
    let mut progress = progress_from_output(task);
    let Some((0, bytes)) = info else {
        return progress;
    };
    if u32::from_le_bytes(bytes[..4].try_into().expect("fixed SDK info size")) != 200 {
        return progress;
    }
    let number = |offset: usize| {
        u64::from_le_bytes(
            bytes[offset..offset + 8]
                .try_into()
                .expect("verified SDK info offset"),
        )
    };
    // Verified against the installed Linux P2spTask::GetTaskInfo ABI and an
    // isolated loopback transfer. 1 = source, 2 = peer, 0x50 = offline/DCDN.
    let source_received = number(104);
    let peer_received = number(120).saturating_add(number(136));
    let native_total = number(16);
    if native_total > 0 {
        progress.total = native_total;
    }
    progress.completed = progress.completed.max(number(24));
    progress.received = source_received.saturating_add(peer_received);
    progress.peer_received = peer_received;
    progress.error_code = i32::from_le_bytes(bytes[8..12].try_into().unwrap());
    progress
}

fn worker_state_directory(socket: &Path) -> Result<PathBuf, ()> {
    let directory = socket.parent().ok_or(())?.join("axdl-state");
    fs::create_dir(&directory).map_err(|_| ())?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(|_| ())?;
    Ok(directory)
}

fn write_private_phub_setting(directory: &Path) -> Result<(), ()> {
    // The installed Linux SDK leaves UAGC PHUB disabled unless this private
    // setting is present. It must live under our worker's isolated state,
    // never in the official client's profile.
    const SETTINGS: &str = r#"{"content":{"xluagc_phub":{"switch":true}}}"#;
    let encoded = base64::engine::general_purpose::STANDARD.encode(SETTINGS);
    fs::write(directory.join("setting.cfg"), encoded).map_err(|_| ())
}

fn parse_arguments(arguments: impl Iterator<Item = OsString>) -> Result<WorkerArguments, ()> {
    let mut values = arguments;
    let mut socket = None;
    let mut runtime_root = None;
    let mut runtime_version = None;
    let mut sdk = None;
    let mut stat = None;
    let mut node = None;
    let mut addon = None;
    let mut node_sha256 = None;
    let mut stat_sha256 = None;
    let mut sdk_sha256 = None;
    let mut addon_sha256 = None;
    while let Some(flag) = values.next() {
        let value = values.next().ok_or(())?;
        match flag.to_str() {
            Some("--socket") => socket = Some(PathBuf::from(value)),
            Some("--runtime-root") => runtime_root = Some(PathBuf::from(value)),
            Some("--runtime-version") => runtime_version = value.into_string().ok(),
            Some("--sdk") => sdk = Some(PathBuf::from(value)),
            Some("--stat") => stat = Some(PathBuf::from(value)),
            Some("--node") => node = Some(PathBuf::from(value)),
            Some("--addon") => addon = Some(PathBuf::from(value)),
            Some("--node-sha256") => node_sha256 = value.into_string().ok(),
            Some("--stat-sha256") => stat_sha256 = value.into_string().ok(),
            Some("--sdk-sha256") => sdk_sha256 = value.into_string().ok(),
            Some("--addon-sha256") => addon_sha256 = value.into_string().ok(),
            _ => return Err(()),
        }
    }
    Ok(WorkerArguments {
        socket: socket.ok_or(())?,
        _runtime_root: runtime_root.ok_or(())?,
        runtime_version: runtime_version.ok_or(())?,
        sdk: sdk.ok_or(())?,
        stat: stat.ok_or(())?,
        node: node.ok_or(())?,
        addon: addon.ok_or(())?,
        node_sha256: node_sha256.ok_or(())?,
        stat_sha256: stat_sha256.ok_or(())?,
        sdk_sha256: sdk_sha256.ok_or(())?,
        addon_sha256: addon_sha256.ok_or(())?,
    })
}

fn read_nonce() -> Result<String, ()> {
    let mut nonce = String::new();
    std::io::stdin()
        .read_to_string(&mut nonce)
        .map_err(|_| ())?;
    (!nonce.is_empty() && nonce.len() <= 128)
        .then_some(nonce)
        .ok_or(())
}

#[cfg(test)]
mod tests {
    use super::{
        progress_from_output, progress_with_axdl_info, run_stream, run_stream_with_axdl,
        WorkerAxdlBridge, WorkerTask,
    };
    use crate::xunlei_native::linux::protocol::{
        read_frame, write_frame, WorkerErrorCode, WorkerHello, WorkerInitialization, WorkerRequest,
        WorkerResponse, WorkerSubmission,
    };
    use crate::xunlei_native::linux::{axdl::AxdlRuntime, runtime::discover_best_runtime};
    use crate::{
        cloud::{
            xunlei::{xunlei_native_identity_markers, XunleiClient, XunleiDownload, XunleiSession},
            CloudHttpRequest, CloudHttpResponse, CloudHttpTransport, CredentialStore,
            ReqwestTransport, SystemCredentialStore,
        },
        xunlei_native::{PEER_ID_MARKER_HEADER, USER_ID_MARKER_HEADER},
    };
    use base64::Engine as _;
    use sha1::{Digest as _, Sha1};
    use std::{
        ffi::c_void,
        fs,
        io::{BufRead, Read, Write},
        net::TcpListener,
        os::unix::net::{UnixListener, UnixStream},
        path::PathBuf,
        process::{Command, Stdio},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread,
        time::{Duration, Instant},
    };

    extern "C" {
        fn fflush(stream: *mut c_void) -> i32;
    }

    #[test]
    fn worker_progress_uses_the_active_output_file_without_exposing_sdk_layout() {
        let output = tempfile::tempdir().unwrap();
        let path = output.path().join("payload.bin");
        fs::write(&path, [1_u8; 9]).unwrap();
        let progress = progress_from_output(&WorkerTask {
            output_path: path,
            total_length: Some(9),
            paused: false,
        });
        assert_eq!(progress.completed, 9);
        assert_eq!(progress.total, 9);
        assert_eq!(progress.status, 7);
    }

    #[test]
    fn private_sdk_state_enables_phub_for_member_resource_queries() {
        let state = tempfile::tempdir().unwrap();
        super::write_private_phub_setting(state.path()).unwrap();
        let encoded = fs::read(state.path().join("setting.cfg")).unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let settings: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(settings["content"]["xluagc_phub"]["switch"], true);
    }

    #[test]
    fn linux_axdl_progress_counts_source_peer_and_offline_bytes_separately() {
        let output = tempfile::tempdir().unwrap();
        let path = output.path().join("payload.bin");
        fs::write(&path, [1_u8; 60]).unwrap();
        let task = WorkerTask {
            output_path: path,
            total_length: Some(100),
            paused: false,
        };
        let mut info = [0_u8; 200];
        info[..4].copy_from_slice(&200_u32.to_le_bytes());
        info[16..24].copy_from_slice(&100_u64.to_le_bytes());
        info[24..32].copy_from_slice(&70_u64.to_le_bytes());
        info[104..112].copy_from_slice(&10_u64.to_le_bytes());
        info[120..128].copy_from_slice(&35_u64.to_le_bytes());
        info[136..144].copy_from_slice(&25_u64.to_le_bytes());
        let progress = progress_with_axdl_info(&task, Some(&(0, info)));
        assert_eq!(progress.completed, 70);
        assert_eq!(progress.total, 100);
        assert_eq!(progress.received, 70);
        assert_eq!(progress.peer_received, 60);
        assert_eq!(progress.status, 3);
    }

    #[test]
    fn worker_loop_requires_initialization_and_shuts_down_cleanly() {
        let (mut parent, worker_stream) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || run_stream(worker_stream, "nonce".into()));

        let hello: WorkerHello = read_frame(&mut parent).unwrap();
        assert_eq!(hello.nonce(), "nonce");

        let submission = WorkerRequest::Submit(WorkerSubmission {
            url: "https://example.test/file".into(),
            headers: vec![],
            output_path: "/tmp".into(),
            output_name: "file".into(),
            acceleration_token: None,
            cid: None,
            gcid: None,
            total_length: None,
        });
        write_frame(&mut parent, &submission).unwrap();
        assert!(matches!(
            read_frame::<WorkerResponse>(&mut parent).unwrap(),
            WorkerResponse::Error {
                code: WorkerErrorCode::OperationFailed
            }
        ));

        write_frame(
            &mut parent,
            &WorkerRequest::Initialize(WorkerInitialization {
                user_id: "user-secret".into(),
                peer_id: "peer-secret".into(),
            }),
        )
        .unwrap();
        assert!(matches!(
            read_frame::<WorkerResponse>(&mut parent).unwrap(),
            WorkerResponse::Acknowledged
        ));

        write_frame(&mut parent, &submission).unwrap();
        assert!(matches!(
            read_frame::<WorkerResponse>(&mut parent).unwrap(),
            WorkerResponse::Error {
                code: WorkerErrorCode::Incompatible
            }
        ));

        write_frame(&mut parent, &WorkerRequest::Shutdown).unwrap();
        assert!(matches!(
            read_frame::<WorkerResponse>(&mut parent).unwrap(),
            WorkerResponse::Acknowledged
        ));
        assert_eq!(worker.join().unwrap(), Ok(()));
    }

    #[test]
    #[ignore = "requires the supported local DEB or Linyaps Xunlei runtime"]
    fn supported_runtime_worker_protocol_writes_direct_link_bytes() {
        const NONCE: &str = "axdl-worker-direct-test";
        if std::env::var_os("ALLDOWNLOADER_AXDL_WORKER_TEST_CHILD").is_some() {
            let output = PathBuf::from(
                std::env::var_os("ALLDOWNLOADER_AXDL_WORKER_TEST_DATA")
                    .expect("isolated AXDL output path"),
            );
            let socket = PathBuf::from(
                std::env::var_os("ALLDOWNLOADER_AXDL_WORKER_TEST_SOCKET")
                    .expect("worker socket path"),
            );
            let state_directory = output.join("state");
            fs::create_dir(&state_directory).expect("isolated AXDL state");
            super::write_private_phub_setting(&state_directory).expect("private PHUB setting");
            let runtime = discover_best_runtime().expect("installed runtime");
            let axdl = AxdlRuntime::load(&runtime).expect("supported AXDL runtime");
            let stream = UnixStream::connect(socket).expect("parent worker socket");
            run_stream_with_axdl(
                stream,
                NONCE.into(),
                Some(WorkerAxdlBridge {
                    axdl,
                    client_version: runtime.version,
                    state_directory,
                }),
            )
            .expect("AXDL worker protocol");
            // The vendor runtime owns native threads. This child only exists
            // to exercise the real worker protocol, so skip unsafe dlclose.
            unsafe { super::_exit(0) };
        }

        let output = tempfile::tempdir().expect("temporary AXDL output");
        let socket = output.path().join("worker.sock");
        let worker_listener = UnixListener::bind(&socket).expect("worker socket listener");
        let http_listener = TcpListener::bind("127.0.0.1:0").expect("local HTTP listener");
        http_listener
            .set_nonblocking(true)
            .expect("nonblocking local HTTP listener");
        let address = http_listener.local_addr().expect("local HTTP address");
        let keep_serving = Arc::new(AtomicBool::new(true));
        let stop_server = Arc::clone(&keep_serving);
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            while stop_server.load(Ordering::Relaxed) && Instant::now() < deadline {
                match http_listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0_u8; 4096];
                        let _ = stream.read(&mut request);
                        let body = vec![0x5a_u8; 64 * 1024];
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&body);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return,
                }
            }
        });

        let test_binary = std::env::current_exe().expect("current test binary");
        let mut child = Command::new(test_binary)
            .args([
                "--exact",
                "xunlei_native::linux::worker::tests::supported_runtime_worker_protocol_writes_direct_link_bytes",
                "--ignored",
            ])
            .env("ALLDOWNLOADER_AXDL_WORKER_TEST_CHILD", "1")
            .env("ALLDOWNLOADER_AXDL_WORKER_TEST_DATA", output.path())
            .env("ALLDOWNLOADER_AXDL_WORKER_TEST_SOCKET", &socket)
            .spawn()
            .expect("spawn isolated AXDL worker");
        let (mut stream, _) = worker_listener.accept().expect("worker connection");
        let hello: WorkerHello = read_frame(&mut stream).expect("worker hello");
        assert_eq!(hello.nonce(), NONCE);

        write_frame(
            &mut stream,
            &WorkerRequest::Initialize(WorkerInitialization {
                // This direct-link fixture deliberately validates the native
                // protocol without consuming a real account credential.
                user_id: "0".into(),
                peer_id: "0000000000000000000000000000000000000000".into(),
            }),
        )
        .expect("initialize request");
        assert!(matches!(
            read_frame::<WorkerResponse>(&mut stream).expect("initialize response"),
            WorkerResponse::Acknowledged
        ));

        let target = output.path().join("payload.bin");
        write_frame(
            &mut stream,
            &WorkerRequest::Submit(WorkerSubmission {
                url: format!("http://{address}/payload.bin"),
                headers: vec![],
                output_path: target.to_string_lossy().into_owned(),
                output_name: "payload.bin".into(),
                acceleration_token: None,
                cid: None,
                gcid: None,
                total_length: Some(64 * 1024),
            }),
        )
        .expect("submit request");
        let task_id = match read_frame::<WorkerResponse>(&mut stream).expect("submit response") {
            WorkerResponse::Submitted { task_id } => task_id,
            response => panic!("unexpected submit response: {response:?}"),
        };

        let deadline = Instant::now() + Duration::from_secs(10);
        while std::fs::metadata(&target)
            .map(|metadata| metadata.len())
            .unwrap_or(0)
            != 64 * 1024
            && Instant::now() < deadline
        {
            write_frame(&mut stream, &WorkerRequest::Poll { task_id }).expect("poll request");
            assert!(matches!(
                read_frame::<WorkerResponse>(&mut stream).expect("poll response"),
                WorkerResponse::Progress(_)
            ));
            thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            fs::metadata(&target).expect("direct task output").len(),
            64 * 1024
        );

        write_frame(&mut stream, &WorkerRequest::Release { task_id }).expect("release request");
        assert!(matches!(
            read_frame::<WorkerResponse>(&mut stream).expect("release response"),
            WorkerResponse::Acknowledged
        ));
        write_frame(&mut stream, &WorkerRequest::Shutdown).expect("shutdown request");
        assert!(matches!(
            read_frame::<WorkerResponse>(&mut stream).expect("shutdown response"),
            WorkerResponse::Acknowledged
        ));
        assert!(child.wait().expect("wait worker").success());
        keep_serving.store(false, Ordering::Relaxed);
        server.join().expect("join local HTTP server");
    }

    #[test]
    #[ignore = "reads the current user's Xunlei session from the system credential store"]
    fn saved_xunlei_session_has_native_bridge_identity() {
        let root = PathBuf::from(
            std::env::var_os("ALLDOWNLOADER_LIVE_XUNLEI_SESSION_ROOT")
                .expect("explicit cloud-session directory"),
        );
        let store = SystemCredentialStore::new(root).expect("system credential store");
        let saved = store
            .load("xunlei")
            .expect("read Xunlei session")
            .expect("saved Xunlei session");
        let session: XunleiSession = serde_json::from_slice(&saved).expect("valid Xunlei session");
        let markers = xunlei_native_identity_markers(&session);
        assert!(
            markers
                .iter()
                .any(|(name, value)| { name == USER_ID_MARKER_HEADER && !value.trim().is_empty() }),
            "saved session has no native Xunlei user ID"
        );
        assert!(
            markers
                .iter()
                .any(|(name, value)| { name == PEER_ID_MARKER_HEADER && !value.trim().is_empty() }),
            "saved session has no native Xunlei peer ID"
        );
    }

    fn resolve_xunlei_file_from_task(large: bool) -> (XunleiSession, XunleiDownload, PathBuf) {
        struct DiagnosticHttp(ReqwestTransport);
        impl CloudHttpTransport for DiagnosticHttp {
            fn send(
                &self,
                request: CloudHttpRequest,
            ) -> Result<CloudHttpResponse, crate::cloud::QuarkError> {
                let method = request.method;
                let path = url::Url::parse(&request.url)
                    .map(|url| {
                        if url.path().starts_with("/drive/v1/files/") {
                            "/drive/v1/files/{id}".to_owned()
                        } else {
                            url.path().to_owned()
                        }
                    })
                    .unwrap_or_default();
                let response = self.0.send(request)?;
                let code = serde_json::from_str::<serde_json::Value>(&response.body)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("error_code")
                            .or_else(|| value.get("code"))
                            .and_then(serde_json::Value::as_i64)
                    });
                eprintln!(
                    "Xunlei live request {method} {path}: HTTP {}, code {:?}",
                    response.status, code
                );
                Ok(response)
            }
        }
        let max_fixture_bytes: u64 = if large {
            500 * 1024 * 1024
        } else {
            2 * 1024 * 1024
        };
        let root = PathBuf::from(
            std::env::var_os("ALLDOWNLOADER_LIVE_XUNLEI_SESSION_ROOT")
                .expect("explicit cloud-session directory"),
        );
        let store = SystemCredentialStore::new(root).expect("system credential store");
        let saved = store
            .load("xunlei")
            .expect("read Xunlei session")
            .expect("saved Xunlei session");
        let session: XunleiSession = serde_json::from_slice(&saved).expect("valid Xunlei session");
        eprintln!(
            "Xunlei saved session: modern={}, version={}, web_version_supported={}, captured_signature_matches_known_salts={}, access_present={}, access_fresh={}, refresh_present={}, ticket_present={}, ticket_fresh={}",
            session.profile.is_modern_web(),
            session.profile.client_version,
            session.profile.web_captcha_signature("1234567890000").is_some(),
            session.profile.captured_web_signature_matches_known_salts(&session.web_security.timestamp, &session.web_security.captcha_sign),
            !session.access_token.is_empty(),
            session.expires_at > std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
            !session.refresh_token.is_empty(),
            !session.web_security.captcha_token.is_empty(),
            session.web_security.expires_at > std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
        );
        let http = DiagnosticHttp(ReqwestTransport::new().expect("Xunlei HTTP client"));
        let client = XunleiClient::new(session, &http);
        if large {
            let candidate_size = std::env::var("ALLDOWNLOADER_AXDL_REAL_VIP_CLOUD_SIZE")
                .ok()
                .and_then(|value| value.parse::<u64>().ok());
            if let Some(candidate_size) = candidate_size {
                let mut pending = vec![("0".to_owned(), 0_u8)];
                while let Some((parent, depth)) = pending.pop() {
                    let directory = client.directory(&parent).expect("read Xunlei directory");
                    for item in directory.items {
                        if item.kind.as_str() == "folder" && depth < 2 {
                            pending.push((item.id, depth + 1));
                        } else if item.size == Some(candidate_size) {
                            let download = client
                                .download_with_acceleration(&item.id)
                                .expect("resolve existing Xunlei candidate");
                            return (client.session(), download, PathBuf::new());
                        }
                    }
                }
                panic!("no existing Xunlei cloud candidate matches the requested size");
            }
        }
        let task_db = PathBuf::from(
            std::env::var_os("ALLDOWNLOADER_LIVE_XUNLEI_TASK_DB")
                .expect("explicit task database path"),
        );
        let database = rusqlite::Connection::open_with_flags(
            task_db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("read-only task database");
        let sql = if large {
            "SELECT t.source, f.path FROM tasks t JOIN task_files f ON f.task_id = t.id WHERE t.kind = 'cloud' AND t.status = 'downloading' AND t.source LIKE 'cloud://xunlei/%' AND t.total_length BETWEEN 8000000 AND ?1 ORDER BY t.total_length DESC LIMIT 1"
        } else {
            "SELECT t.source, f.path FROM tasks t JOIN task_files f ON f.task_id = t.id WHERE t.kind = 'cloud' AND t.status = 'completed' AND t.source LIKE 'cloud://xunlei/%' AND t.total_length BETWEEN 1 AND ?1 ORDER BY t.created_at DESC LIMIT 1"
        };
        let (source, original_path): (String, String) = database
            .query_row(sql, [max_fixture_bytes], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("a bounded Xunlei task fixture");
        let file_id = source
            .strip_prefix("cloud://xunlei/")
            .expect("stable Xunlei task source");
        let file_id = url::form_urlencoded::parse(format!("id={file_id}").as_bytes())
            .next()
            .expect("encoded file ID")
            .1
            .into_owned();
        let download = client
            .download_with_acceleration(&file_id)
            .expect("resolve Xunlei cloud download");
        (client.session(), download, PathBuf::from(original_path))
    }

    fn resolve_existing_small_xunlei_file() -> (XunleiSession, XunleiDownload, PathBuf) {
        resolve_xunlei_file_from_task(false)
    }

    fn file_gcid(path: &std::path::Path, size: u64) -> String {
        let mut block = 0x40000_u64;
        while size > block * 0x200 && block < 0x200000 {
            block <<= 1;
        }
        let mut file = fs::File::open(path).expect("native cloud output");
        let mut total = Sha1::new();
        let mut remaining = size;
        while remaining > 0 {
            let mut bytes = vec![0; remaining.min(block) as usize];
            file.read_exact(&mut bytes)
                .expect("complete native cloud block");
            total.update(Sha1::digest(&bytes));
            remaining -= bytes.len() as u64;
        }
        total
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect()
    }

    #[test]
    #[ignore = "uses the saved Xunlei session to resolve an already downloaded small cloud file"]
    fn saved_xunlei_session_resolves_a_short_lived_svip_certificate() {
        let (_, download, _) = resolve_existing_small_xunlei_file();
        assert!(download.url.starts_with("https://"));
        assert!(
            download
                .acceleration_token
                .is_some_and(|token| !token.is_empty()),
            "Xunlei did not issue an SVIP acceleration certificate for the bounded fixture"
        );
    }

    #[test]
    #[ignore = "downloads a bounded existing Xunlei cloud file through the native Linux worker"]
    fn saved_xunlei_session_downloads_with_native_vip_certificate() {
        const NONCE: &str = "axdl-real-vip-test";
        if std::env::var_os("ALLDOWNLOADER_AXDL_REAL_VIP_TEST_CHILD").is_some() {
            let output = PathBuf::from(
                std::env::var_os("ALLDOWNLOADER_AXDL_REAL_VIP_TEST_DATA")
                    .expect("isolated AXDL output path"),
            );
            let socket = PathBuf::from(
                std::env::var_os("ALLDOWNLOADER_AXDL_REAL_VIP_TEST_SOCKET")
                    .expect("worker socket path"),
            );
            let state_directory = output.join("state");
            fs::create_dir(&state_directory).expect("isolated AXDL state");
            super::write_private_phub_setting(&state_directory).expect("private PHUB setting");
            let runtime = discover_best_runtime().expect("installed runtime");
            let axdl = AxdlRuntime::load(&runtime).expect("supported AXDL runtime");
            let stream = UnixStream::connect(socket).expect("parent worker socket");
            run_stream_with_axdl(
                stream,
                NONCE.into(),
                Some(WorkerAxdlBridge {
                    axdl,
                    client_version: runtime.version,
                    state_directory,
                }),
            )
            .expect("AXDL worker protocol");
            unsafe { fflush(std::ptr::null_mut()) };
            unsafe { super::_exit(0) };
        }

        let large = std::env::var_os("ALLDOWNLOADER_AXDL_REAL_VIP_TEST_LARGE").is_some();
        let full = std::env::var_os("ALLDOWNLOADER_AXDL_REAL_VIP_FULL").is_some();
        assert!(
            !full || large,
            "full transfer requires a large cloud fixture"
        );
        let without_certificate =
            std::env::var_os("ALLDOWNLOADER_AXDL_REAL_VIP_WITHOUT_CERT").is_some();
        let large_probe_bytes = std::env::var("ALLDOWNLOADER_AXDL_REAL_VIP_PROBE_BYTES")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(8 * 1024 * 1024);
        let large_probe_seconds = std::env::var("ALLDOWNLOADER_AXDL_REAL_VIP_PROBE_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(90);
        let (session, download, original_path) = resolve_xunlei_file_from_task(large);
        let certificate = download
            .acceleration_token
            .filter(|token| !token.is_empty())
            .expect("nonempty Xunlei acceleration certificate");
        let expected = if large {
            Vec::new()
        } else {
            fs::read(original_path).expect("existing bounded download bytes")
        };
        if !large {
            assert!(!expected.is_empty() && expected.len() <= 2 * 1024 * 1024);
        }
        let total_length = download.size.unwrap_or(expected.len() as u64);
        let expected_gcid = download.gcid.clone();
        let markers = xunlei_native_identity_markers(&session);
        let marker = |name| {
            markers
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, value)| value.clone())
                .expect("native identity marker")
        };
        let native_peer = super::local_client_peer_id().expect("Linux client peer identity");
        eprintln!(
            "native peer identity: length={}, forty_hex={}",
            native_peer.len(),
            native_peer.len() == 40 && native_peer.bytes().all(|byte| byte.is_ascii_hexdigit())
        );
        let certificate_claims = certificate
            .split('.')
            .nth(1)
            .and_then(|part| {
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(part)
                    .ok()
            })
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .unwrap_or_default();
        let cert_peer = certificate_claims["peer_id"].as_str().unwrap_or("");
        eprintln!(
            "native certificate binding: peer_claim_present={}, peer_claim_type={}, peer_matches_native={}, peer_matches_web_device={}, resource_type={}, gcid_present={}, resource_matches_gcid={}, vip_type={:?}, phub_level={:?}, dcdnhub_level={:?}",
            !cert_peer.is_empty(),
            if certificate_claims["peer_id"].is_string() { "string" } else if certificate_claims["peer_id"].is_number() { "number" } else if certificate_claims["peer_id"].is_object() { "object" } else { "other" },
            !cert_peer.is_empty() && cert_peer == marker(PEER_ID_MARKER_HEADER),
            !cert_peer.is_empty() && cert_peer == session.profile.device_id,
            certificate_claims["res_type"].as_str().unwrap_or("missing"),
            download.gcid.is_some(),
            download.gcid.as_deref().is_some_and(|gcid| certificate_claims["res_value"].as_str().is_some_and(|resource| resource.eq_ignore_ascii_case(gcid))),
            certificate_claims["vip_type"].as_i64(),
            certificate_claims["phub_level"].as_i64(),
            certificate_claims["dcdnhub_level"].as_i64(),
        );

        let output = tempfile::tempdir().expect("temporary AXDL output");
        let socket = output.path().join("worker.sock");
        let listener = UnixListener::bind(&socket).expect("worker socket listener");
        let executable = std::env::current_exe().expect("current test binary");
        let mut child = Command::new(executable)
            .args([
                "--exact",
                "xunlei_native::linux::worker::tests::saved_xunlei_session_downloads_with_native_vip_certificate",
                "--ignored",
                "--nocapture",
            ])
            .env("ALLDOWNLOADER_AXDL_REAL_VIP_TEST_CHILD", "1")
            .env("ALLDOWNLOADER_AXDL_REAL_VIP_TEST_DATA", output.path())
            .env("ALLDOWNLOADER_AXDL_REAL_VIP_TEST_SOCKET", &socket)
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn isolated AXDL worker");
        let vendor_stdout = child.stdout.take().expect("isolated SDK stdout");
        let vendor_events = thread::spawn(move || {
            std::io::BufReader::new(vendor_stdout)
                .lines()
                .filter_map(Result::ok)
                .filter_map(|line| {
                    line.split("OnExecute ")
                        .nth(1)
                        .and_then(|part| part.split_whitespace().next())
                        .and_then(|part| part.parse::<u32>().ok())
                })
                .collect::<Vec<_>>()
        });
        let (mut stream, _) = listener.accept().expect("worker connection");
        stream
            .set_read_timeout(Some(Duration::from_secs(45)))
            .expect("bounded worker response timeout");
        let hello: WorkerHello = read_frame(&mut stream).expect("worker hello");
        assert_eq!(hello.nonce(), NONCE);
        write_frame(
            &mut stream,
            &WorkerRequest::Initialize(WorkerInitialization {
                user_id: marker(USER_ID_MARKER_HEADER),
                peer_id: marker(PEER_ID_MARKER_HEADER),
            }),
        )
        .expect("initialize request");
        assert!(matches!(
            read_frame::<WorkerResponse>(&mut stream).expect("initialize response"),
            WorkerResponse::Acknowledged
        ));

        let target = output.path().join("payload.bin");
        write_frame(
            &mut stream,
            &WorkerRequest::Submit(WorkerSubmission {
                url: download.url,
                headers: vec![("Referer".into(), "https://pan.xunlei.com/".into())],
                output_path: target.to_string_lossy().into_owned(),
                output_name: "payload.bin".into(),
                acceleration_token: (!without_certificate).then_some(certificate),
                cid: download.cid,
                gcid: download.gcid,
                total_length: Some(total_length),
            }),
        )
        .expect("submit request");
        let task_id = match read_frame::<WorkerResponse>(&mut stream).expect("submit response") {
            WorkerResponse::Submitted { task_id } => task_id,
            response => panic!("unexpected submit response: {response:?}"),
        };
        let deadline = Instant::now()
            + Duration::from_secs(if full {
                30 * 60
            } else if large {
                large_probe_seconds
            } else {
                30
            });
        let mut latest_progress = None;
        let mut last_reported_mib = 0;
        while Instant::now() < deadline {
            write_frame(&mut stream, &WorkerRequest::Poll { task_id }).expect("poll request");
            let WorkerResponse::Progress(progress) =
                read_frame(&mut stream).expect("poll response")
            else {
                panic!("native poll response missing");
            };
            latest_progress = Some(progress);
            if full && progress.completed / (16 * 1024 * 1024) > last_reported_mib {
                last_reported_mib = progress.completed / (16 * 1024 * 1024);
                eprintln!(
                    "full native progress: completed={}, total={}, received={}, peer_received={}, status={}, error={}",
                    progress.completed, progress.total, progress.received,
                    progress.peer_received, progress.status, progress.error_code
                );
            }
            if (full && progress.status == 7 && progress.completed >= total_length)
                || (large && !full && progress.received >= large_probe_bytes)
                || (!large && fs::metadata(&target).is_ok_and(|m| m.len() >= expected.len() as u64))
            {
                break;
            }
            if progress.status == 8 {
                break;
            }
            thread::sleep(Duration::from_millis(if full { 500 } else { 100 }));
        }
        if !large {
            let actual = fs::read(&target).expect("native cloud output");
            assert_eq!(
                actual, expected,
                "native cloud output differs from the existing file"
            );
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let native_progress = loop {
            write_frame(&mut stream, &WorkerRequest::Poll { task_id }).expect("native stats poll");
            let WorkerResponse::Progress(progress) = read_frame(&mut stream).expect("native stats")
            else {
                panic!("native stats response missing");
            };
            if progress.received > 0 || Instant::now() >= deadline {
                break progress;
            }
            thread::sleep(Duration::from_millis(100));
        };
        eprintln!(
            "native cloud stats: received={}, peer_received={}",
            native_progress.received, native_progress.peer_received
        );
        if large {
            eprintln!(
                "large native cloud probe: completed={}, received={}, status={}, error={}",
                latest_progress.unwrap().completed,
                latest_progress.unwrap().received,
                latest_progress.unwrap().status,
                latest_progress.unwrap().error_code,
            );
        }
        assert!(
            native_progress.received > 0,
            "AXDL did not report actual transfer bytes"
        );
        write_frame(&mut stream, &WorkerRequest::Release { task_id }).expect("release request");
        assert!(matches!(
            read_frame::<WorkerResponse>(&mut stream).expect("release response"),
            WorkerResponse::Acknowledged
        ));
        write_frame(&mut stream, &WorkerRequest::Shutdown).expect("shutdown request");
        assert!(matches!(
            read_frame::<WorkerResponse>(&mut stream).expect("shutdown response"),
            WorkerResponse::Acknowledged
        ));
        let child_status = child.wait().expect("wait worker");
        let events = vendor_events.join().expect("vendor log reader");
        assert!(child_status.success(), "isolated AXDL worker failed");
        if large && !full {
            assert!(
                latest_progress.expect("large native progress").received >= large_probe_bytes,
                "large native transfer did not reach its configured byte target"
            );
        }
        if full {
            let final_progress = latest_progress.expect("full native progress");
            assert_eq!(final_progress.status, 7, "native task did not complete");
            assert_eq!(final_progress.completed, total_length);
            assert_eq!(
                fs::metadata(&target).expect("full native output").len(),
                total_length
            );
            let expected_gcid = expected_gcid.expect("cloud fixture GCID");
            assert_eq!(
                file_gcid(&target, total_length),
                expected_gcid.to_uppercase()
            );
            eprintln!("full native cloud GCID verified for {total_length} bytes");
        }
        let command_count = |id: u32| events.iter().filter(|kind| **kind == id).count();
        let start_position = events
            .iter()
            .position(|kind| *kind == 18)
            .expect("SDK start command was not processed");
        if without_certificate {
            assert_eq!(command_count(34), 0, "unexpected SDK certificate command");
        } else {
            let certificate_position = events
                .iter()
                .position(|kind| *kind == 34)
                .expect("SDK certificate command was not processed");
            assert!(
                start_position < certificate_position,
                "the certificate was processed before the task entered its running state"
            );
        }
        eprintln!(
            "isolated SDK calls: create={}, cert={}, start={}, query={}",
            command_count(9),
            command_count(34),
            command_count(18),
            command_count(22)
        );
    }
}
