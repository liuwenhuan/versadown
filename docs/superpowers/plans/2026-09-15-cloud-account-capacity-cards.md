# Cloud Account Capacity Cards Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show used and total storage on every authorized cloud account card and refresh it after successful capacity-changing operations.

**Architecture:** Promote the share-only capacity value into the common cloud domain and expose it through `DriveOps::storage_capacity`. A Tauri command returns capacity bound to the current account ID. Backend mutations emit a small invalidation event; the frontend keeps the last successful value per account and coalesces event-driven refreshes.

**Tech Stack:** Rust, Tauri 2 events/commands, React 18, TypeScript, Vitest, Cargo tests.

**Spec:** `docs/superpowers/specs/2026-09-15-cloud-account-capacity-cards-design.md`

## Global Constraints

- Capacity lookup failures must not change authorization state.
- First failure displays `容量暂不可用`; later failures retain the last value and display `更新失败`.
- Results and events are accepted only when both provider and account ID match.
- No periodic polling; mutation bursts are coalesced with a 400 ms trailing debounce.
- Only successful upload, copy, share transfer, trash, restore and permanent delete operations invalidate capacity.
- Tianyi remains disabled in the current product catalog; its backend capacity adapter is implemented without re-enabling the provider.

---

### Task 1: Common capacity model and share-preflight reuse

**Files:**
- Modify: `src-tauri/src/cloud/types.rs`
- Modify: `src-tauri/src/cloud/share/models.rs`
- Modify: `src-tauri/src/cloud/share/preflight.rs`
- Modify: `src-tauri/src/cloud/share/capabilities.rs`
- Modify: `src-tauri/src/cloud/quark.rs`
- Modify: `src-tauri/src/cloud/baidu.rs`
- Modify: `src-tauri/src/cloud/aliyun.rs`
- Modify: `src-tauri/src/cloud/xunlei.rs`
- Modify: `src-tauri/src/cloud/uc.rs`

**Interfaces:**
- Produces: `CloudStorageCapacity::{Limited { total_bytes, used_bytes }, Unlimited { used_bytes }}`.
- Produces: `CloudStorageCapacity::from_json(total, used, minus_one_is_unlimited)` and `available_bytes()`.
- Preserves: share preflight behavior through a `CloudTransferCapacity` alias or direct common-type use.

- [ ] **Step 1: Write failing common-model tests**

Add tests in `types.rs` covering limited numeric/string values, unlimited `-1` with preserved `used_bytes`, malformed/negative values, and `used_bytes > total_bytes`.

```rust
assert_eq!(
    CloudStorageCapacity::from_json(&json!(100), &json!(40), false).unwrap(),
    CloudStorageCapacity::Limited { total_bytes: 100, used_bytes: 40 }
);
assert_eq!(
    CloudStorageCapacity::from_json(&json!(-1), &json!(40), true).unwrap(),
    CloudStorageCapacity::Unlimited { used_bytes: 40 }
);
```

- [ ] **Step 2: Run the model test and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml cloud_storage_capacity -- --nocapture`

Expected: compilation fails because `CloudStorageCapacity` does not exist.

- [ ] **Step 3: Move parsing and availability logic into the common model**

Define the enum in `types.rs`, serialize it with camelCase fields and a stable tagged representation, and update share preflight imports/matches. Preserve the existing finite-space checks; update unlimited assertions to require the used byte count.

- [ ] **Step 4: Delegate five existing transfer-capacity parsers to the common type**

Replace imports and construction in Quark, Baidu, Aliyun, Xunlei and UC without changing their HTTP request behavior.

- [ ] **Step 5: Run focused Rust tests and verify GREEN**

Run: `cargo test --manifest-path src-tauri/Cargo.toml cloud_storage_capacity share_transfer_capacity share_preflight -- --nocapture`

Expected: all selected tests pass.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/cloud/types.rs src-tauri/src/cloud/share src-tauri/src/cloud/{quark,baidu,aliyun,xunlei,uc}.rs
git commit -m "refactor: unify cloud storage capacity model"
```

### Task 2: Provider capacity capability and seven native adapters

**Files:**
- Modify: `src-tauri/src/cloud/driver.rs`
- Modify: `src-tauri/src/cloud/driver/native/quark_driver.rs`
- Modify: `src-tauri/src/cloud/driver/native/pikpak_driver.rs`
- Modify: `src-tauri/src/cloud/driver/native/baidu_driver.rs`
- Modify: `src-tauri/src/cloud/driver/native/tianyi_driver.rs`
- Modify: `src-tauri/src/cloud/driver/native/aliyun_driver.rs`
- Modify: `src-tauri/src/cloud/driver/native/xunlei_driver.rs`
- Modify: `src-tauri/src/cloud/driver/native/uc_driver.rs`
- Modify: `src-tauri/src/cloud/quark.rs`
- Modify: `src-tauri/src/cloud/pikpak.rs`
- Modify: `src-tauri/src/cloud/baidu.rs`
- Modify: `src-tauri/src/cloud/tianyi.rs`
- Modify: `src-tauri/src/cloud/aliyun.rs`
- Modify: `src-tauri/src/cloud/xunlei.rs`
- Modify: `src-tauri/src/cloud/uc.rs`

**Interfaces:**
- Consumes: `CloudStorageCapacity` from Task 1.
- Produces: `DriveOps::storage_capacity(&mut self) -> Result<CloudStorageCapacity, CloudError>`.
- Produces: `ProviderDriver::storage_capacity()`.

- [ ] **Step 1: Write failing driver delegation test**

Extend the driver factory test backend with a capacity result and assert that `ProviderDriver::storage_capacity()` invokes it without requiring a share entry.

```rust
assert_eq!(
    driver.storage_capacity().unwrap(),
    CloudStorageCapacity::Limited { total_bytes: 100, used_bytes: 25 }
);
```

- [ ] **Step 2: Run the driver test and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml provider_driver_storage_capacity -- --nocapture`

Expected: compilation fails because the driver method is missing.

- [ ] **Step 3: Add the object-safe driver capability**

Add a default unsupported method to `DriveOps` and a forwarding method to `ProviderDriver`. Capacity is available to every descriptor without adding a second provider list; unsupported responses remain structured errors.

- [ ] **Step 4: Add focused provider tests before implementations**

For all seven provider modules, add/convert tests that assert the exact quota endpoint, request authentication, valid field parsing and preservation of authentication/challenge errors. PikPak and Tianyi tests must first define fixtures from the provider's authenticated account/quota response used by their web clients.

- [ ] **Step 5: Implement five adapters by reusing existing quota requests**

Move the existing Quark, Baidu, Aliyun, Xunlei and UC request logic into reusable client functions such as `storage_capacity()`. Both native drivers and share adapters call these functions.

- [ ] **Step 6: Implement PikPak and Tianyi adapters**

Use each provider's authenticated account/quota endpoint through the existing transport and session loader. Parse into the common model; do not open a browser or create a new session inside capacity lookup.

- [ ] **Step 7: Run seven provider tests and verify GREEN**

Run: `cargo test --manifest-path src-tauri/Cargo.toml storage_capacity -- --nocapture`

Expected: common, driver and all provider capacity tests pass.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/cloud/driver.rs src-tauri/src/cloud/driver/native src-tauri/src/cloud/*.rs
git commit -m "feat: add cloud provider capacity capability"
```

### Task 3: Capacity command and mutation invalidation events

**Files:**
- Modify: `src-tauri/src/cloud/types.rs`
- Modify: `src-tauri/src/cloud/commands/drive.rs`
- Modify: `src-tauri/src/cloud/commands/upload.rs`
- Modify: `src-tauri/src/cloud/commands/share.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: `src-tauri/tests/cloud_capacity_events.rs`

**Interfaces:**
- Consumes: `ProviderDriver::storage_capacity()`.
- Produces: `cloud_account_capacity(provider) -> CloudAccountCapacity`.
- Produces: `CloudCapacityInvalidated { provider, account_id, reason }` serialized as camelCase.
- Produces event: `cloud-capacity-invalidated`.

- [ ] **Step 1: Write failing command/event tests**

Cover account-bound response, absent account failure, event payload serialization, success-only invalidation and empty-input suppression. Use a fake driver factory and an event sink/helper so command behavior is testable without a live window.

```rust
assert_eq!(result.account_id, "account-1");
assert_eq!(event.reason, CloudCapacityChangeReason::Upload);
assert!(failed_mutation_events.is_empty());
```

- [ ] **Step 2: Run tests and verify RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_capacity_events -- --nocapture`

Expected: compilation fails because DTOs, command and event helper are missing.

- [ ] **Step 3: Add DTOs, query helper and Tauri command**

Within one supervised read operation, restore the current account and query capacity from the same session snapshot. Return `provider`, `accountId`, capacity and an RFC3339/epoch-compatible `updatedAt` value. Register `cloud_account_capacity` in `generate_handler!`.

- [ ] **Step 4: Add a centralized invalidation helper**

Implement one helper that resolves the current account ID after a successful mutation and emits `cloud-capacity-invalidated`. Event emission failure must not turn a successful cloud mutation into a user-visible failure.

- [ ] **Step 5: Wire drive mutations**

After non-empty successful `cloud_copy`, `cloud_trash`, `cloud_delete` and `cloud_restore`, emit reasons `copy`, `trash`, `delete` and `restore`. Failed/empty calls emit nothing.

- [ ] **Step 6: Wire upload and share transfer**

Emit upload invalidation only when `CloudUploadOutcome.uploaded` is non-empty. Emit transfer invalidation after a share transfer creates or reuses a capacity-consuming personal-drive file; coalesce at the frontend rather than emitting per-byte progress.

- [ ] **Step 7: Run command/event tests and verify GREEN**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_capacity_events -- --nocapture`

Expected: all tests pass.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/cloud/types.rs src-tauri/src/cloud/commands src-tauri/src/lib.rs src-tauri/tests/cloud_capacity_events.rs
git commit -m "feat: emit cloud capacity invalidations"
```

### Task 4: Frontend API, cache and capacity card states

**Files:**
- Modify: `src/lib/cloudTypes.ts`
- Modify: `src/lib/cloudApi.ts`
- Modify: `src/lib/cloudApi.test.ts`
- Create: `src/lib/cloudCapacity.ts`
- Create: `src/lib/cloudCapacity.test.ts`
- Modify: `src/components/CloudManager.tsx`
- Modify: `src/components/CloudManager.test.tsx`
- Modify: `src/styles.css`

**Interfaces:**
- Consumes command: `cloud_account_capacity`.
- Consumes event: `cloud-capacity-invalidated`.
- Produces: `CloudAccountCapacity`, `CloudCapacityInvalidated`, `getCloudAccountCapacity()` and `subscribeCloudCapacityInvalidated()`.
- Produces: in-memory state keyed by `${provider}:${accountId}` with `loading`, `value`, and `refreshFailed`.

- [ ] **Step 1: Write failing API and capacity-state tests**

Assert command argument casing, event payload routing, limited/unlimited formatting, stale-value retention, account replacement cleanup and 400 ms trailing debounce.

```ts
expect(invoke).toHaveBeenCalledWith('cloud_account_capacity', { provider: 'quark' });
expect(formatCloudCapacity(limited)).toBe('已使用 12.0 GB / 100.0 GB');
expect(stateAfterRefreshFailure.value).toEqual(lastSuccessfulValue);
```

- [ ] **Step 2: Run tests and verify RED**

Run: `pnpm vitest run src/lib/cloudApi.test.ts src/lib/cloudCapacity.test.ts --exclude '.worktrees/**'`

Expected: tests fail because types/helpers are missing.

- [ ] **Step 3: Implement typed API and event subscription**

Add the discriminated capacity union and event types, invoke the new command, and provide a subscription wrapper that cleans up Tauri listeners on unmount.

- [ ] **Step 4: Implement the capacity store/controller**

Create a small module responsible for keying results, discarding mismatched responses, retaining last successful values and debouncing invalidations. It must expose deterministic timer injection or use fake-timer-compatible `setTimeout` for tests.

- [ ] **Step 5: Write failing CloudManager rendering tests**

Test loading, limited, unlimited, first failure, stale value plus `更新失败`, login success fetch, logout cleanup and ignored events for another account.

- [ ] **Step 6: Render and style capacity in the account card**

Add capacity beneath the account name/status with an auxiliary text class. Do not make the row interactive or selectable, and do not change existing authorization controls.

- [ ] **Step 7: Run frontend tests and verify GREEN**

Run: `pnpm vitest run src/lib/cloudApi.test.ts src/lib/cloudCapacity.test.ts src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx --exclude '.worktrees/**'`

Expected: all selected tests pass.

- [ ] **Step 8: Commit**

```bash
git add src/lib/cloudTypes.ts src/lib/cloudApi.ts src/lib/cloudApi.test.ts src/lib/cloudCapacity.ts src/lib/cloudCapacity.test.ts src/components/CloudManager.tsx src/components/CloudManager.test.tsx src/styles.css
git commit -m "feat: show cloud account capacity cards"
```

### Task 5: Cross-layer verification and release build

**Files:**
- Modify only if verification exposes a defect in files already listed above.
- Modify: `docs/superpowers/plans/2026-09-15-cloud-account-capacity-cards.md` to mark completed checkboxes.

**Interfaces:**
- Consumes all prior tasks.
- Produces a buildable Windows application with tested capacity cards and refresh behavior.

- [ ] **Step 1: Run Rust capacity and share tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml storage_capacity -- --nocapture`

Run: `cargo test --manifest-path src-tauri/Cargo.toml share_preflight -- --nocapture`

Expected: PASS.

- [ ] **Step 2: Run frontend focused tests**

Run: `pnpm vitest run src/lib/cloudApi.test.ts src/lib/cloudCapacity.test.ts src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx --exclude '.worktrees/**'`

Expected: PASS.

- [ ] **Step 3: Run type and production frontend build**

Run: `pnpm run build`

Expected: TypeScript and Vite production build complete without errors.

- [ ] **Step 4: Run Windows release build**

Run: `pnpm run build:windows`

Expected: release executable is produced successfully.

- [ ] **Step 5: Review diff against the approved spec**

Verify there is no polling, no persisted capacity history, no login-state clearing on capacity errors, no event emission for failed/empty mutations, and no accidental Tianyi re-enable.

- [ ] **Step 6: Commit verification fixes and plan completion**

```bash
git add docs/superpowers/plans/2026-09-15-cloud-account-capacity-cards.md
git commit -m "docs: complete cloud capacity card plan"
```
