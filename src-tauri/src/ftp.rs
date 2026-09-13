use std::{
    collections::{HashMap, VecDeque},
    fmt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    time::{Duration, Instant},
};

use percent_encoding::percent_decode_str;
use suppaftp::{tokio::AsyncFtpStream, types::FileType};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{watch, Mutex},
};
use url::Url;
use uuid::Uuid;

use crate::{
    aria2::{DownloadAction, EventSink, UriSubmissionOptions},
    DownloadTask, StoreError, TaskStatus, TaskStore,
};

const ANONYMOUS_PASSWORD: &str = "anonymous@alldownloader.local";

#[derive(Clone)]
pub struct FtpTarget {
    host: String,
    port: u16,
    username: String,
    password: String,
    remote_path: String,
    file_name: String,
    display_source: String,
}

impl FtpTarget {
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn password(&self) -> &str {
        &self.password
    }

    pub fn remote_path(&self) -> &str {
        &self.remote_path
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    pub fn display_source(&self) -> &str {
        &self.display_source
    }
}

impl fmt::Debug for FtpTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FtpTarget")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("remote_path", &self.remote_path)
            .field("file_name", &self.file_name)
            .field("display_source", &self.display_source)
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum FtpDownloadError {
    #[error("FTP 链接无效：{0}")]
    InvalidUrl(String),
    #[error("FTP 链接必须包含文件路径")]
    MissingFilePath,
    #[error("FTP 下载参数无效：{0}")]
    InvalidOption(&'static str),
    #[error("FTP 服务器操作失败：{0}")]
    Protocol(String),
    #[error("FTP 本地文件操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn decode_component(value: &str, label: &str) -> Result<String, FtpDownloadError> {
    percent_decode_str(value)
        .decode_utf8()
        .map(|value| value.into_owned())
        .map_err(|_| FtpDownloadError::InvalidUrl(format!("{label} 不是有效的 UTF-8")))
}

pub fn parse_ftp_url(source: &str) -> Result<FtpTarget, FtpDownloadError> {
    let url =
        Url::parse(source).map_err(|error| FtpDownloadError::InvalidUrl(error.to_string()))?;
    if url.scheme() != "ftp" {
        return Err(FtpDownloadError::InvalidUrl("仅支持 ftp:// 链接".into()));
    }
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| FtpDownloadError::InvalidUrl("缺少服务器地址".into()))?
        .to_owned();
    let port = url.port().unwrap_or(21);
    let username = if url.username().is_empty() {
        "anonymous".to_owned()
    } else {
        decode_component(url.username(), "用户名")?
    };
    let password = match url.password() {
        Some(password) => decode_component(password, "密码")?,
        None if username == "anonymous" => ANONYMOUS_PASSWORD.to_owned(),
        None => String::new(),
    };
    let remote_path = decode_component(url.path(), "文件路径")?;
    let file_name = remote_path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or(FtpDownloadError::MissingFilePath)?
        .to_owned();
    let mut display = url.clone();
    let _ = display.set_username("");
    let _ = display.set_password(None);

    Ok(FtpTarget {
        host,
        port,
        username,
        password,
        remote_path,
        file_name,
        display_source: display.to_string(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkerCommand {
    Running,
    Pause,
    Cancel,
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FtpDownloadLimits {
    max_active_tasks: usize,
    max_connections_per_task: usize,
}

impl FtpDownloadLimits {
    pub fn new(max_active_tasks: u16, max_connections_per_task: u8) -> Self {
        Self {
            max_active_tasks: usize::from(max_active_tasks.max(1)),
            max_connections_per_task: usize::from(max_connections_per_task.max(1)),
        }
    }

    pub fn max_active_tasks(self) -> usize {
        self.max_active_tasks
    }

    pub fn max_connections_per_task(self) -> usize {
        self.max_connections_per_task
    }
}

impl Default for FtpDownloadLimits {
    fn default() -> Self {
        Self::new(5, 8)
    }
}

struct QueuedWorker {
    task_id: Uuid,
    source: String,
    output: PathBuf,
}

#[derive(Default)]
struct WorkerScheduler {
    active: HashMap<Uuid, watch::Sender<WorkerCommand>>,
    queued: VecDeque<QueuedWorker>,
}

pub struct FtpDownloadEngine {
    default_directory: RwLock<PathBuf>,
    store: Arc<TaskStore>,
    events: Arc<dyn EventSink>,
    scheduler: Mutex<WorkerScheduler>,
    limits: RwLock<FtpDownloadLimits>,
    accepting: AtomicBool,
}

impl FtpDownloadEngine {
    pub fn new(
        default_directory: impl AsRef<Path>,
        store: Arc<TaskStore>,
        events: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            default_directory: RwLock::new(default_directory.as_ref().to_path_buf()),
            store,
            events,
            scheduler: Mutex::new(WorkerScheduler::default()),
            limits: RwLock::new(FtpDownloadLimits::default()),
            accepting: AtomicBool::new(true),
        }
    }

    pub fn set_limits(self: &Arc<Self>, max_active_tasks: u16, max_connections_per_task: u8) {
        *self
            .limits
            .write()
            .unwrap_or_else(|error| error.into_inner()) =
            FtpDownloadLimits::new(max_active_tasks, max_connections_per_task);
        self.schedule_pump();
    }

    pub fn limits(&self) -> FtpDownloadLimits {
        *self
            .limits
            .read()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub async fn submit_uri(
        self: &Arc<Self>,
        task_id: Uuid,
        source: &str,
        options: UriSubmissionOptions,
    ) -> Result<DownloadTask, FtpDownloadError> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(FtpDownloadError::Protocol("应用正在退出".into()));
        }
        if !options.headers.is_empty() {
            return Err(FtpDownloadError::InvalidOption(
                "FTP 不支持自定义 HTTP 请求头",
            ));
        }
        if options.proxy.is_some() {
            return Err(FtpDownloadError::InvalidOption("FTP 暂不支持 HTTP 代理"));
        }
        let target = parse_ftp_url(source)?;
        let directory = options.directory.unwrap_or_else(|| {
            self.default_directory
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        });
        if directory.as_os_str().is_empty() {
            return Err(FtpDownloadError::InvalidOption("下载目录为空"));
        }
        tokio::fs::create_dir_all(&directory).await?;
        let output_name = options
            .output_name
            .unwrap_or_else(|| target.file_name.clone());
        if !valid_output_name(&output_name) {
            return Err(FtpDownloadError::InvalidOption("输出文件名无效"));
        }
        let output = directory.join(output_name);
        if options.allow_overwrite && tokio::fs::try_exists(&output).await? {
            tokio::fs::remove_file(&output).await?;
        }

        let mut task = self.store.find(task_id)?;
        task.name = output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(target.file_name())
            .to_owned();
        task.status = TaskStatus::Queued;
        task.error = None;
        task.failure_code = None;
        task.aria_gid = None;
        task.download_speed = 0;
        task.upload_speed = 0;
        task.connections = 0;
        task.eta_seconds = None;
        let existing = tokio::fs::metadata(&output)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        task.completed_length = existing;
        task.progress = 0;
        task = self.persist(task, &output, 0, existing)?;

        self.start_worker(task_id, source.to_owned(), output)
            .await?;
        Ok(task)
    }

    pub async fn control(
        self: &Arc<Self>,
        task_id: Uuid,
        action: DownloadAction,
    ) -> Result<DownloadTask, FtpDownloadError> {
        let current = self.store.find(task_id)?;
        if current.kind != "ftp" {
            return Err(FtpDownloadError::InvalidOption("任务不是 FTP 下载"));
        }
        match action {
            DownloadAction::Pause => {
                self.signal_and_wait(task_id, WorkerCommand::Pause).await;
                let latest = self.store.find(task_id)?;
                if latest.status == TaskStatus::Completed {
                    return Ok(latest);
                }
                self.set_terminal_transfer_state(latest, TaskStatus::Paused, None)
            }
            DownloadAction::Cancel => {
                self.signal_and_wait(task_id, WorkerCommand::Cancel).await;
                let latest = self.store.find(task_id)?;
                if latest.status == TaskStatus::Completed {
                    return Ok(latest);
                }
                self.set_terminal_transfer_state(latest, TaskStatus::Cancelled, None)
            }
            DownloadAction::Resume => {
                if current.status != TaskStatus::Paused {
                    return Err(FtpDownloadError::InvalidOption("当前 FTP 任务不能继续"));
                }
                let output = self
                    .store
                    .files(task_id)?
                    .first()
                    .map(|file| PathBuf::from(&file.path))
                    .ok_or(FtpDownloadError::InvalidOption("缺少 FTP 输出文件路径"))?;
                let source = current.source.clone();
                let mut queued = current;
                queued.status = TaskStatus::Queued;
                queued.error = None;
                queued.download_speed = 0;
                queued.connections = 0;
                let completed = queued.completed_length;
                let total = queued.total_length.unwrap_or(0);
                let queued = self.persist(queued, &output, total, completed)?;
                self.start_worker(task_id, source, output).await?;
                Ok(queued)
            }
        }
    }

    pub async fn recover(self: &Arc<Self>) -> Result<(), FtpDownloadError> {
        for task in self.store.list()? {
            if task.kind != "ftp"
                || !matches!(task.status, TaskStatus::Queued | TaskStatus::Downloading)
            {
                continue;
            }
            let Some(output) = self
                .store
                .files(task.id)?
                .first()
                .map(|file| PathBuf::from(&file.path))
            else {
                let _ = self.store.fail(task.id, "FTP 恢复失败：缺少输出文件路径");
                continue;
            };
            if let Err(error) = self
                .start_worker(task.id, task.source.clone(), output)
                .await
            {
                let _ = self.store.fail(task.id, format!("FTP 恢复失败：{error}"));
            }
        }
        Ok(())
    }

    pub fn set_default_directory(&self, directory: impl AsRef<Path>) {
        *self
            .default_directory
            .write()
            .unwrap_or_else(|error| error.into_inner()) = directory.as_ref().to_path_buf();
    }

    pub fn global_stat(&self) -> Result<crate::Aria2GlobalStat, FtpDownloadError> {
        let tasks = self.store.list()?;
        Ok(crate::Aria2GlobalStat {
            download_speed: tasks
                .iter()
                .filter(|task| task.kind == "ftp")
                .map(|task| task.download_speed)
                .sum(),
            upload_speed: 0,
            num_active: tasks
                .iter()
                .filter(|task| task.kind == "ftp" && task.status == TaskStatus::Downloading)
                .count() as u64,
            num_waiting: tasks
                .iter()
                .filter(|task| task.kind == "ftp" && task.status == TaskStatus::Queued)
                .count() as u64,
            num_stopped: tasks
                .iter()
                .filter(|task| {
                    task.kind == "ftp"
                        && matches!(
                            task.status,
                            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
                        )
                })
                .count() as u64,
            num_stopped_total: tasks
                .iter()
                .filter(|task| {
                    task.kind == "ftp"
                        && matches!(
                            task.status,
                            TaskStatus::Completed
                                | TaskStatus::Failed
                                | TaskStatus::Cancelled
                                | TaskStatus::Recycled
                        )
                })
                .count() as u64,
        })
    }

    pub async fn shutdown(&self) {
        self.accepting.store(false, Ordering::Release);
        let scheduler = self.scheduler.lock().await;
        for sender in scheduler.active.values() {
            let _ = sender.send(WorkerCommand::Shutdown);
        }
    }

    async fn start_worker(
        self: &Arc<Self>,
        task_id: Uuid,
        source: String,
        output: PathBuf,
    ) -> Result<(), FtpDownloadError> {
        {
            let mut scheduler = self.scheduler.lock().await;
            if scheduler.active.contains_key(&task_id)
                || scheduler.queued.iter().any(|work| work.task_id == task_id)
            {
                return Ok(());
            }
            scheduler.queued.push_back(QueuedWorker {
                task_id,
                source,
                output,
            });
        }
        self.pump_queue().await;
        Ok(())
    }

    async fn pump_queue(self: &Arc<Self>) {
        let limits = self.limits();
        let mut starts = Vec::new();
        {
            let mut scheduler = self.scheduler.lock().await;
            while scheduler.active.len() < limits.max_active_tasks() {
                let Some(work) = scheduler.queued.pop_front() else {
                    break;
                };
                let (sender, receiver) = watch::channel(WorkerCommand::Running);
                scheduler.active.insert(work.task_id, sender);
                starts.push((work, receiver));
            }
        }
        for (work, receiver) in starts {
            let engine = Arc::clone(self);
            tauri::async_runtime::spawn(async move {
                let result = engine
                    .run_worker(work.task_id, &work.source, &work.output, receiver)
                    .await;
                engine.scheduler.lock().await.active.remove(&work.task_id);
                if let Err(error) = result {
                    if let Ok(current) = engine.store.find(work.task_id) {
                        if !matches!(current.status, TaskStatus::Paused | TaskStatus::Cancelled) {
                            if let Ok(failed) = engine.store.fail(work.task_id, error.to_string()) {
                                engine.events.task_updated(&failed);
                            }
                        }
                    }
                }
                engine.schedule_pump();
            });
        }
    }

    fn schedule_pump(self: &Arc<Self>) {
        let engine = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            engine.pump_queue().await;
        });
    }

    async fn run_worker(
        &self,
        task_id: Uuid,
        source: &str,
        output: &Path,
        mut command: watch::Receiver<WorkerCommand>,
    ) -> Result<(), FtpDownloadError> {
        if *command.borrow_and_update() != WorkerCommand::Running {
            return Ok(());
        }
        let completed_before_connect = tokio::fs::metadata(output)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let mut connecting = self.store.find(task_id)?;
        connecting.status = TaskStatus::Downloading;
        connecting.completed_length = completed_before_connect;
        connecting.download_speed = 0;
        connecting.connections = 0;
        connecting.error = None;
        let known_total = connecting.total_length.unwrap_or(0);
        let connecting = self.persist(connecting, output, known_total, completed_before_connect)?;
        self.events.task_updated(&connecting);

        let target = parse_ftp_url(source)?;
        let address = format!("{}:{}", target.host(), target.port());
        let mut ftp =
            tokio::time::timeout(Duration::from_secs(20), AsyncFtpStream::connect(address))
                .await
                .map_err(|_| FtpDownloadError::Protocol("连接超时".into()))?
                .map_err(protocol_error)?;
        ftp.login(target.username(), target.password())
            .await
            .map_err(protocol_error)?;
        ftp.transfer_type(FileType::Binary)
            .await
            .map_err(protocol_error)?;
        let total = u64::try_from(
            ftp.size(target.remote_path())
                .await
                .map_err(protocol_error)?,
        )
        .map_err(|_| FtpDownloadError::Protocol("远程文件过大".into()))?;
        let mut completed = tokio::fs::metadata(output)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if completed > total {
            tokio::fs::File::create(output).await?;
            completed = 0;
        }
        if completed == total {
            let task = self.completed_snapshot(task_id, output, total)?;
            self.events.task_updated(&task);
            let _ = ftp.quit().await;
            return Ok(());
        }
        if completed > 0 {
            ftp.resume_transfer(
                usize::try_from(completed)
                    .map_err(|_| FtpDownloadError::Protocol("续传偏移量过大".into()))?,
            )
            .await
            .map_err(|error| {
                FtpDownloadError::Protocol(format!(
                    "服务器不支持从 {completed} 字节处续传：{error}"
                ))
            })?;
        }

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(completed > 0)
            .write(true)
            .truncate(completed == 0)
            .open(output)
            .await?;
        let mut data = ftp
            .retr_as_stream(target.remote_path())
            .await
            .map_err(protocol_error)?;
        let mut task = self.store.find(task_id)?;
        task.status = TaskStatus::Downloading;
        task.total_length = Some(total);
        task.completed_length = completed;
        task.connections = 1;
        task.error = None;
        task.progress = progress(completed, total);
        task = self.persist(task, output, total, completed)?;
        self.events.task_updated(&task);

        let mut buffer = vec![0u8; 64 * 1024];
        let mut sample_started = Instant::now();
        let mut sample_completed = completed;
        let mut last_published = Instant::now();
        loop {
            let read = tokio::select! {
                result = data.read(&mut buffer) => result?,
                changed = command.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    match *command.borrow_and_update() {
                        WorkerCommand::Running => continue,
                        WorkerCommand::Pause | WorkerCommand::Cancel | WorkerCommand::Shutdown => return Ok(()),
                    }
                }
            };
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read]).await?;
            completed = completed.saturating_add(read as u64);
            if last_published.elapsed() >= Duration::from_millis(200) || completed >= total {
                let elapsed = sample_started.elapsed().as_secs_f64().max(0.001);
                let speed = ((completed.saturating_sub(sample_completed)) as f64 / elapsed) as u64;
                let mut task = self.store.find(task_id)?;
                if task.status != TaskStatus::Downloading {
                    return Ok(());
                }
                task.total_length = Some(total);
                task.completed_length = completed;
                task.download_speed = speed;
                task.connections = 1;
                task.progress = progress(completed, total);
                task.eta_seconds = (speed > 0).then(|| total.saturating_sub(completed) / speed);
                let task = self.persist(task, output, total, completed)?;
                self.events.task_updated(&task);
                sample_started = Instant::now();
                sample_completed = completed;
                last_published = Instant::now();
            }
        }
        file.flush().await?;
        ftp.finalize_retr_stream(data)
            .await
            .map_err(protocol_error)?;
        let _ = ftp.quit().await;
        if completed != total {
            return Err(FtpDownloadError::Protocol(format!(
                "传输提前结束：已接收 {completed} / {total} 字节"
            )));
        }
        let task = self.completed_snapshot(task_id, output, total)?;
        self.events.task_updated(&task);
        Ok(())
    }

    fn completed_snapshot(
        &self,
        task_id: Uuid,
        output: &Path,
        total: u64,
    ) -> Result<DownloadTask, FtpDownloadError> {
        let mut task = self.store.find(task_id)?;
        task.status = TaskStatus::Completed;
        task.progress = 100;
        task.error = None;
        task.total_length = Some(total);
        task.completed_length = total;
        task.download_speed = 0;
        task.upload_speed = 0;
        task.connections = 0;
        task.eta_seconds = Some(0);
        self.persist(task, output, total, total)
    }

    fn set_terminal_transfer_state(
        &self,
        mut task: DownloadTask,
        status: TaskStatus,
        error: Option<String>,
    ) -> Result<DownloadTask, FtpDownloadError> {
        task.status = status;
        task.error = error;
        task.download_speed = 0;
        task.upload_speed = 0;
        task.connections = 0;
        task.eta_seconds = None;
        let files = self.store.files(task.id)?;
        let output = files
            .first()
            .map(|file| PathBuf::from(&file.path))
            .unwrap_or_else(|| {
                self.default_directory
                    .read()
                    .unwrap_or_else(|error| error.into_inner())
                    .join(&task.name)
            });
        let total = task.total_length.unwrap_or(0);
        let completed = std::fs::metadata(&output)
            .map(|metadata| metadata.len())
            .unwrap_or(task.completed_length);
        task.completed_length = completed;
        task.progress = progress(completed, total);
        let task = self.persist(task, &output, total, completed)?;
        self.events.task_updated(&task);
        Ok(task)
    }

    fn persist(
        &self,
        task: DownloadTask,
        output: &Path,
        total: u64,
        completed: u64,
    ) -> Result<DownloadTask, FtpDownloadError> {
        Ok(self.store.apply_transfer_snapshot_with_files(
            task,
            &[(
                1,
                output.to_string_lossy().into_owned(),
                total,
                completed,
                true,
            )],
        )?)
    }

    async fn signal_and_wait(&self, task_id: Uuid, command: WorkerCommand) {
        let sender = {
            let mut scheduler = self.scheduler.lock().await;
            scheduler.queued.retain(|work| work.task_id != task_id);
            scheduler.active.get(&task_id).cloned()
        };
        if let Some(sender) = sender {
            let _ = sender.send(command);
            for _ in 0..100 {
                if !self.scheduler.lock().await.active.contains_key(&task_id) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

fn protocol_error(error: suppaftp::FtpError) -> FtpDownloadError {
    FtpDownloadError::Protocol(error.to_string())
}

fn valid_output_name(value: &str) -> bool {
    !value.trim().is_empty()
        && !value.contains(['\r', '\n', '/', '\\', '<', '>', ':', '"', '|', '?', '*'])
        && !value.ends_with(['.', ' '])
}

fn progress(completed: u64, total: u64) -> u8 {
    if total == 0 {
        0
    } else {
        ((u128::from(completed) * 100 / u128::from(total)).min(100)) as u8
    }
}
