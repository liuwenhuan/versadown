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
    store: &DpapiCredentialStore,
    http: &impl CloudHttpTransport,
    file_ids: &[String],
) -> Result<(), QuarkError> {
    ProviderRegistry::global()
        .driver(provider, store, http)
        .restore(file_ids)
}

pub(crate) fn delete_provider_recycle_entries(
    provider: CloudProvider,
    store: &DpapiCredentialStore,
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
    run_session_work(app, provider, SessionWorkKind::Mutation, move |s, h| {
        ProviderRegistry::global()
            .driver(provider, &s.store, h)
            .copy_files(&file_ids, &target_parent_id)
    })
    .await
}

#[tauri::command]
pub async fn cloud_trash(
    app: AppHandle,
    provider: CloudProvider,
    file_ids: Vec<String>,
) -> Result<(), QuarkError> {
    run_session_work(app, provider, SessionWorkKind::Mutation, move |s, h| {
        trash_provider_files(provider, &s.store, h, &file_ids)
    })
    .await
}

pub(crate) fn trash_provider_files(
    provider: CloudProvider,
    store: &DpapiCredentialStore,
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
    run_session_work(app, provider, SessionWorkKind::Mutation, move |s, h| {
        delete_provider_recycle_entries(provider, &s.store, h, &file_ids)
    })
    .await
}

#[tauri::command]
pub async fn cloud_restore(
    app: AppHandle,
    provider: CloudProvider,
    file_ids: Vec<String>,
) -> Result<(), QuarkError> {
    run_session_work(app, provider, SessionWorkKind::Mutation, move |s, h| {
        restore_provider_recycle_entries(provider, &s.store, h, &file_ids)
    })
    .await
}
