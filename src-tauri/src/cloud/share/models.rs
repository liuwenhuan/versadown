use super::*;
pub const SHARE_PLAN_SECRET: &str = "cloud-share-plans-v1";
pub(super) const VERSION: u8 = 1;
pub(super) const MAX_PLANS: usize = 256;
pub(super) const RETENTION_MS: u64 = 30 * 24 * 60 * 60 * 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudTransferCapacity {
    Limited { total_bytes: u64, used_bytes: u64 },
    Unlimited,
}

impl CloudTransferCapacity {
    pub(crate) fn from_json(
        total: &serde_json::Value,
        used: &serde_json::Value,
        minus_one_is_unlimited: bool,
    ) -> Result<Self, QuarkError> {
        let unlimited = minus_one_is_unlimited
            && (total.as_i64() == Some(-1) || total.as_str() == Some("-1"));
        if unlimited {
            parse_capacity_u64(used)?;
            return Ok(Self::Unlimited);
        }
        let total_bytes = parse_capacity_u64(total)?;
        let used_bytes = parse_capacity_u64(used)?;
        if used_bytes > total_bytes {
            return Err(QuarkError::InvalidResponse);
        }
        Ok(Self::Limited { total_bytes, used_bytes })
    }

    pub fn available_bytes(&self) -> Option<u64> {
        match *self {
            Self::Limited { total_bytes, used_bytes } => total_bytes.checked_sub(used_bytes),
            Self::Unlimited => None,
        }
    }
}

fn parse_capacity_u64(value: &serde_json::Value) -> Result<u64, QuarkError> {
    value
        .as_u64()
        .or_else(|| {
            value
                .as_str()
                .filter(|raw| !raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_digit()))
                .and_then(|raw| raw.parse().ok())
        })
        .ok_or(QuarkError::InvalidResponse)
}

pub(super) fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

pub(super) fn valid_owner_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Hash a provider's stable personal-drive identity for the local DPAPI plan
/// journal. Session cookies, bearer tokens and password material must never
/// participate: those rotate while the same account still owns a transfer.
pub(crate) fn stable_owner_fingerprint(
    provider: CloudProvider,
    account_id: &str,
) -> Result<String, QuarkError> {
    let account_id = account_id.trim();
    if account_id.is_empty() || account_id.chars().any(char::is_control) {
        return Err(QuarkError::InvalidResponse);
    }
    let mut hash = Sha256::new();
    hash.update(b"cloud-share-owner/account-v2\0");
    hash.update(provider.key().as_bytes());
    hash.update([0]);
    hash.update(account_id.as_bytes());
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn entry_fingerprint(share_id: &str, entry_id: &str) -> String {
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
    let provider = parts
        .next()
        .and_then(|key| crate::cloud::ProviderRegistry::global().by_key(key))
        .ok_or_else(invalid)?
        .provider;
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

/// Owner hashes are not interchangeable across derivation schemes. Missing
/// data is deliberately legacy/unknown so historical session hashes can never
/// authorize reuse or deletion by an account-derived hash.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudShareTransferOwnerScheme {
    #[default]
    LegacyUnknown,
    AccountV2,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CloudShareTransfer {
    pub entry_fingerprint: String,
    pub created_file_id: String,
    /// SHA-256 of the provider key and stable personal account ID. Its meaning
    /// is guarded by `owner_scheme`; old values without that field are legacy.
    #[serde(default)]
    pub owner_fingerprint: String,
    #[serde(default)]
    pub owner_scheme: CloudShareTransferOwnerScheme,
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

    pub(super) fn validate(&self) -> Result<(), QuarkError> {
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
                    || (!transfer.owner_fingerprint.is_empty()
                        && !valid_owner_fingerprint(&transfer.owner_fingerprint))
                    || (transfer.owner_scheme == CloudShareTransferOwnerScheme::AccountV2
                        && !valid_owner_fingerprint(&transfer.owner_fingerprint))
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

    pub(super) fn is_pinned(&self) -> bool {
        !self.linked_local_tasks.is_empty()
            || self
                .transfers
                .iter()
                .any(|transfer| transfer.cleanup_state != CloudShareCleanupState::Succeeded)
    }

    pub(super) fn remove_terminal_empty_link(&mut self, fingerprint: &str) {
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
