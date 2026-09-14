use super::*;

struct CancelShareOnDrop(Arc<share::ShareWorkControl>);
impl Drop for CancelShareOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub async fn inspect_cloud_share(
    app: AppHandle,
    candidate: crate::parser::CloudShareCandidate,
) -> Result<share::ShareConfirmationOutcome, QuarkError> {
    // Only genuine authentication expiry uses the shared recovery order. Share
    // access codes, CAPTCHA and risk checks retain their direct user flows.
    run_share_session_read_work(app, candidate.provider, move |state, http| {
        let adapter = share::provider_share_adapter(candidate.provider, &state.store, http)?;
        share::inspect_share_with_adapter(&state.share_plans, adapter.as_ref(), &candidate)
    })
    .await
}

pub async fn resolve_stable_cloud_share_download(
    app: AppHandle,
    source: &str,
) -> Result<ResolvedCloudDownload, QuarkError> {
    let (provider, _, _) = share::parse_stable_cloud_share_source(source)?;
    let source = source.to_owned();
    let control = Arc::new(share::ShareWorkControl::new(
        std::time::Duration::from_secs(300),
    ));
    let _cancel_on_drop = CancelShareOnDrop(Arc::clone(&control));
    let completion_control = Arc::clone(&control);
    // Resolution can create personal files. Recovery may renew authorization,
    // but it must never replay this combined operation after a transfer starts.
    run_session_work_checked(
        app,
        provider,
        SessionWorkKind::Mutation,
        move |state, http| {
            share::resolve_current_share_download_cancellable(
                &state.share_plans,
                &state.store,
                http,
                &source,
                &control,
            )
        },
        move || completion_control.check(),
    )
    .await
}

pub async fn submit_cloud_share_file(
    app: AppHandle,
    state: &AppState,
    source: String,
    configured: UriSubmissionOptions,
    private_download: bool,
    on_created: Option<tauri::ipc::Channel<QueueTaskPayload>>,
) -> Result<TaskActionOutcome, QuarkError> {
    let submission = submit_cloud_share_file_internal(
        app.clone(),
        state,
        source,
        configured,
        None,
        true,
        private_download,
        on_created,
    )
    .await?;
    let outcome = match submission {
        CloudShareFileSubmission::Queued(outcome)
        | CloudShareFileSubmission::Duplicate(outcome) => outcome,
    };
    schedule_cloud_share_cleanup(app, &outcome.payload.task);
    Ok(outcome)
}

pub(crate) enum CloudShareFileSubmission {
    Queued(TaskActionOutcome),
    Duplicate(TaskActionOutcome),
}

pub(crate) async fn submit_cloud_share_file_internal(
    app: AppHandle,
    state: &AppState,
    source: String,
    configured: UriSubmissionOptions,
    directory_context: Option<DirectoryLeafContext>,
    append_entry_directory: bool,
    private_download: bool,
    on_created: Option<tauri::ipc::Channel<QueueTaskPayload>>,
) -> Result<CloudShareFileSubmission, QuarkError> {
    share::parse_stable_cloud_share_source(&source)?;
    let _source_submission = state.cloud_download_sources.acquire(&source).await?;
    if let Some(existing) = state
        .store
        .find_by_source(&source)
        .map_err(|_| QuarkError::Storage)?
        .filter(|task| task.status != TaskStatus::Recycled)
    {
        ensure_private_task_access(state, &existing).map_err(QuarkError::InvalidInput)?;
        if !is_retryable_cloud_share_prepare(&existing) {
            let duplicate =
                duplicate_state(&state.store, &existing).map_err(|_| QuarkError::Storage)?;
            return Ok(CloudShareFileSubmission::Duplicate(
                task_action_outcome(&state.store, existing, Some(duplicate))
                    .map_err(QuarkError::Transport)?,
            ));
        }
    }
    let cloud = app
        .try_state::<Arc<CloudState>>()
        .ok_or(QuarkError::Storage)?;
    let resolve_source = source.clone();
    let outcome = submit_new_cloud_share_file_scoped_with(
        state.store.clone(),
        &cloud.share_plans,
        source,
        configured,
        directory_context,
        append_entry_directory,
        private_download,
        on_created.map(|channel| {
            let store = state.store.clone();
            Arc::new(move |task: &DownloadTask| {
                if let Ok(root) =
                    crate::commands::queue_task_payload_from_task(&store, task.clone())
                {
                    let _ = channel.send(root);
                }
            }) as Arc<dyn Fn(&DownloadTask) + Send + Sync>
        }),
        || async {
            state
                .runtime
                .service()
                .await
                .map_err(|_| QuarkError::Transport("下载引擎不可用".into()))
        },
        || resolve_stable_cloud_share_download(app.clone(), &resolve_source),
    )
    .await?;
    Ok(CloudShareFileSubmission::Queued(outcome))
}

pub async fn submit_cloud_share_directory_leaf(
    app: AppHandle,
    state: &AppState,
    source: String,
    configured: UriSubmissionOptions,
    context: DirectoryLeafContext,
    private_download: bool,
) -> Result<DirectoryLeafSubmission, QuarkError> {
    match submit_cloud_share_file_internal(
        app.clone(),
        state,
        source,
        configured,
        Some(context),
        false,
        private_download,
        None,
    )
    .await?
    {
        CloudShareFileSubmission::Queued(outcome) => {
            schedule_cloud_share_cleanup(app, &outcome.payload.task);
            Ok(DirectoryLeafSubmission::Queued(outcome.payload.task.id))
        }
        CloudShareFileSubmission::Duplicate(_) => Ok(DirectoryLeafSubmission::Duplicate),
    }
}

#[cfg(test)]
pub(crate) async fn submit_new_cloud_share_file_with<S, SFut, R, RFut>(
    tasks: Arc<TaskStore>,
    plans: &share::CloudSharePlanStore,
    source: String,
    configured: UriSubmissionOptions,
    service: S,
    resolve: R,
) -> Result<TaskActionOutcome, QuarkError>
where
    S: FnOnce() -> SFut,
    SFut: std::future::Future<Output = Result<Arc<crate::DownloadService>, QuarkError>>,
    R: FnOnce() -> RFut,
    RFut: std::future::Future<Output = Result<ResolvedCloudDownload, QuarkError>>,
{
    submit_new_cloud_share_file_scoped_with(
        tasks, plans, source, configured, None, true, false, None, service, resolve,
    )
    .await
}

pub(crate) async fn submit_new_cloud_share_file_scoped_with<S, SFut, R, RFut>(
    tasks: Arc<TaskStore>,
    plans: &share::CloudSharePlanStore,
    source: String,
    mut configured: UriSubmissionOptions,
    directory_context: Option<DirectoryLeafContext>,
    append_entry_directory: bool,
    private_download: bool,
    on_created: Option<Arc<dyn Fn(&DownloadTask) + Send + Sync>>,
    service: S,
    resolve: R,
) -> Result<TaskActionOutcome, QuarkError>
where
    S: FnOnce() -> SFut,
    SFut: std::future::Future<Output = Result<Arc<crate::DownloadService>, QuarkError>>,
    R: FnOnce() -> RFut,
    RFut: std::future::Future<Output = Result<ResolvedCloudDownload, QuarkError>>,
{
    share::parse_stable_cloud_share_source(&source)?;
    let entry = plans.resolve_entry(&source)?;
    if append_entry_directory {
        if let Some(directory) = configured.directory.as_ref() {
            configured.directory = Some(directory_download_output_directory(
                directory,
                &entry.entry.relative_directory,
            )?);
        } else if !entry.entry.relative_directory.is_empty() {
            return Err(QuarkError::InvalidInput(
                "分享目录下载需要设置绝对下载目录".into(),
            ));
        }
    }
    let output_name = configured
        .output_name
        .clone()
        .unwrap_or_else(|| safe_filename(&entry.entry.name));
    configured.output_name = Some(output_name.clone());
    configured
        .clone()
        .into_aria2_options()
        .map_err(|_| QuarkError::InvalidInput("下载路径或文件名无效".into()))?;

    // Preserve the single-file contract: an unavailable engine must not leave
    // a task behind. A directory leaf is different because the durable group
    // must retain a failed child for Failed/Recycle projections.
    let mut service_factory = Some(service);
    let eager_service = if directory_context.is_none() {
        Some(
            service_factory
                .take()
                .expect("service factory is available")()
            .await?,
        )
    } else {
        None
    };

    let prepared = match tasks
        .find_by_source(&source)
        .map_err(|_| QuarkError::Storage)?
        .filter(is_retryable_cloud_share_prepare)
    {
        Some(mut prepared) => {
            prepared.status = TaskStatus::Parsing;
            prepared.error = None;
            prepared.failure_code = None;
            tasks
                .apply_transfer_snapshot(prepared)
                .map_err(|_| QuarkError::Storage)?
        }
        None => tasks
            .create(crate::NewTask {
                source: source.clone(),
                name: output_name,
                kind: "cloud".into(),
                is_private: private_download,
            })
            .map_err(|_| QuarkError::Storage)?,
    };
    let prepared = tasks
        .set_private(prepared.id, private_download)
        .map_err(|_| QuarkError::Storage)?;
    if let Err(error) = plans.reserve_local_task(&source, prepared.id) {
        return match tasks.remove_task_record(prepared.id) {
            Ok(()) | Err(crate::StoreError::NotFound(_)) => Err(error),
            // The task may still be durable, so report the conservative
            // recoverable state instead of claiming creation was rolled back.
            Err(_) => Ok(cloud_share_failure_outcome(
                &tasks,
                prepared,
                None,
                CLOUD_SHARE_PREPARE_FAILURE,
            )),
        };
    }
    if let Some(context) = directory_context {
        tasks
            .link_cloud_directory_group_item_task(context.group_id, context.item_id, prepared.id)
            .map_err(|_| QuarkError::Storage)?;
    }

    if let Some(on_created) = on_created {
        on_created(&prepared);
    }
    let service = match eager_service {
        Some(service) => service,
        None => match service_factory
            .take()
            .expect("directory service factory is available")()
        .await
        {
            Ok(service) => service,
            Err(error) => {
                return Ok(cloud_share_failure_outcome(
                    &tasks,
                    prepared,
                    Some(cloud_failure_code(&error)),
                    share_resolution_failure_message(&error),
                ))
            }
        },
    };

    let resolved = match resolve().await {
        Ok(resolved) => resolved,
        Err(error) => {
            return Ok(cloud_share_failure_outcome(
                &tasks,
                prepared,
                Some(cloud_failure_code(&error)),
                share_resolution_failure_message(&error),
            ))
        }
    };
    let task = match share::submit_current_share_download(&service, &prepared, resolved, configured)
        .await
    {
        Ok(task) => task,
        Err(_) => {
            return Ok(cloud_share_failure_outcome(
                &tasks,
                prepared,
                None,
                "aria2 未能接受网盘分享下载任务",
            ))
        }
    };
    Ok(cloud_share_task_outcome(&tasks, task))
}

pub(crate) fn cloud_share_failure_outcome(
    tasks: &TaskStore,
    prepared: DownloadTask,
    failure_code: Option<&str>,
    message: &'static str,
) -> TaskActionOutcome {
    let task = tasks
        .fail_with_code(prepared.id, failure_code, message)
        .or_else(|_| tasks.find(prepared.id))
        .unwrap_or(prepared);
    cloud_share_task_outcome(tasks, task)
}

pub(crate) fn is_retryable_cloud_share_prepare(task: &DownloadTask) -> bool {
    task.kind == "cloud"
        && task.aria_gid.is_none()
        && (task.status == TaskStatus::Parsing
            || (task.status == TaskStatus::Failed
                && task.error.as_deref() == Some(CLOUD_SHARE_PREPARE_FAILURE)))
}

pub(crate) fn cloud_share_task_outcome(tasks: &TaskStore, task: DownloadTask) -> TaskActionOutcome {
    if let Ok(outcome) = task_action_outcome(tasks, task.clone(), None) {
        return outcome;
    }
    let files = tasks.files(task.id).unwrap_or_default();
    let reauthorization_required =
        requires_cloud_reauthorization_failure(task.failure_code.as_deref());
    TaskActionOutcome {
        payload: crate::commands::DownloadTaskPayload {
            task,
            files,
            cleanup_pending: false,
            reauthorization_required,
        },
        duplicate: None,
        queue_task: None,
    }
}

pub(crate) fn schedule_cloud_share_cleanup(app: AppHandle, task: &DownloadTask) {
    if task.status != TaskStatus::Completed || !task.source.starts_with("cloud-share://") {
        return;
    }
    let id = task.id;
    tauri::async_runtime::spawn(async move {
        let _ = cleanup_cloud_share_task(app, id, false).await;
    });
}

/// Explicit retry uses the same exact-object journal, and never resolves a URL
/// or resubmits a local download. It also works when aria2 is unavailable.
pub(crate) async fn cleanup_cloud_share_task(
    app: AppHandle,
    id: uuid::Uuid,
    retry: bool,
) -> Result<DownloadTask, QuarkError> {
    use tauri::Emitter;
    let state = app.try_state::<AppState>().ok_or(QuarkError::Storage)?;
    let task = state.store.find(id).map_err(|_| QuarkError::Storage)?;
    share::parse_stable_cloud_share_source(&task.source)?;
    if task.status != TaskStatus::Completed {
        return Err(QuarkError::InvalidInput(
            "只有已完成的分享任务可以重试清理".into(),
        ));
    }
    if !state
        .settings
        .lock()
        .map_err(|_| QuarkError::Storage)?
        .delete_transferred_share_files_after_download
    {
        return Ok(task);
    }
    match cleanup_cloud_share_task_inner(app.clone(), id, retry).await {
        Ok(task) => Ok(task),
        Err(_) => {
            let task = state
                .store
                .set_completed_cleanup_message(id, true)
                .map_err(|_| QuarkError::Storage)?;
            let cloud = app.try_state::<Arc<CloudState>>();
            if let Ok(payload) = crate::commands::task_update_payload(
                &state.store,
                cloud.as_ref().map(|cloud| &cloud.share_plans),
                task.clone(),
            ) {
                if should_emit_task_update(&state, task.is_private) {
                    let _ = app.emit("download-task-updated", payload);
                }
            }
            Ok(task)
        }
    }
}

pub(crate) async fn cleanup_cloud_share_task_inner(
    app: AppHandle,
    id: uuid::Uuid,
    retry: bool,
) -> Result<DownloadTask, QuarkError> {
    use tauri::Emitter;
    let state = app.try_state::<AppState>().ok_or(QuarkError::Storage)?;
    let task = state.store.find(id).map_err(|_| QuarkError::Storage)?;
    if task.status != TaskStatus::Completed {
        return Err(QuarkError::InvalidInput(
            "只有已完成的分享任务可以重试清理".into(),
        ));
    }
    let (provider, _, _) = share::parse_stable_cloud_share_source(&task.source)?;
    let cloud = app
        .try_state::<Arc<CloudState>>()
        .ok_or(QuarkError::Storage)?;
    // Acquire the stable plan lock before reading its current linked tasks.
    // New entries share this lock even if an earlier plan snapshot omitted them.
    let _plan_guard = state.cloud_download_sources.acquire(&task.source).await?;
    cloud.share_plans.link_local_task(&task.source, id)?;
    let tasks = Arc::clone(&state.store);
    let worker_app = app.clone();
    // No automatic session-recovery replay around a destructive action.
    let updated = run_session_work(
        app.clone(),
        provider,
        SessionWorkKind::Mutation,
        move |cloud, http| {
            let state = worker_app
                .try_state::<AppState>()
                .ok_or(QuarkError::Storage)?;
            let enabled = state
                .settings
                .lock()
                .map_err(|_| QuarkError::Storage)?
                .delete_transferred_share_files_after_download;
            let entry = cloud.share_plans.resolve_entry(&task.source)?;
            let adapter = share::provider_share_adapter(provider, &cloud.store, http)?;
            let owner = adapter.transfer_owner_fingerprint(&entry)?;
            crate::engine_runtime::cleanup_completed_share_for_owner(
                &cloud.share_plans,
                &tasks,
                &task,
                enabled,
                retry,
                owner.as_deref(),
                |provider, ids| {
                    ProviderRegistry::global()
                        .driver(provider, &cloud.store, http)
                        .trash_transferred_files(ids)
                },
            )
        },
    )
    .await?;
    if let Some(updated) = updated {
        if let Ok(payload) = crate::commands::task_update_payload(
            &state.store,
            Some(&cloud.share_plans),
            updated.clone(),
        ) {
            if should_emit_task_update(&state, updated.is_private) {
                let _ = app.emit("download-task-updated", payload);
            }
        }
        Ok(updated)
    } else {
        state.store.find(id).map_err(|_| QuarkError::Storage)
    }
}
