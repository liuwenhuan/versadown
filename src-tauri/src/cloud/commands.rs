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
    pub auth_supervisor: auth_supervisor::AuthSupervisor,
    http: ReqwestTransport,
}

impl CloudState {
    pub fn new(root: PathBuf) -> Result<Self, QuarkError> {
        let state = Self {
            share_plans: share::CloudSharePlanStore::new(DpapiCredentialStore::new(root.clone())?),
            share_refresh_budget: Default::default(),
            password_download_refresh_budget: Default::default(),
            pending_authorization_retries: Default::default(),
            auth_supervisor: Default::default(),
            store: DpapiCredentialStore::new(root)?,
            http: ReqwestTransport::new()?,
        };
        for descriptor in ProviderRegistry::global().all() {
            state
                .auth_supervisor
                .restore(&state.store, descriptor.provider)?;
        }
        Ok(state)
    }
    pub fn start_authorization_maintenance(self: &Arc<Self>) {
        for descriptor in ProviderRegistry::global().all() {
            let state = Arc::downgrade(self);
            let provider = descriptor.provider;
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    let Some(state) = state.upgrade() else {
                        return;
                    };
                    let _ =
                        tauri::async_runtime::spawn_blocking(move || -> Result<(), CloudError> {
                            if !state.auth_supervisor.is_due(&state.store, provider)? {
                                return Ok(());
                            }
                            let revision = state
                                .auth_supervisor
                                .record(&state.store, provider)?
                                .session_revision;
                            state.auth_supervisor.maintain(
                                &state.store,
                                provider,
                                Some(revision),
                                false,
                                || {
                                    auth_supervisor::recover_in_order(
                                        provider,
                                        &state.store,
                                        ProviderRegistry::global()
                                            .driver(provider, &state.store, &state.http)
                                            .auth(),
                                        false,
                                        || Ok(false),
                                    )
                                },
                            )
                        })
                        .await;
                }
            });
        }
    }
}

#[cfg(test)]
#[path = "commands_startup_tests.rs"]
mod startup_tests;
