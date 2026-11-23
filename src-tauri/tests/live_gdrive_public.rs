//! Opt-in check of public Google Drive links against the real service and the
//! real aria2 engine (run with `--ignored`). Only public, openly licensed
//! content may be used. Environment:
//!
//! * `VERSADOWN_LIVE_GDRIVE_FILE`  a small public file link (downloaded fully)
//! * `VERSADOWN_LIVE_GDRIVE_LARGE` a public file above the virus-scan limit
//!   (only the first few MiB are downloaded, then the task is removed)
//! * `VERSADOWN_LIVE_GDRIVE_FOLDER` a public folder link (listing only)
//! * `VERSADOWN_LIVE_GDRIVE_DOC`   a public Google document link (exported)
//! * `VERSADOWN_LIVE_GDRIVE_PRIVATE` a link that needs sign-in (access denied)
//! * `VERSADOWN_LIVE_GDRIVE_MISSING` a link to a file that does not exist
//!
//! Unset variables skip their check. No link or token is printed.
#![cfg(any(windows, target_os = "macos", target_os = "linux"))]

use std::{collections::BTreeMap, path::PathBuf, time::Duration};
use versadown_lib::aria2::{Aria2Api, Aria2LaunchConfig, Aria2Process, Aria2RpcClient};
use versadown_lib::cloud::share::{
    provider_share_adapter, CloudSharePlan, CloudShareResolvedEntry, ShareInspect,
    ShareInspection, ShareResolution, ShareResolve,
};
use versadown_lib::cloud::gdrive::GdriveFailure;
use versadown_lib::cloud::{CloudProvider, MemoryCredentialStore, QuarkError, ReqwestTransport};
use versadown_lib::engine_binary::EngineBinary;
use versadown_lib::{extract_cloud_share_candidate, CloudShareCandidate};

const PARTIAL_TARGET_BYTES: u64 = 3 * 1024 * 1024;

fn link(variable: &str) -> Option<CloudShareCandidate> {
    let value = std::env::var(variable).ok()?;
    let candidate = extract_cloud_share_candidate(&value, None)
        .expect("a valid share input")
        .expect("a share link");
    assert_eq!(candidate.provider, CloudProvider::Gdrive, "{variable}");
    Some(candidate)
}

fn inspect(http: &ReqwestTransport, candidate: &CloudShareCandidate) -> CloudSharePlan {
    let credentials = MemoryCredentialStore::default();
    let adapter = provider_share_adapter(CloudProvider::Gdrive, &credentials, http).unwrap();
    match adapter.inspect(candidate).expect("inspection succeeds") {
        ShareInspection::Plan(plan) => plan,
        ShareInspection::AccessCodeRequired => panic!("no access code expected"),
    }
}

fn resolve_first(http: &ReqwestTransport, plan: &CloudSharePlan) -> (String, String) {
    let credentials = MemoryCredentialStore::default();
    let adapter = provider_share_adapter(CloudProvider::Gdrive, &credentials, http).unwrap();
    let entry = plan.entries[0].clone();
    match adapter
        .resolve(&CloudShareResolvedEntry {
            provider: CloudProvider::Gdrive,
            plan: plan.clone(),
            entry: entry.clone(),
        })
        .expect("resolution succeeds")
    {
        ShareResolution::Direct(download) => {
            assert!(download.headers.is_empty(), "no credentials for public links");
            (download.url, entry.name)
        }
        ShareResolution::TransferRequired(_) => panic!("Google never transfers"),
    }
}

struct Engine {
    process: Option<Aria2Process>,
    client: Aria2RpcClient,
    directory: PathBuf,
    _root: tempfile::TempDir,
}

async fn engine() -> Engine {
    let root = tempfile::tempdir().unwrap();
    let mut aria2 = versadown_lib::Aria2Settings::default();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    aria2.rpc_port = listener.local_addr().unwrap().port();
    aria2.rpc_token = format!("gdrive-live-{}", aria2.rpc_port);
    drop(listener);
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let binary = EngineBinary::for_current_target()
        .unwrap()
        .resolve(&manifest, &manifest)
        .unwrap();
    let directory = root.path().join("downloads");
    let (process, client) = Aria2Process::start(Aria2LaunchConfig {
        binary,
        data_dir: root.path().join("data"),
        download_dir: directory.clone(),
        parent_pid: std::process::id(),
        peer_discovery: false,
        aria2,
    })
    .await
    .unwrap();
    Engine {
        process: Some(process),
        client,
        directory,
        _root: root,
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(process) = &mut self.process {
            let _ = process.terminate_fallback();
        }
    }
}

impl Engine {
    /// Download through aria2 as the application does (4 connections) until
    /// `stop_after` bytes arrived or the file completed. Returns the bytes
    /// completed and the output path.
    async fn fetch(&self, url: String, name: &str, stop_after: Option<u64>) -> (u64, PathBuf) {
        let options = BTreeMap::from([
            ("dir".to_owned(), self.directory.to_string_lossy().into_owned()),
            ("out".to_owned(), name.to_owned()),
            ("split".to_owned(), "4".to_owned()),
            ("max-connection-per-server".to_owned(), "4".to_owned()),
            ("min-split-size".to_owned(), "1M".to_owned()),
            ("file-allocation".to_owned(), "none".to_owned()),
            ("pause".to_owned(), "false".to_owned()),
        ]);
        let gid = self.client.add_uri(vec![url], options).await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        loop {
            let status = self.client.tell_status(&gid).await.unwrap();
            assert_ne!(status.status, "error", "aria2 error {:?}", status.error_code);
            let completed: u64 = status.completed_length.parse().unwrap_or(0);
            if status.status == "complete" {
                return (completed, self.directory.join(name));
            }
            if stop_after.is_some_and(|limit| completed >= limit) {
                let _ = self.client.force_remove(&gid).await;
                return (completed, self.directory.join(name));
            }
            assert!(tokio::time::Instant::now() < deadline, "download timed out");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

// The share adapters use the blocking transport, so the test itself is
// synchronous and only drives aria2 through the runtime.
#[test]
#[ignore = "talks to Google Drive and needs VERSADOWN_LIVE_GDRIVE_* links"]
fn public_google_drive_links_download_through_aria2() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let http = ReqwestTransport::new().unwrap();
    let engine = runtime.block_on(engine());
    let mut checked = 0;

    if let Some(candidate) = link("VERSADOWN_LIVE_GDRIVE_FILE") {
        let plan = inspect(&http, &candidate);
        assert_eq!(plan.entries.len(), 1);
        let size = plan.entries[0].size.expect("a file has a size");
        let (url, name) = resolve_first(&http, &plan);
        let (completed, path) = runtime.block_on(engine.fetch(url, &name, None));
        assert_eq!(completed, size);
        assert_eq!(std::fs::metadata(path).unwrap().len(), size);
        println!("file: {name} {size} bytes downloaded completely");
        checked += 1;
    }

    if let Some(candidate) = link("VERSADOWN_LIVE_GDRIVE_LARGE") {
        let plan = inspect(&http, &candidate);
        let size = plan.entries[0].size.expect("a file has a size");
        assert!(size > 100 * 1024 * 1024, "the link must exceed the scan limit");
        let (url, name) = resolve_first(&http, &plan);
        assert!(url.contains("confirm="), "the confirmation form was followed");
        let (completed, _) = runtime.block_on(engine.fetch(url, &name, Some(PARTIAL_TARGET_BYTES)));
        assert!(completed >= PARTIAL_TARGET_BYTES);
        println!("large: {name} {size} bytes total, {completed} bytes fetched");
        checked += 1;
    }

    if let Some(candidate) = link("VERSADOWN_LIVE_GDRIVE_DOC") {
        let plan = inspect(&http, &candidate);
        let (url, name) = resolve_first(&http, &plan);
        let (completed, path) = runtime.block_on(engine.fetch(url, &name, None));
        assert!(completed > 0 && path.is_file());
        println!("document: {name} exported, {completed} bytes");
        checked += 1;
    }

    if let Some(candidate) = link("VERSADOWN_LIVE_GDRIVE_FOLDER") {
        let plan = inspect(&http, &candidate);
        assert!(!plan.entries.is_empty());
        let nested = plan
            .entries
            .iter()
            .filter(|entry| !entry.relative_directory.is_empty())
            .count();
        println!("folder: {} files, {nested} in subfolders", plan.entries.len());
        // One file of the tree goes all the way through the engine.
        let small = plan
            .entries
            .iter()
            .find(|entry| entry.name == "spam.txt")
            .or_else(|| plan.entries.iter().find(|entry| entry.name.ends_with(".txt")))
            .expect("the folder holds a text file")
            .clone();
        let credentials = MemoryCredentialStore::default();
        let adapter = provider_share_adapter(CloudProvider::Gdrive, &credentials, &http).unwrap();
        let ShareResolution::Direct(download) = adapter
            .resolve(&CloudShareResolvedEntry {
                provider: CloudProvider::Gdrive,
                plan: plan.clone(),
                entry: small.clone(),
            })
            .unwrap()
        else {
            panic!("direct download expected")
        };
        let (completed, path) = runtime.block_on(engine.fetch(download.url, &small.name, None));
        assert!(path.is_file());
        println!("folder file: {} {completed} bytes", small.name);
        checked += 1;
    }

    for (variable, expected) in [
        ("VERSADOWN_LIVE_GDRIVE_PRIVATE", GdriveFailure::AccessDenied),
        ("VERSADOWN_LIVE_GDRIVE_MISSING", GdriveFailure::NotFound),
    ] {
        if let Some(candidate) = link(variable) {
            let credentials = MemoryCredentialStore::default();
            let adapter =
                provider_share_adapter(CloudProvider::Gdrive, &credentials, &http).unwrap();
            match adapter.inspect(&candidate) {
                Err(QuarkError::Gdrive(failure)) => assert_eq!(failure, expected, "{variable}"),
                other => panic!("{variable}: expected {expected:?}, got ok={}", other.is_ok()),
            }
            println!("{variable}: reported as {expected:?}");
            checked += 1;
        }
    }

    assert!(checked > 0, "set at least one VERSADOWN_LIVE_GDRIVE_* variable");
}
