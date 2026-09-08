use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use alldownloader_lib::{
    aria2::{Aria2Api, Aria2Error, Aria2GlobalStat, Aria2Options, Aria2Status, RpcCall, RpcFault},
    prepare_download, DownloadAction, DownloadService, TaskStatus, TaskStore,
};
use async_trait::async_trait;
use serde_json::Value;

#[derive(Default)]
struct RecordingAria2 {
    calls: Mutex<Vec<String>>,
    add_error: bool,
    pause_error: bool,
}

impl RecordingAria2 {
    fn successful() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn record(&self, method: &str) {
        self.calls.lock().unwrap().push(method.to_string());
    }
    fn active(gid: &str) -> Aria2Status {
        Aria2Status {
            gid: gid.to_string(),
            status: "active".to_string(),
            total_length: "1000".to_string(),
            completed_length: "250".to_string(),
            download_speed: "50".to_string(),
            upload_speed: "0".to_string(),
            connections: "2".to_string(),
            files: vec![],
            followed_by: vec![],
            error_code: None,
            error_message: None,
        }
    }
}

#[async_trait]
impl Aria2Api for RecordingAria2 {
    async fn add_uri(
        &self,
        _uris: Vec<String>,
        _options: Aria2Options,
    ) -> Result<String, Aria2Error> {
        self.record("aria2.addUri");
        if self.add_error {
            Err(Aria2Error::Transport)
        } else {
            Ok("gid-1".to_string())
        }
    }
    async fn tell_status(&self, gid: &str) -> Result<Aria2Status, Aria2Error> {
        self.record("aria2.tellStatus");
        Ok(Self::active(gid))
    }
    async fn pause(&self, _gid: &str) -> Result<String, Aria2Error> {
        self.record("aria2.pause");
        if self.pause_error {
            Err(Aria2Error::Transport)
        } else {
            Ok("gid-1".to_string())
        }
    }
    async fn unpause(&self, gid: &str) -> Result<String, Aria2Error> {
        self.record("aria2.unpause");
        Ok(gid.to_string())
    }
    async fn remove(&self, gid: &str) -> Result<String, Aria2Error> {
        self.record("aria2.remove");
        Ok(gid.to_string())
    }
    async fn force_remove(&self, gid: &str) -> Result<String, Aria2Error> {
        self.record("aria2.forceRemove");
        Ok(gid.to_string())
    }
    async fn add_torrent(
        &self,
        _: String,
        _: Vec<String>,
        _: Aria2Options,
    ) -> Result<String, Aria2Error> {
        unimplemented!()
    }
    async fn tell_active(&self) -> Result<Vec<Aria2Status>, Aria2Error> {
        unimplemented!()
    }
    async fn tell_waiting(&self, _: i64, _: u32) -> Result<Vec<Aria2Status>, Aria2Error> {
        unimplemented!()
    }
    async fn tell_stopped(&self, _: i64, _: u32) -> Result<Vec<Aria2Status>, Aria2Error> {
        unimplemented!()
    }
    async fn get_global_stat(&self) -> Result<Aria2GlobalStat, Aria2Error> {
        unimplemented!()
    }
    async fn multicall(&self, _: Vec<RpcCall>) -> Result<Vec<Result<Value, RpcFault>>, Aria2Error> {
        unimplemented!()
    }
    async fn change_option(
        &self,
        _: &str,
        _: BTreeMap<String, String>,
    ) -> Result<String, Aria2Error> {
        unimplemented!()
    }
    async fn get_option(&self, _: &str) -> Result<BTreeMap<String, String>, Aria2Error> {
        unimplemented!()
    }
    async fn save_session(&self) -> Result<String, Aria2Error> {
        unimplemented!()
    }
    async fn shutdown(&self) -> Result<String, Aria2Error> {
        unimplemented!()
    }
}

#[tokio::test]
async fn direct_http_binds_gid_only_after_rpc_succeeds() {
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let outcome = prepare_download("https://example.com/file.bin", &store).unwrap();
    assert!(!outcome.requires_confirmation);
    assert_eq!(outcome.task.status, TaskStatus::Parsing);
    let api = RecordingAria2::successful();
    let service = DownloadService::new(api.clone(), store.clone());

    let task = service
        .submit_uri(outcome.task.id, vec![outcome.task.source], BTreeMap::new())
        .await
        .unwrap();

    assert_eq!(task.aria_gid.as_deref(), Some("gid-1"));
    assert_eq!(task.status, TaskStatus::Downloading);
    assert_eq!(api.calls(), ["aria2.addUri", "aria2.tellStatus"]);
}

#[tokio::test]
async fn failed_submission_never_claims_the_task_is_downloading() {
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let outcome = prepare_download("https://example.com/file.bin", &store).unwrap();
    let api = Arc::new(RecordingAria2 {
        add_error: true,
        ..Default::default()
    });
    let service = DownloadService::new(api, store.clone());

    assert!(service
        .submit_uri(outcome.task.id, vec![outcome.task.source], BTreeMap::new())
        .await
        .is_err());
    let task = store.find(outcome.task.id).unwrap();
    assert_eq!(task.status, TaskStatus::Parsing);
    assert_eq!(task.aria_gid, None);
}

#[tokio::test]
async fn failed_pause_does_not_optimistically_change_the_store() {
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let outcome = prepare_download("https://example.com/file.bin", &store).unwrap();
    let api = Arc::new(RecordingAria2 {
        pause_error: true,
        ..Default::default()
    });
    let service = DownloadService::new(api, store.clone());
    service
        .submit_uri(outcome.task.id, vec![outcome.task.source], BTreeMap::new())
        .await
        .unwrap();
    let before = store.find(outcome.task.id).unwrap();

    assert!(service
        .control(outcome.task.id, DownloadAction::Pause)
        .await
        .is_err());

    assert_eq!(store.find(outcome.task.id).unwrap().status, before.status);
}

#[test]
fn magnet_waits_for_file_confirmation() {
    let store = TaskStore::open_in_memory().unwrap();
    let outcome = prepare_download("magnet:?xt=urn:btih:abc", &store).unwrap();
    assert!(outcome.requires_confirmation);
    assert_eq!(outcome.task.status, TaskStatus::AwaitingUser);
}

#[test]
fn torrent_stays_in_parsing_until_bytes_are_loaded_and_aria2_returns_files() {
    let store = TaskStore::open_in_memory().unwrap();
    let outcome = prepare_download("C:\\Downloads\\game.torrent", &store).unwrap();
    assert!(outcome.requires_confirmation);
    assert_eq!(outcome.task.kind, "torrent");
    assert_eq!(outcome.task.status, TaskStatus::Parsing);
    assert!(outcome.resources.is_empty());
}

#[test]
fn thunder_wrapper_is_decoded_before_aria2_submission() {
    let store = TaskStore::open_in_memory().unwrap();
    let outcome = prepare_download("thunder://QUFodHRwOi8vYS9iWlo=", &store).unwrap();

    assert!(!outcome.requires_confirmation);
    assert_eq!(outcome.task.source, "http://a/b");
    assert_eq!(outcome.task.status, TaskStatus::Parsing);
}

#[test]
fn malformed_or_unsupported_thunder_wrapper_is_rejected() {
    let store = TaskStore::open_in_memory().unwrap();
    assert!(prepare_download("thunder://not-base64", &store).is_err());
    assert!(store.list().unwrap().is_empty());
}
