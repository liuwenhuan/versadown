use super::*;
use std::{
    fs::{File, Metadata},
    io::{self, Cursor, Seek, SeekFrom},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::SystemTime,
};

#[derive(Clone, Default)]
pub struct UploadControl {
    cancelled: Arc<AtomicBool>,
    bytes: Arc<AtomicU64>,
}
impl UploadControl {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
    pub fn bytes_read(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
    pub fn check(&self) -> Result<(), QuarkError> {
        if self.cancelled.load(Ordering::SeqCst) {
            Err(QuarkError::InvalidInput("上传已取消".into()))
        } else {
            Ok(())
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
                    .chain(file.reader(control)?)
                    .chain(Cursor::new(suffix.clone())),
            )),
            Self::Controlled { body, control } => body.reader(control.clone()),
        }
    }
    /// AWS requires a payload digest before sending; hash in a bounded buffer.
    pub(crate) fn sha256(&self) -> Result<String, QuarkError> {
        use sha2::Sha256;
        let mut reader = self.reader(UploadControl::default())?;
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
    control: UploadControl,
    digest: Option<UploadDigest>,
}
impl Read for SliceReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.control
            .check()
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
struct CancelOnDrop(UploadControl);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
/// One semaphore permit per file protocol; collect errors after unrelated files settle.
pub async fn schedule_uploads<I: Send + 'static, O: Send + 'static>(
    inputs: Vec<I>,
    limit: usize,
    control: UploadControl,
    work: impl Fn(I, UploadControl) -> Result<O, QuarkError> + Send + Sync + 'static,
) -> Vec<Result<O, QuarkError>> {
    let _cancel_on_drop = CancelOnDrop(control.clone());
    let permits = Arc::new(tokio::sync::Semaphore::new(limit.max(1)));
    let work = Arc::new(work);
    let mut workers = Vec::with_capacity(inputs.len());
    for input in inputs {
        // Acquire in input order so pending files are a predictable FIFO queue.
        let permit = permits
            .clone()
            .acquire_owned()
            .await
            .expect("upload semaphore remains open");
        if let Err(error) = control.check() {
            workers.push(Err(error));
            continue;
        }
        let work = work.clone();
        let control = control.clone();
        workers.push(Ok(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            control.check()?;
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
    results
}
