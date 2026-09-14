# 网盘分享转存容量统一预检 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在所有分享下载任务创建前统一判断是否需要转存并校验个人网盘可用空间，空间不足或容量不可确认时显示错误且不建立任何任务。

**Architecture:** 在 `cloud::share` 新增纯共享层预检模块，一次加载分享计划、识别当前账号可复用转存、探测一个未转存文件并汇总所需字节；现有 `ShareTransfer` 能力由五个网盘插件补充只读容量查询。Tauri 命令使用受监督的 `SessionWorkKind::Read` 执行预检，并在单文件、批量和目录的任何本地任务写入之前返回失败。

**Tech Stack:** Rust 2021、Tauri 2、serde_json、现有 `CloudHttpTransport`/`CloudSharePlanStore`、React 18、Vitest、Windows PowerShell 构建脚本。

**Spec:** `docs/superpowers/specs/2026-09-14-share-transfer-capacity-preflight-design.md`

## Global Constraints

- 统一预检覆盖夸克、UC、百度、迅雷和阿里云盘；PikPak 不需要转存，天翼云盘已停用。
- 预检失败时不得创建根任务、目录组、子任务或任务关联记录，也不得发送 `on_created`；只允许把轮换后的提供方上下文写回原有加密分享计划。
- 预检不得调用转存、复制或恢复接口，不得持久化、记录或通过 IPC 返回探测得到的签名 URL。
- 一个批次只加载一次计划、只探测一个尚未转存的代表条目、最多查询一次容量；checked arithmetic 防止大小累加溢出。
- 已由当前 `AccountV2` owner 转存且仍为 `Pending` 的条目不计入新增空间；其他 owner、legacy owner、已清理记录都必须计入。
- 直链分享以及全部条目都可复用转存的批次不查询容量。
- 任一待转存条目大小未知、容量响应非法或容量请求最终失败时采用失败关闭策略并明确“未建立下载任务”。
- 认证过期、网页登录验证和风控错误在受监督会话内部保持原错误类型，以便完成既有恢复；恢复结束后再补充“未建立下载任务”的用户上下文。
- 验证仅运行相关 Rust/前端定向测试和 Windows 发布构建，不默认执行全量测试；Vitest 必须带 `--exclude '.worktrees/**'`。

## File Structure

- Create: `src-tauri/src/cloud/share/preflight.rs` — 共享层预检算法、容量响应的统一整数校验和定向单元测试。
- Modify: `src-tauri/src/cloud/share.rs` — 注册并导出 `preflight` 模块。
- Modify: `src-tauri/src/cloud/share/models.rs` — 定义 `CloudTransferCapacity` 值对象。
- Modify: `src-tauri/src/cloud/share/capabilities.rs` — 为 `ShareTransfer` 增加 `transfer_capacity` 能力。
- Modify: `src-tauri/src/cloud/quark.rs` — 查询、解析夸克账号容量并测试请求契约。
- Modify: `src-tauri/src/cloud/uc.rs` — 查询、解析 UC 账号容量并测试请求契约。
- Modify: `src-tauri/src/cloud/baidu.rs` — 查询、解析百度账号容量并测试请求契约。
- Modify: `src-tauri/src/cloud/xunlei.rs` — 查询、解析迅雷账号容量并测试请求契约。
- Modify: `src-tauri/src/cloud/aliyun.rs` — 查询、解析阿里云盘 drive 容量及无限容量值并测试请求契约。
- Modify: `src-tauri/src/cloud/commands/share.rs` — 用受监督只读会话包装共享预检，并在会话恢复之后生成用户可见错误。
- Modify: `src-tauri/src/commands.rs` — 在自动单文件和确认批次/目录的提交之前调用预检，增加调用顺序回归测试。
- Modify: `src/App.live.test.tsx` — 验证预检拒绝时确认窗口保留、错误可见且创建回调未触发。
- Modify: `src-tauri/tests/live_share_quark_pikpak.rs` — 对用户的 2427 文件阿里云盘分享执行容量预检并继续用可复用样本验证真实非零下载。

---

### Task 1: Define the shared transfer-capacity contract

**Files:**
- Modify: `src-tauri/src/cloud/share/models.rs`
- Modify: `src-tauri/src/cloud/share/capabilities.rs`
- Create: `src-tauri/src/cloud/share/preflight.rs`
- Modify: `src-tauri/src/cloud/share.rs`

**Interfaces:**
- Produces: `CloudTransferCapacity::{Limited { total_bytes, used_bytes }, Unlimited}`.
- Produces: `CloudTransferCapacity::from_json(total: &serde_json::Value, used: &serde_json::Value, minus_one_is_unlimited: bool) -> Result<Self, QuarkError>`.
- Produces: `CloudTransferCapacity::available_bytes(&self) -> Option<u64>`; `None` means unlimited.
- Extends: `ShareTransfer::transfer_capacity(&self, entry: &CloudShareResolvedEntry) -> Result<CloudTransferCapacity, QuarkError>`.

- [ ] **Step 1: Write failing capacity-value tests**

Add these tests to `src-tauri/src/cloud/share/preflight.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn capacity_accepts_integer_and_decimal_string_fields() {
        assert_eq!(
            CloudTransferCapacity::from_json(&json!(100), &json!(40), false).unwrap(),
            CloudTransferCapacity::Limited { total_bytes: 100, used_bytes: 40 }
        );
        assert_eq!(
            CloudTransferCapacity::from_json(&json!("100"), &json!("40"), false).unwrap(),
            CloudTransferCapacity::Limited { total_bytes: 100, used_bytes: 40 }
        );
        assert_eq!(
            CloudTransferCapacity::Limited { total_bytes: 100, used_bytes: 40 }.available_bytes(),
            Some(60)
        );
    }

    #[test]
    fn capacity_accepts_only_the_explicit_unlimited_sentinel() {
        assert_eq!(
            CloudTransferCapacity::from_json(&json!(-1), &json!(40), true).unwrap(),
            CloudTransferCapacity::Unlimited
        );
        assert_eq!(CloudTransferCapacity::Unlimited.available_bytes(), None);
        assert!(CloudTransferCapacity::from_json(&json!(-1), &json!(40), false).is_err());
    }

    #[test]
    fn capacity_rejects_missing_fractional_negative_and_inverted_values() {
        for (total, used) in [
            (json!(null), json!(1)),
            (json!(10.5), json!(1)),
            (json!(10), json!(-1)),
            (json!(10), json!(11)),
            (json!("+10"), json!("1")),
            (json!("ten"), json!("1")),
        ] {
            assert!(CloudTransferCapacity::from_json(&total, &used, false).is_err());
        }
    }
}
```

- [ ] **Step 2: Run the test and verify the missing type fails compilation**

Run:

```powershell
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --lib cloud::share::preflight::tests::capacity_ -- --nocapture
```

Expected: compilation fails because `CloudTransferCapacity`, `preflight` and `ShareTransfer::transfer_capacity` do not exist.

- [ ] **Step 3: Add the model, strict parser and capability method**

Add to `models.rs`:

```rust
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
    value.as_u64().or_else(|| {
        value.as_str()
            .filter(|raw| !raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|raw| raw.parse().ok())
    }).ok_or(QuarkError::InvalidResponse)
}
```

Add this fail-closed default method to `ShareTransfer`; it keeps intermediate commits compilable and makes any future transfer adapter without a capacity implementation reject rather than guess:

```rust
fn transfer_capacity(
    &self,
    _: &CloudShareResolvedEntry,
) -> Result<CloudTransferCapacity, QuarkError> {
    Err(QuarkError::ProviderUnsupported)
}
```

Register `mod preflight; pub use preflight::*;` in `share.rs`. Existing `ShareTransfer` mocks use the fail-closed default until a test explicitly exercises preflight, so their original assertions remain unchanged.

- [ ] **Step 4: Run the focused tests**

Run:

```powershell
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --lib cloud::share::preflight::tests::capacity_ -- --nocapture
```

Expected: all three capacity-value tests pass.

- [ ] **Step 5: Commit the shared contract**

```powershell
git add src-tauri/src/cloud/share.rs src-tauri/src/cloud/share/models.rs src-tauri/src/cloud/share/capabilities.rs src-tauri/src/cloud/share/preflight.rs
git commit -m "feat: add share transfer capacity contract"
```

### Task 2: Implement the one-load shared preflight algorithm

**Files:**
- Modify: `src-tauri/src/cloud/share/preflight.rs`

**Interfaces:**
- Consumes: `CloudShareDownloadSelection`, `CloudSharePlanStore`, `CloudShareAdapter`, `CloudTransferCapacity`.
- Produces: `preflight_selected_share_download(store: &CloudSharePlanStore, adapter: &dyn CloudShareAdapter, selection: &CloudShareDownloadSelection) -> Result<(), QuarkError>`.

- [ ] **Step 1: Add a configurable fake adapter and failing behavior tests**

Add this test harness to the `preflight.rs` test module; its `transfer` method panics so any accidental mutation fails immediately:

```rust
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};

enum ProbeReply {
    Direct(String),
    TransferRequired,
    RotatedError(serde_json::Value, QuarkError),
}

struct PreflightAdapter {
    owner: Option<String>,
    probe: Mutex<Option<ProbeReply>>,
    capacity: Mutex<Option<Result<CloudTransferCapacity, QuarkError>>>,
    resolve_calls: AtomicUsize,
    capacity_calls: AtomicUsize,
}

impl PreflightAdapter {
    fn new() -> Self {
        Self {
            owner: Some("a".repeat(64)),
            probe: Mutex::new(None),
            capacity: Mutex::new(Some(Ok(CloudTransferCapacity::Unlimited))),
            resolve_calls: AtomicUsize::new(0),
            capacity_calls: AtomicUsize::new(0),
        }
    }
    fn resolve_direct(&self, url: &str) {
        *self.probe.lock().unwrap() = Some(ProbeReply::Direct(url.into()));
    }
    fn require_transfer(&self) {
        *self.probe.lock().unwrap() = Some(ProbeReply::TransferRequired);
    }
    fn capacity(&self, value: CloudTransferCapacity) {
        *self.capacity.lock().unwrap() = Some(Ok(value));
    }
    fn capacity_error(&self, error: QuarkError) {
        *self.capacity.lock().unwrap() = Some(Err(error));
    }
    fn resolve_calls(&self) -> usize { self.resolve_calls.load(Ordering::SeqCst) }
    fn capacity_calls(&self) -> usize { self.capacity_calls.load(Ordering::SeqCst) }
}

impl ShareInspect for PreflightAdapter {
    fn inspect(&self, _: &crate::parser::CloudShareCandidate) -> Result<ShareInspection, QuarkError> {
        unreachable!("preflight never inspects a share")
    }
}
impl ShareResolve for PreflightAdapter {
    fn resolve(&self, entry: &CloudShareResolvedEntry) -> Result<ShareResolution, QuarkError> {
        self.resolve_with_context(entry)?.result
    }
    fn resolve_with_context(
        &self,
        entry: &CloudShareResolvedEntry,
    ) -> Result<ShareOperationResult<ShareResolution>, QuarkError> {
        self.resolve_calls.fetch_add(1, Ordering::SeqCst);
        match self.probe.lock().unwrap().take().expect("one configured probe") {
            ProbeReply::Direct(url) => Ok(ShareOperationResult {
                result: Ok(ShareResolution::Direct(crate::cloud::commands::ResolvedCloudDownload {
                    url,
                    name: entry.entry.name.clone(),
                    headers: vec![],
                })),
                updated_context: None,
            }),
            ProbeReply::TransferRequired => Ok(ShareOperationResult {
                result: Ok(ShareResolution::TransferRequired(TransferRequest {
                    source: entry.plan.source_for(&entry.entry.entry_id),
                })),
                updated_context: None,
            }),
            ProbeReply::RotatedError(context, error) => Ok(ShareOperationResult {
                result: Err(error),
                updated_context: Some(context),
            }),
        }
    }
}
impl ShareTransfer for PreflightAdapter {
    fn transfer_capacity(&self, _: &CloudShareResolvedEntry) -> Result<CloudTransferCapacity, QuarkError> {
        self.capacity_calls.fetch_add(1, Ordering::SeqCst);
        self.capacity.lock().unwrap().take().expect("one configured capacity")
    }
    fn transfer_owner_fingerprint(&self, _: &CloudShareResolvedEntry) -> Result<Option<String>, QuarkError> {
        Ok(self.owner.clone())
    }
    fn transfer(
        &self,
        _: &CloudShareResolvedEntry,
        _: &CloudSharePlanStore,
    ) -> Result<crate::cloud::commands::ResolvedCloudDownload, QuarkError> {
        panic!("capacity preflight must not transfer")
    }
}
impl CloudShareAdapter for PreflightAdapter {
    fn transfer_capability(&self) -> Option<&dyn ShareTransfer> { Some(self) }
}

struct ReadOnlyAdapter;
impl ShareInspect for ReadOnlyAdapter {
    fn inspect(&self, _: &crate::parser::CloudShareCandidate) -> Result<ShareInspection, QuarkError> {
        unreachable!("preflight never inspects a share")
    }
}
impl ShareResolve for ReadOnlyAdapter {
    fn resolve(&self, _: &CloudShareResolvedEntry) -> Result<ShareResolution, QuarkError> {
        unreachable!("a provider without transfer capability skips preflight")
    }
}
impl CloudShareAdapter for ReadOnlyAdapter {}

struct PreflightFixture {
    store: CloudSharePlanStore,
    reads: std::sync::Arc<test_support::CountingPlanCredentials>,
    selection: CloudShareDownloadSelection,
    adapter: PreflightAdapter,
}

fn preflight_fixture(count: usize, size: Option<u64>) -> PreflightFixture {
    let (store, reads) = test_support::counted_plan_store();
    let entries = (0..count)
        .map(|index| CloudShareEntry::new(
            "share-capacity",
            format!("entry-{index}"),
            format!("file-{index}.bin"),
            vec![],
            size,
        ))
        .collect::<Vec<_>>();
    let plan = CloudSharePlan::new(
        CloudProvider::Aliyun,
        "share-capacity".into(),
        None,
        entries,
        serde_json::json!({"token":"old"}),
    );
    let plan_source = plan.source_for(&plan.entries[0].entry_id);
    let selected_ids = plan.entries.iter().map(|entry| entry.entry_fingerprint.clone()).collect::<Vec<_>>();
    store.upsert(&plan).unwrap();
    let selection = selected_share_download(&store, &plan_source, &selected_ids).unwrap();
    reads.reads.store(0, Ordering::SeqCst);
    PreflightFixture { store, reads, selection, adapter: PreflightAdapter::new() }
}
```

Add named tests with these exact assertions:

```rust
#[test]
fn direct_share_discards_probe_and_never_queries_capacity() {
    let fixture = preflight_fixture(2, Some(10));
    fixture.adapter.resolve_direct("https://cdn.example/signed?secret=1");
    preflight_selected_share_download(&fixture.store, &fixture.adapter, &fixture.selection).unwrap();
    assert_eq!(fixture.adapter.resolve_calls(), 1);
    assert_eq!(fixture.adapter.capacity_calls(), 0);
}

#[test]
fn transfer_required_with_insufficient_space_reports_no_task_error() {
    let fixture = preflight_fixture(2, Some(60));
    fixture.adapter.require_transfer();
    fixture.adapter.capacity(CloudTransferCapacity::Limited { total_bytes: 100, used_bytes: 10 });
    let error = preflight_selected_share_download(&fixture.store, &fixture.adapter, &fixture.selection).unwrap_err();
    assert_eq!(error.to_string(), "阿里云盘转存空间不足：本次需要 120 B，当前可用 90 B，未建立下载任务。");
    assert_eq!(fixture.adapter.resolve_calls(), 1);
    assert_eq!(fixture.adapter.capacity_calls(), 1);
}

#[test]
fn unknown_size_and_capacity_failure_fail_closed() {
    let unknown = preflight_fixture(1, None);
    unknown.adapter.require_transfer();
    let error = preflight_selected_share_download(
        &unknown.store, &unknown.adapter, &unknown.selection
    ).unwrap_err();
    assert!(error.to_string().contains("无法确定本次转存所需空间"));
    assert_eq!(unknown.adapter.capacity_calls(), 0);

    let failed = preflight_fixture(1, Some(7));
    failed.adapter.require_transfer();
    failed.adapter.capacity_error(QuarkError::Transport("offline".into()));
    assert_eq!(
        preflight_selected_share_download(&failed.store, &failed.adapter, &failed.selection)
            .unwrap_err().to_string(),
        "无法读取阿里云盘可用转存空间，未建立下载任务，请稍后重试。"
    );
}
```

Add the remaining tests with these concrete arrangements:

```rust
#[test]
fn provider_without_transfer_capability_skips_preflight() {
    let fixture = preflight_fixture(1, Some(7));
    preflight_selected_share_download(&fixture.store, &ReadOnlyAdapter, &fixture.selection).unwrap();
    assert_eq!(fixture.reads.reads.load(Ordering::SeqCst), 0);
}

#[test]
fn current_owner_pending_transfers_are_excluded_but_other_records_are_not() {
    let fixture = preflight_fixture(2, Some(60));
    fixture.store.record_transfer_with_owner(
        &fixture.selection.entries[0].source, "personal-0", &"a".repeat(64)
    ).unwrap();
    fixture.store.record_transfer_with_owner(
        &fixture.selection.entries[1].source, "other-personal-1", &"b".repeat(64)
    ).unwrap();
    fixture.adapter.require_transfer();
    fixture.adapter.capacity(CloudTransferCapacity::Limited { total_bytes: 100, used_bytes: 30 });
    preflight_selected_share_download(&fixture.store, &fixture.adapter, &fixture.selection).unwrap();
}

#[test]
fn all_current_owner_transfers_skip_resolve_and_capacity() {
    let fixture = preflight_fixture(2, Some(60));
    for (index, entry) in fixture.selection.entries.iter().enumerate() {
        fixture.store.record_transfer_with_owner(
            &entry.source, &format!("personal-{index}"), &"a".repeat(64)
        ).unwrap();
    }
    preflight_selected_share_download(&fixture.store, &fixture.adapter, &fixture.selection).unwrap();
    assert_eq!(fixture.adapter.resolve_calls(), 0);
    assert_eq!(fixture.adapter.capacity_calls(), 0);
}

#[test]
fn rotated_context_is_persisted_before_resolve_error_returns() {
    let fixture = preflight_fixture(1, Some(7));
    *fixture.adapter.probe.lock().unwrap() = Some(ProbeReply::RotatedError(
        serde_json::json!({"token":"rotated"}), QuarkError::AuthorizationExpired
    ));
    assert!(matches!(
        preflight_selected_share_download(&fixture.store, &fixture.adapter, &fixture.selection),
        Err(QuarkError::AuthorizationExpired)
    ));
    assert_eq!(
        fixture.store.resolve_entry(&fixture.selection.plan_source).unwrap().plan.provider_context["token"],
        "rotated"
    );
}

#[test]
fn large_selection_loads_once_resolves_once_and_queries_capacity_once() {
    let fixture = preflight_fixture(2_427, Some(1));
    fixture.adapter.require_transfer();
    fixture.adapter.capacity(CloudTransferCapacity::Limited { total_bytes: 10_000, used_bytes: 0 });
    preflight_selected_share_download(&fixture.store, &fixture.adapter, &fixture.selection).unwrap();
    assert_eq!(fixture.reads.reads.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.adapter.resolve_calls(), 1);
    assert_eq!(fixture.adapter.capacity_calls(), 1);
}

#[test]
fn capacity_overflow_fails_before_capacity_query() {
    let fixture = preflight_fixture(2, Some(u64::MAX));
    fixture.adapter.require_transfer();
    assert!(preflight_selected_share_download(&fixture.store, &fixture.adapter, &fixture.selection).is_err());
    assert_eq!(fixture.adapter.capacity_calls(), 0);
}
```

- [ ] **Step 2: Run tests and confirm they fail before implementation**

Run:

```powershell
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --lib cloud::share::preflight::tests -- --nocapture
```

Expected: the new behavior tests fail because `preflight_selected_share_download` is not implemented.

- [ ] **Step 3: Implement preflight with one plan snapshot**

Implement the algorithm with this structure:

```rust
pub fn preflight_selected_share_download(
    store: &CloudSharePlanStore,
    adapter: &dyn CloudShareAdapter,
    selection: &CloudShareDownloadSelection,
) -> Result<(), QuarkError> {
    let Some(transfer) = adapter.transfer_capability() else {
        return Ok(());
    };
    let mut plan = store.resolve_entry(&selection.plan_source)?.plan;
    if plan.provider != selection.provider {
        return Err(QuarkError::InvalidResponse);
    }
    let first_selected = selection.entries.first()
        .ok_or_else(|| QuarkError::InvalidInput("请至少选择一个分享文件".into()))?;
    let first = resolved_selection_entry(&plan, first_selected)?;
    let owner = transfer.transfer_owner_fingerprint(&first)?;
    let reusable = reusable_entry_fingerprints(&plan, owner.as_deref());
    let pending = selection.entries.iter()
        .filter(|selected| !reusable.contains(&selected.selected_id))
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(());
    }

    let mut representative = resolved_selection_entry(&plan, pending[0])?;
    let resolution = adapter.resolve_with_context(&representative)?;
    if let Some(context) = resolution.updated_context {
        store.update_provider_context(&selection.plan_source, &context)?;
        plan.provider_context = context;
        representative.plan = plan.clone();
    }
    match resolution.result? {
        ShareResolution::Direct(_) => Ok(()),
        ShareResolution::TransferRequired(request)
            if request.source == pending[0].source => {
                let required = required_transfer_bytes(&pending)?;
                let capacity = transfer.transfer_capacity(&representative)
                    .map_err(|error| capacity_query_error(selection.provider, error))?;
                ensure_capacity(selection.provider, required, capacity)
            }
        ShareResolution::TransferRequired(_) => Err(QuarkError::InvalidResponse),
    }
}
```

Implement private helpers with these rules:

- `resolved_selection_entry` matches `selected_id` to `entry_fingerprint` and also verifies the durable `source` equals `plan.source_for(entry_id)`.
- `reusable_entry_fingerprints` includes only `Pending + AccountV2 + matching owner` records.
- `required_transfer_bytes` uses `try_fold(0_u64, u64::checked_add)` and returns the exact unknown-size error before capacity is queried.
- `capacity_query_error` propagates `NotAuthorized`, `AuthorizationExpired`, `SessionRenewedRetryOperation`, `WebAuthorizationRequired`, `CaptchaRequired` and `PikpakOperationVerificationRequired` unchanged; all transport/provider/invalid-response failures become the provider-specific capacity-read message.
- `ensure_capacity` uses `ProviderRegistry::global().get(provider).share_display_name` and existing `format_confirmation_size`; `Unlimited` succeeds.

- [ ] **Step 4: Run the shared preflight suite**

Run:

```powershell
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --lib cloud::share::preflight::tests -- --nocapture
```

Expected: direct, insufficient, unknown, owner reuse, context rotation, overflow and 2427-entry tests all pass.

- [ ] **Step 5: Commit the shared algorithm**

```powershell
git add src-tauri/src/cloud/share/preflight.rs
git commit -m "feat: preflight share transfer capacity"
```

### Task 3: Add Quark and UC capacity adapters

**Files:**
- Modify: `src-tauri/src/cloud/quark.rs`
- Modify: `src-tauri/src/cloud/uc.rs`

**Interfaces:**
- Consumes: `ShareTransfer::transfer_capacity`, `CloudTransferCapacity::from_json`.
- Produces: one authenticated member request per provider and validated `CloudTransferCapacity`.

- [ ] **Step 1: Write failing Quark and UC request-contract tests**

Add to the existing provider test modules:

```rust
fn capacity_entry(provider: CloudProvider) -> CloudShareResolvedEntry {
    let plan = CloudSharePlan::new(
        provider,
        "capacity-share".into(),
        None,
        vec![CloudShareEntry::new(
            "capacity-share", "entry-1".into(), "file.bin".into(), vec![], Some(1),
        )],
        json!({}),
    );
    CloudShareResolvedEntry { provider, entry: plan.entries[0].clone(), plan }
}

#[test]
fn quark_share_transfer_capacity_uses_member_bytes() {
    let credentials = MemoryCredentialStore::default();
    credentials.save("quark", b"__uid=account-7; sid=current").unwrap();
    let http = Http::new(vec![json!({
        "status": 200, "code": 0,
        "data": {"total_capacity": "1000", "use_capacity": 250}
    })]);
    let adapter = QuarkShareAdapter { credentials: &credentials, http: &http };
    assert_eq!(
        adapter.transfer_capacity(&capacity_entry(CloudProvider::Quark)).unwrap(),
        CloudTransferCapacity::Limited { total_bytes: 1000, used_bytes: 250 }
    );
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert!(requests[0].url.contains("/1/clouddrive/member?"));
    assert!(!requests[0].url.contains("sharepage/save"));
}

#[test]
fn uc_share_transfer_capacity_uses_member_bytes() {
    let credentials = share_credentials();
    let http = Http::new(vec![json!({
        "code": 0, "status": 200,
        "data": {"total_capacity": 1000, "use_capacity": "250"}
    })]);
    let adapter = UcShareAdapter { credentials: &credentials, http: &http };
    assert_eq!(
        adapter.transfer_capacity(&share_entry()).unwrap(),
        CloudTransferCapacity::Limited { total_bytes: 1000, used_bytes: 250 }
    );
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].url.contains("/member?fetch_subscribe=true"));
    assert!(!requests[0].url.contains("sharepage/save"));
}
```

Put `capacity_entry` in the Quark test module; the UC test module uses its existing `share_entry`. Add these invalid-field assertions to prove the shared parser is actually used:

```rust
let invalid_quark = Http::new(vec![json!({
    "status": 200, "code": 0,
    "data": {"total_capacity": 100, "use_capacity": 101}
})]);
assert!(QuarkShareAdapter { credentials: &credentials, http: &invalid_quark }
    .transfer_capacity(&capacity_entry(CloudProvider::Quark)).is_err());

let invalid_uc = Http::new(vec![json!({
    "code": 0, "status": 200,
    "data": {"total_capacity": "unknown", "use_capacity": 1}
})]);
assert!(UcShareAdapter { credentials: &credentials, http: &invalid_uc }
    .transfer_capacity(&share_entry()).is_err());
```

- [ ] **Step 2: Run provider tests and verify the required trait method fails**

Run:

```powershell
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --lib share_transfer_capacity_uses_member_bytes -- --nocapture
```

Expected: compilation or assertions fail until both adapters implement the capacity method.

- [ ] **Step 3: Implement the two member queries**

Add to the Quark `ShareTransfer` implementation:

```rust
fn transfer_capacity(
    &self,
    _: &CloudShareResolvedEntry,
) -> Result<CloudTransferCapacity, QuarkError> {
    let client = self.client()?;
    let response = client.request(
        "GET",
        "member?fetch_subscribe=true&_ch=home&fetch_identity=true",
        None,
    )?;
    let value = response_json(&response)?;
    CloudTransferCapacity::from_json(
        &value["data"]["total_capacity"],
        &value["data"]["use_capacity"],
        false,
    )
}
```

Add to the UC implementation:

```rust
fn transfer_capacity(
    &self,
    _: &CloudShareResolvedEntry,
) -> Result<CloudTransferCapacity, QuarkError> {
    let client = UcClient::new(self.stored_session()?, self.http);
    let value = client.request_json(
        "GET",
        "member",
        "fetch_subscribe=true&_ch=home&fetch_identity=true",
        None,
    )?;
    CloudTransferCapacity::from_json(
        &value["data"]["total_capacity"],
        &value["data"]["use_capacity"],
        false,
    )
}
```

- [ ] **Step 4: Run Quark and UC capacity tests**

Run:

```powershell
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --lib share_transfer_capacity_uses_member_bytes -- --nocapture
```

Expected: both request-contract tests and invalid-field assertions pass.

- [ ] **Step 5: Commit both adapters**

```powershell
git add src-tauri/src/cloud/quark.rs src-tauri/src/cloud/uc.rs
git commit -m "feat: query Quark and UC transfer capacity"
```

### Task 4: Add Baidu and Xunlei capacity adapters

**Files:**
- Modify: `src-tauri/src/cloud/baidu.rs`
- Modify: `src-tauri/src/cloud/xunlei.rs`

**Interfaces:**
- Consumes: `CloudTransferCapacity::from_json`.
- Produces: Baidu `/api/quota` and Xunlei `/drive/v1/about` capacity implementations with existing auth/challenge mapping.

- [ ] **Step 1: Write failing request and error-mapping tests**

Add these behavioral tests using the existing provider HTTP fakes:

```rust
fn baidu_capacity_entry() -> CloudShareResolvedEntry {
    let plan = CloudSharePlan::new(
        CloudProvider::Baidu,
        "capacity-share".into(),
        None,
        vec![CloudShareEntry::new(
            "capacity-share", "1".into(), "file.bin".into(), vec![], Some(1),
        )],
        json!({}),
    );
    CloudShareResolvedEntry {
        provider: CloudProvider::Baidu,
        entry: plan.entries[0].clone(),
        plan,
    }
}

#[test]
fn baidu_share_transfer_capacity_uses_quota_bytes() {
    let credentials = MemoryCredentialStore::default();
    credentials.save(
        "baidu",
        &serde_json::to_vec(&BaiduSession::cookie("BDUSS=current")).unwrap(),
    ).unwrap();
    let http = Http::new(vec![json!({"errno": 0, "total": "2048", "used": 512})]);
    let adapter = BaiduShareAdapter { credentials: &credentials, http: &http };
    assert_eq!(
        adapter.transfer_capacity(&baidu_capacity_entry()).unwrap(),
        CloudTransferCapacity::Limited { total_bytes: 2048, used_bytes: 512 }
    );
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].url.contains("/api/quota?"));
    assert!(!requests[0].url.contains("/share/transfer"));
}

#[test]
fn xunlei_share_transfer_capacity_uses_drive_about_quota() {
    let credentials = credentials();
    let http = Http::new(vec![
        json!({"captcha_token":"read-token"}),
        json!({"quota":{"limit":"4096","usage":1024}}),
    ]);
    let adapter = XunleiShareAdapter { credentials: &credentials, http: &http };
    assert_eq!(
        adapter.transfer_capacity(&selected()).unwrap(),
        CloudTransferCapacity::Limited { total_bytes: 4096, used_bytes: 1024 }
    );
    let requests = http.requests.lock().unwrap();
    assert!(requests.iter().any(|request| request.url.ends_with("/drive/v1/about")));
    assert!(!requests.iter().any(|request| request.url.contains("/share/restore")));
}
```

Add the auth fixtures below so capacity code cannot erase existing recovery semantics:

```rust
struct FixedStatusHttp(u16);
impl CloudHttpTransport for FixedStatusHttp {
    fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        Ok(CloudHttpResponse { status: self.0, body: "{}".into(), ..Default::default() })
    }
}

#[test]
fn baidu_share_transfer_capacity_preserves_expired_authorization() {
    let credentials = MemoryCredentialStore::default();
    credentials.save(
        "baidu",
        &serde_json::to_vec(&BaiduSession::cookie("BDUSS=current")).unwrap(),
    ).unwrap();
    assert!(matches!(
        BaiduShareAdapter { credentials: &credentials, http: &FixedStatusHttp(401) }
            .transfer_capacity(&baidu_capacity_entry()),
        Err(QuarkError::AuthorizationExpired)
    ));
}
```

For Xunlei, add this two-response transport and assertion:

```rust
struct CapacityStatusHttp(std::sync::atomic::AtomicUsize);
impl CapacityStatusHttp {
    fn new() -> Self { Self(std::sync::atomic::AtomicUsize::new(0)) }
}
impl CloudHttpTransport for CapacityStatusHttp {
    fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
        let index = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(if index == 0 {
            CloudHttpResponse {
                status: 200,
                body: json!({"captcha_token":"read-token","expires_in":300}).to_string(),
                ..Default::default()
            }
        } else {
            CloudHttpResponse { status: 412, body: "{}".into(), ..Default::default() }
        })
    }
}

#[test]
fn xunlei_share_transfer_capacity_preserves_web_verification() {
    let stored = credentials();
    let http = CapacityStatusHttp::new();
assert!(matches!(
    XunleiShareAdapter { credentials: &stored, http: &http }
        .transfer_capacity(&selected()),
    Err(QuarkError::WebAuthorizationRequired)
));
}
```

- [ ] **Step 2: Run tests and verify they fail before implementation**

Run:

```powershell
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --lib share_transfer_capacity_uses_ -- --nocapture
```

Expected: the Baidu and Xunlei capacity tests fail until the endpoints are implemented.

- [ ] **Step 3: Implement Baidu quota parsing**

Add this method to `BaiduShareAdapter`'s `ShareTransfer` implementation:

```rust
fn transfer_capacity(
    &self,
    _: &CloudShareResolvedEntry,
) -> Result<CloudTransferCapacity, QuarkError> {
    let value = self.client()?.request(
        "GET",
        format!("{PAN}/api/quota?checkexpire=1&checkfree=1&channel=chunlei&web=1&app_id=250528&clienttype=0"),
        None,
    )?;
    CloudTransferCapacity::from_json(&value["total"], &value["used"], false)
}
```

- [ ] **Step 4: Implement Xunlei about parsing without a restore call**

Add an internal `XunleiClient::drive_capacity()` that performs `GET self.drive_url("/drive/v1/about")` through the existing request path and parses `quota.limit`/`quota.usage`. In `XunleiShareAdapter::transfer_capacity`, load the stored session, wrap `self.http` in `XunleiShareTransport`, select `.supervised()` when `credentials.automatic_session_recovery()` is false, call `drive_capacity`, then save `client.session()` so a legitimate read-time token rotation is retained.

The parser body is:

```rust
fn drive_capacity(&self) -> Result<CloudTransferCapacity, QuarkError> {
    let value = self.request("GET", self.drive_url("/drive/v1/about"), None)?;
    CloudTransferCapacity::from_json(
        &value["quota"]["limit"],
        &value["quota"]["usage"],
        false,
    )
}

fn transfer_capacity(
    &self,
    _: &CloudShareResolvedEntry,
) -> Result<CloudTransferCapacity, QuarkError> {
    let bytes = self.credentials.load("xunlei")?.ok_or(QuarkError::NotAuthorized)?;
    let session: XunleiSession = serde_json::from_slice(&bytes).map_err(|_| QuarkError::Storage)?;
    let transport = XunleiShareTransport(self.http);
    let client = XunleiClient::new(session, &transport);
    let client = if self.credentials.automatic_session_recovery() {
        client
    } else {
        client.supervised()
    };
    let result = client.drive_capacity();
    self.credentials.save(
        "xunlei",
        &serde_json::to_vec(&client.session()).map_err(|_| QuarkError::Storage)?,
    )?;
    result
}
```

- [ ] **Step 5: Run Baidu and Xunlei tests**

Run:

```powershell
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --lib share_transfer_capacity_uses_ -- --nocapture
```

Expected: request URLs, parsed bytes, 401/412 preservation and no-transfer assertions pass.

- [ ] **Step 6: Commit both adapters**

```powershell
git add src-tauri/src/cloud/baidu.rs src-tauri/src/cloud/xunlei.rs
git commit -m "feat: query Baidu and Xunlei transfer capacity"
```

### Task 5: Add Aliyun capacity and unlimited-drive support

**Files:**
- Modify: `src-tauri/src/cloud/aliyun.rs`

**Interfaces:**
- Consumes: current `AliyunSession.drive_id`, `personal_request`, and `CloudTransferCapacity::from_json`.
- Produces: authenticated `POST /v2/drive/get` capacity query; `total_size == -1` maps to `Unlimited`.

- [ ] **Step 1: Write failing Aliyun contract tests**

Add to the existing Aliyun test module:

```rust
fn aliyun_capacity_entry() -> CloudShareResolvedEntry {
    entry(CloudSharePlan::new(
        CloudProvider::Aliyun,
        "capacity-share".into(),
        None,
        vec![CloudShareEntry::new(
            "capacity-share", "entry-1".into(), "file.bin".into(), vec![], Some(1),
        )],
        json!({"share_token":"share-token"}),
    ))
}

fn aliyun_capacity_credentials() -> MemoryCredentialStore {
    let credentials = MemoryCredentialStore::default();
    credentials.save(
        "aliyun",
        br#"{"refresh_token":"refresh","access_token":"personal-secret","drive_id":"drive-secret"}"#,
    ).unwrap();
    credentials
}

#[test]
fn aliyun_share_transfer_capacity_uses_current_drive() {
    let credentials = aliyun_capacity_credentials();
    let http = Http::new(vec![json!({"total_size":"1099511627776","used_size":1024})]);
    let adapter = AliyunShareAdapter { credentials: &credentials, http: &http };
    assert_eq!(
        adapter.transfer_capacity(&aliyun_capacity_entry()).unwrap(),
        CloudTransferCapacity::Limited { total_bytes: 1_099_511_627_776, used_bytes: 1024 }
    );
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].url.ends_with("/v2/drive/get"));
    assert_eq!(requests[0].headers["Authorization"], "Bearer personal-secret");
    assert_eq!(serde_json::from_str::<Value>(requests[0].body.as_ref().unwrap()).unwrap()["drive_id"], "drive-secret");
    assert!(!requests[0].url.ends_with("/v2/file/copy"));
}

#[test]
fn aliyun_share_transfer_capacity_accepts_unlimited_drive() {
    let credentials = aliyun_capacity_credentials();
    let http = Http::new(vec![json!({"total_size":-1,"used_size":"1024"})]);
    let adapter = AliyunShareAdapter { credentials: &credentials, http: &http };
    assert_eq!(
        adapter.transfer_capacity(&aliyun_capacity_entry()).unwrap(),
        CloudTransferCapacity::Unlimited
    );
}
```

Add these invalid-response and 401 assertions:

```rust
#[test]
fn aliyun_share_transfer_capacity_rejects_invalid_bytes() {
    for response in [
        json!({"used_size":1}),
        json!({"total_size":100,"used_size":-1}),
        json!({"total_size":100,"used_size":101}),
    ] {
        let credentials = aliyun_capacity_credentials();
        let http = Http::new(vec![response]);
        assert!(AliyunShareAdapter { credentials: &credentials, http: &http }
            .transfer_capacity(&aliyun_capacity_entry()).is_err());
    }
}

#[test]
fn aliyun_share_transfer_capacity_preserves_expired_authorization() {
    let credentials = aliyun_capacity_credentials();
    let http = StatusHttp {
        replies: std::sync::Mutex::new([
            CloudHttpResponse { status: 401, body: "{}".into(), ..Default::default() }
        ].into()),
        requests: std::sync::Mutex::new(vec![]),
    };
    assert!(matches!(
        AliyunShareAdapter { credentials: &credentials, http: &http }
            .transfer_capacity(&aliyun_capacity_entry()),
        Err(QuarkError::AuthorizationExpired)
    ));
}
```

- [ ] **Step 2: Run Aliyun capacity tests and verify failure**

Run:

```powershell
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --lib aliyun_share_transfer_capacity -- --nocapture
```

Expected: tests fail because `transfer_capacity` is not implemented.

- [ ] **Step 3: Implement `POST /v2/drive/get`**

Add to `AliyunShareAdapter`'s `ShareTransfer` implementation:

```rust
fn transfer_capacity(
    &self,
    _: &CloudShareResolvedEntry,
) -> Result<CloudTransferCapacity, QuarkError> {
    let session = self.personal_session()?;
    let value = self.personal_request(
        &session,
        "/v2/drive/get",
        json!({"drive_id": session.drive_id}),
    )?;
    CloudTransferCapacity::from_json(&value["total_size"], &value["used_size"], true)
}
```

- [ ] **Step 4: Run Aliyun tests**

Run:

```powershell
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --lib aliyun_share_transfer_capacity -- --nocapture
```

Expected: limited, unlimited, invalid-response, auth and no-copy tests pass.

- [ ] **Step 5: Commit the Aliyun adapter**

```powershell
git add src-tauri/src/cloud/aliyun.rs
git commit -m "feat: query Aliyun transfer capacity"
```

### Task 6: Gate every new share task before creation

**Files:**
- Modify: `src-tauri/src/cloud/commands/share.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src/App.live.test.tsx`

**Interfaces:**
- Consumes: `preflight_selected_share_download` and `run_share_session_read_work`.
- Produces: `preflight_cloud_share_download(app: AppHandle, selection: &CloudShareDownloadSelection) -> Result<(), QuarkError>`.
- Preserves: existing `confirmCloudShareDownload` IPC signature; no frontend production API/type change.

- [ ] **Step 1: Add backend ordering tests before wiring**

In the existing `commands.rs` test module, add:

```rust
#[test]
fn confirmed_share_preflight_precedes_directory_and_file_submission() {
    let body = include_str!("commands.rs")
        .split_once("pub async fn confirm_cloud_share_download(").unwrap().1
        .split_once("fn mark_cloud_share_outcomes_private(").unwrap().0;
    let preflight = body.find("preflight_cloud_share_download").unwrap();
    assert!(preflight < body.find("submit_cloud_directory_plan_with_group_event").unwrap());
    assert!(preflight < body.find("submit_cloud_share_batch_with").unwrap());
}

#[test]
fn automatic_share_preflight_precedes_file_submission() {
    let body = include_str!("commands.rs")
        .split_once("pub async fn parse_download(").unwrap().1
        .split_once("let mut outcome = prepare_download").unwrap().0;
    assert!(body.find("preflight_cloud_share_download").unwrap()
        < body.find("submit_cloud_share_file").unwrap());
}
```

- [ ] **Step 2: Add the frontend rejection regression test**

Add to `src/App.live.test.tsx` beside the existing rejected-share test:

```tsx
it('keeps share confirmation open and creates nothing when transfer capacity preflight rejects', async () => {
  state.parse.mockResolvedValueOnce({ confirmation: {
    taskId: 'cloud-share://aliyun/plan', title: '阿里云盘分享', requiresConfirmation: true,
    resources: [{ id: 'f1', name: 'movie.mp4', size: '92.5 MB', selected: true }], processing: [],
  } });
  state.confirmCloud.mockRejectedValueOnce(new Error(
    '阿里云盘转存空间不足：本次需要 92.5 MB，当前可用 10.0 MB，未建立下载任务。'
  ));
  render(<App />);
  await waitFor(() => expect(state.listeners.get('launcher-parse')).toBeDefined());
  await act(async () => {
    state.listeners.get('launcher-parse')?.({ payload: 'https://www.alipan.com/s/share' });
  });
  fireEvent.click(screen.getByRole('button', { name: /立即下载 1/ }));
  expect(await screen.findByRole('status')).toHaveTextContent('未建立下载任务');
  expect(screen.getByRole('dialog', { name: '确认下载内容' })).toBeVisible();
  expect(state.confirmCloud.mock.calls[0][4]).toBeTypeOf('function');
  expect(within(screen.getByRole('region', { name: '下载队列' })).queryByTitle('movie.mp4')).not.toBeInTheDocument();
});
```

- [ ] **Step 3: Run the tests and verify the backend order test fails**

Run:

```powershell
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --lib share_preflight_precedes -- --nocapture
pnpm exec vitest run src/App.live.test.tsx --exclude '.worktrees/**' -t "transfer capacity preflight rejects"
```

Expected: Rust ordering tests fail because the call is absent; the frontend regression already passes against the existing catch behavior.

- [ ] **Step 4: Add the supervised read wrapper**

In `cloud/commands/share.rs`, implement:

```rust
pub async fn preflight_cloud_share_download(
    app: AppHandle,
    selection: &share::CloudShareDownloadSelection,
) -> Result<(), QuarkError> {
    let provider = selection.provider;
    let selection = selection.clone();
    let result = run_share_session_read_work(app, provider, move |state, http| {
        let retrying_http = share::RetryingShareReadTransport::new(http);
        let adapter = share::provider_share_adapter(provider, &state.store, &retrying_http)?;
        share::preflight_selected_share_download(&state.share_plans, adapter.as_ref(), &selection)
    }).await;
    result.map_err(share_preflight_user_error)
}
```

Implement the wrapper after the supervised call so auth recovery still sees the original typed variant:

```rust
fn share_preflight_user_error(error: QuarkError) -> QuarkError {
    if error.to_string().contains("未建立下载任务") {
        error
    } else {
        QuarkError::InvalidInput(format!("{error}，未建立下载任务。"))
    }
}
```

- [ ] **Step 5: Wire confirmation and automatic submission before any create path**

In `confirm_cloud_share_download`, immediately after submission options/private-mode validation and before the directory branch, add:

```rust
crate::cloud::commands::preflight_cloud_share_download(app.clone(), &selection).await?;
```

In `parse_download`'s `AutoSubmit` arm, construct the exact one-file selection from `confirmation.plan_source` and its one resource, then preflight before `submit_cloud_share_file`:

```rust
let cloud = app
    .try_state::<Arc<crate::cloud::commands::CloudState>>()
    .ok_or_else(|| "网盘分享下载状态不可用，请重新解析分享链接".to_string())?;
let selected_ids = confirmation.resources.iter().map(|resource| resource.id.clone()).collect::<Vec<_>>();
let selection = crate::cloud::share::selected_share_download(
    &cloud.share_plans,
    &confirmation.plan_source,
    &selected_ids,
).map_err(|error| error.to_string())?;
crate::cloud::commands::preflight_cloud_share_download(app.clone(), &selection)
    .await
    .map_err(|error| error.to_string())?;
```

Keep `require_confirmation == true` returning the confirmation without preflight; the capacity check belongs to the later “立即下载” action.

- [ ] **Step 6: Run backend and frontend focused tests**

Run:

```powershell
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --lib share_preflight_precedes -- --nocapture
pnpm exec vitest run src/App.live.test.tsx --exclude '.worktrees/**' -t "transfer capacity preflight rejects"
```

Expected: two Rust ordering tests pass; the confirmation remains visible, the exact error is shown and no queue row is created.

- [ ] **Step 7: Commit the command gate and UI regression test**

```powershell
git add src-tauri/src/cloud/commands/share.rs src-tauri/src/commands.rs src/App.live.test.tsx
git commit -m "fix: reject share tasks before transfer overflow"
```

### Task 7: Extend the exact Aliyun live acceptance

**Files:**
- Modify: `src-tauri/tests/live_share_quark_pikpak.rs`

**Interfaces:**
- Consumes: `preflight_selected_share_download`, exact public share URL from `ALLDOWNLOADER_LIVE_ALIYUN_SHARE`, current DPAPI Aliyun session and isolated Aria2.
- Produces: redacted evidence for one-shot 2427-file inspection, one capacity query without `/v2/file/copy`, the real account's sufficient/insufficient result, and nonzero bytes for two reusable files; deterministic insufficient/no-task behavior is always covered by Task 2.

- [ ] **Step 1: Update the live test before running it**

Extend `RedactedTransport`'s Aliyun allowlist with `"/v2/drive/get"`. In `live_aliyun_share_parses_once_and_two_files_download`, retain all confirmation resource IDs, build a full `CloudShareDownloadSelection`, and run:

```rust
let all_ids = confirmation.resources
    .iter()
    .map(|resource| resource.id.clone())
    .collect::<Vec<_>>();
let full_selection = selected_share_download(
    &plans,
    &confirmation.plan_source,
    &all_ids,
)?;
let before = transport.observation_count();
let preflight = preflight_selected_share_download(
    &plans,
    adapter.as_ref(),
    &full_selection,
);
let remote = transport.observations_since(before);
assert!(remote.contains("request-path:/v2/drive/get"));
assert!(!remote.contains("request-path:/v2/file/copy"));
match preflight {
    Err(error) if error.to_string().contains("转存空间不足") => {
        assert!(error.to_string().contains("未建立下载任务"));
        println!("LIVE_ALIYUN_PREFLIGHT state=insufficient no_task=true remote={remote}");
    }
    Ok(()) => {
        println!("LIVE_ALIYUN_PREFLIGHT state=sufficient no_task=true remote={remote}");
    }
    Err(error) => return Err(error.into()),
}
```

Keep the existing reusable-transfer selection and `wait_for_first_download_bytes` assertions so two real sample tasks each receive nonzero bytes without creating new cloud copies. If the current account is large enough for the entire share, the deterministic insufficient-capacity/no-task proof remains the shared fixture test from Task 2, as required by the spec; do not fill or delete user storage to manufacture a live shortage. Do not print the share URL, tokens, file IDs, signed URLs, headers or account identity.

- [ ] **Step 2: Run the opt-in exact-share acceptance**

Run:

```powershell
$env:ALLDOWNLOADER_LIVE_CLOUD='1'
$env:ALLDOWNLOADER_LIVE_ALIYUN_SHARE='https://www.alipan.com/s/2VsPJYYxm1u'
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --test live_share_quark_pikpak live_aliyun_share_parses_once_and_two_files_download -- --ignored --nocapture
```

Expected output includes:

```text
LIVE_ALIYUN_PARSE state=confirmation entries=2427 selected=2
LIVE_ALIYUN_PREFLIGHT state=insufficient no_task=true
# or, only when the account really has enough free space:
LIVE_ALIYUN_PREFLIGHT state=sufficient no_task=true
LIVE_ALIYUN_DOWNLOAD sample=1 ...
LIVE_ALIYUN_DOWNLOAD sample=2 ...
```

The two download lines must each be followed by the existing `completed > 0` assertion. The test must fail if inspection needs a second attempt, capacity preflight issues `/v2/file/copy`, capacity lookup returns an unexpected error, or either sample remains at zero bytes. When the real account reports insufficient capacity, the test also requires the exact no-task error; when it reports sufficient capacity, Task 2's deterministic insufficient fixture remains authoritative.

- [ ] **Step 3: Commit the live acceptance update**

```powershell
git add src-tauri/tests/live_share_quark_pikpak.rs
git commit -m "test: verify Aliyun transfer capacity preflight live"
```

### Task 8: Directed verification, master integration and Windows binary

**Files:**
- Verify: all files listed above.
- Output: `artifacts/alldownloader.exe` or, if that running file is locked, a versioned `artifacts/alldownloader-master-*.exe` whose suffix is computed from `git rev-parse --short HEAD`.

**Interfaces:**
- Consumes: completed feature branch and current `master` checkout with unrelated user changes preserved.
- Produces: verified `master` commit and SHA-256-addressable Windows executable.

- [ ] **Step 1: Run the complete directed Rust set**

Run:

```powershell
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --lib cloud::share::preflight::tests -- --nocapture
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --lib share_transfer_capacity -- --nocapture
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --lib share_preflight_precedes -- --nocapture
```

Expected: every selected test passes; no full Rust suite is run unless these tests expose a shared regression.

- [ ] **Step 2: Run the directed frontend test and production frontend build**

Run:

```powershell
pnpm exec vitest run src/App.live.test.tsx --exclude '.worktrees/**' -t "transfer capacity preflight rejects"
pnpm run build
```

Expected: selected Vitest passes and TypeScript/Vite production build succeeds.

- [ ] **Step 3: Re-run the exact live Aliyun acceptance immediately before completion**

Run:

```powershell
$env:ALLDOWNLOADER_LIVE_CLOUD='1'
$env:ALLDOWNLOADER_LIVE_ALIYUN_SHARE='https://www.alipan.com/s/2VsPJYYxm1u'
C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri\Cargo.toml --test live_share_quark_pikpak live_aliyun_share_parses_once_and_two_files_download -- --ignored --nocapture
```

Expected: 2427 entries on the first inspection, a valid full-selection capacity result without copy, and two nonzero download byte counts. If the account reports insufficient space, the exact error must state that no task was created.

- [ ] **Step 4: Review branch scope and integrate through the finishing skill**

Run:

```powershell
git status --short
git log --oneline master..HEAD
git diff --check master...HEAD
git diff --stat master...HEAD
```

Expected: only the spec, plan and implementation files in this plan are changed. Invoke `superpowers:finishing-a-development-branch`, preserve all unrelated dirty files in the root checkout, and fast-forward `master` to the verified feature branch when Git confirms there is no overlap.

- [ ] **Step 5: Build the latest master Windows binary**

From `D:\projects\downloader` after master integration, run:

```powershell
$buildStarted = Get-Date
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows.ps1
if ($LASTEXITCODE -ne 0) {
    $builtBinary = Get-Item '.tools\release-target\release\alldownloader.exe' -ErrorAction Stop
    if ($builtBinary.LastWriteTime -lt $buildStarted) {
        throw 'Windows release build failed before producing a fresh binary'
    }
    $shortSha = git rev-parse --short HEAD
    $versionedBinary = "artifacts\alldownloader-master-$shortSha.exe"
    Copy-Item -LiteralPath $builtBinary.FullName -Destination $versionedBinary -Force
}
```

Expected: release build with `custom-protocol` succeeds. If the final `artifacts\alldownloader.exe` copy is locked by the running application after compilation succeeds, the fresh `.tools\release-target\release\alldownloader.exe` is copied to the computed versioned name without terminating the user's running app.

- [ ] **Step 6: Record the binary identity and final repository state**

Run:

```powershell
$binary = Get-ChildItem artifacts\alldownloader*.exe | Sort-Object LastWriteTime -Descending | Select-Object -First 1
Get-FileHash -Algorithm SHA256 -LiteralPath $binary.FullName
git rev-parse HEAD
git status --short
```

Expected: report the exact master commit, binary absolute path, SHA-256, directed test results and live nonzero byte evidence; explicitly list unrelated pre-existing dirty files without claiming they were changed by this feature.
