use super::protocol::{
    read_frame, write_frame, WorkerErrorCode, WorkerHello, WorkerRequest, WorkerResponse,
};
use std::{ffi::OsString, io::Read, os::unix::net::UnixStream, path::PathBuf};

struct WorkerArguments {
    socket: PathBuf,
    // These immutable runtime descriptors are intentionally accepted as
    // command-line metadata. The worker never receives account/task secrets
    // by argv, environment variables, or files.
    _runtime_root: PathBuf,
    _sdk: PathBuf,
    _stat: PathBuf,
    _sdk_sha256: String,
}

pub(super) fn run_from_args() -> Result<(), ()> {
    let arguments = parse_arguments(std::env::args_os().skip(2))?;
    let nonce = read_nonce()?;
    let stream = UnixStream::connect(arguments.socket).map_err(|_| ())?;
    run_stream(stream, nonce)
}

fn run_stream(mut stream: UnixStream, nonce: String) -> Result<(), ()> {
    write_frame(&mut stream, &WorkerHello::new(nonce)).map_err(|_| ())?;
    let mut initialized = false;
    loop {
        let request = read_frame::<WorkerRequest>(&mut stream).map_err(|_| ())?;
        let should_exit = matches!(request, WorkerRequest::Shutdown);
        let response = match request {
            WorkerRequest::Initialize(_) => {
                initialized = true;
                WorkerResponse::Acknowledged
            }
            // Task creation is deliberately unavailable until Task 4 installs
            // the fingerprint-gated SDK adapter in this worker process.
            WorkerRequest::Submit(_) if initialized => WorkerResponse::Error {
                code: WorkerErrorCode::Incompatible,
            },
            WorkerRequest::Submit(_) => WorkerResponse::Error {
                code: WorkerErrorCode::OperationFailed,
            },
            WorkerRequest::Poll { .. }
            | WorkerRequest::Control { .. }
            | WorkerRequest::Release { .. } => WorkerResponse::Error {
                code: WorkerErrorCode::TaskUnknown,
            },
            WorkerRequest::Shutdown => WorkerResponse::Acknowledged,
        };
        write_frame(&mut stream, &response).map_err(|_| ())?;
        if should_exit {
            return Ok(());
        }
    }
}

fn parse_arguments(arguments: impl Iterator<Item = OsString>) -> Result<WorkerArguments, ()> {
    let mut values = arguments;
    let mut socket = None;
    let mut runtime_root = None;
    let mut sdk = None;
    let mut stat = None;
    let mut sdk_sha256 = None;
    while let Some(flag) = values.next() {
        let value = values.next().ok_or(())?;
        match flag.to_str() {
            Some("--socket") => socket = Some(PathBuf::from(value)),
            Some("--runtime-root") => runtime_root = Some(PathBuf::from(value)),
            Some("--sdk") => sdk = Some(PathBuf::from(value)),
            Some("--stat") => stat = Some(PathBuf::from(value)),
            Some("--sdk-sha256") => sdk_sha256 = value.into_string().ok(),
            _ => return Err(()),
        }
    }
    Ok(WorkerArguments {
        socket: socket.ok_or(())?,
        _runtime_root: runtime_root.ok_or(())?,
        _sdk: sdk.ok_or(())?,
        _stat: stat.ok_or(())?,
        _sdk_sha256: sdk_sha256.ok_or(())?,
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
    use super::run_stream;
    use crate::xunlei_native::linux::protocol::{
        read_frame, write_frame, WorkerErrorCode, WorkerHello, WorkerInitialization, WorkerRequest,
        WorkerResponse, WorkerSubmission,
    };
    use std::{os::unix::net::UnixStream, thread};

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
}
