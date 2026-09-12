use super::*;
/// The only cloud-share plan projection permitted across the IPC boundary.
/// Its source is an opaque, validated `cloud-share://` reference; credentials,
/// provider session context, and canonical share identifiers remain DPAPI-only.
#[derive(Clone, Serialize)]
pub struct CloudShareConfirmation {
    pub plan_source: String,
    pub title: String,
    pub resources: Vec<CloudShareConfirmationResource>,
}

#[derive(Clone, Serialize)]
pub struct CloudShareConfirmationResource {
    pub id: String,
    pub name: String,
    pub size: String,
    pub selected: bool,
}

pub fn inspect_share_with_adapter(
    store: &CloudSharePlanStore,
    adapter: &dyn CloudShareAdapter,
    candidate: &crate::parser::CloudShareCandidate,
) -> Result<ShareConfirmationOutcome, QuarkError> {
    let plan = match adapter.inspect(candidate)? {
        ShareInspection::AccessCodeRequired => {
            return Ok(ShareConfirmationOutcome::AccessCodeRequired)
        }
        ShareInspection::Plan(plan) => plan,
    };
    if plan.provider != candidate.provider || plan.entries.is_empty() {
        return Err(QuarkError::InvalidResponse);
    }
    let automatic_source = match &plan.root_kind {
        CloudShareRootKind::File { entry_id }
            if plan.entries.len() == 1 && plan.entries[0].entry_id == *entry_id =>
        {
            Some(plan.source_for(entry_id))
        }
        _ => None,
    };
    let confirmation = CloudShareConfirmation {
        plan_source: plan.source_for(&plan.entries[0].entry_id),
        title: "网盘分享文件".into(),
        resources: plan
            .entries
            .iter()
            .map(|entry| CloudShareConfirmationResource {
                id: entry.entry_fingerprint.clone(),
                name: entry
                    .relative_directory
                    .iter()
                    .chain(std::iter::once(&entry.name))
                    .map(|name| crate::cloud::commands::safe_filename(name))
                    .collect::<Vec<_>>()
                    .join("/"),
                size: entry.size.map(format_confirmation_size).unwrap_or_default(),
                selected: true,
            })
            .collect(),
    };
    store.upsert(&plan)?;
    Ok(match automatic_source {
        Some(source) => ShareConfirmationOutcome::AutoSubmit { source },
        None => ShareConfirmationOutcome::Confirmation(confirmation),
    })
}

pub(super) fn format_confirmation_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudShareDownloadEntry {
    pub selected_id: String,
    pub source: String,
    pub name: String,
    pub relative_directory: Vec<String>,
    pub size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudShareDownloadSelection {
    pub provider: CloudProvider,
    /// A stable, opaque source from the encrypted plan. It is safe to persist
    /// as the directory-group identity and contains no share code or token.
    pub plan_source: String,
    /// Present only when the selected share resources must be represented by
    /// one durable, expandable directory task.
    pub directory_name: Option<String>,
    pub entries: Vec<CloudShareDownloadEntry>,
}

pub(crate) fn provider_share_directory_name(provider: CloudProvider) -> &'static str {
    crate::cloud::ProviderRegistry::global()
        .get(provider)
        .share_directory_name
}

pub(super) fn parsed_share_directory_name(entries: &[CloudShareDownloadEntry]) -> Option<String> {
    let root = entries.first()?.relative_directory.first()?.trim();
    (!root.is_empty()
        && entries.iter().all(|entry| {
            entry
                .relative_directory
                .first()
                .is_some_and(|component| component == root)
        }))
    .then(|| root.to_owned())
}

pub fn selected_share_download(
    store: &CloudSharePlanStore,
    plan_source: &str,
    selected_ids: &[String],
) -> Result<CloudShareDownloadSelection, QuarkError> {
    if selected_ids.is_empty() {
        return Err(QuarkError::InvalidInput("请至少选择一个分享文件".into()));
    }
    let plan = store.resolve_entry(plan_source)?.plan;
    let mut selected = BTreeSet::new();
    let entries = selected_ids
        .iter()
        .map(|id| {
            let entry = plan
                .entries
                .iter()
                .find(|entry| &entry.entry_fingerprint == id)
                .filter(|_| selected.insert(id))
                .ok_or_else(missing_plan)?;
            Ok(CloudShareDownloadEntry {
                selected_id: id.clone(),
                source: plan.source_for(&entry.entry_id),
                name: entry.name.clone(),
                relative_directory: entry.relative_directory.clone(),
                size: entry.size,
            })
        })
        .collect::<Result<Vec<_>, QuarkError>>()?;
    let exact_shared_file = matches!(
        &plan.root_kind,
        CloudShareRootKind::File { entry_id }
            if entries.len() == 1
                && plan.entries.iter().any(|entry| {
                    entry.entry_id == *entry_id
                        && entry.entry_fingerprint == entries[0].selected_id
                })
    );
    let grouped = !exact_shared_file
        && (matches!(
            plan.root_kind,
            CloudShareRootKind::Directory | CloudShareRootKind::Collection
        ) || entries.len() > 1
            || entries
                .iter()
                .any(|entry| !entry.relative_directory.is_empty()));
    Ok(CloudShareDownloadSelection {
        provider: plan.provider,
        plan_source: plan_source.to_owned(),
        directory_name: grouped.then(|| {
            parsed_share_directory_name(&entries)
                .unwrap_or_else(|| provider_share_directory_name(plan.provider).to_owned())
        }),
        entries,
    })
}

pub fn selected_share_sources(
    store: &CloudSharePlanStore,
    plan_source: &str,
    selected_ids: &[String],
) -> Result<Vec<String>, QuarkError> {
    Ok(selected_share_download(store, plan_source, selected_ids)?
        .entries
        .into_iter()
        .map(|entry| entry.source)
        .collect())
}

pub(crate) fn validate_direct_url(url: &str) -> Result<(), QuarkError> {
    // Do not let URL normalization strip controls that hide authority syntax.
    if url.chars().any(char::is_control) {
        return Err(QuarkError::InvalidResponse);
    }
    let parsed = url::Url::parse(url).map_err(|_| QuarkError::InvalidResponse)?;
    // URL parsing normalizes empty userinfo away. Check the original authority
    // too, without rejecting ordinary @ characters in paths or query strings.
    let authority = url
        .split_once(':')
        .map(|(_, rest)| rest.trim_start_matches(['/', '\\']))
        .and_then(|rest| rest.split(['/', '\\', '?', '#']).next())
        .ok_or(QuarkError::InvalidResponse)?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || authority.contains('@')
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !matches!(
            crate::parser::extract_cloud_share_candidate(url, None),
            Ok(None)
        )
    {
        return Err(QuarkError::InvalidResponse);
    }
    Ok(())
}

pub fn resolve_current_share_download(
    store: &CloudSharePlanStore,
    adapter: &dyn CloudShareAdapter,
    source: &str,
) -> Result<crate::cloud::commands::ResolvedCloudDownload, QuarkError> {
    let mut entry = store.resolve_entry(source)?;
    // A personal-drive file recorded for this exact source can outlive a
    // revoked/expired share. Refresh it before touching the share endpoint so
    // a normal retry neither depends on the share still existing nor creates a
    // duplicate transfer. The second owner check makes this fail closed after
    // a credential/session rotation.
    if store.has_pending_transfer(source)? {
        if let Some(owner) = adapter
            .transfer_capability()
            .ok_or(QuarkError::ProviderUnsupported)?
            .transfer_owner_fingerprint(&entry)?
        {
            if let Some(record) = store.reusable_transfer(source, &owner)? {
                let refreshed = adapter
                    .recorded_capability()
                    .ok_or(QuarkError::ProviderUnsupported)?
                    .refresh_recorded_transfer(&entry, &record.created_file_id)?;
                if let Some(context) = refreshed.updated_context {
                    store.update_provider_context(source, &context)?;
                    entry.plan.provider_context = context;
                }
                let RecordedTransferDownload::Available(mut download) = refreshed.result?;
                validate_direct_url(&download.url)?;
                download.name = entry.entry.name;
                return Ok(download);
            }
        }
    }
    let resolved = adapter.resolve_with_context(&entry)?;
    if let Some(context) = resolved.updated_context {
        store.update_provider_context(source, &context)?;
        entry.plan.provider_context = context;
    }
    let mut download = match resolved.result? {
        ShareResolution::Direct(download) => download,
        ShareResolution::TransferRequired(request) => {
            if request.source != source {
                return Err(QuarkError::InvalidResponse);
            }
            let mut reused = None;
            if store.has_pending_transfer(source)? {
                if let Some(owner) = adapter
                    .transfer_capability()
                    .ok_or(QuarkError::ProviderUnsupported)?
                    .transfer_owner_fingerprint(&entry)?
                {
                    if let Some(record) = store.reusable_transfer(source, &owner)? {
                        let refreshed = adapter
                            .recorded_capability()
                            .ok_or(QuarkError::ProviderUnsupported)?
                            .refresh_recorded_transfer(&entry, &record.created_file_id)?;
                        if let Some(context) = refreshed.updated_context {
                            store.update_provider_context(source, &context)?;
                            entry.plan.provider_context = context;
                        }
                        let RecordedTransferDownload::Available(download) = refreshed.result?;
                        reused = Some(download);
                    }
                }
            }
            if let Some(download) = reused {
                download
            } else {
                let transferred = adapter
                    .transfer_capability()
                    .ok_or(QuarkError::ProviderUnsupported)?
                    .transfer_with_context(&entry, store)?;
                if let Some(context) = transferred.updated_context {
                    store.update_provider_context(source, &context)?;
                }
                transferred.result?
            }
        }
    };
    validate_direct_url(&download.url)?;
    download.name = entry.entry.name;
    Ok(download)
}

pub(crate) fn share_id(
    candidate: &crate::parser::CloudShareCandidate,
    provider: CloudProvider,
) -> Result<String, QuarkError> {
    if candidate.provider != provider {
        return Err(QuarkError::InvalidResponse);
    }
    let url = url::Url::parse(&candidate.url).map_err(|_| QuarkError::InvalidResponse)?;
    let descriptor = crate::cloud::ProviderRegistry::global().get(provider);
    if !descriptor.accepts_share_url(&url) {
        return Err(QuarkError::InvalidResponse);
    }
    let id = descriptor
        .share_patterns
        .iter()
        .find_map(|prefix| url.path().strip_prefix(prefix))
        .filter(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        .ok_or(QuarkError::InvalidResponse)?;
    Ok(id.into())
}

pub(crate) fn download_headers(
    cookie: &str,
    response_cookies: &[String],
    ua: &str,
    referer: Option<&str>,
) -> Vec<(String, String)> {
    let mut pairs = BTreeMap::new();
    for part in cookie.split(';').chain(
        response_cookies
            .iter()
            .filter_map(|raw| raw.split(';').next()),
    ) {
        if let Some((name, value)) = part.trim().split_once('=') {
            if !name.is_empty() && !value.chars().any(char::is_control) {
                pairs.insert(name, value);
            }
        }
    }
    let mut headers = vec![
        (
            "Cookie".into(),
            pairs
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; "),
        ),
        ("User-Agent".into(), ua.into()),
    ];
    if let Some(referer) = referer {
        headers.push(("Referer".into(), referer.into()));
    }
    headers
}

/// This consumed budget is separate from the source's RAII submission lock.
/// Dropping that lock on failure must not re-arm automatic retries.
#[derive(Default)]
pub struct CloudShareRefreshBudget(Mutex<BTreeSet<String>>);

impl CloudShareRefreshBudget {
    pub fn claim(&self, source: &str, aria_error_code: Option<&str>) -> Result<bool, QuarkError> {
        if !source.starts_with("cloud-share://") || aria_error_code != Some("22") {
            return Ok(false);
        }
        parse_stable_cloud_share_source(source)?;
        Ok(self
            .0
            .lock()
            .map_err(|_| QuarkError::Storage)?
            .insert(source.into()))
    }
}

/// URL and headers go directly to aria2, while only the opaque source and safe
/// filename enter SQLite. Callers hold the source gate for duplicate detection.
pub async fn submit_current_share_download(
    service: &crate::DownloadService,
    prepared: &crate::DownloadTask,
    resolved: crate::cloud::commands::ResolvedCloudDownload,
    configured: crate::UriSubmissionOptions,
) -> Result<crate::DownloadTask, QuarkError> {
    parse_stable_cloud_share_source(&prepared.source)?;
    let (signed_url, options) = share_submission_options(resolved, configured)?;
    let task = service
        .store()
        .find(prepared.id)
        .map_err(|_| QuarkError::Storage)?;
    if task.source != prepared.source
        || task.kind != "cloud"
        || task.status != crate::TaskStatus::Parsing
        || task.aria_gid.is_some()
    {
        return Err(QuarkError::InvalidResponse);
    }
    match service
        .submit_uri_with_options(task.id, vec![signed_url], options)
        .await
    {
        Ok(task) => Ok(task),
        Err(_) => {
            // RPC failures may contain signed URIs. Never persist or expose them.
            let message = "aria2 未能接受网盘分享下载任务";
            let _ = service.store().fail(task.id, message);
            Err(QuarkError::Transport(message.into()))
        }
    }
}

pub async fn resubmit_current_share_download(
    service: &crate::DownloadService,
    expected: &crate::DownloadTask,
    resolved: crate::cloud::commands::ResolvedCloudDownload,
    configured: crate::UriSubmissionOptions,
) -> Result<Option<crate::DownloadTask>, QuarkError> {
    parse_stable_cloud_share_source(&expected.source)?;
    resubmit_current_cloud_download(service, expected, resolved, configured).await
}

/// Compare the durable task again after a potentially interactive login, so
/// cancellation, recycling and manual retries always win over auto recovery.
pub async fn resubmit_current_cloud_download(
    service: &crate::DownloadService,
    expected: &crate::DownloadTask,
    resolved: crate::cloud::commands::ResolvedCloudDownload,
    mut configured: crate::UriSubmissionOptions,
) -> Result<Option<crate::DownloadTask>, QuarkError> {
    if expected.source.starts_with("cloud-share://") {
        parse_stable_cloud_share_source(&expected.source)?;
    } else {
        crate::cloud::commands::parse_stable_cloud_source(&expected.source)?;
    }
    if let Some(file) = service
        .store()
        .files(expected.id)
        .map_err(|_| QuarkError::Storage)?
        .first()
    {
        let path = std::path::Path::new(&file.path);
        if path.is_absolute() {
            configured.directory = path.parent().map(std::path::Path::to_path_buf);
            configured.output_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned);
        }
    }
    let (signed_url, options) = share_submission_options(resolved, configured)?;
    service
        .compare_and_resubmit_uri_with_options(expected, vec![signed_url], options)
        .await
        .map_err(|_| QuarkError::Transport("aria2 未能接受网盘下载任务".into()))
}

pub(super) fn share_submission_options(
    resolved: crate::cloud::commands::ResolvedCloudDownload,
    configured: crate::UriSubmissionOptions,
) -> Result<(String, crate::UriSubmissionOptions), QuarkError> {
    validate_direct_url(&resolved.url)?;
    let signed_url = resolved.url.clone();
    let mut options = resolved.into_uri_submission_options();
    options.directory = configured.directory;
    options.output_name = configured.output_name.or(options.output_name);
    options.proxy = configured.proxy;
    options.allow_overwrite = configured.allow_overwrite;
    options.continue_download = configured.continue_download;
    options
        .clone()
        .into_aria2_options()
        .map_err(|_| QuarkError::InvalidResponse)?;
    Ok((signed_url, options))
}
