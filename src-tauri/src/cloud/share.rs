//! Secret share context lives only in the fixed DPAPI credential document.
//! Do not serialize these internal models into IPC, task records, or logs.
use super::{CloudProvider, CredentialStore, DpapiCredentialStore, QuarkError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

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

/// Adapters own provider protocol details. Neither inspection nor resolution
/// performs implicit session recovery: challenges must return to the caller.
pub trait CloudShareAdapter: Send + Sync {
    fn inspect(
        &self,
        candidate: &crate::parser::CloudShareCandidate,
    ) -> Result<ShareInspection, QuarkError>;
    fn resolve(&self, entry: &CloudShareResolvedEntry) -> Result<ShareResolution, QuarkError>;
}

pub enum ShareInspection {
    Plan(CloudSharePlan),
    AccessCodeRequired,
}

pub enum ShareConfirmationOutcome {
    Confirmation(CloudShareConfirmation),
    AccessCodeRequired,
}

/// A transfer request carries only a durable reference. Transfer execution and
/// cleanup are deliberately separate from the direct-download dispatcher.
pub struct TransferRequest {
    pub source: String,
}

pub enum ShareResolution {
    Direct(super::commands::ResolvedCloudDownload),
    TransferRequired(TransferRequest),
}

/// Tasks 5–7 register audited provider adapters here. Failing closed is vital:
/// a recognized share page must never fall back to ordinary URI downloading.
pub fn provider_share_adapter<'a>(
    _provider: CloudProvider,
    _credentials: &'a dyn CredentialStore,
    _http: &'a dyn super::CloudHttpTransport,
) -> Result<Box<dyn CloudShareAdapter + 'a>, QuarkError> {
    Err(QuarkError::ProviderUnsupported)
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
                    .map(|name| super::commands::safe_filename(name))
                    .collect::<Vec<_>>()
                    .join("/"),
                size: entry.size.map(|size| size.to_string()).unwrap_or_default(),
                selected: true,
            })
            .collect(),
    };
    store.upsert(&plan)?;
    Ok(ShareConfirmationOutcome::Confirmation(confirmation))
}

pub fn selected_share_sources(
    store: &CloudSharePlanStore,
    plan_source: &str,
    selected_ids: &[String],
) -> Result<Vec<String>, QuarkError> {
    if selected_ids.is_empty() {
        return Err(QuarkError::InvalidInput("请至少选择一个分享文件".into()));
    }
    let plan = store.resolve_entry(plan_source)?.plan;
    let mut selected = BTreeSet::new();
    selected_ids
        .iter()
        .map(|id| {
            let entry = plan
                .entries
                .iter()
                .find(|entry| &entry.entry_fingerprint == id)
                .filter(|_| selected.insert(id))
                .ok_or_else(missing_plan)?;
            Ok(plan.source_for(&entry.entry_id))
        })
        .collect()
}

fn validate_direct_url(url: &str) -> Result<(), QuarkError> {
    let parsed = url::Url::parse(url).map_err(|_| QuarkError::InvalidResponse)?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
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
) -> Result<super::commands::ResolvedCloudDownload, QuarkError> {
    let entry = store.resolve_entry(source)?;
    match adapter.resolve(&entry)? {
        ShareResolution::Direct(mut download) => {
            validate_direct_url(&download.url)?;
            // The inspected filename remains stable across expiring URL renewals.
            download.name = entry.entry.name;
            Ok(download)
        }
        ShareResolution::TransferRequired(_) => Err(QuarkError::ProviderUnsupported),
    }
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
    source: &str,
    resolved: super::commands::ResolvedCloudDownload,
    configured: crate::UriSubmissionOptions,
) -> Result<crate::DownloadTask, QuarkError> {
    parse_stable_cloud_share_source(source)?;
    let (signed_url, options) = share_submission_options(resolved, configured)?;
    let task = service
        .store()
        .create(crate::NewTask {
            source: source.into(),
            name: options
                .output_name
                .clone()
                .ok_or(QuarkError::InvalidResponse)?,
            kind: "cloud".into(),
        })
        .map_err(|_| QuarkError::Storage)?;
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
    task_id: uuid::Uuid,
    resolved: super::commands::ResolvedCloudDownload,
    mut configured: crate::UriSubmissionOptions,
) -> Result<crate::DownloadTask, QuarkError> {
    let task = service
        .store()
        .find(task_id)
        .map_err(|_| QuarkError::Storage)?;
    parse_stable_cloud_share_source(&task.source)?;
    if let Some(file) = service
        .store()
        .files(task_id)
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
        .resubmit_uri_with_options(task_id, vec![signed_url], options)
        .await
        .map_err(|_| QuarkError::Transport("aria2 未能接受网盘分享下载任务".into()))
}

fn share_submission_options(
    resolved: super::commands::ResolvedCloudDownload,
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
use zeroize::Zeroizing;

pub const SHARE_PLAN_SECRET: &str = "cloud-share-plans-v1";
const VERSION: u8 = 1;
const MAX_PLANS: usize = 256;
const RETENTION_MS: u64 = 30 * 24 * 60 * 60 * 1_000;

fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn entry_fingerprint(share_id: &str, entry_id: &str) -> String {
    sha256_hex(&format!("{share_id}\0{entry_id}"))
}

pub fn stable_share_source(provider: CloudProvider, share_id: &str, entry_id: &str) -> String {
    format!(
        "cloud-share://{}/{}/{}",
        provider.key(),
        sha256_hex(share_id),
        entry_fingerprint(share_id, entry_id)
    )
}

/// Parse literally: URL normalization must not accept credentials, ports,
/// percent escapes, uppercase hashes, queries, fragments, or extra segments.
pub fn parse_stable_cloud_share_source(
    source: &str,
) -> Result<(CloudProvider, String, String), QuarkError> {
    let invalid = || QuarkError::InvalidInput("网盘分享来源无效，请重新解析分享链接".into());
    let mut parts = source
        .strip_prefix("cloud-share://")
        .ok_or_else(invalid)?
        .split('/');
    let provider = match parts.next() {
        Some("quark") => CloudProvider::Quark,
        Some("pikpak") => CloudProvider::Pikpak,
        Some("baidu") => CloudProvider::Baidu,
        Some("tianyi") => CloudProvider::Tianyi,
        Some("aliyun") => CloudProvider::Aliyun,
        Some("xunlei") => CloudProvider::Xunlei,
        Some("uc") => CloudProvider::Uc,
        _ => return Err(invalid()),
    };
    let share = parts.next().ok_or_else(invalid)?;
    let entry = parts.next().ok_or_else(invalid)?;
    let valid_hash = |value: &str| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    if parts.next().is_some() || !valid_hash(share) || !valid_hash(entry) {
        return Err(invalid());
    }
    Ok((provider, share.into(), entry.into()))
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CloudShareEntry {
    pub entry_id: String,
    pub entry_fingerprint: String,
    pub name: String,
    pub relative_directory: Vec<String>,
    pub size: Option<u64>,
}

impl CloudShareEntry {
    pub fn new(
        share_id: &str,
        entry_id: String,
        name: String,
        relative_directory: Vec<String>,
        size: Option<u64>,
    ) -> Self {
        Self {
            entry_fingerprint: entry_fingerprint(share_id, &entry_id),
            entry_id,
            name,
            relative_directory,
            size,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudShareCleanupState {
    Pending,
    Attempted,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CloudShareTransfer {
    pub entry_fingerprint: String,
    pub created_file_id: String,
    pub cleanup_state: CloudShareCleanupState,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CloudSharePlan {
    pub version: u8,
    pub provider: CloudProvider,
    pub share_fingerprint: String,
    pub canonical_share_id: String,
    pub access_code: Option<String>,
    pub entries: Vec<CloudShareEntry>,
    /// Includes canonical URL and provider-specific share/session tokens.
    pub provider_context: serde_json::Value,
    pub created_at_ms: u64,
    pub last_used_at_ms: u64,
    pub transfers: Vec<CloudShareTransfer>,
}

impl CloudSharePlan {
    pub fn new(
        provider: CloudProvider,
        canonical_share_id: String,
        access_code: Option<String>,
        entries: Vec<CloudShareEntry>,
        provider_context: serde_json::Value,
    ) -> Self {
        Self {
            version: VERSION,
            provider,
            share_fingerprint: sha256_hex(&canonical_share_id),
            canonical_share_id,
            access_code,
            entries,
            provider_context,
            created_at_ms: 0,
            last_used_at_ms: 0,
            transfers: Vec::new(),
        }
    }

    pub fn source_for(&self, entry_id: &str) -> String {
        stable_share_source(self.provider, &self.canonical_share_id, entry_id)
    }

    fn validate(&self) -> Result<(), QuarkError> {
        let mut entries = BTreeSet::new();
        if self.version != VERSION
            || self.canonical_share_id.is_empty()
            || self.canonical_share_id.contains('\0')
            || self.share_fingerprint != sha256_hex(&self.canonical_share_id)
            || self.entries.iter().any(|entry| {
                entry.entry_id.is_empty()
                    || entry.entry_id.contains('\0')
                    || entry.entry_fingerprint
                        != entry_fingerprint(&self.canonical_share_id, &entry.entry_id)
                    || !entries.insert(entry.entry_fingerprint.as_str())
            })
            || self.transfers.iter().any(|transfer| {
                transfer.created_file_id.is_empty()
                    || !entries.contains(transfer.entry_fingerprint.as_str())
            })
        {
            return Err(QuarkError::Storage);
        }
        Ok(())
    }
}

/// Deliberately not serializable: callers must build a safe IPC projection.
#[derive(Clone)]
pub struct CloudShareResolvedEntry {
    pub provider: CloudProvider,
    pub plan: CloudSharePlan,
    pub entry: CloudShareEntry,
}

impl CloudShareResolvedEntry {
    pub fn access_code(&self) -> Option<&str> {
        self.plan.access_code.as_deref()
    }
}

// Debug output must never expand raw IDs, names, URLs, access codes or tokens.
macro_rules! redacted_debug {
    ($($name:ty),+ $(,)?) => { $(
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), " { [REDACTED] }"))
            }
        }
    )+ };
}
redacted_debug!(
    CloudSharePlan,
    CloudShareEntry,
    CloudShareTransfer,
    CloudShareResolvedEntry
);

#[derive(Serialize, Deserialize)]
struct StoredPlan {
    plan: CloudSharePlan,
    last_used_order: u64,
}

#[derive(Serialize, Deserialize)]
struct PlanDocument {
    version: u8,
    sequence: u64,
    plans: BTreeMap<String, StoredPlan>,
}

impl Default for PlanDocument {
    fn default() -> Self {
        Self {
            version: VERSION,
            sequence: 0,
            plans: BTreeMap::new(),
        }
    }
}

fn plan_key(provider: CloudProvider, fingerprint: &str) -> String {
    format!("{}/{fingerprint}", provider.key())
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

impl PlanDocument {
    fn touch(&mut self, key: &str, now: u64) -> Result<(), QuarkError> {
        self.sequence = self.sequence.checked_add(1).ok_or(QuarkError::Storage)?;
        let stored = self.plans.get_mut(key).ok_or_else(missing_plan)?;
        stored.last_used_order = self.sequence;
        stored.plan.last_used_at_ms = now;
        Ok(())
    }

    fn prune(&mut self, now: u64) -> usize {
        let previous = self.plans.len();
        self.plans
            .retain(|_, stored| now.saturating_sub(stored.plan.last_used_at_ms) < RETENTION_MS);
        while self.plans.len() > MAX_PLANS {
            let oldest = self
                .plans
                .iter()
                .min_by_key(|(_, stored)| stored.last_used_order)
                .map(|(key, _)| key.clone())
                .unwrap();
            self.plans.remove(&oldest);
        }
        previous - self.plans.len()
    }
}

fn missing_plan() -> QuarkError {
    QuarkError::InvalidInput("网盘分享计划已失效或文件不存在，请重新解析分享链接".into())
}

/// Keep one store in CloudState. Its lock serializes load/modify/DPAPI-save,
/// including claiming cleanup, so concurrent workers cannot lose updates.
pub struct CloudSharePlanStore {
    credentials: Arc<dyn CredentialStore>,
    mutation: Mutex<()>,
    now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl CloudSharePlanStore {
    /// Production construction requires DPAPI; plaintext backing is test-only.
    pub fn new(credentials: DpapiCredentialStore) -> Self {
        Self {
            credentials: Arc::new(credentials),
            mutation: Mutex::new(()),
            now_ms: Arc::new(now_ms),
        }
    }

    fn load(&self) -> Result<PlanDocument, QuarkError> {
        let Some(bytes) = self.credentials.load(SHARE_PLAN_SECRET)? else {
            return Ok(PlanDocument::default());
        };
        let bytes = Zeroizing::new(bytes);
        let document: PlanDocument =
            serde_json::from_slice(&bytes).map_err(|_| QuarkError::Storage)?;
        if document.version != VERSION {
            return Err(QuarkError::Storage);
        }
        for (key, stored) in &document.plans {
            stored.plan.validate()?;
            if *key != plan_key(stored.plan.provider, &stored.plan.share_fingerprint)
                || stored.last_used_order > document.sequence
            {
                return Err(QuarkError::Storage);
            }
        }
        Ok(document)
    }

    fn save(&self, document: &PlanDocument) -> Result<(), QuarkError> {
        let bytes = Zeroizing::new(serde_json::to_vec(document).map_err(|_| QuarkError::Storage)?);
        self.credentials.save(SHARE_PLAN_SECRET, &bytes)
    }

    fn update<T>(
        &self,
        action: impl FnOnce(&mut PlanDocument, u64) -> Result<T, QuarkError>,
    ) -> Result<T, QuarkError> {
        let _guard = self.mutation.lock().map_err(|_| QuarkError::Storage)?;
        let now = (self.now_ms)();
        let mut document = self.load()?;
        let pruned = document.prune(now);
        let result = action(&mut document, now);
        // Persist expiration even when the requested source no longer resolves.
        if result.is_ok() {
            document.prune(now);
            self.save(&document)?;
        } else if pruned != 0 {
            self.save(&document)?;
        }
        result
    }

    pub fn upsert(&self, plan: &CloudSharePlan) -> Result<(), QuarkError> {
        plan.validate()?;
        self.update(|document, now| {
            let key = plan_key(plan.provider, &plan.share_fingerprint);
            let mut incoming = plan.clone();
            // Reinspection refreshes secrets/metadata without discarding recovery
            // context for existing tasks or rearming an already claimed cleanup.
            if let Some(previous) = document.plans.get(&key) {
                incoming.created_at_ms = previous.plan.created_at_ms;
                incoming.transfers = previous.plan.transfers.clone();
                for entry in &previous.plan.entries {
                    if !incoming
                        .entries
                        .iter()
                        .any(|current| current.entry_fingerprint == entry.entry_fingerprint)
                    {
                        incoming.entries.push(entry.clone());
                    }
                }
            } else {
                incoming.created_at_ms = now;
                incoming.transfers.clear();
            }
            document.plans.insert(
                key.clone(),
                StoredPlan {
                    plan: incoming,
                    last_used_order: 0,
                },
            );
            document.touch(&key, now)
        })
    }

    pub fn resolve_entry(&self, source: &str) -> Result<CloudShareResolvedEntry, QuarkError> {
        let (provider, share, fingerprint) = parse_stable_cloud_share_source(source)?;
        self.update(|document, now| {
            let key = plan_key(provider, &share);
            let entry = document
                .plans
                .get(&key)
                .and_then(|stored| {
                    stored
                        .plan
                        .entries
                        .iter()
                        .find(|entry| entry.entry_fingerprint == fingerprint)
                })
                .cloned()
                .ok_or_else(missing_plan)?;
            document.touch(&key, now)?;
            Ok(CloudShareResolvedEntry {
                provider,
                plan: document.plans[&key].plan.clone(),
                entry,
            })
        })
    }

    pub fn record_transfer(&self, source: &str, created_file_id: &str) -> Result<(), QuarkError> {
        let (provider, share, fingerprint) = parse_stable_cloud_share_source(source)?;
        if created_file_id.trim().is_empty() {
            return Err(QuarkError::InvalidResponse);
        }
        self.update(|document, now| {
            let key = plan_key(provider, &share);
            let plan = &mut document.plans.get_mut(&key).ok_or_else(missing_plan)?.plan;
            if !plan
                .entries
                .iter()
                .any(|entry| entry.entry_fingerprint == fingerprint)
            {
                return Err(missing_plan());
            }
            if !plan.transfers.iter().any(|transfer| {
                transfer.entry_fingerprint == fingerprint
                    && transfer.created_file_id == created_file_id
            }) {
                plan.transfers.push(CloudShareTransfer {
                    entry_fingerprint: fingerprint,
                    created_file_id: created_file_id.into(),
                    cleanup_state: CloudShareCleanupState::Pending,
                });
            }
            document.touch(&key, now)
        })
    }

    /// Claims only the exact entry's recorded objects. Completion/settings checks
    /// belong to the caller; this function performs no provider deletion itself.
    pub fn mark_cleanup_attempted(
        &self,
        source: &str,
    ) -> Result<Vec<CloudShareTransfer>, QuarkError> {
        let (provider, share, fingerprint) = parse_stable_cloud_share_source(source)?;
        self.update(|document, now| {
            let key = plan_key(provider, &share);
            let plan = &mut document.plans.get_mut(&key).ok_or_else(missing_plan)?.plan;
            if !plan
                .entries
                .iter()
                .any(|entry| entry.entry_fingerprint == fingerprint)
            {
                return Err(missing_plan());
            }
            let mut claimed = Vec::new();
            for transfer in &mut plan.transfers {
                if transfer.entry_fingerprint == fingerprint
                    && transfer.cleanup_state == CloudShareCleanupState::Pending
                {
                    transfer.cleanup_state = CloudShareCleanupState::Attempted;
                    claimed.push(transfer.clone());
                }
            }
            document.touch(&key, now)?;
            Ok(claimed)
        })
    }

    pub fn prune(&self) -> Result<usize, QuarkError> {
        let _guard = self.mutation.lock().map_err(|_| QuarkError::Storage)?;
        let mut document = self.load()?;
        let removed = document.prune((self.now_ms)());
        if removed > 0 {
            self.save(&document)?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::MemoryCredentialStore;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct DirectAdapter(AtomicU64);
    impl CloudShareAdapter for DirectAdapter {
        fn inspect(
            &self,
            _: &crate::parser::CloudShareCandidate,
        ) -> Result<ShareInspection, QuarkError> {
            Ok(ShareInspection::Plan(fixture("private-share")))
        }
        fn resolve(&self, _: &CloudShareResolvedEntry) -> Result<ShareResolution, QuarkError> {
            let revision = self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ShareResolution::Direct(
                super::super::commands::ResolvedCloudDownload {
                    url: format!("https://cdn.example/file?signature={revision}"),
                    name: "file.txt".into(),
                    headers: vec![("x-share-token".into(), format!("token-{revision}"))],
                },
            ))
        }
    }

    #[test]
    fn expired_share_url_refreshes_once_with_new_headers_without_persisting_url() {
        let (store, credentials, _) = memory_store();
        let plan = fixture("private-share");
        store.upsert(&plan).unwrap();
        let source = plan.source_for("f1");
        let adapter = DirectAdapter(AtomicU64::new(0));
        let first = resolve_current_share_download(&store, &adapter, &source).unwrap();
        let refresh = CloudShareRefreshBudget::default();
        assert!(refresh.claim(&source, Some("22")).unwrap());
        let second = resolve_current_share_download(&store, &adapter, &source).unwrap();
        assert_eq!(first.url, "https://cdn.example/file?signature=0");
        assert_eq!(second.url, "https://cdn.example/file?signature=1");
        assert_eq!(
            second.headers,
            vec![("x-share-token".into(), "token-1".into())]
        );
        assert!(!refresh.claim(&source, Some("22")).unwrap());
        let saved =
            String::from_utf8(credentials.load(SHARE_PLAN_SECRET).unwrap().unwrap()).unwrap();
        assert!(!saved.contains("cdn.example") && !saved.contains("token-1"));
        assert!(!source.contains("signature") && !source.contains("private-share"));
    }

    #[test]
    fn refresh_budget_ignores_non_share_and_non_http_failures() {
        let budget = CloudShareRefreshBudget::default();
        assert!(!budget.claim("cloud://baidu/file", Some("22")).unwrap());
        let source = fixture("share").source_for("f1");
        for code in [None, Some("3"), Some("24"), Some("403")] {
            assert!(!budget.claim(&source, code).unwrap());
        }
        assert!(budget.claim(&source, Some("22")).unwrap());
        assert!(!budget.claim(&source, Some("22")).unwrap());
    }

    #[test]
    fn inspection_projects_only_opaque_file_ids_and_validates_confirmation_selection() {
        let (store, _, _) = memory_store();
        let adapter = DirectAdapter(AtomicU64::new(0));
        let candidate = crate::parser::extract_cloud_share_candidate(
            "https://pan.baidu.com/s/private-share",
            None,
        )
        .unwrap()
        .unwrap();
        let ShareConfirmationOutcome::Confirmation(confirmation) =
            inspect_share_with_adapter(&store, &adapter, &candidate).unwrap()
        else {
            panic!("expected plan")
        };
        let json = serde_json::to_string(&confirmation).unwrap();
        for secret in ["private-share", "secret-code", "secret-token", "https://"] {
            assert!(!json.contains(secret));
        }
        let sources = selected_share_sources(
            &store,
            &confirmation.plan_source,
            &[confirmation.resources[0].id.clone()],
        )
        .unwrap();
        assert_eq!(sources, vec![confirmation.plan_source.clone()]);
        assert!(selected_share_sources(
            &store,
            &confirmation.plan_source,
            &["forged-file-id".into()]
        )
        .is_err());
        assert!(selected_share_sources(&store, &confirmation.plan_source, &[]).is_err());
    }

    struct FixedAdapter(fn() -> Result<ShareResolution, QuarkError>);
    impl CloudShareAdapter for FixedAdapter {
        fn inspect(
            &self,
            _: &crate::parser::CloudShareCandidate,
        ) -> Result<ShareInspection, QuarkError> {
            Ok(ShareInspection::AccessCodeRequired)
        }
        fn resolve(&self, _: &CloudShareResolvedEntry) -> Result<ShareResolution, QuarkError> {
            (self.0)()
        }
    }

    #[test]
    fn share_pages_and_transfer_requirements_never_become_direct_downloads() {
        let (store, _, _) = memory_store();
        let plan = fixture("private-share");
        store.upsert(&plan).unwrap();
        let source = plan.source_for("f1");
        let page = FixedAdapter(|| {
            Ok(ShareResolution::Direct(
                super::super::commands::ResolvedCloudDownload {
                    url: "https://pan.baidu.com/s/private-share?pwd=secret".into(),
                    name: "file.txt".into(),
                    headers: vec![],
                },
            ))
        });
        assert!(matches!(
            resolve_current_share_download(&store, &page, &source),
            Err(QuarkError::InvalidResponse)
        ));
        let transfer = FixedAdapter(|| {
            Ok(ShareResolution::TransferRequired(TransferRequest {
                source: fixture("private-share").source_for("f1"),
            }))
        });
        assert!(matches!(
            resolve_current_share_download(&store, &transfer, &source),
            Err(QuarkError::ProviderUnsupported)
        ));
    }

    #[test]
    fn challenge_errors_are_returned_without_an_automatic_adapter_retry() {
        let (store, _, _) = memory_store();
        let plan = fixture("private-share");
        store.upsert(&plan).unwrap();
        let source = plan.source_for("f1");
        for error in [
            QuarkError::CaptchaRequired,
            QuarkError::WebAuthorizationRequired,
            QuarkError::PikpakRegionalMembershipRestricted,
            QuarkError::InvalidInput("访问码错误".into()),
            QuarkError::Provider("forbidden".into()),
        ] {
            struct OnceAdapter(Mutex<Option<QuarkError>>);
            impl CloudShareAdapter for OnceAdapter {
                fn inspect(
                    &self,
                    _: &crate::parser::CloudShareCandidate,
                ) -> Result<ShareInspection, QuarkError> {
                    unreachable!()
                }
                fn resolve(
                    &self,
                    _: &CloudShareResolvedEntry,
                ) -> Result<ShareResolution, QuarkError> {
                    Err(self
                        .0
                        .lock()
                        .unwrap()
                        .take()
                        .expect("must not retry challenge"))
                }
            }
            let adapter = OnceAdapter(Mutex::new(Some(error)));
            assert!(resolve_current_share_download(&store, &adapter, &source).is_err());
        }
    }

    fn fixture(id: &str) -> CloudSharePlan {
        CloudSharePlan::new(
            CloudProvider::Baidu,
            id.into(),
            Some("secret-code".into()),
            vec![CloudShareEntry::new(
                id,
                "f1".into(),
                "file.txt".into(),
                vec![],
                Some(7),
            )],
            serde_json::json!({"url":"https://pan.baidu.com/s/private-share", "token":"secret-token"}),
        )
    }

    fn memory_store() -> (
        CloudSharePlanStore,
        Arc<MemoryCredentialStore>,
        Arc<AtomicU64>,
    ) {
        let credentials = Arc::new(MemoryCredentialStore::default());
        let now = Arc::new(AtomicU64::new(1_000));
        let clock = now.clone();
        let store = CloudSharePlanStore {
            credentials: credentials.clone(),
            mutation: Mutex::new(()),
            now_ms: Arc::new(move || clock.load(Ordering::SeqCst)),
        };
        (store, credentials, now)
    }

    #[test]
    fn stable_share_source_has_only_hashes_and_binds_entry_to_share() {
        let source = stable_share_source(CloudProvider::Baidu, "share-id", "file-id");
        let (provider, share, entry) = parse_stable_cloud_share_source(&source).unwrap();
        assert_eq!(provider, CloudProvider::Baidu);
        assert_eq!(share.len(), 64);
        assert_eq!(entry.len(), 64);
        assert!(!source.contains("share-id") && !source.contains("file-id"));
        assert_eq!(source, stable_share_source(provider, "share-id", "file-id"));
        let other = stable_share_source(provider, "different-share", "file-id");
        assert_ne!(entry, parse_stable_cloud_share_source(&other).unwrap().2);
    }

    #[test]
    fn parser_accepts_only_exact_providers_and_lowercase_sha256_segments() {
        let hash = "a".repeat(64);
        for provider in [
            CloudProvider::Quark,
            CloudProvider::Pikpak,
            CloudProvider::Baidu,
            CloudProvider::Tianyi,
            CloudProvider::Aliyun,
            CloudProvider::Xunlei,
            CloudProvider::Uc,
        ] {
            assert_eq!(
                parse_stable_cloud_share_source(&format!(
                    "cloud-share://{}/{hash}/{hash}",
                    provider.key()
                ))
                .unwrap()
                .0,
                provider
            );
        }
        for source in [
            format!("cloud-share://other/{hash}/{hash}"),
            format!("cloud-share://Baidu/{hash}/{hash}"),
            format!("CLOUD-SHARE://baidu/{hash}/{hash}"),
            format!("cloud-share://user@baidu/{hash}/{hash}"),
            format!("cloud-share://baidu:443/{hash}/{hash}"),
            format!("cloud-share://baidu/{hash}/{hash}/"),
            format!("cloud-share://baidu/{hash}/{hash}?pwd=secret"),
            format!("cloud-share://baidu/{hash}/{hash}#secret"),
            format!(" cloud-share://baidu/{hash}/{hash}"),
            format!("cloud-share://baidu/{}/{hash}", "A".repeat(64)),
            format!("cloud-share://baidu/{}/{hash}", "g".repeat(64)),
            format!("cloud-share://baidu/{}/{hash}", "a".repeat(63)),
            format!("cloud-share://baidu/{}/{hash}", "a".repeat(65)),
            format!("cloud-share://baidu/%61{}/{hash}", "a".repeat(63)),
            "cloud-share://baidu//".into(),
        ] {
            assert!(
                parse_stable_cloud_share_source(&source).is_err(),
                "accepted {source}"
            );
        }
    }

    #[test]
    fn round_trip_uses_fixed_key_and_recovers_secrets_without_exposing_them() {
        let (store, credentials, _) = memory_store();
        let plan = fixture("private-share");
        store.upsert(&plan).unwrap();
        assert!(credentials.load("cloud-share-plans-v1").unwrap().is_some());
        let resolved = store.resolve_entry(&plan.source_for("f1")).unwrap();
        assert_eq!(resolved.access_code(), Some("secret-code"));
        assert_eq!(resolved.plan.provider_context["token"], "secret-token");
        assert_eq!(resolved.entry.entry_id, "f1");
        for secret in ["secret-code", "secret-token", "private-share", "https://"] {
            assert!(!plan.source_for("f1").contains(secret));
            assert!(!format!("{resolved:?}").contains(secret));
        }
        let unknown = stable_share_source(CloudProvider::Baidu, "private-share", "unknown");
        assert!(store.resolve_entry(&unknown).is_err());
    }

    #[test]
    fn plans_expire_at_thirty_days_and_resolution_refreshes_retention() {
        let (store, _, now) = memory_store();
        let old = fixture("old");
        let active = fixture("active");
        store.upsert(&old).unwrap();
        store.upsert(&active).unwrap();
        now.store(1_000 + 29 * 24 * 60 * 60 * 1_000, Ordering::SeqCst);
        store.resolve_entry(&active.source_for("f1")).unwrap();
        now.store(1_000 + 30 * 24 * 60 * 60 * 1_000, Ordering::SeqCst);
        assert_eq!(store.prune().unwrap(), 1);
        assert!(store.resolve_entry(&old.source_for("f1")).is_err());
        assert!(store.resolve_entry(&active.source_for("f1")).is_ok());
    }

    #[test]
    fn lru_caps_plans_at_256_even_when_accesses_share_a_timestamp() {
        let (store, _, _) = memory_store();
        for index in 0..256 {
            store.upsert(&fixture(&format!("share-{index}"))).unwrap();
        }
        let first = fixture("share-0").source_for("f1");
        store.resolve_entry(&first).unwrap();
        store.upsert(&fixture("share-256")).unwrap();
        assert!(store.resolve_entry(&first).is_ok());
        assert!(store
            .resolve_entry(&fixture("share-1").source_for("f1"))
            .is_err());
        assert!(store
            .resolve_entry(&fixture("share-256").source_for("f1"))
            .is_ok());
    }

    #[test]
    fn transfer_records_survive_reinspection_and_cleanup_claims_only_once() {
        let (store, _, _) = memory_store();
        let plan = fixture("private-share");
        let source = plan.source_for("f1");
        store.upsert(&plan).unwrap();
        store.record_transfer(&source, "created-id").unwrap();
        store.record_transfer(&source, "created-id").unwrap();
        store.upsert(&plan).unwrap();
        let transfers = store.mark_cleanup_attempted(&source).unwrap();
        assert_eq!(transfers.len(), 1);
        assert_eq!(transfers[0].created_file_id, "created-id");
        assert_eq!(
            transfers[0].cleanup_state,
            CloudShareCleanupState::Attempted
        );
        assert!(store.mark_cleanup_attempted(&source).unwrap().is_empty());
        store.record_transfer(&source, "created-id").unwrap();
        assert!(store.mark_cleanup_attempted(&source).unwrap().is_empty());
        assert!(store.record_transfer(&source, "").is_err());
        assert!(store
            .record_transfer("cloud-share://baidu/bad/bad", "created-id")
            .is_err());
    }

    #[test]
    fn corrupt_or_unsupported_documents_fail_closed_without_leaking_contents() {
        let (store, credentials, _) = memory_store();
        for bytes in [
            b"secret-token".as_slice(),
            br#"{"version":99,"plans":{}}"#.as_slice(),
        ] {
            credentials.save(SHARE_PLAN_SECRET, bytes).unwrap();
            let error = store.upsert(&fixture("share")).unwrap_err();
            assert!(!error.to_string().contains("secret-token"));
            assert_eq!(credentials.load(SHARE_PLAN_SECRET).unwrap().unwrap(), bytes);
        }
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_plan_round_trip_recovers_access_code_but_disk_and_source_do_not() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            CloudSharePlanStore::new(DpapiCredentialStore::new(directory.path().into()).unwrap());
        let plan = fixture("private-share");
        store.upsert(&plan).unwrap();
        let encrypted = std::fs::read(directory.path().join("cloud-share-plans-v1.bin")).unwrap();
        for secret in ["secret-code", "secret-token", "private-share"] {
            assert!(!encrypted
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()));
        }
        drop(store);
        let reopened =
            CloudSharePlanStore::new(DpapiCredentialStore::new(directory.path().into()).unwrap());
        assert_eq!(
            reopened
                .resolve_entry(&plan.source_for("f1"))
                .unwrap()
                .access_code(),
            Some("secret-code")
        );
    }
}
