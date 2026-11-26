//! Reporting for a failed application start.
//!
//! A setup failure used to end the process without a trace: the error was
//! dropped, no window existed yet, and the process exited with code 0. The
//! error chain is now written to stderr and to a bounded log in the profile,
//! and the desktop shows a native dialog once the event loop has ended.
use crate::native_menu_labels::{NativeMenuLabelStore, NativeMenuLabels};
use std::error::Error;
use std::io::Write;
use std::path::{Path, PathBuf};

const LOG_DIRECTORY: &str = "logs";
const LOG_FILE: &str = "startup-error.log";
/// Only the most recent failures are of interest.
const MAX_LOG_ENTRIES: usize = 20;
const MAX_LOG_BYTES: usize = 32 * 1024;
const MAX_ENTRY_CHARS: usize = 2000;
const MAX_SUMMARY_CHARS: usize = 300;

/// Exit status the Agent clients (`vd-cli`, `vd-mcp-server`) read from a host
/// they started: 0 asks them to keep waiting for a host another process is
/// bringing up, 4 reports that Agent access is disabled, anything else means
/// the host is unavailable.
const EXIT_HOST_STARTING_ELSEWHERE: i32 = 0;
const EXIT_AGENT_ACCESS_DISABLED: i32 = 4;
const EXIT_STARTUP_FAILED: i32 = 1;

/// Why setup stopped, as far as the exit status is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// Another process holds the profile lease.
    ProfileBusy,
    /// An Agent host was started while Agent access is off.
    AgentAccessDisabled,
    Other,
}

pub fn failure_exit_code(agent_host: bool, kind: FailureKind) -> i32 {
    match (agent_host, kind) {
        (true, FailureKind::ProfileBusy) => EXIT_HOST_STARTING_ELSEWHERE,
        (true, FailureKind::AgentAccessDisabled) => EXIT_AGENT_ACCESS_DISABLED,
        _ => EXIT_STARTUP_FAILED,
    }
}

/// `Tauri::run_return` reports 0 for every orderly exit: the runtime handles
/// `exit(code)` by stopping the loop with `ControlFlow::Exit`, so the requested
/// code only ever reaches the `ExitRequested` event. A non-zero loop status
/// (an abnormal loop end) still wins.
pub fn resolve_exit_code(loop_code: i32, requested: Option<i32>) -> i32 {
    if loop_code != 0 {
        loop_code
    } else {
        requested.unwrap_or(0)
    }
}

/// Agent hosts have no UI and background UI tests run unattended, so neither
/// may block on a dialog; both still get stderr and the log.
pub fn shows_dialog(agent_host: bool, background_ui_test: bool) -> bool {
    !agent_host && !background_ui_test
}

/// The messages of an error and of every error it wraps, outermost first.
pub fn error_chain(error: &(dyn Error + 'static)) -> Vec<String> {
    let mut chain: Vec<String> = Vec::new();
    let mut current: Option<&(dyn Error + 'static)> = Some(error);
    while let Some(error) = current {
        let message = error.to_string();
        // A wrapper that already prints its source would repeat it.
        if chain.last() != Some(&message) {
            chain.push(message);
        }
        current = error.source();
    }
    chain
}

pub fn format_chain(chain: &[String]) -> String {
    chain.join(": ")
}

fn single_line(text: &str, limit: usize) -> String {
    let flat: String = text
        .chars()
        .map(|character| if character.is_control() { ' ' } else { character })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let kept: String = flat.chars().take(limit.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn log_entry(timestamp: &str, version: &str, chain: &str) -> String {
    let message = single_line(chain, MAX_ENTRY_CHARS);
    format!("{timestamp} VersaDown {version} startup failed: {message}")
}

/// Appends one entry and drops the oldest ones beyond the entry and size
/// limits; the newest entry always survives.
fn trimmed_log(existing: &str, entry: &str) -> String {
    let mut entries: Vec<&str> = existing.lines().filter(|line| !line.trim().is_empty()).collect();
    entries.push(entry);
    let first = entries.len().saturating_sub(MAX_LOG_ENTRIES);
    let mut kept = &entries[first..];
    let size = |entries: &[&str]| entries.iter().map(|line| line.len() + 1).sum::<usize>();
    while kept.len() > 1 && size(kept) > MAX_LOG_BYTES {
        kept = &kept[1..];
    }
    let mut text = kept.join("\n");
    text.push('\n');
    text
}

pub fn log_path(data_dir: &Path) -> PathBuf {
    data_dir.join(LOG_DIRECTORY).join(LOG_FILE)
}

fn append_log(data_dir: &Path, entry: &str) -> std::io::Result<()> {
    let path = log_path(data_dir);
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let temporary = path.with_extension("log.tmp");
    std::fs::write(&temporary, trimmed_log(&existing, entry))?;
    std::fs::rename(temporary, path)
}

fn timestamp_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown-time".to_string())
}

/// What the desktop shows after a failed start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupReport {
    pub summary: String,
    pub log_path: PathBuf,
}

/// Writes the chain to `stderr` and the profile's startup log. The log is best
/// effort: a profile that cannot be written must not hide the failure itself.
pub fn record(
    data_dir: &Path,
    version: &str,
    timestamp: &str,
    error: &(dyn Error + 'static),
    stderr: &mut dyn Write,
) -> StartupReport {
    let chain = format_chain(&error_chain(error));
    let _ = writeln!(stderr, "VersaDown {version} failed to start: {}", single_line(&chain, MAX_ENTRY_CHARS));
    if let Err(write_error) = append_log(data_dir, &log_entry(timestamp, version, &chain)) {
        let _ = writeln!(stderr, "Unable to write the startup error log: {write_error}");
    }
    StartupReport {
        summary: single_line(&chain, MAX_SUMMARY_CHARS),
        log_path: log_path(data_dir),
    }
}

pub fn record_now(data_dir: &Path, error: &(dyn Error + 'static)) -> StartupReport {
    record(
        data_dir,
        &crate::app_version(),
        &timestamp_now(),
        error,
        &mut std::io::stderr(),
    )
}

/// Title and body of the failure dialog from the last labels the renderer
/// pushed (cached in the profile); English before the first push.
pub fn dialog_text(labels: &NativeMenuLabels, report: &StartupReport) -> (String, String) {
    let body = labels
        .startup_failed_body
        .replace("{summary}", &report.summary)
        .replace("{logPath}", &report.log_path.display().to_string());
    (labels.startup_failed_title.clone(), body)
}

/// Shows the failure dialog and waits for it to be dismissed.
///
/// This must run after the event loop has ended, never from `setup`: setup runs
/// inside the loop's callback on the main thread, so a blocking dialog there
/// either freezes the loop (`tauri_plugin_dialog`'s `blocking_show` dispatches
/// to the main thread and waits for it) or, with GTK, waits for a main context
/// the callback still owns. Once the loop has returned, the main thread is free
/// to run the dialog's own nested loop.
pub fn present(data_dir: &Path, report: &StartupReport) {
    let (title, body) = dialog_text(&NativeMenuLabelStore::open(data_dir).current(), report);
    show_native_error(&title, &body);
}

/// GTK is already initialised on the main thread by the event loop, so the
/// dialog runs there directly. rfd would hand it to a separate GTK thread,
/// which never gets to display it once the main thread has stopped iterating.
/// Without an initialised GTK (no display) there is nothing to show.
#[cfg(all(unix, not(target_os = "macos")))]
fn show_native_error(title: &str, body: &str) {
    use gtk::prelude::*;
    if !gtk::is_initialized() {
        return;
    }
    let dialog = gtk::MessageDialog::new(
        None::<&gtk::Window>,
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Error,
        gtk::ButtonsType::Ok,
        body,
    );
    dialog.set_title(title);
    dialog.set_position(gtk::WindowPosition::Center);
    dialog.run();
    dialog.close();
}

/// Win32 `MessageBox` and `NSAlert` both run on the calling thread.
#[cfg(any(target_os = "macos", windows))]
fn show_native_error(title: &str, body: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(title)
        .set_description(body)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    #[derive(Debug)]
    struct Wrapper {
        message: &'static str,
        source: Option<Box<dyn Error + 'static>>,
    }

    impl fmt::Display for Wrapper {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.message)
        }
    }

    impl Error for Wrapper {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.source.as_deref()
        }
    }

    fn nested() -> Wrapper {
        Wrapper {
            message: "cannot open the task store",
            source: Some(Box::new(Wrapper {
                message: "file is not a database",
                source: Some(Box::new(std::io::Error::other("disk"))),
            })),
        }
    }

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn english_report() -> StartupReport {
        StartupReport {
            summary: "boom".into(),
            log_path: PathBuf::from("/data/logs/startup-error.log"),
        }
    }

    #[test]
    fn the_chain_lists_every_source_outermost_first() {
        assert_eq!(
            error_chain(&nested()),
            ["cannot open the task store", "file is not a database", "disk"]
        );
        assert_eq!(
            format_chain(&error_chain(&nested())),
            "cannot open the task store: file is not a database: disk"
        );
    }

    #[test]
    fn a_source_repeated_by_its_wrapper_is_listed_once() {
        let error = Wrapper {
            message: "same",
            source: Some(Box::new(Wrapper { message: "same", source: None })),
        };
        assert_eq!(error_chain(&error), ["same"]);
    }

    #[test]
    fn an_entry_is_one_line_with_time_and_version() {
        let entry = log_entry("2026-01-02T03:04:05Z", "1.2.3", "first\nsecond\u{7}\tthird");
        assert_eq!(
            entry,
            "2026-01-02T03:04:05Z VersaDown 1.2.3 startup failed: first second third"
        );
    }

    #[test]
    fn an_overlong_message_is_truncated() {
        let entry = log_entry("t", "v", &"x".repeat(MAX_ENTRY_CHARS * 2));
        assert!(entry.chars().count() < MAX_ENTRY_CHARS + 40);
        assert!(entry.ends_with('…'));
    }

    #[test]
    fn the_log_keeps_only_the_latest_entries() {
        let mut log = String::new();
        for index in 0..MAX_LOG_ENTRIES + 5 {
            log = trimmed_log(&log, &format!("entry {index}"));
        }
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), MAX_LOG_ENTRIES);
        assert_eq!(lines[0], "entry 5");
        assert_eq!(*lines.last().unwrap(), format!("entry {}", MAX_LOG_ENTRIES + 4));
    }

    #[test]
    fn the_log_stays_within_its_size_limit_and_keeps_the_newest_entry() {
        let big = "y".repeat(MAX_ENTRY_CHARS);
        let mut log = String::new();
        for index in 0..MAX_LOG_ENTRIES {
            log = trimmed_log(&log, &format!("{index} {big}"));
        }
        assert!(log.len() <= MAX_LOG_BYTES, "{}", log.len());
        let newest = format!("{} {big}", MAX_LOG_ENTRIES - 1);
        assert_eq!(log.lines().last(), Some(newest.as_str()));
        assert!(log.lines().count() < MAX_LOG_ENTRIES);
    }

    #[test]
    fn recording_writes_stderr_and_a_bounded_log_under_the_profile() {
        let directory = scratch();
        let mut stderr = Vec::new();
        let report = record(directory.path(), "9.9.9", "2026-01-02T03:04:05Z", &nested(), &mut stderr);

        let stderr = String::from_utf8(stderr).unwrap();
        assert!(stderr.contains("9.9.9"), "{stderr}");
        assert!(stderr.contains("cannot open the task store: file is not a database: disk"));
        let path = directory.path().join("logs").join("startup-error.log");
        assert_eq!(report.log_path, path);
        let log = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            log,
            "2026-01-02T03:04:05Z VersaDown 9.9.9 startup failed: cannot open the task store: file is not a database: disk\n"
        );

        for _ in 0..MAX_LOG_ENTRIES * 2 {
            record(directory.path(), "9.9.9", "t", &nested(), &mut Vec::new());
        }
        let log = std::fs::read_to_string(&path).unwrap();
        assert_eq!(log.lines().count(), MAX_LOG_ENTRIES);
        assert!(!path.with_extension("log.tmp").exists());
    }

    #[test]
    fn an_unwritable_profile_still_reports_on_stderr() {
        let directory = scratch();
        let blocker = directory.path().join("profile");
        std::fs::write(&blocker, b"a file, not a directory").unwrap();
        let mut stderr = Vec::new();
        let report = record(&blocker, "1.0.0", "t", &nested(), &mut stderr);
        let stderr = String::from_utf8(stderr).unwrap();
        assert!(stderr.contains("failed to start"), "{stderr}");
        assert!(stderr.contains("Unable to write the startup error log"), "{stderr}");
        assert_eq!(report.summary, "cannot open the task store: file is not a database: disk");
    }

    #[test]
    fn the_dialog_summary_is_bounded() {
        let directory = scratch();
        let long = std::io::Error::other("z".repeat(MAX_SUMMARY_CHARS * 3));
        let report = record(directory.path(), "1", "t", &long, &mut Vec::new());
        assert_eq!(report.summary.chars().count(), MAX_SUMMARY_CHARS);
    }

    #[test]
    fn the_dialog_text_falls_back_to_english_without_a_cache() {
        let directory = scratch();
        let labels = NativeMenuLabelStore::open(directory.path()).current();
        let (title, body) = dialog_text(&labels, &english_report());
        assert_eq!(title, "VersaDown failed to start");
        assert!(body.contains("Reason: boom"), "{body}");
        assert!(body.contains("/data/logs/startup-error.log"), "{body}");
        assert!(!body.contains('{'), "{body}");
    }

    #[test]
    fn the_dialog_text_uses_the_cached_renderer_language() {
        let directory = scratch();
        let store = NativeMenuLabelStore::open(directory.path());
        let mut chinese = NativeMenuLabels::fallback();
        chinese.startup_failed_title = "VersaDown 启动失败".into();
        chinese.startup_failed_body = "原因：{summary}\n日志：{logPath}".into();
        store.update(chinese).unwrap();

        let labels = NativeMenuLabelStore::open(directory.path()).current();
        let (title, body) = dialog_text(&labels, &english_report());
        assert_eq!(title, "VersaDown 启动失败");
        assert_eq!(body, "原因：boom\n日志：/data/logs/startup-error.log");
    }

    #[test]
    fn only_the_attended_desktop_shows_a_dialog() {
        assert!(shows_dialog(false, false));
        assert!(!shows_dialog(true, false));
        assert!(!shows_dialog(false, true));
        assert!(!shows_dialog(true, true));
    }

    #[test]
    fn setup_failures_exit_non_zero_except_for_the_agent_wait_signal() {
        for kind in [FailureKind::ProfileBusy, FailureKind::AgentAccessDisabled, FailureKind::Other] {
            assert_eq!(failure_exit_code(false, kind), 1, "{kind:?}");
        }
        assert_eq!(failure_exit_code(true, FailureKind::ProfileBusy), 0);
        assert_eq!(failure_exit_code(true, FailureKind::AgentAccessDisabled), 4);
        assert_eq!(failure_exit_code(true, FailureKind::Other), 1);
    }

    #[test]
    fn the_requested_exit_code_survives_an_event_loop_that_reports_zero() {
        assert_eq!(resolve_exit_code(0, Some(1)), 1);
        assert_eq!(resolve_exit_code(0, Some(4)), 4);
        assert_eq!(resolve_exit_code(0, Some(0)), 0);
        assert_eq!(resolve_exit_code(0, None), 0);
        assert_eq!(resolve_exit_code(1, Some(0)), 1);
        assert_eq!(resolve_exit_code(1, None), 1);
    }
}
