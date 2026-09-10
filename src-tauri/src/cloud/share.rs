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
    /// Internal-only context rotation. The dispatcher commits this to DPAPI
    /// before releasing a URL or starting the transfer phase.
    fn resolve_with_context(
        &self,
        entry: &CloudShareResolvedEntry,
    ) -> Result<ShareOperationResult<ShareResolution>, QuarkError> {
        Ok(ShareOperationResult {
            result: self.resolve(entry),
            updated_context: None,
        })
    }
    /// Called only for the matching durable source after a typed transfer
    /// requirement. Implementations recheck login and journal returned personal
    /// IDs before requesting the personal download URL.
    fn transfer(
        &self,
        _entry: &CloudShareResolvedEntry,
        _store: &CloudSharePlanStore,
    ) -> Result<super::commands::ResolvedCloudDownload, QuarkError> {
        Err(QuarkError::ProviderUnsupported)
    }
    fn transfer_with_context(
        &self,
        entry: &CloudShareResolvedEntry,
        store: &CloudSharePlanStore,
    ) -> Result<ShareOperationResult<super::commands::ResolvedCloudDownload>, QuarkError> {
        Ok(ShareOperationResult {
            result: self.transfer(entry, store),
            updated_context: None,
        })
    }
}

/// Never serialize this result into IPC or task records: context is DPAPI-only.
pub struct ShareOperationResult<T> {
    /// A successful response may revoke context even when the operation fails.
    /// Persist updated_context before propagating this error.
    pub result: Result<T, QuarkError>,
    pub updated_context: Option<serde_json::Value>,
}

pub enum ShareInspection {
    Plan(CloudSharePlan),
    AccessCodeRequired,
}

pub enum ShareConfirmationOutcome {
    Confirmation(CloudShareConfirmation),
    AutoSubmit { source: String },
    AccessCodeRequired,
}

/// A transfer request carries only a durable reference. Cleanup remains
/// separate; the dispatcher may materialize this request through the adapter.
pub struct TransferRequest {
    pub source: String,
}

pub enum ShareResolution {
    Direct(super::commands::ResolvedCloudDownload),
    TransferRequired(TransferRequest),
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    use crate::cloud::{CloudHttpRequest, CloudHttpResponse, CloudHttpTransport};
    use serde_json::Value;
    pub struct Http {
        pub requests: Mutex<Vec<CloudHttpRequest>>,
        replies: Mutex<std::collections::VecDeque<Value>>,
    }
    impl Http {
        pub fn new(replies: Vec<Value>) -> Self {
            Self {
                requests: Mutex::new(vec![]),
                replies: Mutex::new(replies.into()),
            }
        }
    }
    impl CloudHttpTransport for Http {
        fn send(&self, request: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            self.requests.lock().unwrap().push(request);
            Ok(CloudHttpResponse {
                status: 200,
                body: self
                    .replies
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected share HTTP request")
                    .to_string(),
                set_cookies: vec!["SESSION=issued; Path=/; Secure".into()],
            })
        }
    }
    pub fn candidate(provider: CloudProvider) -> crate::parser::CloudShareCandidate {
        let url = match provider {
            CloudProvider::Aliyun => "https://www.alipan.com/s/share123",
            CloudProvider::Pikpak => "https://mypikpak.com/s/share123",
            CloudProvider::Tianyi => "https://cloud.189.cn/t/share123",
            _ => unreachable!(),
        };
        crate::parser::CloudShareCandidate {
            provider,
            url: url.into(),
            access_code: Some("a123".into()),
        }
    }
    pub fn entry(plan: CloudSharePlan) -> CloudShareResolvedEntry {
        CloudShareResolvedEntry {
            provider: plan.provider,
            entry: plan.entries[0].clone(),
            plan,
        }
    }
    #[test]
    fn all_seven_share_providers_are_registered_without_network_side_effects() {
        let credentials = crate::cloud::MemoryCredentialStore::default();
        let http = Http::new(vec![]);
        for provider in [
            CloudProvider::Quark,
            CloudProvider::Pikpak,
            CloudProvider::Baidu,
            CloudProvider::Tianyi,
            CloudProvider::Aliyun,
            CloudProvider::Xunlei,
            CloudProvider::Uc,
        ] {
            assert!(provider_share_adapter(provider, &credentials, &http).is_ok());
        }
        assert!(http.requests.lock().unwrap().is_empty());
    }
}

/// Tasks 5–7 register audited provider adapters here. Failing closed is vital:
/// a recognized share page must never fall back to ordinary URI downloading.
pub fn provider_share_adapter<'a>(
    provider: CloudProvider,
    credentials: &'a dyn CredentialStore,
    http: &'a dyn super::CloudHttpTransport,
) -> Result<Box<dyn CloudShareAdapter + 'a>, QuarkError> {
    match provider {
        CloudProvider::Quark => Ok(Box::new(super::quark::QuarkShareAdapter {
            credentials,
            http,
        })),
        CloudProvider::Baidu => Ok(Box::new(super::baidu::BaiduShareAdapter {
            credentials,
            http,
        })),
        CloudProvider::Aliyun => Ok(Box::new(super::aliyun::AliyunShareAdapter { http })),
        CloudProvider::Pikpak => Ok(Box::new(super::pikpak::PikPakShareAdapter {
            credentials,
            http,
        })),
        CloudProvider::Tianyi => Ok(Box::new(super::tianyi::TianyiShareAdapter {
            credentials,
            http,
        })),
        CloudProvider::Uc => Ok(Box::new(super::uc::UcShareAdapter { credentials, http })),
        CloudProvider::Xunlei => Ok(Box::new(super::xunlei::XunleiShareAdapter {
            credentials,
            http,
        })),
    }
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
                    .map(|name| super::commands::safe_filename(name))
                    .collect::<Vec<_>>()
                    .join("/"),
                size: entry.size.map(|size| size.to_string()).unwrap_or_default(),
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
        .collect::<Result<_, _>>()
}

pub(super) fn validate_direct_url(url: &str) -> Result<(), QuarkError> {
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
) -> Result<super::commands::ResolvedCloudDownload, QuarkError> {
    let mut entry = store.resolve_entry(source)?;
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
            let transferred = adapter.transfer_with_context(&entry, store)?;
            if let Some(context) = transferred.updated_context {
                store.update_provider_context(source, &context)?;
            }
            transferred.result?
        }
    };
    validate_direct_url(&download.url)?;
    download.name = entry.entry.name;
    Ok(download)
}

pub(super) fn share_id(
    candidate: &crate::parser::CloudShareCandidate,
    provider: CloudProvider,
) -> Result<String, QuarkError> {
    if candidate.provider != provider {
        return Err(QuarkError::InvalidResponse);
    }
    let url = url::Url::parse(&candidate.url).map_err(|_| QuarkError::InvalidResponse)?;
    let expected: &[&str] = match provider {
        CloudProvider::Quark => &["pan.quark.cn"],
        CloudProvider::Uc => &["drive.uc.cn"],
        CloudProvider::Xunlei => &["pan.xunlei.com"],
        CloudProvider::Baidu => &["pan.baidu.com"],
        CloudProvider::Pikpak => &["mypikpak.com", "www.mypikpak.com"],
        CloudProvider::Tianyi => &["cloud.189.cn"],
        CloudProvider::Aliyun => &[
            "alipan.com",
            "www.alipan.com",
            "aliyundrive.com",
            "www.aliyundrive.com",
        ],
    };
    if !url.host_str().is_some_and(|host| expected.contains(&host))
        || !matches!(url.scheme(), "https" | "http")
    {
        return Err(QuarkError::InvalidResponse);
    }
    let id = url
        .path()
        .strip_prefix(if provider == CloudProvider::Tianyi {
            "/t/"
        } else {
            "/s/"
        })
        .filter(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        .ok_or(QuarkError::InvalidResponse)?;
    Ok(id.into())
}

pub(super) fn download_headers(
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
    resolved: super::commands::ResolvedCloudDownload,
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
    resolved: super::commands::ResolvedCloudDownload,
    mut configured: crate::UriSubmissionOptions,
) -> Result<Option<crate::DownloadTask>, QuarkError> {
    parse_stable_cloud_share_source(&expected.source)?;
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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CloudShareRootKind {
    #[default]
    Unknown,
    File {
        entry_id: String,
    },
    Directory,
    Collection,
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
    Failed,
    Succeeded,
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
    #[serde(default)]
    pub root_kind: CloudShareRootKind,
    /// Includes canonical URL and provider-specific share/session tokens.
    pub provider_context: serde_json::Value,
    pub created_at_ms: u64,
    pub last_used_at_ms: u64,
    pub transfers: Vec<CloudShareTransfer>,
    /// Selected entries are registered before submission starts. Empty task sets
    /// block cleanup until submission succeeds; removed/failed tasks also block.
    #[serde(default)]
    pub linked_local_tasks: BTreeMap<String, BTreeSet<uuid::Uuid>>,
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
            root_kind: CloudShareRootKind::Unknown,
            provider_context,
            created_at_ms: 0,
            last_used_at_ms: 0,
            transfers: Vec::new(),
            linked_local_tasks: BTreeMap::new(),
        }
    }

    pub fn source_for(&self, entry_id: &str) -> String {
        stable_share_source(self.provider, &self.canonical_share_id, entry_id)
    }

    fn validate(&self) -> Result<(), QuarkError> {
        let mut entries = BTreeSet::new();
        let valid_root = match &self.root_kind {
            CloudShareRootKind::File { entry_id } => {
                self.entries.iter().any(|entry| entry.entry_id == *entry_id)
            }
            CloudShareRootKind::Unknown
            | CloudShareRootKind::Directory
            | CloudShareRootKind::Collection => true,
        };
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
            || self
                .linked_local_tasks
                .keys()
                .any(|fingerprint| !entries.contains(fingerprint.as_str()))
            || !valid_root
        {
            return Err(QuarkError::Storage);
        }
        Ok(())
    }

    fn is_pinned(&self) -> bool {
        !self.linked_local_tasks.is_empty()
            || self
                .transfers
                .iter()
                .any(|transfer| transfer.cleanup_state != CloudShareCleanupState::Succeeded)
    }

    fn remove_terminal_empty_link(&mut self, fingerprint: &str) {
        let has_unfinished_transfer = self.transfers.iter().any(|transfer| {
            transfer.entry_fingerprint == fingerprint
                && transfer.cleanup_state != CloudShareCleanupState::Succeeded
        });
        let is_empty = self
            .linked_local_tasks
            .get(fingerprint)
            .is_some_and(BTreeSet::is_empty);
        if is_empty && !has_unfinished_transfer {
            self.linked_local_tasks.remove(fingerprint);
        }
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
    CloudShareRootKind,
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
        self.plans.retain(|_, stored| {
            stored.plan.is_pinned()
                || now.saturating_sub(stored.plan.last_used_at_ms) < RETENTION_MS
        });
        while self.plans.len() > MAX_PLANS {
            let Some(oldest) = self
                .plans
                .iter()
                .filter(|(_, stored)| !stored.plan.is_pinned())
                .min_by_key(|(_, stored)| stored.last_used_order)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
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
        let _guard = self.mutation.lock().map_err(|_| QuarkError::Storage)?;
        let now = (self.now_ms)();
        let mut document = self.load()?;
        document.prune(now);
        let key = plan_key(plan.provider, &plan.share_fingerprint);
        if !document.plans.contains_key(&key)
            && document.plans.len() >= MAX_PLANS
            && !document
                .plans
                .values()
                .any(|stored| !stored.plan.is_pinned())
        {
            return Err(QuarkError::InvalidInput(
                "网盘分享计划存储已满，请先永久删除已完成的相关下载任务".into(),
            ));
        }
        let mut incoming = plan.clone();
        // Reinspection refreshes secrets/metadata without discarding recovery
        // context for existing tasks or rearming an already claimed cleanup.
        if let Some(previous) = document.plans.get(&key) {
            incoming.created_at_ms = previous.plan.created_at_ms;
            incoming.transfers = previous.plan.transfers.clone();
            incoming.linked_local_tasks = previous.plan.linked_local_tasks.clone();
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
            incoming.linked_local_tasks.clear();
        }
        document.plans.insert(
            key.clone(),
            StoredPlan {
                plan: incoming,
                last_used_order: 0,
            },
        );
        document.touch(&key, now)?;
        document.prune(now);
        self.save(&document)
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

    fn update_provider_context(
        &self,
        source: &str,
        context: &serde_json::Value,
    ) -> Result<(), QuarkError> {
        let (provider, share, fingerprint) = parse_stable_cloud_share_source(source)?;
        self.update(|document, now| {
            let key = plan_key(provider, &share);
            let plan = &mut document.plans.get_mut(&key).ok_or_else(missing_plan)?.plan;
            if !plan.entries.iter().any(|entry| entry.entry_fingerprint == fingerprint) {
                return Err(missing_plan());
            }
            plan.provider_context = context.clone();
            document.touch(&key, now)
        })
        .map_err(|_| QuarkError::Storage)
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

    pub fn link_local_sources(&self, sources: &[String]) -> Result<(), QuarkError> {
        self.update(|document, now| {
            for source in sources {
                let (provider, share, fingerprint) = parse_stable_cloud_share_source(source)?;
                let key = plan_key(provider, &share);
                let plan = &mut document.plans.get_mut(&key).ok_or_else(missing_plan)?.plan;
                if !plan
                    .entries
                    .iter()
                    .any(|entry| entry.entry_fingerprint == fingerprint)
                {
                    return Err(missing_plan());
                }
                plan.linked_local_tasks.entry(fingerprint).or_default();
                document.touch(&key, now)?;
            }
            Ok(())
        })
    }

    pub fn link_local_task(&self, source: &str, task_id: uuid::Uuid) -> Result<(), QuarkError> {
        let (provider, share, fingerprint) = parse_stable_cloud_share_source(source)?;
        self.update(|document, now| {
            let key = plan_key(provider, &share);
            let plan = &mut document.plans.get_mut(&key).ok_or_else(missing_plan)?.plan;
            plan.linked_local_tasks
                .get_mut(&fingerprint)
                .ok_or_else(missing_plan)?
                .insert(task_id);
            document.touch(&key, now)
        })
    }

    pub fn reserve_local_task(&self, source: &str, task_id: uuid::Uuid) -> Result<(), QuarkError> {
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
            plan.linked_local_tasks
                .entry(fingerprint)
                .or_default()
                .insert(task_id);
            document.touch(&key, now)
        })
    }

    pub fn unlink_local_task(&self, source: &str, task_id: uuid::Uuid) -> Result<(), QuarkError> {
        let (provider, share, fingerprint) = parse_stable_cloud_share_source(source)?;
        self.update(|document, now| {
            let key = plan_key(provider, &share);
            let plan = &mut document.plans.get_mut(&key).ok_or_else(missing_plan)?.plan;
            if plan
                .linked_local_tasks
                .get_mut(&fingerprint)
                .is_some_and(|ids| ids.remove(&task_id))
            {
                plan.remove_terminal_empty_link(&fingerprint);
            }
            document.touch(&key, now)
        })
    }

    /// Claim under the encrypted document lock, checking every exact linked task
    /// against current SQLite state. The caller holds submission gates while the
    /// claim and provider mutation run. Returning IDs requires a successful save.
    pub fn mark_local_completion(
        &self,
        source: &str,
        retry: bool,
        is_completed: impl Fn(uuid::Uuid, &str) -> bool,
    ) -> Result<Vec<String>, QuarkError> {
        let (provider, share, fingerprint) = parse_stable_cloud_share_source(source)?;
        self.update(|document, now| {
            let key = plan_key(provider, &share);
            let plan = &mut document.plans.get_mut(&key).ok_or_else(missing_plan)?.plan;
            if !plan.linked_local_tasks.contains_key(&fingerprint) {
                return Ok(vec![]);
            }
            for (entry, ids) in &plan.linked_local_tasks {
                let source = format!("cloud-share://{}/{}/{}", provider.key(), share, entry);
                if ids.is_empty() || !ids.iter().all(|id| is_completed(*id, &source)) {
                    return Ok(vec![]);
                }
            }
            // Unlinked historical transfers are never swept into this action.
            let mut claimed = BTreeSet::new();
            for transfer in &mut plan.transfers {
                if plan
                    .linked_local_tasks
                    .contains_key(&transfer.entry_fingerprint)
                    && (transfer.cleanup_state == CloudShareCleanupState::Pending
                        || (retry
                            && matches!(
                                transfer.cleanup_state,
                                CloudShareCleanupState::Failed | CloudShareCleanupState::Attempted
                            )))
                {
                    transfer.cleanup_state = CloudShareCleanupState::Attempted;
                    claimed.insert(transfer.created_file_id.clone());
                }
            }
            document.touch(&key, now)?;
            Ok(claimed.into_iter().collect())
        })
    }

    /// Rebuild safe task messages from the durable journal after a restart.
    /// This reads the document without claiming, pruning, saving or provider IO.
    pub(crate) fn restore_cleanup_pending_messages(
        &self,
        tasks: &crate::TaskStore,
    ) -> Result<(), QuarkError> {
        let _guard = self.mutation.lock().map_err(|_| QuarkError::Storage)?;
        let document = self.load()?;
        for stored in document.plans.values() {
            let plan = &stored.plan;
            let pending = plan.transfers.iter().any(|transfer| {
                plan.linked_local_tasks
                    .contains_key(&transfer.entry_fingerprint)
                    && matches!(
                        transfer.cleanup_state,
                        CloudShareCleanupState::Attempted | CloudShareCleanupState::Failed
                    )
            });
            for (entry, ids) in &plan.linked_local_tasks {
                let source = format!(
                    "cloud-share://{}/{}/{entry}",
                    plan.provider.key(),
                    plan.share_fingerprint
                );
                for id in ids {
                    let Ok(task) = tasks.find(*id) else {
                        continue;
                    };
                    if task.status == crate::TaskStatus::Completed && task.source == source {
                        tasks
                            .set_completed_cleanup_message(*id, pending)
                            .map_err(|_| QuarkError::Storage)?;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn finish_cleanup(
        &self,
        source: &str,
        ids: &[String],
        succeeded: bool,
    ) -> Result<(), QuarkError> {
        let (provider, share, _) = parse_stable_cloud_share_source(source)?;
        self.update(|document, now| {
            let key = plan_key(provider, &share);
            let plan = &mut document.plans.get_mut(&key).ok_or_else(missing_plan)?.plan;
            let mut terminal_entries = BTreeSet::new();
            for transfer in &mut plan.transfers {
                if transfer.cleanup_state == CloudShareCleanupState::Attempted
                    && ids.contains(&transfer.created_file_id)
                {
                    transfer.cleanup_state = if succeeded {
                        CloudShareCleanupState::Succeeded
                    } else {
                        CloudShareCleanupState::Failed
                    };
                    if succeeded {
                        terminal_entries.insert(transfer.entry_fingerprint.clone());
                    }
                }
            }
            for fingerprint in terminal_entries {
                plan.remove_terminal_empty_link(&fingerprint);
            }
            document.touch(&key, now)
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
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    #[derive(Default)]
    struct SagaAria2 {
        add_calls: AtomicU64,
        reject_add: AtomicBool,
        drop_task_files: Mutex<Option<std::path::PathBuf>>,
    }

    #[async_trait]
    impl crate::aria2::Aria2Api for SagaAria2 {
        async fn add_uri(
            &self,
            _: Vec<String>,
            _: crate::aria2::Aria2Options,
        ) -> Result<String, crate::aria2::Aria2Error> {
            self.add_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(path) = self.drop_task_files.lock().unwrap().take() {
                rusqlite::Connection::open(path)
                    .unwrap()
                    .execute_batch("DROP TABLE task_files")
                    .unwrap();
            }
            if self.reject_add.load(Ordering::SeqCst) {
                Err(crate::aria2::Aria2Error::Transport)
            } else {
                Ok("saga-gid".into())
            }
        }
        async fn tell_status(
            &self,
            gid: &str,
        ) -> Result<crate::aria2::Aria2Status, crate::aria2::Aria2Error> {
            Ok(crate::aria2::Aria2Status {
                gid: gid.into(),
                status: "active".into(),
                total_length: "1".into(),
                completed_length: "0".into(),
                download_speed: "0".into(),
                upload_speed: "0".into(),
                connections: "0".into(),
                files: vec![],
                followed_by: vec![],
                error_code: None,
                error_message: None,
            })
        }
        async fn add_torrent(&self, _: String, _: Vec<String>, _: crate::aria2::Aria2Options) -> Result<String, crate::aria2::Aria2Error> { unreachable!() }
        async fn tell_active(&self) -> Result<Vec<crate::aria2::Aria2Status>, crate::aria2::Aria2Error> { unreachable!() }
        async fn tell_waiting(&self, _: i64, _: u32) -> Result<Vec<crate::aria2::Aria2Status>, crate::aria2::Aria2Error> { unreachable!() }
        async fn tell_stopped(&self, _: i64, _: u32) -> Result<Vec<crate::aria2::Aria2Status>, crate::aria2::Aria2Error> { unreachable!() }
        async fn get_global_stat(&self) -> Result<crate::aria2::Aria2GlobalStat, crate::aria2::Aria2Error> { unreachable!() }
        async fn multicall(&self, _: Vec<crate::aria2::RpcCall>) -> Result<Vec<Result<serde_json::Value, crate::aria2::RpcFault>>, crate::aria2::Aria2Error> { unreachable!() }
        async fn pause(&self, _: &str) -> Result<String, crate::aria2::Aria2Error> { unreachable!() }
        async fn unpause(&self, _: &str) -> Result<String, crate::aria2::Aria2Error> { unreachable!() }
        async fn remove(&self, gid: &str) -> Result<String, crate::aria2::Aria2Error> { Ok(gid.into()) }
        async fn force_remove(&self, _: &str) -> Result<String, crate::aria2::Aria2Error> { unreachable!() }
        async fn change_option(&self, _: &str, _: crate::aria2::Aria2Options) -> Result<String, crate::aria2::Aria2Error> { unreachable!() }
        async fn get_option(&self, _: &str) -> Result<crate::aria2::Aria2Options, crate::aria2::Aria2Error> { unreachable!() }
        async fn save_session(&self) -> Result<String, crate::aria2::Aria2Error> { unreachable!() }
        async fn shutdown(&self) -> Result<String, crate::aria2::Aria2Error> { unreachable!() }
    }

    struct SagaAdapter {
        resolve_calls: AtomicU64,
        transfer_calls: AtomicU64,
        fail_resolve: bool,
    }

    struct FailNthSaveCredentials {
        inner: MemoryCredentialStore,
        saves: AtomicU64,
        fail_at: u64,
    }

    impl CredentialStore for FailNthSaveCredentials {
        fn save(&self, key: &str, value: &[u8]) -> Result<(), QuarkError> {
            let save = self.saves.fetch_add(1, Ordering::SeqCst) + 1;
            if save == self.fail_at {
                Err(QuarkError::Storage)
            } else {
                self.inner.save(key, value)
            }
        }

        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, QuarkError> {
            self.inner.load(key)
        }

        fn remove(&self, key: &str) -> Result<(), QuarkError> {
            self.inner.remove(key)
        }
    }

    impl CloudShareAdapter for SagaAdapter {
        fn inspect(
            &self,
            _: &crate::parser::CloudShareCandidate,
        ) -> Result<ShareInspection, QuarkError> {
            unreachable!()
        }
        fn resolve(&self, entry: &CloudShareResolvedEntry) -> Result<ShareResolution, QuarkError> {
            self.resolve_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_resolve {
                Err(QuarkError::AuthorizationExpired)
            } else {
                Ok(ShareResolution::Direct(
                    super::super::commands::ResolvedCloudDownload {
                        url: "https://cdn.example/file".into(),
                        name: entry.entry.name.clone(),
                        headers: vec![],
                    },
                ))
            }
        }
        fn transfer(
            &self,
            _: &CloudShareResolvedEntry,
            _: &CloudSharePlanStore,
        ) -> Result<super::super::commands::ResolvedCloudDownload, QuarkError> {
            self.transfer_calls.fetch_add(1, Ordering::SeqCst);
            unreachable!()
        }
    }

    #[test]
    fn cleanup_claim_is_durable_and_one_shot_across_concurrent_completions() {
        let (store, _, _) = memory_store();
        let plan = fixture("share");
        store.upsert(&plan).unwrap();
        let source = plan.source_for("f1");
        store
            .link_local_sources(std::slice::from_ref(&source))
            .unwrap();
        store
            .link_local_task(&source, uuid::Uuid::new_v4())
            .unwrap();
        store.record_transfer(&source, "exact-created-id").unwrap();
        store.upsert(&plan).unwrap();
        let store = Arc::new(store);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let store = store.clone();
                let source = source.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .mark_local_completion(&source, false, |_, _| true)
                        .unwrap()
                })
            })
            .collect();
        let claims: Vec<_> = threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(claims, vec!["exact-created-id"]);
        assert_eq!(
            store.resolve_entry(&source).unwrap().plan.transfers[0].cleanup_state,
            CloudShareCleanupState::Attempted
        );
    }

    #[test]
    fn cleanup_cannot_release_ids_when_encrypted_claim_save_fails() {
        use std::sync::atomic::AtomicBool;
        struct FailingCredentials {
            inner: MemoryCredentialStore,
            fail: AtomicBool,
        }
        impl CredentialStore for FailingCredentials {
            fn load(&self, key: &str) -> Result<Option<Vec<u8>>, QuarkError> {
                self.inner.load(key)
            }
            fn remove(&self, key: &str) -> Result<(), QuarkError> {
                self.inner.remove(key)
            }
            fn save(&self, key: &str, bytes: &[u8]) -> Result<(), QuarkError> {
                if self.fail.load(Ordering::SeqCst) {
                    Err(QuarkError::Storage)
                } else {
                    self.inner.save(key, bytes)
                }
            }
        }
        let credentials = Arc::new(FailingCredentials {
            inner: MemoryCredentialStore::default(),
            fail: AtomicBool::new(false),
        });
        let store = CloudSharePlanStore {
            credentials: credentials.clone(),
            mutation: Mutex::new(()),
            now_ms: Arc::new(|| 1_000),
        };
        let plan = fixture("share");
        let source = plan.source_for("f1");
        store.upsert(&plan).unwrap();
        store
            .link_local_sources(std::slice::from_ref(&source))
            .unwrap();
        store
            .link_local_task(&source, uuid::Uuid::new_v4())
            .unwrap();
        store.record_transfer(&source, "created-id").unwrap();
        credentials.fail.store(true, Ordering::SeqCst);
        assert!(store
            .mark_local_completion(&source, false, |_, _| true)
            .is_err());
        credentials.fail.store(false, Ordering::SeqCst);
        assert_eq!(
            store.resolve_entry(&source).unwrap().plan.transfers[0].cleanup_state,
            CloudShareCleanupState::Pending
        );
    }

    #[test]
    fn rotated_share_context_must_be_persisted_before_transfer_or_download_handoff() {
        use std::sync::atomic::AtomicBool;
        struct FailingStore {
            inner: MemoryCredentialStore,
            fail: Arc<AtomicBool>,
        }
        impl CredentialStore for FailingStore {
            fn save(&self, key: &str, value: &[u8]) -> Result<(), QuarkError> {
                if self.fail.load(Ordering::SeqCst) {
                    return Err(QuarkError::Provider("private-write-failure".into()));
                }
                self.inner.save(key, value)
            }
            fn load(&self, key: &str) -> Result<Option<Vec<u8>>, QuarkError> {
                self.inner.load(key)
            }
            fn remove(&self, key: &str) -> Result<(), QuarkError> {
                self.inner.remove(key)
            }
        }
        struct UpdatingAdapter {
            fail: Arc<AtomicBool>,
            at_transfer: bool,
            operation_error: bool,
        }
        impl CloudShareAdapter for UpdatingAdapter {
            fn inspect(
                &self,
                _: &crate::parser::CloudShareCandidate,
            ) -> Result<ShareInspection, QuarkError> {
                unreachable!()
            }
            fn resolve(&self, _: &CloudShareResolvedEntry) -> Result<ShareResolution, QuarkError> {
                unreachable!()
            }
            fn resolve_with_context(
                &self,
                entry: &CloudShareResolvedEntry,
            ) -> Result<ShareOperationResult<ShareResolution>, QuarkError> {
                if !self.at_transfer {
                    self.fail.store(true, Ordering::SeqCst);
                }
                Ok(ShareOperationResult {
                    result: if self.operation_error && !self.at_transfer {
                        Err(QuarkError::WebAuthorizationRequired)
                    } else {
                        Ok(if self.at_transfer {
                            ShareResolution::TransferRequired(TransferRequest {
                                source: entry.plan.source_for(&entry.entry.entry_id),
                            })
                        } else {
                            ShareResolution::Direct(super::super::commands::ResolvedCloudDownload {
                                url: "https://cdn.example/file".into(),
                                name: "file".into(),
                                headers: vec![],
                            })
                        })
                    },
                    updated_context: Some(serde_json::json!({"token":"rotated-before-transfer"})),
                })
            }
            fn transfer_with_context(
                &self,
                entry: &CloudShareResolvedEntry,
                store: &CloudSharePlanStore,
            ) -> Result<ShareOperationResult<super::super::commands::ResolvedCloudDownload>, QuarkError>
            {
                assert_eq!(
                    entry.plan.provider_context["token"],
                    "rotated-before-transfer"
                );
                assert_eq!(
                    store
                        .resolve_entry(&entry.plan.source_for(&entry.entry.entry_id))
                        .unwrap()
                        .plan
                        .provider_context["token"],
                    "rotated-before-transfer"
                );
                self.fail.store(true, Ordering::SeqCst);
                Ok(ShareOperationResult {
                    result: if self.operation_error {
                        Err(QuarkError::WebAuthorizationRequired)
                    } else {
                        Ok(super::super::commands::ResolvedCloudDownload {
                            url: "https://cdn.example/personal".into(),
                            name: "file".into(),
                            headers: vec![],
                        })
                    },
                    updated_context: Some(serde_json::json!({"token":"rotated-after-transfer"})),
                })
            }
        }
        for (at_transfer, operation_error) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let fail = Arc::new(AtomicBool::new(false));
            let credentials = Arc::new(FailingStore {
                inner: MemoryCredentialStore::default(),
                fail: fail.clone(),
            });
            let store = CloudSharePlanStore {
                credentials,
                mutation: Mutex::new(()),
                now_ms: Arc::new(|| 1000),
            };
            let plan = fixture("private-share");
            store.upsert(&plan).unwrap();
            let adapter = UpdatingAdapter {
                fail,
                at_transfer,
                operation_error,
            };
            assert!(matches!(
                resolve_current_share_download(&store, &adapter, &plan.source_for("f1")),
                Err(QuarkError::Storage)
            ));
        }
    }

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

    #[test]
    fn selecting_share_sources_is_read_only_and_does_not_create_a_reservation() {
        let (store, _, _) = memory_store();
        let plan = fixture("read-only-selection");
        let source = plan.source_for("f1");
        store.upsert(&plan).unwrap();
        let selected_id = plan.entries[0].entry_fingerprint.clone();

        assert_eq!(
            selected_share_sources(&store, &source, &[selected_id]).unwrap(),
            vec![source.clone()]
        );

        assert!(store
            .resolve_entry(&source)
            .unwrap()
            .plan
            .linked_local_tasks
            .is_empty());
    }

    struct PlanAdapter(CloudSharePlan);
    impl CloudShareAdapter for PlanAdapter {
        fn inspect(
            &self,
            _: &crate::parser::CloudShareCandidate,
        ) -> Result<ShareInspection, QuarkError> {
            Ok(ShareInspection::Plan(self.0.clone()))
        }

        fn resolve(&self, _: &CloudShareResolvedEntry) -> Result<ShareResolution, QuarkError> {
            unreachable!("inspection does not resolve a download")
        }
    }

    #[test]
    fn only_an_explicit_exact_root_file_is_selected_for_automatic_submission() {
        let candidate = crate::parser::extract_cloud_share_candidate(
            "https://pan.baidu.com/s/private-share",
            None,
        )
        .unwrap()
        .unwrap();

        let mut file = fixture("private-share");
        file.root_kind = CloudShareRootKind::File {
            entry_id: "f1".into(),
        };
        let (store, _, _) = memory_store();
        let expected = file.source_for("f1");
        assert!(matches!(
            inspect_share_with_adapter(&store, &PlanAdapter(file), &candidate).unwrap(),
            ShareConfirmationOutcome::AutoSubmit { source } if source == expected
        ));

        for root_kind in [
            CloudShareRootKind::Unknown,
            CloudShareRootKind::Directory,
            CloudShareRootKind::Collection,
        ] {
            let mut plan = fixture("private-share");
            plan.root_kind = root_kind;
            let (store, _, _) = memory_store();
            assert!(matches!(
                inspect_share_with_adapter(&store, &PlanAdapter(plan), &candidate).unwrap(),
                ShareConfirmationOutcome::Confirmation(_)
            ));
        }
    }

    #[test]
    fn legacy_plan_without_root_kind_defaults_to_unknown_and_requires_confirmation() {
        let candidate = crate::parser::extract_cloud_share_candidate(
            "https://pan.baidu.com/s/private-share",
            None,
        )
        .unwrap()
        .unwrap();
        let mut value = serde_json::to_value(fixture("private-share")).unwrap();
        value.as_object_mut().unwrap().remove("root_kind");
        let legacy: CloudSharePlan = serde_json::from_value(value).unwrap();
        assert!(matches!(legacy.root_kind, CloudShareRootKind::Unknown));
        let (store, _, _) = memory_store();
        assert!(matches!(
            inspect_share_with_adapter(&store, &PlanAdapter(legacy), &candidate).unwrap(),
            ShareConfirmationOutcome::Confirmation(_)
        ));
    }

    #[test]
    fn reinspection_of_a_root_file_keeps_older_linked_entries_loadable() {
        let (store, _, _) = memory_store();
        let mut previous = fixture("private-share");
        previous.root_kind = CloudShareRootKind::Directory;
        previous.entries.push(CloudShareEntry::new(
            "private-share",
            "old-file".into(),
            "old.txt".into(),
            vec![],
            Some(3),
        ));
        store.upsert(&previous).unwrap();

        let mut current = fixture("private-share");
        current.root_kind = CloudShareRootKind::File {
            entry_id: "f1".into(),
        };
        store.upsert(&current).unwrap();

        assert!(store.resolve_entry(&current.source_for("f1")).is_ok());
        assert!(store
            .resolve_entry(&previous.source_for("old-file"))
            .is_ok());
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
    fn ttl_pruning_keeps_every_live_plan_and_removes_only_terminal_or_unbound_plans() {
        let (store, _, now) = memory_store();
        let reservation = fixture("reservation");
        let linked = fixture("linked");
        let pending = fixture("pending");
        let attempted = fixture("attempted");
        let failed = fixture("failed");
        let succeeded = fixture("succeeded");
        let unbound = fixture("unbound");

        for plan in [
            &reservation,
            &linked,
            &pending,
            &attempted,
            &failed,
            &succeeded,
            &unbound,
        ] {
            store.upsert(plan).unwrap();
        }
        store
            .link_local_sources(&[reservation.source_for("f1")])
            .unwrap();
        store
            .link_local_sources(&[linked.source_for("f1")])
            .unwrap();
        store
            .link_local_task(&linked.source_for("f1"), uuid::Uuid::new_v4())
            .unwrap();
        store
            .record_transfer(&pending.source_for("f1"), "pending-id")
            .unwrap();
        store
            .record_transfer(&attempted.source_for("f1"), "attempted-id")
            .unwrap();
        store
            .mark_cleanup_attempted(&attempted.source_for("f1"))
            .unwrap();
        store
            .record_transfer(&failed.source_for("f1"), "failed-id")
            .unwrap();
        store
            .mark_cleanup_attempted(&failed.source_for("f1"))
            .unwrap();
        store
            .finish_cleanup(&failed.source_for("f1"), &["failed-id".into()], false)
            .unwrap();
        store
            .record_transfer(&succeeded.source_for("f1"), "succeeded-id")
            .unwrap();
        store
            .mark_cleanup_attempted(&succeeded.source_for("f1"))
            .unwrap();
        store
            .finish_cleanup(&succeeded.source_for("f1"), &["succeeded-id".into()], true)
            .unwrap();

        now.store(1_000 + RETENTION_MS, Ordering::SeqCst);
        assert_eq!(store.prune().unwrap(), 2);
        for plan in [&reservation, &linked, &pending, &attempted, &failed] {
            assert!(store.resolve_entry(&plan.source_for("f1")).is_ok());
        }
        for plan in [&succeeded, &unbound] {
            assert!(store.resolve_entry(&plan.source_for("f1")).is_err());
        }
    }

    #[test]
    fn distinct_upsert_fails_without_rewriting_a_full_pinned_document_but_update_is_allowed() {
        let (store, credentials, _) = memory_store();
        let mut first = fixture("share-0");
        for index in 0..MAX_PLANS {
            let plan = fixture(&format!("share-{index}"));
            store.upsert(&plan).unwrap();
            store.link_local_sources(&[plan.source_for("f1")]).unwrap();
        }
        let before = credentials.load(SHARE_PLAN_SECRET).unwrap().unwrap();

        assert!(store.upsert(&fixture("overflow")).is_err());
        assert_eq!(
            credentials.load(SHARE_PLAN_SECRET).unwrap().unwrap(),
            before
        );

        first.provider_context = serde_json::json!({"token":"updated"});
        store.upsert(&first).unwrap();
        assert_eq!(
            store
                .resolve_entry(&first.source_for("f1"))
                .unwrap()
                .plan
                .provider_context["token"],
            "updated"
        );
        assert!(store
            .resolve_entry(&fixture("overflow").source_for("f1"))
            .is_err());
    }

    #[test]
    fn unlink_is_exact_and_releases_an_empty_entry_only_after_cleanup_is_terminal() {
        let (store, _, _) = memory_store();
        let mut plan = fixture("unlink");
        plan.entries.push(CloudShareEntry::new(
            "unlink",
            "f2".into(),
            "other.txt".into(),
            vec![],
            Some(9),
        ));
        let first = plan.source_for("f1");
        let second = plan.source_for("f2");
        let first_task = uuid::Uuid::new_v4();
        let second_task = uuid::Uuid::new_v4();
        store.upsert(&plan).unwrap();
        store
            .link_local_sources(&[first.clone(), second.clone()])
            .unwrap();
        store.link_local_task(&first, first_task).unwrap();
        store.link_local_task(&second, second_task).unwrap();
        store.record_transfer(&first, "created-id").unwrap();

        store.unlink_local_task(&first, first_task).unwrap();
        let pending = store.resolve_entry(&second).unwrap().plan;
        assert_eq!(
            pending.linked_local_tasks[&parse_stable_cloud_share_source(&first).unwrap().2].len(),
            0
        );
        assert_eq!(
            pending.linked_local_tasks[&parse_stable_cloud_share_source(&second).unwrap().2],
            BTreeSet::from([second_task])
        );

        store.mark_cleanup_attempted(&first).unwrap();
        store
            .finish_cleanup(&first, &["created-id".into()], true)
            .unwrap();
        let terminal = store.resolve_entry(&second).unwrap().plan;
        assert!(!terminal
            .linked_local_tasks
            .contains_key(&parse_stable_cloud_share_source(&first).unwrap().2));
        assert_eq!(terminal.linked_local_tasks.len(), 1);
    }

    #[test]
    fn finishing_one_unlinked_transfer_keeps_an_unrelated_empty_submission_reservation() {
        let (store, _, _) = memory_store();
        let mut plan = fixture("reservation-isolation");
        plan.entries.push(CloudShareEntry::new(
            "reservation-isolation",
            "f2".into(),
            "reserved.txt".into(),
            vec![],
            Some(9),
        ));
        let transferred = plan.source_for("f1");
        let reserved = plan.source_for("f2");
        let task_id = uuid::Uuid::new_v4();
        store.upsert(&plan).unwrap();
        store
            .link_local_sources(&[transferred.clone(), reserved.clone()])
            .unwrap();
        store.link_local_task(&transferred, task_id).unwrap();
        store.record_transfer(&transferred, "created-id").unwrap();
        store.unlink_local_task(&transferred, task_id).unwrap();
        store.mark_cleanup_attempted(&transferred).unwrap();

        store
            .finish_cleanup(&transferred, &["created-id".into()], true)
            .unwrap();

        let current = store.resolve_entry(&reserved).unwrap().plan;
        let reserved_fingerprint = parse_stable_cloud_share_source(&reserved).unwrap().2;
        assert!(current
            .linked_local_tasks
            .contains_key(&reserved_fingerprint));
        assert!(current.linked_local_tasks[&reserved_fingerprint].is_empty());
    }

    #[test]
    fn unlink_with_an_unregistered_task_id_cannot_release_a_submission_reservation() {
        let (store, _, _) = memory_store();
        let plan = fixture("reserved-without-task");
        let source = plan.source_for("f1");
        store.upsert(&plan).unwrap();
        store
            .link_local_sources(std::slice::from_ref(&source))
            .unwrap();

        store
            .unlink_local_task(&source, uuid::Uuid::new_v4())
            .unwrap();

        let current = store.resolve_entry(&source).unwrap().plan;
        let fingerprint = parse_stable_cloud_share_source(&source).unwrap().2;
        assert!(current.linked_local_tasks.contains_key(&fingerprint));
        assert!(current.linked_local_tasks[&fingerprint].is_empty());
    }

    #[test]
    fn atomic_task_reservation_survives_an_old_same_source_task_being_unlinked_first() {
        let (store, _, _) = memory_store();
        let plan = fixture("same-source-replacement");
        let source = plan.source_for("f1");
        let old_task = uuid::Uuid::new_v4();
        let new_task = uuid::Uuid::new_v4();
        store.upsert(&plan).unwrap();
        store.reserve_local_task(&source, old_task).unwrap();

        store.unlink_local_task(&source, old_task).unwrap();
        store.reserve_local_task(&source, new_task).unwrap();

        let current = store.resolve_entry(&source).unwrap().plan;
        let fingerprint = parse_stable_cloud_share_source(&source).unwrap().2;
        assert_eq!(
            current.linked_local_tasks[&fingerprint],
            BTreeSet::from([new_task])
        );
    }

    #[tokio::test]
    async fn unavailable_engine_prevents_task_creation_and_provider_resolution() {
        let (plans, _, _) = memory_store();
        let plan = fixture("engine-unavailable");
        let source = plan.source_for("f1");
        plans.upsert(&plan).unwrap();
        let tasks = Arc::new(crate::TaskStore::open_in_memory().unwrap());
        let resolver_calls = Arc::new(AtomicU64::new(0));
        let calls = resolver_calls.clone();

        let result = crate::cloud::commands::submit_new_cloud_share_file_with(
            tasks.clone(),
            &plans,
            source,
            crate::UriSubmissionOptions::default(),
            || async {
                Err::<Arc<crate::DownloadService>, _>(QuarkError::Transport(
                    "engine unavailable".into(),
                ))
            },
            || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(QuarkError::InvalidResponse)
            },
        )
        .await;

        assert!(result.is_err());
        assert!(tasks.list().unwrap().is_empty());
        assert_eq!(resolver_calls.load(Ordering::SeqCst), 0);
        assert!(plans
            .resolve_entry(&plan.source_for("f1"))
            .unwrap()
            .plan
            .transfers
            .is_empty());
    }

    #[tokio::test]
    async fn failed_atomic_reservation_removes_the_unsubmitted_task_without_provider_calls() {
        let credentials = Arc::new(FailNthSaveCredentials {
            inner: MemoryCredentialStore::default(),
            saves: AtomicU64::new(0),
            fail_at: 3,
        });
        let plans = CloudSharePlanStore {
            credentials,
            mutation: Mutex::new(()),
            now_ms: Arc::new(|| 1_000),
        };
        let plan = fixture("reservation-failure");
        let source = plan.source_for("f1");
        plans.upsert(&plan).unwrap();
        let tasks = Arc::new(crate::TaskStore::open_in_memory().unwrap());
        let api = Arc::new(SagaAria2::default());
        let service = Arc::new(crate::DownloadService::new(api.clone(), tasks.clone()));
        let adapter = SagaAdapter {
            resolve_calls: AtomicU64::new(0),
            transfer_calls: AtomicU64::new(0),
            fail_resolve: false,
        };
        let resolve_source = source.clone();

        let result = crate::cloud::commands::submit_new_cloud_share_file_with(
            tasks.clone(),
            &plans,
            source,
            crate::UriSubmissionOptions::default(),
            || async { Ok(service) },
            || async {
                resolve_current_share_download(&plans, &adapter, &resolve_source)
            },
        )
        .await;

        assert!(result.is_err());
        assert!(tasks.list().unwrap().is_empty());
        assert_eq!(adapter.resolve_calls.load(Ordering::SeqCst), 0);
        assert_eq!(adapter.transfer_calls.load(Ordering::SeqCst), 0);
        assert_eq!(api.add_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn failed_reservation_and_failed_rollback_returns_a_durable_failed_task() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("tasks.sqlite");
        let tasks = Arc::new(crate::TaskStore::open_path(&database).unwrap());
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_task_delete BEFORE DELETE ON tasks \
                 BEGIN SELECT RAISE(FAIL, 'delete rejected'); END;",
            )
            .unwrap();
        let credentials = Arc::new(FailNthSaveCredentials {
            inner: MemoryCredentialStore::default(),
            saves: AtomicU64::new(0),
            fail_at: 3,
        });
        let plans = CloudSharePlanStore {
            credentials,
            mutation: Mutex::new(()),
            now_ms: Arc::new(|| 1_000),
        };
        let plan = fixture("reservation-rollback-failure");
        let source = plan.source_for("f1");
        plans.upsert(&plan).unwrap();
        let api = Arc::new(SagaAria2::default());
        let service = Arc::new(crate::DownloadService::new(api.clone(), tasks.clone()));

        let outcome = crate::cloud::commands::submit_new_cloud_share_file_with(
            tasks.clone(),
            &plans,
            source.clone(),
            crate::UriSubmissionOptions::default(),
            || async { Ok(service) },
            || async { unreachable!("provider must not run before reservation succeeds") },
        )
        .await
        .expect("a failed rollback must return the task which may still be durable");

        assert_eq!(outcome.payload.task.status, crate::TaskStatus::Failed);
        assert_eq!(
            outcome.payload.task.error.as_deref(),
            Some("网盘分享下载准备失败，请重试")
        );
        assert_eq!(tasks.find(outcome.payload.task.id).unwrap().status, crate::TaskStatus::Failed);
        assert_eq!(api.add_calls.load(Ordering::SeqCst), 0);
        assert!(plans
            .resolve_entry(&source)
            .unwrap()
            .plan
            .linked_local_tasks
            .is_empty());
    }

    #[tokio::test]
    async fn failed_prepare_retry_reuses_and_reserves_the_exact_task() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("tasks.sqlite");
        let tasks = Arc::new(crate::TaskStore::open_path(&database).unwrap());
        let administration = rusqlite::Connection::open(&database).unwrap();
        administration
            .execute_batch(
                "CREATE TRIGGER reject_task_delete BEFORE DELETE ON tasks \
                 BEGIN SELECT RAISE(FAIL, 'delete rejected'); END;",
            )
            .unwrap();
        let credentials = Arc::new(FailNthSaveCredentials {
            inner: MemoryCredentialStore::default(),
            saves: AtomicU64::new(0),
            fail_at: 3,
        });
        let plans = CloudSharePlanStore {
            credentials,
            mutation: Mutex::new(()),
            now_ms: Arc::new(|| 1_000),
        };
        let plan = fixture("retry-failed-prepare");
        let source = plan.source_for("f1");
        plans.upsert(&plan).unwrap();
        let api = Arc::new(SagaAria2::default());
        let service = Arc::new(crate::DownloadService::new(api.clone(), tasks.clone()));

        let failed = crate::cloud::commands::submit_new_cloud_share_file_with(
            tasks.clone(),
            &plans,
            source.clone(),
            crate::UriSubmissionOptions::default(),
            || async { Ok(service.clone()) },
            || async { unreachable!("provider must not run before reservation succeeds") },
        )
        .await
        .unwrap();
        administration
            .execute_batch("DROP TRIGGER reject_task_delete")
            .unwrap();
        let adapter = SagaAdapter {
            resolve_calls: AtomicU64::new(0),
            transfer_calls: AtomicU64::new(0),
            fail_resolve: false,
        };
        let resolve_source = source.clone();

        let retried = crate::cloud::commands::submit_new_cloud_share_file_with(
            tasks.clone(),
            &plans,
            source.clone(),
            crate::UriSubmissionOptions::default(),
            || async { Ok(service) },
            || async { resolve_current_share_download(&plans, &adapter, &resolve_source) },
        )
        .await
        .unwrap();

        assert_eq!(retried.payload.task.id, failed.payload.task.id);
        assert_eq!(retried.payload.task.status, crate::TaskStatus::Downloading);
        assert_eq!(api.add_calls.load(Ordering::SeqCst), 1);
        assert!(plans
            .resolve_entry(&source)
            .unwrap()
            .plan
            .linked_local_tasks
            .values()
            .any(|ids| ids.contains(&failed.payload.task.id)));
    }

    #[tokio::test]
    async fn provider_resolution_failure_returns_the_reserved_failed_task() {
        let (plans, _, _) = memory_store();
        let plan = fixture("provider-resolution-failure");
        let source = plan.source_for("f1");
        plans.upsert(&plan).unwrap();
        let tasks = Arc::new(crate::TaskStore::open_in_memory().unwrap());
        let api = Arc::new(SagaAria2::default());
        let service = Arc::new(crate::DownloadService::new(api.clone(), tasks.clone()));
        let adapter = SagaAdapter {
            resolve_calls: AtomicU64::new(0),
            transfer_calls: AtomicU64::new(0),
            fail_resolve: true,
        };
        let resolve_source = source.clone();

        let outcome = crate::cloud::commands::submit_new_cloud_share_file_with(
            tasks.clone(),
            &plans,
            source.clone(),
            crate::UriSubmissionOptions::default(),
            || async { Ok(service) },
            || async { resolve_current_share_download(&plans, &adapter, &resolve_source) },
        )
        .await
        .expect("a prepared cloud-share task must be returned after resolution fails");

        assert_eq!(outcome.payload.task.status, crate::TaskStatus::Failed);
        assert_eq!(outcome.payload.task.source, source);
        assert_eq!(
            outcome.payload.task.error.as_deref(),
            Some("网盘分享下载地址获取失败，请检查登录状态后重试")
        );
        assert_eq!(tasks.find(outcome.payload.task.id).unwrap().status, crate::TaskStatus::Failed);
        assert_eq!(adapter.resolve_calls.load(Ordering::SeqCst), 1);
        assert_eq!(adapter.transfer_calls.load(Ordering::SeqCst), 0);
        assert_eq!(api.add_calls.load(Ordering::SeqCst), 0);
        assert!(plans
            .resolve_entry(&source)
            .unwrap()
            .plan
            .linked_local_tasks
            .values()
            .any(|ids| ids.contains(&outcome.payload.task.id)));
    }

    #[tokio::test]
    async fn aria2_rejection_returns_the_reserved_failed_task() {
        let (plans, _, _) = memory_store();
        let plan = fixture("aria2-rejection");
        let source = plan.source_for("f1");
        plans.upsert(&plan).unwrap();
        let tasks = Arc::new(crate::TaskStore::open_in_memory().unwrap());
        let api = Arc::new(SagaAria2::default());
        api.reject_add.store(true, Ordering::SeqCst);
        let service = Arc::new(crate::DownloadService::new(api.clone(), tasks.clone()));
        let adapter = SagaAdapter {
            resolve_calls: AtomicU64::new(0),
            transfer_calls: AtomicU64::new(0),
            fail_resolve: false,
        };
        let resolve_source = source.clone();

        let outcome = crate::cloud::commands::submit_new_cloud_share_file_with(
            tasks.clone(),
            &plans,
            source.clone(),
            crate::UriSubmissionOptions::default(),
            || async { Ok(service) },
            || async { resolve_current_share_download(&plans, &adapter, &resolve_source) },
        )
        .await
        .expect("a prepared cloud-share task must be returned after aria2 rejects it");

        assert_eq!(outcome.payload.task.status, crate::TaskStatus::Failed);
        assert_eq!(outcome.payload.task.source, source);
        assert_eq!(
            outcome.payload.task.error.as_deref(),
            Some("aria2 未能接受网盘分享下载任务")
        );
        assert_eq!(tasks.find(outcome.payload.task.id).unwrap().status, crate::TaskStatus::Failed);
        assert_eq!(adapter.resolve_calls.load(Ordering::SeqCst), 1);
        assert_eq!(api.add_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_store_recovery_still_returns_the_prepared_task_in_memory() {
        let (plans, _, _) = memory_store();
        let plan = fixture("failed-store-recovery");
        let source = plan.source_for("f1");
        plans.upsert(&plan).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("tasks.sqlite");
        let tasks = Arc::new(crate::TaskStore::open_path(&database).unwrap());
        let api = Arc::new(SagaAria2::default());
        let service = Arc::new(crate::DownloadService::new(api, tasks.clone()));
        let drop_database = database.clone();

        let outcome = crate::cloud::commands::submit_new_cloud_share_file_with(
            tasks,
            &plans,
            source.clone(),
            crate::UriSubmissionOptions::default(),
            || async { Ok(service) },
            || async move {
                rusqlite::Connection::open(drop_database)
                    .unwrap()
                    .execute_batch("DROP TABLE tasks")
                    .unwrap();
                Err(QuarkError::AuthorizationExpired)
            },
        )
        .await
        .expect("a durable-create result must not be reported as no task when recovery fails");

        assert_eq!(outcome.payload.task.source, source);
        assert_eq!(outcome.payload.task.status, crate::TaskStatus::Failed);
        assert_eq!(
            outcome.payload.task.error.as_deref(),
            Some("网盘分享下载地址获取失败，请检查登录状态后重试")
        );
        assert!(outcome.payload.files.is_empty());
    }

    #[tokio::test]
    async fn stale_parsing_task_after_failed_failure_write_is_reused_on_retry() {
        let (plans, _, _) = memory_store();
        let plan = fixture("stale-parsing-retry");
        let source = plan.source_for("f1");
        plans.upsert(&plan).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("tasks.sqlite");
        let tasks = Arc::new(crate::TaskStore::open_path(&database).unwrap());
        let administration = rusqlite::Connection::open(&database).unwrap();
        let api = Arc::new(SagaAria2::default());
        let service = Arc::new(crate::DownloadService::new(api.clone(), tasks.clone()));
        let create_failure_trigger = database.clone();

        let failed = crate::cloud::commands::submit_new_cloud_share_file_with(
            tasks.clone(),
            &plans,
            source.clone(),
            crate::UriSubmissionOptions::default(),
            || async { Ok(service.clone()) },
            || async move {
                rusqlite::Connection::open(create_failure_trigger)
                    .unwrap()
                    .execute_batch(
                        "CREATE TRIGGER reject_failed_status BEFORE UPDATE OF status ON tasks \
                         WHEN NEW.status = 'failed' \
                         BEGIN SELECT RAISE(FAIL, 'failed status rejected'); END;",
                    )
                    .unwrap();
                Err(QuarkError::AuthorizationExpired)
            },
        )
        .await
        .unwrap();
        assert_eq!(failed.payload.task.status, crate::TaskStatus::Failed);
        assert_eq!(tasks.find(failed.payload.task.id).unwrap().status, crate::TaskStatus::Parsing);
        administration
            .execute_batch("DROP TRIGGER reject_failed_status")
            .unwrap();
        let adapter = SagaAdapter {
            resolve_calls: AtomicU64::new(0),
            transfer_calls: AtomicU64::new(0),
            fail_resolve: false,
        };
        let resolve_source = source.clone();

        let retried = crate::cloud::commands::submit_new_cloud_share_file_with(
            tasks.clone(),
            &plans,
            source,
            crate::UriSubmissionOptions::default(),
            || async { Ok(service) },
            || async { resolve_current_share_download(&plans, &adapter, &resolve_source) },
        )
        .await
        .unwrap();

        assert_eq!(retried.payload.task.id, failed.payload.task.id);
        assert_eq!(retried.payload.task.status, crate::TaskStatus::Downloading);
        assert_eq!(api.add_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn files_read_failure_falls_back_to_an_empty_outcome_file_list() {
        let (plans, _, _) = memory_store();
        let plan = fixture("files-read-failure");
        let source = plan.source_for("f1");
        plans.upsert(&plan).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("tasks.sqlite");
        let tasks = Arc::new(crate::TaskStore::open_path(&database).unwrap());
        let api = Arc::new(SagaAria2::default());
        *api.drop_task_files.lock().unwrap() = Some(database);
        let service = Arc::new(crate::DownloadService::new(api.clone(), tasks.clone()));
        let adapter = SagaAdapter {
            resolve_calls: AtomicU64::new(0),
            transfer_calls: AtomicU64::new(0),
            fail_resolve: false,
        };
        let resolve_source = source.clone();

        let outcome = crate::cloud::commands::submit_new_cloud_share_file_with(
            tasks,
            &plans,
            source,
            crate::UriSubmissionOptions::default(),
            || async { Ok(service) },
            || async { resolve_current_share_download(&plans, &adapter, &resolve_source) },
        )
        .await
        .expect("files projection failure must not discard the prepared task outcome");

        assert_eq!(outcome.payload.task.status, crate::TaskStatus::Failed);
        assert!(outcome.payload.files.is_empty());
        assert_eq!(api.add_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_unlink_save_preserves_the_durable_task_pin() {
        use std::sync::atomic::AtomicBool;
        struct FailingCredentials {
            inner: MemoryCredentialStore,
            fail: AtomicBool,
        }
        impl CredentialStore for FailingCredentials {
            fn load(&self, key: &str) -> Result<Option<Vec<u8>>, QuarkError> {
                self.inner.load(key)
            }
            fn remove(&self, key: &str) -> Result<(), QuarkError> {
                self.inner.remove(key)
            }
            fn save(&self, key: &str, bytes: &[u8]) -> Result<(), QuarkError> {
                if self.fail.load(Ordering::SeqCst) {
                    Err(QuarkError::Storage)
                } else {
                    self.inner.save(key, bytes)
                }
            }
        }
        let credentials = Arc::new(FailingCredentials {
            inner: MemoryCredentialStore::default(),
            fail: AtomicBool::new(false),
        });
        let store = CloudSharePlanStore {
            credentials: credentials.clone(),
            mutation: Mutex::new(()),
            now_ms: Arc::new(|| 1_000),
        };
        let plan = fixture("unlink-save-failure");
        let source = plan.source_for("f1");
        let task_id = uuid::Uuid::new_v4();
        store.upsert(&plan).unwrap();
        store
            .link_local_sources(std::slice::from_ref(&source))
            .unwrap();
        store.link_local_task(&source, task_id).unwrap();

        credentials.fail.store(true, Ordering::SeqCst);
        assert!(store.unlink_local_task(&source, task_id).is_err());
        credentials.fail.store(false, Ordering::SeqCst);
        let durable = store.resolve_entry(&source).unwrap().plan;
        assert!(durable
            .linked_local_tasks
            .values()
            .any(|ids| ids.contains(&task_id)));
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
