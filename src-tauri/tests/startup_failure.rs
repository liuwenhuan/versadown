//! Starts the real binary in the isolated background-UI-test mode, on a private
//! X server and with a private HOME, and checks how a failing and a healthy
//! profile end. Nothing here touches the user's desktop or configuration.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const EXIT_TIMEOUT: Duration = Duration::from_secs(90);
const SETTLE_TIME: Duration = Duration::from_secs(8);

/// A private X server, so the hidden test windows never reach a real display.
struct PrivateDisplay {
    server: Child,
    name: String,
}

impl PrivateDisplay {
    fn start() -> Option<Self> {
        let mut server = Command::new("Xvfb")
            .args(["-displayfd", "1", "-screen", "0", "800x600x24", "-nolisten", "tcp"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut number = String::new();
        BufReader::new(server.stdout.take()?).read_line(&mut number).ok()?;
        let number = number.trim();
        if number.is_empty() {
            let _ = server.kill();
            return None;
        }
        Some(Self { server, name: format!(":{number}") })
    }
}

impl Drop for PrivateDisplay {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
    }
}

/// The application in its own process group, so that the engine it starts
/// disappears with it.
struct Launched {
    child: Child,
}

impl Launched {
    fn start(display: &PrivateDisplay, home: &Path, profile: &Path) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_versadown"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("DISPLAY", &display.name)
            .env("GDK_BACKEND", "x11")
            .env("HOME", home)
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            // No session bus: the credential store and tray must not reach the
            // user's keyring or panel.
            .env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={}", home.join("no-bus").display()))
            .env("VERSADOWN_BACKGROUND_UI_TEST", "1")
            .env("VERSADOWN_TEST_DATA_DIR", profile)
            .env("VERSADOWN_TEST_DOWNLOAD_DIR", home.join("downloads"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("the versadown binary starts");
        Self { child }
    }

    fn stderr_text(&mut self) -> String {
        let mut text = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_string(&mut text);
        }
        text
    }
}

impl Drop for Launched {
    fn drop(&mut self) {
        // SAFETY: signals the process group created for this child.
        unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
        let _ = self.child.wait();
    }
}

fn scratch(name: &str) -> tempfile::TempDir {
    tempfile::Builder::new().prefix(&format!("versadown-{name}-")).tempdir().unwrap()
}

#[test]
fn a_profile_that_cannot_be_opened_ends_with_an_error_status_a_message_and_a_log() {
    let Some(display) = PrivateDisplay::start() else {
        eprintln!("skipped: Xvfb is not available");
        return;
    };
    let home = scratch("failing-home");
    let profile = home.path().join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("tasks.sqlite"), b"this is not a sqlite database").unwrap();

    let mut app = Launched::start(&display, home.path(), &profile);
    let started = Instant::now();
    let status = loop {
        if let Some(status) = app.child.try_wait().unwrap() {
            break status;
        }
        assert!(started.elapsed() < EXIT_TIMEOUT, "the application kept running after a failed setup");
        std::thread::sleep(Duration::from_millis(100));
    };
    let stderr = app.stderr_text();

    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("failed to start"), "stderr: {stderr}");
    let log = std::fs::read_to_string(profile.join("logs").join("startup-error.log"))
        .expect("the startup error log is written");
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(log.contains(&format!("VersaDown {}", env!("CARGO_PKG_VERSION"))), "{log}");
    assert!(log.contains("startup failed:"), "{log}");
    assert!(log.contains("not a database"), "{log}");
}

#[test]
fn an_empty_profile_starts_and_keeps_running() {
    let Some(display) = PrivateDisplay::start() else {
        eprintln!("skipped: Xvfb is not available");
        return;
    };
    let home = scratch("healthy-home");
    let profile = home.path().join("profile");
    std::fs::create_dir_all(&profile).unwrap();

    let mut app = Launched::start(&display, home.path(), &profile);
    // Setup opens the task database early; a failing setup ends within a
    // moment of that, so surviving this window means it completed.
    let setup_reached = profile.join("tasks.sqlite");
    let started = Instant::now();
    while !setup_reached.exists() {
        if let Some(status) = app.child.try_wait().unwrap() {
            panic!("the application exited with {status} during startup: {}", app.stderr_text());
        }
        assert!(started.elapsed() < EXIT_TIMEOUT, "setup did not finish");
        std::thread::sleep(Duration::from_millis(200));
    }
    std::thread::sleep(SETTLE_TIME);
    assert!(
        app.child.try_wait().unwrap().is_none(),
        "the application stopped after starting: {}",
        app.stderr_text()
    );
    assert!(!profile.join("logs").join("startup-error.log").exists());
}
