use super::CloudProvider;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudUploadProgress {
    pub upload_id: String,
    pub provider: CloudProvider,
    pub phase: &'static str,
    pub total_bytes: u64,
    pub uploaded_bytes: u64,
    pub speed_bytes_per_second: f64,
    pub total_files: usize,
    pub completed_files: usize,
    pub failed_items: usize,
    pub current_files: Vec<String>,
    pub progress: f64,
}

#[derive(Clone)]
pub(crate) struct UploadProgressTracker(Arc<Mutex<ProgressState>>);

struct ProgressState {
    upload_id: String,
    control: Option<super::UploadControl>,
    provider: CloudProvider,
    phase: &'static str,
    files: BTreeMap<usize, BTreeMap<PathBuf, FileProgress>>,
    failed_inputs: BTreeSet<usize>,
    failed_items: usize,
    traffic_since_sample: u64,
}

struct FileProgress {
    size: u64,
    ranges: Vec<(u64, u64)>,
    uploaded: u64,
    completed: bool,
    active: bool,
}

impl UploadProgressTracker {
    pub fn bind_session(&self, upload_id: String, control: super::UploadControl) {
        let mut state = self.state();
        state.upload_id = upload_id;
        state.control = Some(control);
    }
    pub fn finish_cancelled(&self, failed_items: usize) {
        self.finish(failed_items);
        self.state().phase = "cancelled";
    }
    pub fn new(provider: CloudProvider) -> Self {
        Self(Arc::new(Mutex::new(ProgressState {
            upload_id: String::new(),
            control: None,
            provider,
            phase: "preparing",
            files: BTreeMap::new(),
            failed_inputs: BTreeSet::new(),
            failed_items: 0,
            traffic_since_sample: 0,
        })))
    }
    fn state(&self) -> std::sync::MutexGuard<'_, ProgressState> {
        // Progress reporting must never abort an otherwise valid upload.
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    pub fn add_file(&self, input: usize, path: &Path, size: u64) {
        self.state()
            .files
            .entry(input)
            .or_default()
            .entry(path.into())
            .or_insert(FileProgress {
                size,
                ranges: Vec::new(),
                uploaded: 0,
                completed: false,
                active: false,
            });
    }
    pub fn start_uploading(&self) {
        self.state().phase = "uploading";
    }
    pub fn start_file(&self, input: usize, path: &Path) {
        if let Some(file) = self
            .state()
            .files
            .get_mut(&input)
            .and_then(|files| files.get_mut(path))
        {
            file.active = true;
        }
    }
    pub fn record_bytes(&self, input: usize, path: &Path, offset: u64, count: u64) {
        let mut state = self.state();
        let Some(file) = state
            .files
            .get_mut(&input)
            .and_then(|files| files.get_mut(path))
        else {
            return;
        };
        let mut start = offset.min(file.size);
        let mut end = offset.saturating_add(count).min(file.size);
        if start < end && !file.completed {
            // Sorted interval union: adjacent slices merge; overlapping retries
            // increase traffic/speed but do not advance logical file progress.
            let first = file.ranges.partition_point(|range| range.1 < start);
            let mut last = first;
            while last < file.ranges.len() && file.ranges[last].0 <= end {
                start = start.min(file.ranges[last].0);
                end = end.max(file.ranges[last].1);
                file.uploaded -= file.ranges[last].1 - file.ranges[last].0;
                last += 1;
            }
            file.ranges.splice(first..last, [(start, end)]);
            file.uploaded += end - start;
        }
        state.traffic_since_sample = state.traffic_since_sample.saturating_add(count);
    }
    pub fn complete_file(&self, input: usize, path: &Path) {
        let mut state = self.state();
        if state.failed_inputs.contains(&input) {
            return;
        }
        if let Some(file) = state
            .files
            .get_mut(&input)
            .and_then(|files| files.get_mut(path))
        {
            file.completed = true;
            file.active = false;
            // Provider confirmation also covers rapid uploads and empty files.
            // It never contributes to the transfer traffic used for speed.
            file.uploaded = file.size;
            file.ranges.clear();
        }
    }
    pub fn fail_input(&self, input: usize) {
        let mut state = self.state();
        state.failed_inputs.insert(input);
        state.failed_items = state.failed_inputs.len();
        if let Some(files) = state.files.get_mut(&input) {
            for file in files.values_mut() {
                file.completed = false;
                file.active = false;
            }
        }
    }
    pub fn finish(&self, failed_items: usize) {
        let mut state = self.state();
        state.failed_items = failed_items;
        state.phase = if failed_items == 0 {
            "completed"
        } else {
            "failed"
        };
        for file in state
            .files
            .values_mut()
            .flat_map(|files| files.values_mut())
        {
            file.active = false;
        }
    }
    pub fn sample(&self, elapsed: Duration) -> CloudUploadProgress {
        let mut state = self.state();
        let mut total_bytes = 0_u64;
        let mut uploaded_bytes = 0_u64;
        let mut total_files = 0;
        let mut completed_files = 0;
        let mut current_files = Vec::new();
        for (path, file) in state.files.values().flat_map(|files| files.iter()) {
            total_bytes = total_bytes.saturating_add(file.size);
            uploaded_bytes = uploaded_bytes.saturating_add(file.uploaded);
            total_files += 1;
            completed_files += usize::from(file.completed);
            if file.active {
                current_files.push(
                    path.file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        let terminal = matches!(state.phase, "completed" | "failed" | "cancelled");
        let paused = state
            .control
            .as_ref()
            .is_some_and(|control| control.is_paused());
        let cancelling = state
            .control
            .as_ref()
            .is_some_and(|control| control.is_cancelled());
        // `traffic_since_sample` is incremented only when reqwest asks the
        // streaming body for more file bytes. Do not hide that real traffic
        // merely because a pause or cancel request raced with the sample; the
        // following sample will naturally become zero once the reader stops.
        let speed = if terminal || elapsed.is_zero() {
            0.0
        } else {
            state.traffic_since_sample as f64 / elapsed.as_secs_f64()
        };
        state.traffic_since_sample = 0;
        let progress = if state.phase == "completed" && completed_files == total_files {
            100.0
        } else if total_bytes == 0 {
            0.0
        } else {
            (uploaded_bytes as f64 / total_bytes as f64 * 100.0).min(99.0)
        };
        CloudUploadProgress {
            upload_id: state.upload_id.clone(),
            provider: state.provider,
            phase: if !terminal && cancelling {
                "cancelling"
            } else if !terminal && paused {
                "paused"
            } else if state.phase == "uploading" && total_files > 0 && uploaded_bytes == total_bytes
            {
                "finalizing"
            } else {
                state.phase
            },
            total_bytes,
            uploaded_bytes,
            speed_bytes_per_second: speed,
            total_files,
            completed_files,
            failed_items: state.failed_items,
            current_files,
            progress,
        }
    }
}

pub(crate) async fn report_upload_progress<F: Future>(
    tracker: UploadProgressTracker,
    send: impl Fn(CloudUploadProgress),
    operation: F,
) -> F::Output {
    use tokio::time::{Instant, MissedTickBehavior};
    send(tracker.sample(Duration::ZERO));
    let period = Duration::from_millis(250);
    let mut sampled_at = Instant::now();
    let mut timer = tokio::time::interval_at(sampled_at + period, period);
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    tokio::pin!(operation);
    loop {
        tokio::select! {
            result = &mut operation => {
                send(tracker.sample(Duration::ZERO));
                return result;
            }
            _ = timer.tick() => {
                let now = Instant::now();
                send(tracker.sample(now.duration_since(sampled_at)));
                sampled_at = now;
            }
        }
    }
}

#[cfg(test)]
mod upload_progress_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn upload_control_progress_reports_recent_traffic_before_a_pause_takes_effect() {
        let tracker = UploadProgressTracker::new(CloudProvider::Xunlei);
        let control = super::super::UploadControl::default();
        tracker.bind_session("fixture-upload".into(), control.clone());
        tracker.add_file(0, Path::new("file.bin"), 100);
        tracker.start_uploading();
        tracker.record_bytes(0, Path::new("file.bin"), 0, 20);
        control.pause().unwrap();
        let paused = tracker.sample(Duration::from_millis(250));
        assert_eq!(paused.phase, "paused");
        assert_eq!(paused.uploaded_bytes, 20);
        assert_eq!(paused.speed_bytes_per_second, 80.0);
        assert_eq!(
            serde_json::to_value(&paused).unwrap()["uploadId"],
            "fixture-upload"
        );
        control.resume().unwrap();
        let resumed = tracker.sample(Duration::from_millis(250));
        assert_eq!(resumed.phase, "uploading");
        assert_eq!(resumed.speed_bytes_per_second, 0.0);
        control.cancel();
        assert_eq!(tracker.sample(Duration::ZERO).phase, "cancelling");
        tracker.finish_cancelled(0);
        let cancelled = tracker.sample(Duration::ZERO);
        assert_eq!(cancelled.phase, "cancelled");
        assert_eq!(cancelled.failed_items, 0);
    }

    #[test]
    fn upload_progress_unions_overlapping_parts_and_counts_retry_traffic_only_for_speed() {
        let tracker = UploadProgressTracker::new(CloudProvider::Xunlei);
        let path = Path::new("payload.bin");
        tracker.add_file(0, path, 100);
        tracker.start_uploading();
        tracker.start_file(0, path);
        tracker.record_bytes(0, path, 40, 60);
        tracker.record_bytes(0, path, 0, 60);
        tracker.record_bytes(0, path, 0, 100);
        let event = tracker.sample(Duration::from_secs(1));
        assert_eq!(event.uploaded_bytes, 100);
        assert_eq!(event.speed_bytes_per_second, 220.0);
        assert_eq!(event.current_files, ["payload.bin"]);
        assert_eq!(event.completed_files, 0);
        assert!(event.progress < 100.0);
        assert_eq!(event.phase, "finalizing");
        tracker.complete_file(0, path);
        tracker.finish(0);
        let event = tracker.sample(Duration::from_secs(1));
        assert_eq!(event.progress, 100.0);
        assert_eq!(event.phase, "completed");
        assert_eq!(event.completed_files, 1);
        assert!(event.current_files.is_empty());
    }

    #[test]
    fn upload_progress_confirms_rapid_uploads_and_empty_files_without_fabricating_speed() {
        let tracker = UploadProgressTracker::new(CloudProvider::Xunlei);
        tracker.add_file(0, Path::new("rapid.bin"), 500);
        tracker.add_file(1, Path::new("empty.bin"), 0);
        tracker.start_uploading();
        tracker.complete_file(0, Path::new("rapid.bin"));
        let pending = tracker.sample(Duration::from_secs(1));
        assert_eq!(pending.uploaded_bytes, 500);
        assert_eq!(pending.completed_files, 1);
        assert_eq!(pending.total_files, 2);
        assert_eq!(pending.speed_bytes_per_second, 0.0);
        assert!(pending.progress < 100.0);
        tracker.complete_file(1, Path::new("empty.bin"));
        tracker.finish(0);
        assert_eq!(tracker.sample(Duration::from_secs(1)).completed_files, 2);
    }

    #[test]
    fn upload_progress_rolls_back_completed_files_of_failed_directory_without_losing_other_inputs()
    {
        let tracker = UploadProgressTracker::new(CloudProvider::Xunlei);
        let path = Path::new("shared-name.bin");
        tracker.add_file(0, path, 10);
        tracker.add_file(1, path, 10);
        tracker.start_uploading();
        tracker.complete_file(0, path);
        tracker.complete_file(1, path);
        tracker.fail_input(1);
        tracker.fail_input(1);
        tracker.finish(1);
        let event = tracker.sample(Duration::from_secs(1));
        assert_eq!(event.completed_files, 1);
        assert_eq!(event.failed_items, 1);
        assert_eq!(event.phase, "failed");
        assert!(event.progress < 100.0);
        assert_eq!(event.speed_bytes_per_second, 0.0);
    }

    #[tokio::test(start_paused = true)]
    async fn upload_progress_emits_preparing_periodic_zero_speed_and_terminal_in_order() {
        let tracker = UploadProgressTracker::new(CloudProvider::Xunlei);
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        report_upload_progress(
            tracker.clone(),
            move |event| sink.lock().unwrap().push(event),
            async {
                tracker.add_file(0, Path::new("small.bin"), 2);
                tracker.start_uploading();
                tracker.record_bytes(0, Path::new("small.bin"), 0, 1);
                tokio::time::sleep(Duration::from_millis(650)).await;
                tracker.fail_input(0);
                tracker.finish(1);
                42
            },
        )
        .await;
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].phase, "preparing");
        assert_eq!(events[1].speed_bytes_per_second, 4.0);
        assert_eq!(events[2].speed_bytes_per_second, 0.0);
        assert_eq!(events[3].phase, "failed");
        assert_eq!(events[3].speed_bytes_per_second, 0.0);
    }
}
