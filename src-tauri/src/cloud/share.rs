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
