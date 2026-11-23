//! Opt-in checks of real public OneDrive share links through the real
//! pipeline: the anonymous share adapter, then the bundled aria2 engine.
//!
//! Required environment (use only links whose content you may download):
//!   VERSADOWN_ONEDRIVE_TEST_FILE_LINK    a public single-file link
//!   VERSADOWN_ONEDRIVE_TEST_FOLDER_LINK  a public folder link (listing, then
//!                                        one file of it is downloaded)
//! Optional:
//!   VERSADOWN_ONEDRIVE_TEST_FILE_SHA1    expected SHA-1 (hex) of the file
//!   VERSADOWN_ONEDRIVE_TEST_MAX_BYTES    skip folder entries larger than this
//!                                        when picking the file to download
//!                                        (default 8 MiB)
//!
//! Run: cargo test --test onedrive_live -- --ignored --nocapture
//! Only counts, sizes and hashes are printed, never URLs or tokens.

use sha1::{Digest, Sha1};
use std::{path::PathBuf, sync::Arc, time::Duration};
use versadown_lib::aria2::{Aria2Api, Aria2LaunchConfig, Aria2Process};
use versadown_lib::cloud::share::{CloudShareEntry, CloudShareResolvedEntry, ShareResolve};
use versadown_lib::cloud::{
    share::{
        provider_share_adapter, CloudSharePlan, ShareInspect, ShareInspection, ShareResolution,
    },
    CloudProvider, MemoryCredentialStore, ReqwestTransport,
};
use versadown_lib::{
    extract_cloud_share_candidate, DownloadService, NewTask, TaskStore, UriSubmissionOptions,
};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// The transport is a blocking client, which must not live in the async runtime.
async fn inspect(link: &str) -> CloudSharePlan {
    let link = link.to_owned();
    tokio::task::spawn_blocking(move || inspect_blocking(&link))
        .await
        .unwrap()
}

async fn resolve_url(plan: &CloudSharePlan, entry: &CloudShareEntry) -> String {
    let (plan, entry) = (plan.clone(), entry.clone());
    tokio::task::spawn_blocking(move || resolve_url_blocking(&plan, &entry))
        .await
        .unwrap()
}

fn inspect_blocking(link: &str) -> CloudSharePlan {
    let candidate = extract_cloud_share_candidate(link, None)
        .expect("a valid link")
        .expect("a cloud share link");
    assert_eq!(candidate.provider, CloudProvider::Onedrive);
    let credentials = MemoryCredentialStore::default();
    let http = ReqwestTransport::new().unwrap();
    let adapter = provider_share_adapter(CloudProvider::Onedrive, &credentials, &http).unwrap();
    match adapter.inspect(&candidate).expect("inspection") {
        ShareInspection::Plan(plan) => plan,
        ShareInspection::AccessCodeRequired => panic!("unexpected access code request"),
    }
}

fn resolve_url_blocking(plan: &CloudSharePlan, entry: &CloudShareEntry) -> String {
    let credentials = MemoryCredentialStore::default();
    let http = ReqwestTransport::new().unwrap();
    let adapter = provider_share_adapter(CloudProvider::Onedrive, &credentials, &http).unwrap();
    let resolved = adapter
        .resolve(&CloudShareResolvedEntry {
            provider: CloudProvider::Onedrive,
            plan: plan.clone(),
            entry: entry.clone(),
        })
        .expect("resolution");
    match resolved {
        ShareResolution::Direct(download) => download.url,
        ShareResolution::TransferRequired(_) => panic!("OneDrive never transfers"),
    }
}

/// Download `url` with the bundled engine; returns the file's bytes.
async fn engine_download(url: &str, name: &str) -> Vec<u8> {
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store.create(NewTask::direct(url)).unwrap();
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut aria2 = versadown_lib::Aria2Settings::default();
    aria2.rpc_port = reserved.local_addr().unwrap().port();
    aria2.rpc_token = format!("onedrive-live-{}", aria2.rpc_port);
    drop(reserved);
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let binary = versadown_lib::engine_binary::EngineBinary::for_current_target()
        .unwrap()
        .resolve(&manifest, &manifest)
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("downloads");
    let (mut process, client) = Aria2Process::start(Aria2LaunchConfig {
        binary,
        data_dir: root.path().join("engine"),
        download_dir: output.clone(),
        parent_pid: std::process::id(),
        peer_discovery: false,
        aria2,
    })
    .await
    .unwrap();
    let service = DownloadService::new(Arc::new(client.clone()), store);
    let submitted = service
        .submit_uri_with_options(
            task.id,
            vec![url.to_owned()],
            UriSubmissionOptions {
                directory: Some(output.clone()),
                output_name: Some(name.to_owned()),
                max_connections_per_server: Some(4),
                max_tries: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let started = tokio::time::Instant::now();
    let (state, message) = loop {
        let status = client
            .tell_status(submitted.aria_gid.as_deref().unwrap())
            .await
            .unwrap();
        if matches!(status.status.as_str(), "error" | "complete") {
            break (status.status, status.error_message);
        }
        assert!(
            started.elapsed() < Duration::from_secs(300),
            "transfer timed out"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    process.terminate_fallback().unwrap();
    assert_eq!(state, "complete", "{message:?}");
    std::fs::read(output.join(name)).unwrap()
}

fn sha1_hex(bytes: &[u8]) -> String {
    Sha1::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test]
#[ignore = "needs VERSADOWN_ONEDRIVE_TEST_FILE_LINK and network access"]
async fn public_file_link_downloads_completely_through_aria2() {
    let link = env("VERSADOWN_ONEDRIVE_TEST_FILE_LINK").expect("VERSADOWN_ONEDRIVE_TEST_FILE_LINK");
    let plan = inspect(&link).await;
    assert_eq!(plan.entries.len(), 1);
    let entry = &plan.entries[0];
    println!("file: {} bytes", entry.size.unwrap_or_default());
    let url = resolve_url(&plan, entry).await;
    let bytes = engine_download(&url, "download.bin").await;
    assert_eq!(Some(bytes.len() as u64), entry.size);
    let digest = sha1_hex(&bytes);
    println!("downloaded {} bytes, sha1 {digest}", bytes.len());
    if let Some(expected) = env("VERSADOWN_ONEDRIVE_TEST_FILE_SHA1") {
        assert_eq!(digest, expected.to_ascii_lowercase());
    }
}

#[tokio::test]
#[ignore = "needs VERSADOWN_ONEDRIVE_TEST_FOLDER_LINK and network access"]
async fn public_folder_link_lists_and_downloads_one_file() {
    let link =
        env("VERSADOWN_ONEDRIVE_TEST_FOLDER_LINK").expect("VERSADOWN_ONEDRIVE_TEST_FOLDER_LINK");
    let limit: u64 = env("VERSADOWN_ONEDRIVE_TEST_MAX_BYTES")
        .and_then(|value| value.parse().ok())
        .unwrap_or(8 * 1024 * 1024);
    let plan = inspect(&link).await;
    assert!(!plan.entries.is_empty(), "the folder lists no files");
    for entry in &plan.entries {
        println!(
            "entry: {}/{} ({} bytes)",
            entry.relative_directory.join("/"),
            entry.name,
            entry.size.unwrap_or_default()
        );
    }
    let entry = plan
        .entries
        .iter()
        .filter(|entry| entry.size.is_some_and(|size| size > 0 && size <= limit))
        .min_by_key(|entry| entry.size)
        .expect("a file within the size limit");
    let url = resolve_url(&plan, entry).await;
    let bytes = engine_download(&url, "download.bin").await;
    assert_eq!(Some(bytes.len() as u64), entry.size);
    println!(
        "downloaded {} ({} bytes, sha1 {})",
        entry.name,
        bytes.len(),
        sha1_hex(&bytes)
    );
}
