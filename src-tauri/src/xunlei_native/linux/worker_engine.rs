use super::{
    protocol::{
        read_frame, write_frame, WorkerAction, WorkerErrorCode, WorkerHello, WorkerInitialization,
        WorkerProgress, WorkerRequest, WorkerResponse, WorkerSubmission, PROTOCOL_VERSION,
    },
    runtime::LinuxXunleiRuntime,
};
use crate::{
    xunlei_native::engine::{
        NativeEngine, NativeEngineError, NativeEngineSession, NativeProgress, NativeSubmission,
        NativeTaskHandle,
    },
    DownloadAction,
};
use rand::RngCore;
use std::{
    fs,
    io::Write,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    time::{Duration, Instant},
};

pub(crate) trait WorkerTransport {
    fn request(&mut self, request: WorkerRequest) -> Result<WorkerResponse, NativeEngineError>;
    fn is_alive(&mut self) -> bool;
    fn shutdown(&mut self);
}

pub(crate) struct LinuxWorkerEngine<T = SocketWorkerTransport> {
    session: NativeEngineSession,
    transport: T,
    live_handles: Vec<NativeTaskHandle>,
    stopped_handles: Vec<NativeTaskHandle>,
}

impl LinuxWorkerEngine<SocketWorkerTransport> {
    pub(super) fn start(
        runtime: LinuxXunleiRuntime,
        session: NativeEngineSession,
    ) -> Result<Self, NativeEngineError> {
        Ok(Self {
            session,
            transport: SocketWorkerTransport::start(runtime)?,
            live_handles: Vec::new(),
            stopped_handles: Vec::new(),
        })
    }
}

impl<T: WorkerTransport> LinuxWorkerEngine<T> {
    #[cfg(test)]
    fn from_transport_for_tests(session: NativeEngineSession, transport: T) -> Self {
        Self {
            session,
            transport,
            live_handles: Vec::new(),
            stopped_handles: Vec::new(),
        }
    }

    #[cfg(test)]
    fn into_transport_for_tests(self) -> T {
        self.transport
    }

    #[cfg(test)]
    fn take_stopped_handles(&mut self) -> Vec<NativeTaskHandle> {
        std::mem::take(&mut self.stopped_handles)
    }

    fn worker_stopped(&mut self) -> NativeEngineError {
        self.stopped_handles.append(&mut self.live_handles);
        NativeEngineError::WorkerStopped
    }

    fn request(&mut self, request: WorkerRequest) -> Result<WorkerResponse, NativeEngineError> {
        if !self.transport.is_alive() {
            return Err(self.worker_stopped());
        }
        self.transport
            .request(request)
            .map_err(|error| match error {
                NativeEngineError::WorkerStopped => self.worker_stopped(),
                other => other,
            })
    }

    fn response_acknowledged(&mut self, request: WorkerRequest) -> Result<(), NativeEngineError> {
        match self.request(request)? {
            WorkerResponse::Acknowledged => Ok(()),
            WorkerResponse::Error { code } => Err(map_worker_error(code)),
            _ => Err(NativeEngineError::OperationFailed),
        }
    }

    fn is_live_handle(&self, handle: NativeTaskHandle) -> bool {
        self.session.owns(handle) && self.live_handles.contains(&handle)
    }

    #[cfg(test)]
    fn shutdown(&mut self) {
        let _ = self.response_acknowledged(WorkerRequest::Shutdown);
        self.transport.shutdown();
        self.live_handles.clear();
    }
}

impl<T: WorkerTransport> NativeEngine for LinuxWorkerEngine<T> {
    fn submit(&mut self, request: NativeSubmission) -> Result<NativeTaskHandle, NativeEngineError> {
        self.response_acknowledged(WorkerRequest::Initialize(WorkerInitialization {
            user_id: request.user_id,
            peer_id: request.peer_id,
        }))?;
        let response = self.request(WorkerRequest::Submit(WorkerSubmission {
            url: request.url,
            headers: request.headers,
            output_path: request.output_path.to_string_lossy().into_owned(),
            output_name: request.output_name,
            acceleration_token: request.acceleration_token,
            cid: request.cid,
            gcid: request.gcid,
            total_length: request.total_length,
        }))?;
        let WorkerResponse::Submitted { task_id } = response else {
            return match response {
                WorkerResponse::Error { code } => Err(map_worker_error(code)),
                _ => Err(NativeEngineError::OperationFailed),
            };
        };
        let handle = self
            .session
            .allocate_handle(task_id)
            .ok_or(NativeEngineError::OperationFailed)?;
        self.live_handles.push(handle);
        Ok(handle)
    }

    fn poll(&mut self, handle: NativeTaskHandle) -> Result<NativeProgress, NativeEngineError> {
        if !self.is_live_handle(handle) {
            return Err(NativeEngineError::WorkerStopped);
        }
        match self.request(WorkerRequest::Poll {
            task_id: handle.task_id,
        })? {
            WorkerResponse::Progress(progress) => Ok(native_progress(progress)),
            WorkerResponse::Error { code } => Err(map_worker_error(code)),
            _ => Err(NativeEngineError::OperationFailed),
        }
    }

    fn control(
        &mut self,
        handle: NativeTaskHandle,
        action: DownloadAction,
    ) -> Result<(), NativeEngineError> {
        if !self.is_live_handle(handle) {
            return Err(NativeEngineError::WorkerStopped);
        }
        self.response_acknowledged(WorkerRequest::Control {
            task_id: handle.task_id,
            action: match action {
                DownloadAction::Pause => WorkerAction::Pause,
                DownloadAction::Resume => WorkerAction::Resume,
                DownloadAction::Cancel => WorkerAction::Cancel,
            },
        })
    }

    fn release(&mut self, handle: NativeTaskHandle) -> Result<(), NativeEngineError> {
        if !self.is_live_handle(handle) {
            return Err(NativeEngineError::WorkerStopped);
        }
        self.response_acknowledged(WorkerRequest::Release {
            task_id: handle.task_id,
        })?;
        self.live_handles.retain(|candidate| *candidate != handle);
        Ok(())
    }
}

fn native_progress(progress: WorkerProgress) -> NativeProgress {
    NativeProgress {
        completed: progress.completed,
        total: progress.total,
        status: progress.status,
        error_code: progress.error_code,
        connections: progress.connections,
        received: progress.received,
        peer_received: progress.peer_received,
    }
}

fn map_worker_error(code: WorkerErrorCode) -> NativeEngineError {
    match code {
        WorkerErrorCode::Incompatible => NativeEngineError::Incompatible,
        WorkerErrorCode::TaskUnknown => NativeEngineError::TaskUnknown,
        WorkerErrorCode::OperationFailed => NativeEngineError::OperationFailed,
    }
}

pub(crate) struct SocketWorkerTransport {
    child: Child,
    stream: UnixStream,
    runtime_directory: PathBuf,
}

impl SocketWorkerTransport {
    fn start(runtime: LinuxXunleiRuntime) -> Result<Self, NativeEngineError> {
        let runtime_directory = create_runtime_directory()?;
        let socket = runtime_directory.join("worker.sock");
        let listener = UnixListener::bind(&socket).map_err(|_| NativeEngineError::Unavailable)?;
        set_mode(&socket, 0o600)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| NativeEngineError::Unavailable)?;

        let nonce = random_nonce();
        let executable = std::env::current_exe().map_err(|_| NativeEngineError::Unavailable)?;
        let mut child = Command::new(executable)
            .args([
                "--xunlei-native-worker",
                "--socket",
                socket.to_string_lossy().as_ref(),
                "--runtime-root",
                runtime.root.to_string_lossy().as_ref(),
                "--runtime-version",
                &runtime.version,
                "--sdk",
                runtime.sdk_library.to_string_lossy().as_ref(),
                "--stat",
                runtime.stat_library.to_string_lossy().as_ref(),
                "--node",
                runtime.node_library.to_string_lossy().as_ref(),
                "--addon",
                runtime.axdl_addon.to_string_lossy().as_ref(),
                "--stat-sha256",
                &runtime.stat_sha256,
                "--node-sha256",
                &runtime.node_sha256,
                "--sdk-sha256",
                &runtime.sdk_sha256,
                "--addon-sha256",
                &runtime.axdl_addon_sha256,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| NativeEngineError::Unavailable)?;
        if write_nonce(child.stdin.take(), &nonce).is_err() {
            cleanup_failed_start(&mut child, &runtime_directory);
            return Err(NativeEngineError::Unavailable);
        }
        let mut stream = match accept_worker(&listener, &mut child) {
            Ok(stream) => stream,
            Err(error) => {
                cleanup_failed_start(&mut child, &runtime_directory);
                return Err(error);
            }
        };
        let hello: WorkerHello = match read_frame(&mut stream) {
            Ok(hello) => hello,
            Err(_) => {
                cleanup_failed_start(&mut child, &runtime_directory);
                return Err(NativeEngineError::WorkerStopped);
            }
        };
        if !hello_is_authenticated(&hello, &nonce) {
            cleanup_failed_start(&mut child, &runtime_directory);
            return Err(NativeEngineError::Unavailable);
        }
        Ok(Self {
            child,
            stream,
            runtime_directory,
        })
    }
}

impl WorkerTransport for SocketWorkerTransport {
    fn request(&mut self, request: WorkerRequest) -> Result<WorkerResponse, NativeEngineError> {
        if !self.is_alive() {
            return Err(NativeEngineError::WorkerStopped);
        }
        write_frame(&mut self.stream, &request).map_err(|_| NativeEngineError::WorkerStopped)?;
        read_frame(&mut self.stream).map_err(|_| NativeEngineError::WorkerStopped)
    }

    fn is_alive(&mut self) -> bool {
        self.child.try_wait().is_ok_and(|status| status.is_none())
    }

    fn shutdown(&mut self) {
        if self.is_alive() {
            let _ = self
                .stream
                .set_read_timeout(Some(Duration::from_millis(250)));
            let _ = write_frame(&mut self.stream, &WorkerRequest::Shutdown);
            let _ = read_frame::<WorkerResponse>(&mut self.stream);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for SocketWorkerTransport {
    fn drop(&mut self) {
        self.shutdown();
        let _ = fs::remove_dir_all(&self.runtime_directory);
    }
}

fn create_runtime_directory() -> Result<PathBuf, NativeEngineError> {
    let directory = std::env::temp_dir().join(format!("alldownloader-xunlei-{}", random_nonce()));
    fs::create_dir(&directory).map_err(|_| NativeEngineError::Unavailable)?;
    set_mode(&directory, 0o700)?;
    Ok(directory)
}

fn set_mode(path: &std::path::Path, mode: u32) -> Result<(), NativeEngineError> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|_| NativeEngineError::Unavailable)
}

fn random_nonce() -> String {
    let mut bytes = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hello_is_authenticated(hello: &WorkerHello, expected_nonce: &str) -> bool {
    hello.protocol_version == PROTOCOL_VERSION && hello.nonce() == expected_nonce
}

fn write_nonce(mut stdin: Option<ChildStdin>, nonce: &str) -> Result<(), NativeEngineError> {
    let Some(stdin) = stdin.as_mut() else {
        return Err(NativeEngineError::Unavailable);
    };
    stdin
        .write_all(nonce.as_bytes())
        .map_err(|_| NativeEngineError::Unavailable)
}

fn cleanup_failed_start(child: &mut Child, runtime_directory: &std::path::Path) {
    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(runtime_directory);
}

fn accept_worker(
    listener: &UnixListener,
    child: &mut Child,
) -> Result<UnixStream, NativeEngineError> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => return Err(NativeEngineError::Unavailable),
        }
        if child.try_wait().is_ok_and(|status| status.is_some()) || Instant::now() >= deadline {
            return Err(NativeEngineError::Unavailable);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::{hello_is_authenticated, LinuxWorkerEngine, WorkerTransport};
    use crate::{
        xunlei_native::{
            engine::{NativeEngine, NativeEngineError, NativeEngineSession, NativeSubmission},
            linux::protocol::{
                WorkerAction, WorkerHello, WorkerProgress, WorkerRequest, WorkerResponse,
            },
        },
        DownloadAction,
    };
    use std::path::PathBuf;

    #[test]
    fn worker_hello_requires_the_expected_nonce_and_protocol_version() {
        assert!(hello_is_authenticated(
            &WorkerHello::new("nonce".into()),
            "nonce"
        ));
        assert!(!hello_is_authenticated(
            &WorkerHello::new("unexpected".into()),
            "nonce"
        ));
        let mut wrong_version = WorkerHello::new("nonce".into());
        wrong_version.protocol_version += 1;
        assert!(!hello_is_authenticated(&wrong_version, "nonce"));
    }

    #[derive(Default)]
    struct FakeWorker {
        requests: Vec<WorkerRequest>,
        alive: bool,
        crash_after_submit: bool,
    }

    impl WorkerTransport for FakeWorker {
        fn request(&mut self, request: WorkerRequest) -> Result<WorkerResponse, NativeEngineError> {
            if !self.alive {
                return Err(NativeEngineError::WorkerStopped);
            }
            self.requests.push(request);
            let response = match self.requests.last().unwrap() {
                WorkerRequest::Submit(_) => WorkerResponse::Submitted { task_id: 17 },
                WorkerRequest::Poll { .. } => WorkerResponse::Progress(WorkerProgress::default()),
                _ => WorkerResponse::Acknowledged,
            };
            if self.crash_after_submit
                && matches!(self.requests.last(), Some(WorkerRequest::Submit(_)))
            {
                self.alive = false;
            }
            Ok(response)
        }

        fn is_alive(&mut self) -> bool {
            self.alive
        }

        fn shutdown(&mut self) {
            self.alive = false;
        }
    }

    fn submission() -> NativeSubmission {
        NativeSubmission {
            url: "https://example.test/signed-url".into(),
            headers: vec![("cookie".into(), "credential".into())],
            output_path: PathBuf::from("/tmp/file.bin"),
            output_name: "file.bin".into(),
            user_id: "user-secret".into(),
            peer_id: "peer-secret".into(),
            acceleration_token: Some("token-secret".into()),
            cid: Some("cid".into()),
            gcid: Some("gcid".into()),
            total_length: Some(42),
        }
    }

    #[test]
    fn fake_worker_receives_all_lifecycle_commands_and_a_crash_invalidates_its_session() {
        let transport = FakeWorker {
            alive: true,
            ..Default::default()
        };
        let session = NativeEngineSession::new();
        let mut engine = LinuxWorkerEngine::from_transport_for_tests(session, transport);

        let handle = engine.submit(submission()).unwrap();
        let _ = engine.poll(handle).unwrap();
        engine.control(handle, DownloadAction::Pause).unwrap();
        engine.control(handle, DownloadAction::Resume).unwrap();
        engine.control(handle, DownloadAction::Cancel).unwrap();
        engine.release(handle).unwrap();
        engine.shutdown();

        let requests = engine.into_transport_for_tests().requests;
        assert!(matches!(requests[0], WorkerRequest::Initialize(_)));
        assert!(matches!(requests[1], WorkerRequest::Submit(_)));
        assert!(requests
            .iter()
            .any(|request| matches!(request, WorkerRequest::Poll { .. })));
        assert!(requests.iter().any(|request| matches!(
            request,
            WorkerRequest::Control {
                action: WorkerAction::Pause,
                ..
            }
        )));
        assert!(requests.iter().any(|request| matches!(
            request,
            WorkerRequest::Control {
                action: WorkerAction::Resume,
                ..
            }
        )));
        assert!(requests.iter().any(|request| matches!(
            request,
            WorkerRequest::Control {
                action: WorkerAction::Cancel,
                ..
            }
        )));
        assert!(requests
            .iter()
            .any(|request| matches!(request, WorkerRequest::Release { .. })));
        assert!(requests
            .iter()
            .any(|request| matches!(request, WorkerRequest::Shutdown)));

        let crashed_transport = FakeWorker {
            alive: true,
            crash_after_submit: true,
            ..Default::default()
        };
        let mut crashed = LinuxWorkerEngine::from_transport_for_tests(session, crashed_transport);
        let handle = crashed.submit(submission()).unwrap();
        assert!(matches!(
            crashed.poll(handle),
            Err(NativeEngineError::WorkerStopped)
        ));
        assert_eq!(crashed.take_stopped_handles(), vec![handle]);
        assert_eq!(
            crashed.control(handle, DownloadAction::Pause),
            Err(NativeEngineError::WorkerStopped)
        );
    }
}
