use super::*;

pub fn restore_account(
    provider: CloudProvider,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
) -> Result<CloudAccount, QuarkError> {
    ProviderRegistry::global()
        .driver(provider, store, http)
        .account()
}

pub(crate) fn list_cloud_directory(
    provider: CloudProvider,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
    parent_id: &str,
) -> Result<CloudDirectory, QuarkError> {
    ProviderRegistry::global()
        .driver(provider, store, http)
        .list_directory(parent_id)
}

pub(crate) fn restore_provider_recycle_entries(
    provider: CloudProvider,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
    file_ids: &[String],
) -> Result<(), QuarkError> {
    ProviderRegistry::global()
        .driver(provider, store, http)
        .restore(file_ids)
}

pub(crate) fn delete_provider_recycle_entries(
    provider: CloudProvider,
    store: &impl CredentialStore,
    http: &impl CloudHttpTransport,
    file_ids: &[String],
) -> Result<(), QuarkError> {
    ProviderRegistry::global()
        .driver(provider, store, http)
        .delete(file_ids)
}

#[tauri::command]
pub async fn cloud_account(
    app: AppHandle,
    provider: CloudProvider,
) -> Result<CloudAccount, QuarkError> {
    run_session_work(app, provider, SessionWorkKind::Read, move |s, h| {
        restore_account(provider, &s.store, h)
    })
    .await
}

#[tauri::command]
pub async fn cloud_account_capacity(
    app: AppHandle,
    provider: CloudProvider,
) -> Result<CloudAccountCapacity, QuarkError> {
    run_session_work(app, provider, SessionWorkKind::Read, move |s, h| {
        let mut driver = ProviderRegistry::global().driver(provider, &s.store, h);
        let account = driver.account()?;
        let capacity = driver.storage_capacity()?;
        Ok(CloudAccountCapacity {
            provider,
            account_id: account.id,
            capacity,
            updated_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
        })
    })
    .await
}

async fn emit_cloud_capacity_invalidated(
    app: AppHandle,
    provider: CloudProvider,
    reason: CloudCapacityChangeReason,
) {
    let account = run_session_work(app.clone(), provider, SessionWorkKind::Read, move |s, h| {
        restore_account(provider, &s.store, h)
    })
    .await;
    if let Ok(account) = account {
        let _ = app.emit(
            "cloud-capacity-invalidated",
            CloudCapacityInvalidated {
                provider,
                account_id: account.id,
                reason,
            },
        );
    }
}

pub(crate) fn schedule_cloud_capacity_invalidation(
    app: AppHandle,
    provider: CloudProvider,
    reason: CloudCapacityChangeReason,
) {
    tauri::async_runtime::spawn(async move {
        emit_cloud_capacity_invalidated(app, provider, reason).await;
    });
}

#[tauri::command]
pub async fn cloud_list_directory(
    app: AppHandle,
    provider: CloudProvider,
    parent_id: String,
) -> Result<CloudDirectory, QuarkError> {
    run_session_work(app, provider, SessionWorkKind::Read, move |s, h| {
        list_cloud_directory(provider, &s.store, h, &parent_id)
    })
    .await
}

#[tauri::command]
pub async fn cloud_create_directory(
    app: AppHandle,
    provider: CloudProvider,
    parent_id: String,
    name: String,
) -> Result<CloudFile, QuarkError> {
    valid_name(&name)?;
    run_session_work(app, provider, SessionWorkKind::Mutation, move |s, h| {
        ProviderRegistry::global()
            .driver(provider, &s.store, h)
            .create_directory(&parent_id, &name)
    })
    .await
}

#[tauri::command]
pub async fn cloud_rename(
    app: AppHandle,
    provider: CloudProvider,
    file_id: String,
    name: String,
) -> Result<(), QuarkError> {
    valid_name(&name)?;
    run_session_work(app, provider, SessionWorkKind::Mutation, move |s, h| {
        ProviderRegistry::global()
            .driver(provider, &s.store, h)
            .rename(&file_id, &name)
    })
    .await
}

#[tauri::command]
pub async fn cloud_move(
    app: AppHandle,
    provider: CloudProvider,
    file_ids: Vec<String>,
    target_parent_id: String,
) -> Result<(), QuarkError> {
    run_session_work(app, provider, SessionWorkKind::Mutation, move |s, h| {
        ProviderRegistry::global()
            .driver(provider, &s.store, h)
            .move_files(&file_ids, &target_parent_id)
    })
    .await
}

#[tauri::command]
pub async fn cloud_copy(
    app: AppHandle,
    provider: CloudProvider,
    file_ids: Vec<String>,
    target_parent_id: String,
) -> Result<(), QuarkError> {
    let changed = !file_ids.is_empty();
    run_session_work(
        app.clone(),
        provider,
        SessionWorkKind::Mutation,
        move |s, h| {
            ProviderRegistry::global()
                .driver(provider, &s.store, h)
                .copy_files(&file_ids, &target_parent_id)
        },
    )
    .await?;
    if changed {
        schedule_cloud_capacity_invalidation(app, provider, CloudCapacityChangeReason::Copy);
    }
    Ok(())
}

#[tauri::command]
pub async fn cloud_trash(
    app: AppHandle,
    provider: CloudProvider,
    file_ids: Vec<String>,
) -> Result<(), QuarkError> {
    let changed = !file_ids.is_empty();
    run_session_work(
        app.clone(),
        provider,
        SessionWorkKind::Mutation,
        move |s, h| trash_provider_files(provider, &s.store, h, &file_ids),
    )
    .await?;
    if changed {
        schedule_cloud_capacity_invalidation(app, provider, CloudCapacityChangeReason::Trash);
    }
    Ok(())
}

pub(crate) fn trash_provider_files(
    provider: CloudProvider,
    store: &impl CredentialStore,
    h: &impl CloudHttpTransport,
    file_ids: &[String],
) -> Result<(), QuarkError> {
    ProviderRegistry::global()
        .driver(provider, store, h)
        .trash(file_ids)
}

#[tauri::command]
pub async fn cloud_delete(
    app: AppHandle,
    provider: CloudProvider,
    file_ids: Vec<String>,
) -> Result<(), QuarkError> {
    let changed = !file_ids.is_empty();
    run_session_work(
        app.clone(),
        provider,
        SessionWorkKind::Mutation,
        move |s, h| delete_provider_recycle_entries(provider, &s.store, h, &file_ids),
    )
    .await?;
    if changed {
        schedule_cloud_capacity_invalidation(app, provider, CloudCapacityChangeReason::Delete);
    }
    Ok(())
}

#[tauri::command]
pub async fn cloud_restore(
    app: AppHandle,
    provider: CloudProvider,
    file_ids: Vec<String>,
) -> Result<(), QuarkError> {
    let changed = !file_ids.is_empty();
    run_session_work(
        app.clone(),
        provider,
        SessionWorkKind::Mutation,
        move |s, h| restore_provider_recycle_entries(provider, &s.store, h, &file_ids),
    )
    .await?;
    if changed {
        schedule_cloud_capacity_invalidation(app, provider, CloudCapacityChangeReason::Restore);
    }
    Ok(())
}
