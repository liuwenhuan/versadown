use super::*;

pub const MY_SHARES_DIRECTORY_ID: &str = "__all_downloader_my_shares__";

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

fn validate_share_options(options: &CloudShareOptions) -> Result<(), QuarkError> {
    if options.name.trim().is_empty() {
        return Err(QuarkError::InvalidInput("分享名称不能为空".into()));
    }
    if let Some(pass_code) = &options.pass_code {
        if pass_code.trim().is_empty() || pass_code.chars().count() > 32 {
            return Err(QuarkError::InvalidInput("提取码格式无效".into()));
        }
    }
    if options.generate_pass_code && options.pass_code.is_some() {
        return Err(QuarkError::InvalidInput(
            "不能同时指定提取码和自动生成提取码".into(),
        ));
    }
    if let Some(days) = options.expires_in_days {
        if days == 0 || days > 365 {
            return Err(QuarkError::InvalidInput(
                "有效期必须在 1 到 365 天之间".into(),
            ));
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn cloud_create_share(
    app: AppHandle,
    provider: CloudProvider,
    file_ids: Vec<String>,
    options: CloudShareOptions,
) -> Result<CloudShare, QuarkError> {
    if file_ids.is_empty() {
        return Err(QuarkError::InvalidInput("请选择至少一个文件或目录".into()));
    }
    validate_share_options(&options)?;
    run_session_work(app, provider, SessionWorkKind::Mutation, move |s, h| {
        ProviderRegistry::global()
            .driver(provider, &s.store, h)
            .create_share(&file_ids, &options)
    })
    .await
}

#[tauri::command]
pub async fn cloud_list_shares(
    app: AppHandle,
    provider: CloudProvider,
) -> Result<Vec<CloudShare>, QuarkError> {
    run_session_work(app, provider, SessionWorkKind::Read, move |s, h| {
        ProviderRegistry::global()
            .driver(provider, &s.store, h)
            .list_shares()
    })
    .await
}

#[tauri::command]
pub async fn cloud_update_share_pass_code(
    app: AppHandle,
    provider: CloudProvider,
    share_id: String,
    pass_code: String,
) -> Result<(), QuarkError> {
    if share_id.trim().is_empty() || pass_code.trim().is_empty() {
        return Err(QuarkError::InvalidInput("分享 ID 和提取码不能为空".into()));
    }
    run_session_work(app, provider, SessionWorkKind::Mutation, move |s, h| {
        ProviderRegistry::global()
            .driver(provider, &s.store, h)
            .update_share_pass_code(&share_id, &pass_code)
    })
    .await
}

#[tauri::command]
pub async fn cloud_cancel_shares(
    app: AppHandle,
    provider: CloudProvider,
    share_ids: Vec<String>,
) -> Result<(), QuarkError> {
    if share_ids.is_empty() {
        return Err(QuarkError::InvalidInput("请选择至少一个分享".into()));
    }
    run_session_work(app, provider, SessionWorkKind::Mutation, move |s, h| {
        ProviderRegistry::global()
            .driver(provider, &s.store, h)
            .cancel_shares(&share_ids)
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
