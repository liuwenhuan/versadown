use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use regex::Regex;
use reqwest::header::{HeaderName, HeaderValue};
use thiserror::Error;
use tokio::{
    io::AsyncWriteExt,
    sync::{watch, Mutex},
};
use url::Url;
use uuid::Uuid;

use crate::{
    aria2::{Aria2Api, DownloadAction, DownloadService, EventSink, UriSubmissionOptions},
    DownloadTask, StoreError, TaskStatus, TaskStore,
};

#[derive(Debug, Error)]
pub enum MediaDownloadError {
    #[error("媒体清单无效：{0}")]
    InvalidManifest(String),
    #[error("媒体下载失败：{0}")]
    Transfer(String),
    #[error("媒体下载参数无效：{0}")]
    InvalidOption(&'static str),
    #[error("媒体文件操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkerCommand {
    Running,
    Pause,
    Cancel,
    Shutdown,
}

#[derive(Clone, Debug)]
struct MediaPlan {
    segments: Vec<String>,
}

pub struct MediaDownloadEngine {
    data_directory: PathBuf,
    store: Arc<TaskStore>,
    events: Arc<dyn EventSink>,
    workers: Mutex<HashMap<Uuid, watch::Sender<WorkerCommand>>>,
    accepting: AtomicBool,
}

impl MediaDownloadEngine {
    pub fn new(
        data_directory: impl AsRef<Path>,
        store: Arc<TaskStore>,
        events: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            data_directory: data_directory.as_ref().join("media-transfers"),
            store,
            events,
            workers: Mutex::new(HashMap::new()),
            accepting: AtomicBool::new(true),
        }
    }

    pub async fn submit_uri(
        self: &Arc<Self>,
        service: Arc<DownloadService>,
        task_id: Uuid,
        source: &str,
        options: UriSubmissionOptions,
    ) -> Result<DownloadTask, MediaDownloadError> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(MediaDownloadError::Transfer("应用正在退出".into()));
        }
        let task = self.store.find(task_id)?;
        if !matches!(task.kind.as_str(), "hls" | "dash") {
            return Err(MediaDownloadError::InvalidOption("任务不是 HLS/DASH 下载"));
        }
        let directory = options
            .directory
            .clone()
            .ok_or(MediaDownloadError::InvalidOption("下载目录为空"))?;
        tokio::fs::create_dir_all(&directory).await?;
        let output_name = options
            .output_name
            .clone()
            .unwrap_or_else(|| media_output_name(source, &task.kind));
        if !valid_output_name(&output_name) {
            return Err(MediaDownloadError::InvalidOption("输出文件名无效"));
        }
        let output = directory.join(output_name);
        if options.allow_overwrite && tokio::fs::try_exists(&output).await? {
            tokio::fs::remove_file(&output).await?;
        }

        let mut queued = task;
        queued.name = output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("media-download")
            .to_owned();
        queued.status = TaskStatus::Queued;
        queued.progress = 0;
        queued.error = None;
        queued.failure_code = None;
        queued.aria_gid = None;
        queued.download_speed = 0;
        queued.upload_speed = 0;
        queued.connections = 0;
        queued.eta_seconds = None;
        queued.completed_length = 0;
        queued.total_length = None;
        let queued = self.persist(queued, &output, 0, 0)?;
        self.start_worker(service, task_id, source.to_owned(), output, options)
            .await?;
        Ok(queued)
    }

    pub async fn control(
        self: &Arc<Self>,
        service: Arc<DownloadService>,
        task_id: Uuid,
        action: DownloadAction,
    ) -> Result<DownloadTask, MediaDownloadError> {
        let current = self.store.find(task_id)?;
        if !matches!(current.kind.as_str(), "hls" | "dash") {
            return Err(MediaDownloadError::InvalidOption("任务不是 HLS/DASH 下载"));
        }
        match action {
            DownloadAction::Pause => {
                self.signal_and_wait(task_id, WorkerCommand::Pause).await;
                let latest = self.store.find(task_id)?;
                if latest.status == TaskStatus::Completed {
                    return Ok(latest);
                }
                self.set_status(latest, TaskStatus::Paused)
            }
            DownloadAction::Cancel => {
                self.signal_and_wait(task_id, WorkerCommand::Cancel).await;
                let latest = self.store.find(task_id)?;
                if latest.status == TaskStatus::Completed {
                    return Ok(latest);
                }
                self.set_status(latest, TaskStatus::Cancelled)
            }
            DownloadAction::Resume => {
                if current.status != TaskStatus::Paused {
                    return Err(MediaDownloadError::InvalidOption("当前媒体任务不能继续"));
                }
                let output = self.output_path(&current)?;
                let directory = output
                    .parent()
                    .ok_or(MediaDownloadError::InvalidOption("输出目录无效"))?
                    .to_path_buf();
                let source = current.source.clone();
                let mut queued = current;
                queued.status = TaskStatus::Queued;
                queued.error = None;
                queued.download_speed = 0;
                queued.connections = 0;
                let queued = self.persist(
                    queued.clone(),
                    &output,
                    queued.total_length.unwrap_or(0),
                    queued.completed_length,
                )?;
                self.start_worker(
                    service,
                    task_id,
                    source,
                    output,
                    UriSubmissionOptions {
                        directory: Some(directory),
                        continue_download: true,
                        ..Default::default()
                    },
                )
                .await?;
                Ok(queued)
            }
        }
    }

    pub async fn recover(
        self: &Arc<Self>,
        service: Arc<DownloadService>,
    ) -> Result<(), MediaDownloadError> {
        for task in self.store.list()? {
            if !matches!(task.kind.as_str(), "hls" | "dash")
                || !matches!(task.status, TaskStatus::Queued | TaskStatus::Downloading)
            {
                continue;
            }
            let output = match self.output_path(&task) {
                Ok(output) => output,
                Err(error) => {
                    let _ = self.store.fail(task.id, format!("媒体恢复失败：{error}"));
                    continue;
                }
            };
            let Some(directory) = output.parent().map(Path::to_path_buf) else {
                let _ = self.store.fail(task.id, "媒体恢复失败：输出目录无效");
                continue;
            };
            let _ = self
                .start_worker(
                    Arc::clone(&service),
                    task.id,
                    task.source.clone(),
                    output,
                    UriSubmissionOptions {
                        directory: Some(directory),
                        continue_download: true,
                        ..Default::default()
                    },
                )
                .await;
        }
        Ok(())
    }

    pub async fn shutdown(&self) {
        self.accepting.store(false, Ordering::Release);
        let senders = self
            .workers
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for sender in senders {
            let _ = sender.send(WorkerCommand::Shutdown);
        }
        for _ in 0..200 {
            if self.workers.lock().await.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn start_worker(
        self: &Arc<Self>,
        service: Arc<DownloadService>,
        task_id: Uuid,
        source: String,
        output: PathBuf,
        options: UriSubmissionOptions,
    ) -> Result<(), MediaDownloadError> {
        let mut workers = self.workers.lock().await;
        if workers.contains_key(&task_id) {
            return Ok(());
        }
        let (sender, receiver) = watch::channel(WorkerCommand::Running);
        workers.insert(task_id, sender);
        drop(workers);
        let engine = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let result = engine
                .run_worker(service.api(), task_id, &source, &output, options, receiver)
                .await;
            engine.workers.lock().await.remove(&task_id);
            if let Err(error) = result {
                if let Ok(current) = engine.store.find(task_id) {
                    if !matches!(current.status, TaskStatus::Paused | TaskStatus::Cancelled) {
                        if let Ok(failed) = engine.store.fail(task_id, error.to_string()) {
                            engine.events.task_updated(&failed);
                        }
                    }
                }
            }
        });
        Ok(())
    }

    async fn run_worker(
        &self,
        api: Arc<dyn Aria2Api>,
        task_id: Uuid,
        source: &str,
        output: &Path,
        options: UriSubmissionOptions,
        mut command: watch::Receiver<WorkerCommand>,
    ) -> Result<(), MediaDownloadError> {
        if *command.borrow_and_update() != WorkerCommand::Running {
            return Ok(());
        }
        let mut task = self.store.find(task_id)?;
        task.status = TaskStatus::Downloading;
        task.error = None;
        task.download_speed = 0;
        task.connections = 0;
        task = self.persist(task, output, 0, 0)?;
        self.events.task_updated(&task);

        let plan = resolve_media_plan(source, &task.kind, &options).await?;
        if plan.segments.is_empty() {
            return Err(MediaDownloadError::InvalidManifest(
                "没有可下载的媒体分片".into(),
            ));
        }
        let work = self.data_directory.join(task_id.to_string());
        tokio::fs::create_dir_all(&work).await?;
        let mut completed_bytes = 0u64;
        let mut parts = Vec::with_capacity(plan.segments.len());

        for (index, segment) in plan.segments.iter().enumerate() {
            let part = work.join(format!("{index:08}.part"));
            parts.push(part.clone());
            let control = PathBuf::from(format!("{}.aria2", part.display()));
            if part.is_file() && !control.exists() {
                completed_bytes = completed_bytes.saturating_add(std::fs::metadata(&part)?.len());
                continue;
            }
            let mut aria_options = options
                .clone()
                .into_aria2_options()
                .map_err(|error| MediaDownloadError::InvalidManifest(error.to_string()))?;
            aria_options.insert("dir".into(), work.to_string_lossy().into_owned());
            aria_options.insert(
                "out".into(),
                part.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("segment.part")
                    .to_owned(),
            );
            aria_options.insert("continue".into(), "true".into());
            aria_options.insert("allow-overwrite".into(), "true".into());
            let gid = api
                .add_uri(vec![segment.clone()], aria_options)
                .await
                .map_err(|error| MediaDownloadError::Transfer(error.to_string()))?;
            loop {
                let status = api
                    .tell_status(&gid)
                    .await
                    .map_err(|error| MediaDownloadError::Transfer(error.to_string()))?;
                match status.status.as_str() {
                    "complete" => {
                        completed_bytes = completed_bytes
                            .saturating_add(status.completed_length.parse::<u64>().unwrap_or(0));
                        break;
                    }
                    "error" => {
                        return Err(MediaDownloadError::Transfer(
                            status
                                .error_message
                                .unwrap_or_else(|| "分片下载失败".into()),
                        ));
                    }
                    "removed" => return Ok(()),
                    _ => {}
                }
                let segment_total = status.total_length.parse::<u64>().unwrap_or(0);
                let segment_completed = status.completed_length.parse::<u64>().unwrap_or(0);
                let segment_progress = if segment_total == 0 {
                    0
                } else {
                    ((segment_completed.saturating_mul(100) / segment_total).min(100)) as usize
                };
                let scaled = ((index * 100 + segment_progress) / plan.segments.len()).min(99) as u8;
                let mut task = self.store.find(task_id)?;
                if task.status != TaskStatus::Downloading {
                    let _ = api.force_remove(&gid).await;
                    return Ok(());
                }
                task.progress = scaled;
                task.completed_length = completed_bytes.saturating_add(segment_completed);
                task.total_length = None;
                task.download_speed = status.download_speed.parse().unwrap_or(0);
                task.connections = status.connections.parse().unwrap_or(0);
                task.eta_seconds = None;
                let task = self.persist(task, output, 0, completed_bytes + segment_completed)?;
                self.events.task_updated(&task);

                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                    changed = command.changed() => {
                        if changed.is_err() || *command.borrow_and_update() != WorkerCommand::Running {
                            let _ = api.pause(&gid).await;
                            let _ = api.remove(&gid).await;
                            return Ok(());
                        }
                    }
                }
            }
        }

        let mut final_file = tokio::fs::File::create(output).await?;
        let mut total = 0u64;
        for part in &parts {
            let mut input = tokio::fs::File::open(part).await?;
            total = total.saturating_add(tokio::io::copy(&mut input, &mut final_file).await?);
        }
        final_file.flush().await?;
        let mut completed = self.store.find(task_id)?;
        completed.status = TaskStatus::Completed;
        completed.progress = 100;
        completed.error = None;
        completed.total_length = Some(total);
        completed.completed_length = total;
        completed.download_speed = 0;
        completed.connections = 0;
        completed.eta_seconds = Some(0);
        let completed = self.persist(completed, output, total, total)?;
        self.events.task_updated(&completed);
        let _ = tokio::fs::remove_dir_all(work).await;
        Ok(())
    }

    fn set_status(
        &self,
        mut task: DownloadTask,
        status: TaskStatus,
    ) -> Result<DownloadTask, MediaDownloadError> {
        task.status = status;
        task.download_speed = 0;
        task.connections = 0;
        task.eta_seconds = None;
        let output = self.output_path(&task)?;
        let saved = self.persist(
            task.clone(),
            &output,
            task.total_length.unwrap_or(0),
            task.completed_length,
        )?;
        self.events.task_updated(&saved);
        Ok(saved)
    }

    fn output_path(&self, task: &DownloadTask) -> Result<PathBuf, MediaDownloadError> {
        self.store
            .files(task.id)?
            .first()
            .map(|file| PathBuf::from(&file.path))
            .ok_or(MediaDownloadError::InvalidOption("缺少媒体输出文件路径"))
    }

    fn persist(
        &self,
        task: DownloadTask,
        output: &Path,
        total: u64,
        completed: u64,
    ) -> Result<DownloadTask, MediaDownloadError> {
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
        let sender = self.workers.lock().await.get(&task_id).cloned();
        if let Some(sender) = sender {
            let _ = sender.send(command);
            for _ in 0..100 {
                if !self.workers.lock().await.contains_key(&task_id) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

async fn resolve_media_plan(
    source: &str,
    kind: &str,
    options: &UriSubmissionOptions,
) -> Result<MediaPlan, MediaDownloadError> {
    let mut client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(8))
        .timeout(Duration::from_secs(20));
    if let Some(proxy) = options.proxy.as_deref() {
        client = client.proxy(
            reqwest::Proxy::all(proxy)
                .map_err(|_| MediaDownloadError::InvalidOption("代理地址无效"))?,
        );
    } else {
        client = client.no_proxy();
    }
    let client = client
        .build()
        .map_err(|_| MediaDownloadError::InvalidOption("无法创建媒体请求"))?;
    let mut current = source.to_owned();
    for _ in 0..4 {
        let mut request = client.get(&current);
        for (name, value) in &options.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| MediaDownloadError::InvalidOption("请求头名称无效"))?;
            let value = HeaderValue::from_str(value)
                .map_err(|_| MediaDownloadError::InvalidOption("请求头内容无效"))?;
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|_| MediaDownloadError::Transfer("无法读取媒体清单".into()))?;
        if !response.status().is_success() {
            return Err(MediaDownloadError::Transfer(format!(
                "媒体清单服务器返回 HTTP {}",
                response.status().as_u16()
            )));
        }
        let final_url = response.url().clone();
        let body = response
            .text()
            .await
            .map_err(|_| MediaDownloadError::InvalidManifest("清单不是有效文本".into()))?;
        if kind == "hls" {
            match parse_hls_manifest(&final_url, &body)? {
                HlsManifest::Master(next) => current = next,
                HlsManifest::Media(segments) => return Ok(MediaPlan { segments }),
            }
        } else {
            return Ok(MediaPlan {
                segments: parse_dash_manifest(&final_url, &body)?,
            });
        }
    }
    Err(MediaDownloadError::InvalidManifest(
        "HLS 主清单嵌套层级过深".into(),
    ))
}

enum HlsManifest {
    Master(String),
    Media(Vec<String>),
}

fn parse_hls_manifest(base: &Url, body: &str) -> Result<HlsManifest, MediaDownloadError> {
    if !body
        .lines()
        .next()
        .is_some_and(|line| line.trim() == "#EXTM3U")
    {
        return Err(MediaDownloadError::InvalidManifest("缺少 #EXTM3U".into()));
    }
    if body.lines().any(|line| {
        line.trim_start().starts_with("#EXT-X-KEY:")
            && !line.to_ascii_uppercase().contains("METHOD=NONE")
    }) {
        return Err(MediaDownloadError::InvalidManifest(
            "暂不支持加密 HLS 清单".into(),
        ));
    }
    if body
        .lines()
        .any(|line| line.trim_start().starts_with("#EXT-X-BYTERANGE:"))
    {
        return Err(MediaDownloadError::InvalidManifest(
            "暂不支持 HLS 字节范围分片".into(),
        ));
    }
    let lines = body.lines().map(str::trim).collect::<Vec<_>>();
    let mut variants = Vec::new();
    let mut segments = Vec::new();
    let map_uri = Regex::new(r#"URI="([^"]+)""#).unwrap();
    for (index, line) in lines.iter().enumerate() {
        if line.starts_with("#EXT-X-STREAM-INF:") {
            let bandwidth = line
                .split(',')
                .find_map(|part| part.trim().strip_prefix("BANDWIDTH="))
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            if let Some(uri) = lines.get(index + 1).filter(|uri| !uri.starts_with('#')) {
                variants.push((
                    bandwidth,
                    base.join(uri).map_err(invalid_relative_url)?.to_string(),
                ));
            }
        } else if line.starts_with("#EXT-X-MAP:") {
            if let Some(uri) = map_uri.captures(line).and_then(|capture| capture.get(1)) {
                segments.push(
                    base.join(uri.as_str())
                        .map_err(invalid_relative_url)?
                        .to_string(),
                );
            }
        } else if !line.is_empty() && !line.starts_with('#') {
            segments.push(base.join(line).map_err(invalid_relative_url)?.to_string());
        }
    }
    if let Some((_, selected)) = variants.into_iter().max_by_key(|(bandwidth, _)| *bandwidth) {
        return Ok(HlsManifest::Master(selected));
    }
    Ok(HlsManifest::Media(segments))
}

fn parse_dash_manifest(base: &Url, body: &str) -> Result<Vec<String>, MediaDownloadError> {
    if !body.contains("<MPD") {
        return Err(MediaDownloadError::InvalidManifest(
            "缺少 MPD 根节点".into(),
        ));
    }
    let representation = Regex::new(r"(?is)<Representation\b([^>]*)>(.*?)</Representation>")
        .expect("static DASH representation pattern");
    let bandwidth = Regex::new(r#"(?i)\bbandwidth\s*=\s*"(\d+)""#).unwrap();
    let base_url = Regex::new(r"(?is)<BaseURL[^>]*>\s*([^<]+?)\s*</BaseURL>").unwrap();
    let segment_url = Regex::new(
        r#"(?i)<(?:Initialization|SegmentURL)\b[^>]*(?:sourceURL|media)\s*=\s*"([^"]+)"[^>]*/?>"#,
    )
    .unwrap();
    let mut candidates = Vec::new();
    for capture in representation.captures_iter(body) {
        let representation_start = capture.get(0).map_or(0, |value| value.start());
        let representation_attributes = capture.get(1).map_or("", |value| value.as_str());
        let weight = bandwidth
            .captures(representation_attributes)
            .and_then(|capture| capture.get(1))
            .and_then(|value| value.as_str().parse::<u64>().ok())
            .unwrap_or(0);
        let content = capture.get(2).map_or("", |value| value.as_str());
        let representation_base = base_url
            .captures(content)
            .and_then(|capture| capture.get(1))
            .map(|value| base.join(value.as_str()).map_err(invalid_relative_url))
            .transpose()?
            .unwrap_or_else(|| base.clone());
        let mut segments = segment_url
            .captures_iter(content)
            .filter_map(|capture| capture.get(1))
            .map(|value| {
                representation_base
                    .join(value.as_str())
                    .map(|url| url.to_string())
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(invalid_relative_url)?;
        if segments.is_empty() {
            segments = expand_dash_segment_template(
                &representation_base,
                representation_attributes,
                content,
                inherited_dash_segment_template(body, representation_start).as_deref(),
                body,
            )?;
        }
        if segments.is_empty() {
            segments.extend(
                base_url
                    .captures_iter(content)
                    .filter_map(|capture| capture.get(1))
                    .map(|value| base.join(value.as_str()).map(|url| url.to_string()))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(invalid_relative_url)?,
            );
        }
        if !segments.is_empty() {
            candidates.push((weight, segments));
        }
    }
    if let Some((_, segments)) = candidates.into_iter().max_by_key(|(weight, _)| *weight) {
        return Ok(segments);
    }
    let fallback = base_url
        .captures_iter(body)
        .filter_map(|capture| capture.get(1))
        .map(|value| base.join(value.as_str()).map(|url| url.to_string()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid_relative_url)?;
    if fallback.is_empty() {
        return Err(MediaDownloadError::InvalidManifest(
            "暂不支持此 DASH 分片模板".into(),
        ));
    }
    Ok(fallback)
}

fn expand_dash_segment_template(
    base: &Url,
    representation_attributes: &str,
    representation_body: &str,
    inherited_template: Option<&str>,
    manifest: &str,
) -> Result<Vec<String>, MediaDownloadError> {
    let template = Regex::new(r"(?is)<SegmentTemplate\b([^>]*?)(?:/\s*>|>(.*?)</SegmentTemplate>)")
        .expect("static DASH template pattern");
    let Some(template) = template
        .captures(representation_body)
        .or_else(|| inherited_template.and_then(|inherited| template.captures(inherited)))
    else {
        return Ok(Vec::new());
    };
    let attributes = template.get(1).map_or("", |value| value.as_str());
    let timeline = template.get(2).map_or("", |value| value.as_str());
    let representation_id = xml_attribute(representation_attributes, "id").unwrap_or_default();
    let bandwidth = xml_attribute(representation_attributes, "bandwidth")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let initialization = xml_attribute(attributes, "initialization");
    let media = xml_attribute(attributes, "media")
        .ok_or_else(|| MediaDownloadError::InvalidManifest("DASH 分片模板缺少 media".into()))?;
    let start_number = xml_attribute(attributes, "startNumber")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1);
    let timescale = xml_attribute(attributes, "timescale")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1)
        .max(1);
    let duration = xml_attribute(attributes, "duration")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0);
    let presentation_seconds = xml_attribute(manifest, "mediaPresentationDuration")
        .as_deref()
        .and_then(parse_iso8601_duration_seconds);
    let total_ticks =
        presentation_seconds.map(|seconds| (seconds * timescale as f64).ceil() as u64);

    let mut segments = Vec::new();
    if let Some(initialization) = initialization {
        segments.push(resolve_dash_template_url(
            base,
            &initialization,
            &representation_id,
            bandwidth,
            start_number,
            0,
        )?);
    }

    let timeline_entry = Regex::new(r"(?is)<S\b([^>]*)/?>").unwrap();
    let timeline_entries = timeline_entry.captures_iter(timeline).collect::<Vec<_>>();
    if !timeline_entries.is_empty() {
        let mut number = start_number;
        let mut current_time = 0u64;
        for entry in timeline_entries {
            let attributes = entry.get(1).map_or("", |value| value.as_str());
            if let Some(time) =
                xml_attribute(attributes, "t").and_then(|value| value.parse::<u64>().ok())
            {
                current_time = time;
            }
            let segment_duration = xml_attribute(attributes, "d")
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    MediaDownloadError::InvalidManifest("DASH SegmentTimeline 缺少有效的 d".into())
                })?;
            let repeat = xml_attribute(attributes, "r")
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(0);
            let occurrences = if repeat >= 0 {
                repeat as u64 + 1
            } else if let Some(total_ticks) = total_ticks {
                total_ticks
                    .saturating_sub(current_time)
                    .div_ceil(segment_duration)
            } else {
                return Err(MediaDownloadError::InvalidManifest(
                    "DASH 无限时间线缺少可计算的总时长".into(),
                ));
            };
            append_dash_template_segments(
                &mut segments,
                base,
                &media,
                &representation_id,
                bandwidth,
                &mut number,
                &mut current_time,
                segment_duration,
                occurrences,
            )?;
        }
        return Ok(segments);
    }

    let segment_duration = duration.ok_or_else(|| {
        MediaDownloadError::InvalidManifest("DASH 分片模板缺少 duration 或时间线".into())
    })?;
    let total_ticks = total_ticks
        .ok_or_else(|| MediaDownloadError::InvalidManifest("DASH 分片模板缺少媒体总时长".into()))?;
    let occurrences = total_ticks.div_ceil(segment_duration);
    let mut number = start_number;
    let mut current_time = 0u64;
    append_dash_template_segments(
        &mut segments,
        base,
        &media,
        &representation_id,
        bandwidth,
        &mut number,
        &mut current_time,
        segment_duration,
        occurrences,
    )?;
    Ok(segments)
}

fn inherited_dash_segment_template(manifest: &str, representation_start: usize) -> Option<String> {
    let before = manifest.get(..representation_start)?;
    let adaptation_start = before.rfind("<AdaptationSet")?;
    let adaptation_prefix = manifest.get(adaptation_start..representation_start)?;
    let inherited_region = adaptation_prefix
        .find("<Representation")
        .map(|index| &adaptation_prefix[..index])
        .unwrap_or(adaptation_prefix);
    Regex::new(r"(?is)<SegmentTemplate\b[^>]*?(?:/\s*>|>.*?</SegmentTemplate>)")
        .ok()?
        .find(inherited_region)
        .map(|value| value.as_str().to_owned())
}

#[allow(clippy::too_many_arguments)]
fn append_dash_template_segments(
    segments: &mut Vec<String>,
    base: &Url,
    template: &str,
    representation_id: &str,
    bandwidth: u64,
    number: &mut u64,
    current_time: &mut u64,
    segment_duration: u64,
    occurrences: u64,
) -> Result<(), MediaDownloadError> {
    if occurrences > 100_000 || segments.len().saturating_add(occurrences as usize) > 100_000 {
        return Err(MediaDownloadError::InvalidManifest(
            "DASH 分片数量超过安全限制".into(),
        ));
    }
    for _ in 0..occurrences {
        segments.push(resolve_dash_template_url(
            base,
            template,
            representation_id,
            bandwidth,
            *number,
            *current_time,
        )?);
        *number = number.saturating_add(1);
        *current_time = current_time.saturating_add(segment_duration);
    }
    Ok(())
}

fn resolve_dash_template_url(
    base: &Url,
    template: &str,
    representation_id: &str,
    bandwidth: u64,
    number: u64,
    time: u64,
) -> Result<String, MediaDownloadError> {
    let value = template
        .replace("$RepresentationID$", representation_id)
        .replace("$Bandwidth$", &bandwidth.to_string());
    let value = replace_dash_numeric_token(&value, "Number", number);
    let value = replace_dash_numeric_token(&value, "Time", time).replace("$$", "$");
    Ok(base.join(&value).map_err(invalid_relative_url)?.to_string())
}

fn replace_dash_numeric_token(value: &str, name: &str, number: u64) -> String {
    let token = Regex::new(&format!(r"\${name}(?:%0(\d+)d)?\$")).unwrap();
    token
        .replace_all(value, |capture: &regex::Captures<'_>| {
            let width = capture
                .get(1)
                .and_then(|value| value.as_str().parse::<usize>().ok())
                .unwrap_or(0)
                .min(20);
            if width == 0 {
                number.to_string()
            } else {
                format!("{number:0width$}")
            }
        })
        .into_owned()
}

fn xml_attribute(attributes: &str, name: &str) -> Option<String> {
    Regex::new(&format!(
        r#"(?i)\b{}\s*=\s*["']([^"']+)["']"#,
        regex::escape(name)
    ))
    .ok()?
    .captures(attributes)?
    .get(1)
    .map(|value| value.as_str().to_owned())
}

fn parse_iso8601_duration_seconds(value: &str) -> Option<f64> {
    let duration =
        Regex::new(r"(?i)^PT(?:(\d+(?:\.\d+)?)H)?(?:(\d+(?:\.\d+)?)M)?(?:(\d+(?:\.\d+)?)S)?$")
            .ok()?;
    let capture = duration.captures(value)?;
    let hours = capture
        .get(1)
        .and_then(|value| value.as_str().parse::<f64>().ok())
        .unwrap_or(0.0);
    let minutes = capture
        .get(2)
        .and_then(|value| value.as_str().parse::<f64>().ok())
        .unwrap_or(0.0);
    let seconds = capture
        .get(3)
        .and_then(|value| value.as_str().parse::<f64>().ok())
        .unwrap_or(0.0);
    (hours > 0.0 || minutes > 0.0 || seconds > 0.0)
        .then_some(hours * 3600.0 + minutes * 60.0 + seconds)
}

fn invalid_relative_url(_: url::ParseError) -> MediaDownloadError {
    MediaDownloadError::InvalidManifest("分片地址无效".into())
}

fn media_output_name(source: &str, kind: &str) -> String {
    let stem = Url::parse(source)
        .ok()
        .and_then(|url| {
            url.path_segments()?.next_back().map(|name| {
                name.rsplit_once('.')
                    .map_or(name, |(stem, _)| stem)
                    .to_owned()
            })
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "media".into());
    format!("{stem}.{}", if kind == "hls" { "ts" } else { "mp4" })
}

fn valid_output_name(value: &str) -> bool {
    !value.trim().is_empty()
        && !value.contains(['\r', '\n', '/', '\\', '<', '>', ':', '"', '|', '?', '*'])
        && !value.ends_with(['.', ' '])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_master_selects_the_highest_bandwidth_variant() {
        let base = Url::parse("https://media.test/master.m3u8").unwrap();
        let manifest = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100\nlow/list.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=900\nhigh/list.m3u8\n";
        match parse_hls_manifest(&base, manifest).unwrap() {
            HlsManifest::Master(url) => {
                assert_eq!(url, "https://media.test/high/list.m3u8")
            }
            HlsManifest::Media(_) => panic!("master playlist was treated as media"),
        }
    }

    #[test]
    fn hls_media_preserves_initialization_and_segment_order() {
        let base = Url::parse("https://media.test/path/list.m3u8").unwrap();
        let manifest =
            "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:1,\none.m4s\n#EXTINF:1,\n../two.m4s\n";
        match parse_hls_manifest(&base, manifest).unwrap() {
            HlsManifest::Media(segments) => assert_eq!(
                segments,
                [
                    "https://media.test/path/init.mp4",
                    "https://media.test/path/one.m4s",
                    "https://media.test/two.m4s",
                ]
            ),
            HlsManifest::Master(_) => panic!("media playlist was treated as master"),
        }
    }

    #[test]
    fn dash_selects_highest_bandwidth_representation_and_keeps_init_first() {
        let base = Url::parse("https://media.test/path/manifest.mpd").unwrap();
        let manifest = r#"<MPD><Period><AdaptationSet>
          <Representation bandwidth="100"><BaseURL>low.mp4</BaseURL></Representation>
          <Representation bandwidth="900"><SegmentList>
            <Initialization sourceURL="init.mp4"/>
            <SegmentURL media="one.m4s"/><SegmentURL media="two.m4s"/>
          </SegmentList></Representation>
        </AdaptationSet></Period></MPD>"#;
        assert_eq!(
            parse_dash_manifest(&base, manifest).unwrap(),
            [
                "https://media.test/path/init.mp4",
                "https://media.test/path/one.m4s",
                "https://media.test/path/two.m4s",
            ]
        );
    }

    #[test]
    fn dash_expands_a_numbered_segment_template_from_static_duration() {
        let base = Url::parse("https://media.test/vod/manifest.mpd").unwrap();
        let manifest = r#"<MPD mediaPresentationDuration="PT6S"><Period><AdaptationSet>
          <Representation id="video-main" bandwidth="900">
            <SegmentTemplate timescale="1" duration="2" startNumber="7"
              initialization="init-$RepresentationID$.mp4"
              media="chunk-$Number%03d$.m4s"/>
          </Representation>
        </AdaptationSet></Period></MPD>"#;
        assert_eq!(
            parse_dash_manifest(&base, manifest).unwrap(),
            [
                "https://media.test/vod/init-video-main.mp4",
                "https://media.test/vod/chunk-007.m4s",
                "https://media.test/vod/chunk-008.m4s",
                "https://media.test/vod/chunk-009.m4s",
            ]
        );
    }

    #[test]
    fn dash_expands_an_inherited_time_timeline_template() {
        let base = Url::parse("https://media.test/vod/manifest.mpd").unwrap();
        let manifest = r#"<MPD mediaPresentationDuration="PT6S"><Period><AdaptationSet>
          <SegmentTemplate initialization="init-$RepresentationID$.mp4" media="chunk-$Time$.m4s">
            <SegmentTimeline><S t="10" d="2" r="2"/></SegmentTimeline>
          </SegmentTemplate>
          <Representation id="video-main" bandwidth="900"></Representation>
        </AdaptationSet></Period></MPD>"#;
        assert_eq!(
            parse_dash_manifest(&base, manifest).unwrap(),
            [
                "https://media.test/vod/init-video-main.mp4",
                "https://media.test/vod/chunk-10.m4s",
                "https://media.test/vod/chunk-12.m4s",
                "https://media.test/vod/chunk-14.m4s",
            ]
        );
    }

    #[test]
    fn default_media_outputs_are_playable_extensions() {
        assert_eq!(
            media_output_name("https://media.test/live/main.m3u8?token=x", "hls"),
            "main.ts"
        );
        assert_eq!(
            media_output_name("https://media.test/vod/manifest.mpd", "dash"),
            "manifest.mp4"
        );
    }
}
