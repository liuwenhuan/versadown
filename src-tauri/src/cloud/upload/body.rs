use super::*;
use std::{
    fs::{File, Metadata},
    io::{self, Cursor, Seek, SeekFrom},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    time::SystemTime,
};

#[derive(Clone, Default)]
pub struct UploadControl {
    signal: Arc<UploadSignal>,
    bytes: Arc<AtomicU64>,
    progress: Option<(UploadProgressTracker, usize)>,
}
#[derive(Default)]
struct UploadSignal {
    state: Mutex<UploadSignalState>,
    changed: Condvar,
    async_changed: tokio::sync::Notify,
}
#[derive(Default, Clone, Copy)]
struct UploadSignalState {
    paused: bool,
    cancelled: bool,
}
impl UploadControl {
    pub fn pause(&self) -> Result<(), QuarkError> {
        self.set_paused(true)
    }
    pub fn resume(&self) -> Result<(), QuarkError> {
        self.set_paused(false)
    }
    fn set_paused(&self, paused: bool) -> Result<(), QuarkError> {
        let mut state = self
            .signal
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.cancelled {
            return Err(upload_cancelled());
        }
        state.paused = paused;
        drop(state);
        self.signal.changed.notify_all();
        self.signal.async_changed.notify_waiters();
        Ok(())
    }
    pub fn is_paused(&self) -> bool {
        self.signal
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .paused
    }
    pub fn is_cancelled(&self) -> bool {
        self.signal
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cancelled
    }
    pub(crate) fn with_progress(mut self, tracker: UploadProgressTracker, input: usize) -> Self {
        self.progress = Some((tracker, input));
        self
    }
    pub(crate) fn start_file(&self, path: &Path) {
        if let Some((tracker, input)) = &self.progress {
            tracker.start_file(*input, path);
        }
    }
    pub(crate) fn complete_file(&self, path: &Path) {
        if let Some((tracker, input)) = &self.progress {
            tracker.complete_file(*input, path);
        }
    }
    pub fn cancel(&self) {
        let mut state = self
            .signal
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.cancelled = true;
        state.paused = false;
        drop(state);
        self.signal.changed.notify_all();
        self.signal.async_changed.notify_waiters();
    }
    pub fn bytes_read(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
    pub fn check(&self) -> Result<(), QuarkError> {
        // Session checks can run while auth locks are held: never pause here.
        if self.is_cancelled() {
            Err(upload_cancelled())
        } else {
            Ok(())
        }
    }
    pub(crate) fn wait_ready_blocking(&self) -> Result<(), QuarkError> {
        let mut state = self
            .signal
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while state.paused && !state.cancelled {
            state = self
                .signal
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
        if state.cancelled {
            Err(upload_cancelled())
        } else {
            Ok(())
        }
    }
    async fn wait_ready(&self) -> Result<(), QuarkError> {
        loop {
            let changed = self.signal.async_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let state = *self
                .signal
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if state.cancelled {
                return Err(upload_cancelled());
            }
            if !state.paused {
                return Ok(());
            }
            changed.await;
        }
    }
}

/// Captured before hashing/initialization. Bodies carry metadata, never an open
/// file handle or a buffer containing the file.
#[derive(Clone, Debug)]
pub struct UploadFileSnapshot {
    path: PathBuf,
    length: u64,
    modified: SystemTime,
    identity: (u64, u64),
}
impl UploadFileSnapshot {
    pub fn capture(path: &Path) -> Result<Self, QuarkError> {
        let metadata = std::fs::symlink_metadata(path).map_err(|_| file_error())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(file_error());
        }
        let file = File::open(path).map_err(|_| file_error())?;
        let opened = file.metadata().map_err(|_| file_error())?;
        Ok(Self {
            path: path.into(),
            length: opened.len(),
            modified: opened.modified().map_err(|_| file_error())?,
            identity: file_identity(&file, &opened).map_err(|_| file_error())?,
        })
    }
    pub fn len(&self) -> u64 {
        self.length
    }
    pub fn validate(&self) -> Result<(), QuarkError> {
        let file = File::open(&self.path).map_err(|_| file_error())?;
        self.validate_file(&file).map_err(|_| changed_file())
    }
    fn validate_file(&self, file: &File) -> io::Result<()> {
        let metadata = file.metadata()?;
        let current = std::fs::symlink_metadata(&self.path)?;
        let same = current.is_file()
            && !current.file_type().is_symlink()
            && metadata.len() == self.length
            && current.len() == self.length
            && metadata.modified()? == self.modified
            && current.modified()? == self.modified
            && file_identity(file, &metadata)? == self.identity;
        // Detect a replacement even while this reader still owns the old file.
        let path_file = File::open(&self.path)?;
        if !same || file_identity(&path_file, &path_file.metadata()?)? != self.identity {
            return Err(io::Error::other("local upload file changed"));
        }
        Ok(())
    }
    pub fn slice(&self, offset: u64, length: u64) -> Result<UploadBody, QuarkError> {
        if offset
            .checked_add(length)
            .is_none_or(|end| end > self.length)
        {
            return Err(changed_file());
        }
        Ok(UploadBody::FileSlice {
            snapshot: self.clone(),
            offset,
            length,
            digest: None,
        })
    }
}
fn changed_file() -> QuarkError {
    QuarkError::InvalidInput("本地文件在上传过程中发生变化".into())
}
#[cfg(windows)]
fn file_identity(file: &File, _: &Metadata) -> io::Result<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(io::Error::other)?;
    Ok((
        info.dwVolumeSerialNumber as u64,
        ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
    ))
}
#[cfg(unix)]
fn file_identity(_: &File, metadata: &Metadata) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Ok((metadata.dev(), metadata.ino()))
}
pub enum UploadBody {
    Bytes(Vec<u8>),
    FileSlice {
        snapshot: UploadFileSnapshot,
        offset: u64,
        length: u64,
        digest: Option<UploadDigest>,
    },
    Multipart {
        prefix: Vec<u8>,
        file: Box<UploadBody>,
        suffix: Vec<u8>,
    },
    Controlled {
        body: Box<UploadBody>,
        control: UploadControl,
    },
}
impl From<Vec<u8>> for UploadBody {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Bytes(bytes)
    }
}
impl UploadBody {
    pub fn len(&self) -> u64 {
        match self {
            Self::Bytes(bytes) => bytes.len() as u64,
            Self::FileSlice { length, .. } => *length,
            Self::Multipart {
                prefix,
                file,
                suffix,
            } => prefix.len() as u64 + file.len() + suffix.len() as u64,
            Self::Controlled { body, .. } => body.len(),
        }
    }
    pub fn reader(&self, control: UploadControl) -> Result<Box<dyn Read + Send>, QuarkError> {
        self.reader_for(control, true)
    }
    fn reader_for(
        &self,
        control: UploadControl,
        transfer: bool,
    ) -> Result<Box<dyn Read + Send>, QuarkError> {
        control.check()?;
        match self {
            Self::Bytes(bytes) => Ok(Box::new(Cursor::new(bytes.clone()))),
            Self::FileSlice {
                snapshot,
                offset,
                length,
                digest,
            } => {
                let mut file = File::open(&snapshot.path).map_err(|_| file_error())?;
                snapshot.validate_file(&file).map_err(|_| changed_file())?;
                file.seek(SeekFrom::Start(*offset))
                    .map_err(|_| file_error())?;
                Ok(Box::new(SliceReader {
                    file: file.take(*length),
                    snapshot: snapshot.clone(),
                    remaining: *length,
                    position: *offset,
                    control,
                    digest: digest.clone(),
                }))
            }
            Self::Multipart {
                prefix,
                file,
                suffix,
            } => Ok(Box::new(
                Cursor::new(prefix.clone())
                    .chain(file.reader_for(control, transfer)?)
                    .chain(Cursor::new(suffix.clone())),
            )),
            Self::Controlled {
                body,
                control: attached,
            } => body.reader_for(
                if transfer {
                    attached.clone()
                } else {
                    UploadControl {
                        signal: attached.signal.clone(),
                        ..UploadControl::default()
                    }
                },
                transfer,
            ),
        }
    }
    /// AWS requires a payload digest before sending; hash in a bounded buffer.
    pub(crate) fn sha256(&self) -> Result<String, QuarkError> {
        use sha2::Sha256;
        let mut reader = self.reader_for(UploadControl::default(), false)?;
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = reader.read(&mut buffer).map_err(|_| changed_file())?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        Ok(format!("{:x}", hash.finalize()))
    }
}
struct SliceReader {
    file: io::Take<File>,
    snapshot: UploadFileSnapshot,
    remaining: u64,
    position: u64,
    control: UploadControl,
    digest: Option<UploadDigest>,
}
impl Read for SliceReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.control
            .wait_ready_blocking()
            .map_err(|_| io::Error::other("upload cancelled"))?;
        self.snapshot.validate_file(self.file.get_ref())?;
        if buffer.is_empty() || self.remaining == 0 {
            return Ok(0);
        }
        let count = self.file.read(buffer)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "local upload file truncated",
            ));
        }
        self.remaining -= count as u64;
        self.snapshot.validate_file(self.file.get_ref())?;
        if let Some(digest) = &self.digest {
            digest.update(&buffer[..count])?;
        }
        self.control
            .bytes
            .fetch_add(count as u64, Ordering::Relaxed);
        if let Some((tracker, input)) = &self.control.progress {
            tracker.record_bytes(*input, &self.snapshot.path, self.position, count as u64);
        }
        self.position += count as u64;
        Ok(count)
    }
}

/// CloudDrive compares the actual streamed bytes to its registered MD5/SHA1.
/// Only digest state is shared across parts; file buffers remain in the reader.
#[derive(Clone, Default)]
pub struct UploadDigest(Arc<std::sync::Mutex<(md5::Context, Sha1)>>);
impl UploadDigest {
    fn update(&self, bytes: &[u8]) -> io::Result<()> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| io::Error::other("upload digest unavailable"))?;
        state.0.consume(bytes);
        state.1.update(bytes);
        Ok(())
    }
    pub(crate) fn verify(&self, md5: &str, sha1: &str) -> Result<(), QuarkError> {
        let state = self.0.lock().map_err(|_| changed_file())?;
        if format!("{:x}", state.0.clone().finalize()) != md5
            || format!("{:x}", state.1.clone().finalize()) != sha1
        {
            return Err(changed_file());
        }
        Ok(())
    }
}
struct CancelOnDrop(Option<UploadControl>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(control) = &self.0 {
            control.cancel();
        }
    }
}
/// One semaphore permit per file protocol; collect errors after unrelated files settle.
pub async fn schedule_uploads<I: Send + 'static, O: Send + 'static>(
    inputs: Vec<I>,
    limit: usize,
    control: UploadControl,
    work: impl Fn(I, UploadControl) -> Result<O, QuarkError> + Send + Sync + 'static,
) -> Vec<Result<O, QuarkError>> {
    let mut cancel_on_drop = CancelOnDrop(Some(control.clone()));
    let permits = Arc::new(tokio::sync::Semaphore::new(limit.max(1)));
    let work = Arc::new(work);
    let mut workers = Vec::with_capacity(inputs.len());
    for input in inputs {
        if let Err(error) = control.wait_ready().await {
            workers.push(Err(error));
            continue;
        }
        // Acquire in input order so pending files are a predictable FIFO queue.
        let permit = permits
            .clone()
            .acquire_owned()
            .await
            .expect("upload semaphore remains open");
        if let Err(error) = control.wait_ready().await {
            workers.push(Err(error));
            continue;
        }
        let work = work.clone();
        let control = control.clone();
        workers.push(Ok(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            control.wait_ready_blocking()?;
            work(input, control)
        })));
    }
    let mut results = Vec::with_capacity(workers.len());
    for worker in workers {
        results.push(match worker {
            Ok(worker) => worker
                .await
                .unwrap_or_else(|_| Err(QuarkError::Transport("上传任务中断".into()))),
            Err(error) => Err(error),
        });
    }
    cancel_on_drop.0 = None;
    results
}

#[cfg(test)]
mod upload_progress_body_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn upload_progress_counts_only_streamed_file_ranges_not_multipart_framing_or_retries() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.bin");
        std::fs::write(&path, b"PAYLOAD").unwrap();
        let tracker = UploadProgressTracker::new(CloudProvider::Xunlei);
        tracker.add_file(0, &path, 7);
        let control = UploadControl::default().with_progress(tracker.clone(), 0);
        let body = UploadBody::Controlled {
            body: Box::new(UploadBody::Multipart {
                prefix: b"header".to_vec(),
                file: Box::new(
                    UploadFileSnapshot::capture(&path)
                        .unwrap()
                        .slice(0, 7)
                        .unwrap(),
                ),
                suffix: b"footer".to_vec(),
            }),
            control: control.clone(),
        };
        for _ in 0..2 {
            let mut actual = Vec::new();
            body.reader(UploadControl::default())
                .unwrap()
                .read_to_end(&mut actual)
                .unwrap();
            assert_eq!(actual, b"headerPAYLOADfooter");
        }
        let event = tracker.sample(Duration::from_secs(1));
        assert_eq!(event.uploaded_bytes, 7);
        assert_eq!(event.speed_bytes_per_second, 14.0);
        assert_eq!(control.bytes_read(), 14);
    }

    #[test]
    fn upload_progress_hashing_a_controlled_body_does_not_count_transfer_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.bin");
        std::fs::write(&path, b"PAYLOAD").unwrap();
        let tracker = UploadProgressTracker::new(CloudProvider::Xunlei);
        tracker.add_file(0, &path, 7);
        let control = UploadControl::default().with_progress(tracker.clone(), 0);
        let body = UploadBody::Controlled {
            body: Box::new(
                UploadFileSnapshot::capture(&path)
                    .unwrap()
                    .slice(0, 7)
                    .unwrap(),
            ),
            control: control.clone(),
        };
        assert_eq!(body.sha256().unwrap().len(), 64);
        assert_eq!(control.bytes_read(), 0);
        let event = tracker.sample(Duration::from_secs(1));
        assert_eq!(event.uploaded_bytes, 0);
        assert_eq!(event.speed_bytes_per_second, 0.0);
    }

    #[test]
    fn upload_progress_hashing_keeps_the_attached_cancellation_signal() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.bin");
        std::fs::write(&path, b"PAYLOAD").unwrap();
        let control = UploadControl::default();
        let body = UploadBody::Controlled {
            body: Box::new(
                UploadFileSnapshot::capture(&path)
                    .unwrap()
                    .slice(0, 7)
                    .unwrap(),
            ),
            control: control.clone(),
        };
        control.cancel();
        assert!(body.sha256().is_err());
        assert_eq!(control.bytes_read(), 0);
    }
}

#[cfg(test)]
mod upload_control_body_tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    #[test]
    fn upload_control_pause_stops_real_reader_until_resume() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.bin");
        std::fs::write(&path, b"PAYLOAD").unwrap();
        let control = UploadControl::default();
        let mut reader = UploadFileSnapshot::capture(&path)
            .unwrap()
            .slice(0, 7)
            .unwrap()
            .reader(control.clone())
            .unwrap();
        control.pause().unwrap();
        let (sent, received) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = reader.read_to_end(&mut bytes);
            sent.send((result, bytes)).unwrap();
        });
        assert!(received.recv_timeout(Duration::from_millis(75)).is_err());
        assert_eq!(control.bytes_read(), 0);
        control.resume().unwrap();
        let (result, bytes) = received.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(result.is_ok());
        assert_eq!(bytes, b"PAYLOAD");
        worker.join().unwrap();
    }

    #[test]
    fn upload_control_cancel_wakes_a_paused_reader_without_consuming_payload() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.bin");
        std::fs::write(&path, b"PAYLOAD").unwrap();
        let control = UploadControl::default();
        let mut reader = UploadFileSnapshot::capture(&path)
            .unwrap()
            .slice(0, 7)
            .unwrap()
            .reader(control.clone())
            .unwrap();
        control.pause().unwrap();
        let (sent, received) = mpsc::channel();
        let worker = std::thread::spawn(move || sent.send(reader.read(&mut [0; 7])).unwrap());
        assert!(received.recv_timeout(Duration::from_millis(75)).is_err());
        control.cancel();
        assert!(received
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .is_err());
        assert_eq!(control.bytes_read(), 0);
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn upload_control_paused_scheduler_remains_async_and_cancel_stops_every_queued_file() {
        let control = UploadControl::default();
        control.pause().unwrap();
        let work_control = control.clone();
        let workers = tokio::spawn(async move {
            schedule_uploads(vec![1, 2], 1, work_control, |input, _| Ok(input)).await
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!workers.is_finished());
        control.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), workers)
            .await
            .unwrap()
            .unwrap();
        assert!(result
            .iter()
            .all(|item| item.as_ref().is_err_and(|error| is_upload_cancelled(error))));
    }

    #[tokio::test]
    async fn upload_control_normal_scheduler_completion_is_not_cancellation() {
        let control = UploadControl::default();
        let result = schedule_uploads(vec![1], 1, control.clone(), |input, _| Ok(input)).await;
        assert_eq!(result[0].as_ref().unwrap(), &1);
        assert!(!control.is_cancelled());
    }
}
