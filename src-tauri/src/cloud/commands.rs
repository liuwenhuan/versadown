pub use super::ResolvedCloudDownload;
use super::*;
use crate::models::{
    CloudDirectoryGroup, NewCloudDirectoryGroup, NewCloudDirectoryGroupItem, QueueTaskPayload,
};
use crate::{
    commands::{
        cloud_directory_group_queue_task, task_action_outcome, AppState, TaskActionOutcome,
    },
    duplicate_state, DownloadTask, NewTask, TaskStatus, TaskStore, UriSubmissionOptions,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};
use tauri::{
    webview::{NewWindowResponse, PageLoadEvent},
    AppHandle, Manager, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_dialog::DialogExt;

mod auth;
mod browser;
mod common;
pub(crate) use browser::*;
mod download;
mod drive;
#[path = "commands/share.rs"]
mod share_commands;
#[path = "commands/upload.rs"]
mod upload_commands;
pub use auth::*;
pub use common::*;
pub use download::*;
pub use drive::*;
pub use share_commands::*;
pub use upload_commands::*;
#[cfg(test)]
#[path = "commands/tests.rs"]
mod tests;

pub struct CloudState {
    pub store: DpapiCredentialStore,
    pub share_plans: share::CloudSharePlanStore,
    pub share_refresh_budget: share::CloudShareRefreshBudget,
    pub(crate) password_download_refresh_budget: browser_login::PasswordDownloadRefreshBudget,
    pending_authorization_retries: Mutex<HashMap<String, PendingCloudAuthorizationRetries>>,
    password_recovery_attempts: Mutex<HashMap<String, std::time::Instant>>,
    http: ReqwestTransport,
    provider_operations: HashMap<CloudProvider, Mutex<()>>,
}

impl CloudState {
    pub fn new(root: PathBuf) -> Result<Self, QuarkError> {
        Ok(Self {
            share_plans: share::CloudSharePlanStore::new(DpapiCredentialStore::new(root.clone())?),
            share_refresh_budget: Default::default(),
            password_download_refresh_budget: Default::default(),
            pending_authorization_retries: Default::default(),
            password_recovery_attempts: Default::default(),
            store: DpapiCredentialStore::new(root)?,
            http: ReqwestTransport::new()?,
            provider_operations: ProviderRegistry::global()
                .all()
                .iter()
                .map(|d| (d.provider, Mutex::new(())))
                .collect(),
        })
    }
}

#[cfg(test)]
#[path = "commands_startup_tests.rs"]
mod startup_tests;
