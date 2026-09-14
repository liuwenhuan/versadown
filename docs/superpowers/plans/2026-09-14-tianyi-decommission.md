# Tianyi Decommission Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Temporarily remove Tianyi Cloud from the product and purge application-owned Tianyi state without touching unrelated downloads.

**Architecture:** Keep the Tianyi enum, descriptor and connector source for serialization compatibility, but make the registry catalog and share matcher expose only enabled providers and classify disabled Tianyi share URLs as unsupported. A startup decommissioner runs before the download engine, preflights its profile and exact task-owned paths, removes safe payloads and durable Tianyi records, then clears Tianyi credentials and browser state.

**Tech Stack:** Rust, SQLite/rusqlite task store, Tauri, React/Vitest.

**Spec:** `docs/superpowers/specs/2026-09-14-tianyi-decommission-design.md`

## Global Constraints

- Never recursively remove a download directory; delete only recorded absolute file paths and their `.aria2` sidecars.
- Tianyi links must be rejected rather than treated as direct downloads.
- Any failed safety preflight retains the affected Tianyi task and its credentials for a later retry.
- Do not delete non-Tianyi task, file, credential, or share-plan data.

---

### Task 1: Disable catalog and share recognition

**Files:**
- Modify: `src-tauri/src/cloud/types.rs`
- Modify: `src-tauri/src/parser.rs`
- Test: `src-tauri/src/parser.rs`

**Interfaces:**
- Produces `CloudProvider::is_enabled()` for registry/catalog exposure while retaining `CloudProvider::ALL` and the internal descriptor for deserialization-compatible metadata.
- Produces disabled-share rejection through `validate_download_input` and `extract_cloud_share_candidate`.

- [ ] **Step 1: Write the failing tests**

```rust
assert!(CloudProvider::ACTIVE.iter().all(|provider| *provider != CloudProvider::Tianyi));
assert_eq!(
    validate_download_input("https://cloud.189.cn/t/private-share"),
    Err(InputValidationError::InvalidCloudShare)
);
assert_eq!(
    extract_cloud_share_candidate("https://cloud.189.cn/t/private-share", None),
    Err(ShareInputError::InvalidShare)
);
```

- [ ] **Step 2: Run the parser tests to verify they fail**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml --lib parser::tests`

Expected: FAIL because Tianyi is still active and its link resolves as a cloud share.

- [ ] **Step 3: Implement the smallest active-provider and disabled-host checks**

```rust
pub fn is_enabled(self) -> bool {
    self != Self::Tianyi
}
```

Filter `catalog` and `for_share_url` with `is_enabled`; retain `registered_share_provider`'s full descriptor view so a disabled Tianyi URL is rejected before generic HTTP classification.

- [ ] **Step 4: Run the parser tests to verify they pass**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml --lib parser::tests`

Expected: PASS.

### Task 2: Add a safe Tianyi state decommissioner

**Files:**
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/cloud/commands.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: `src-tauri/src/commands.rs`

**Interfaces:**
- Produces `purge_tianyi_local_state(store, cloud_state) -> Result<TianyiLocalStatePurgeReport, String>`.
- Consumes task sources prefixed by `cloud://tianyi/` or `cloud-share://tianyi/`, and directory groups whose provider is `Tianyi`.

- [ ] **Step 1: Write a failing isolated cleanup test**

```rust
let report = purge_tianyi_local_state(&store, &cloud).unwrap();
assert_eq!(report.removed_tasks, 1);
assert!(!tianyi_payload.exists());
assert!(store.find(baidu.id).is_ok());
assert!(baidu_payload.exists());
```

The fixture creates an absolute Tianyi payload and task, a Tianyi share plan, a Tianyi credential key, and a separate Baidu task/payload.

- [ ] **Step 2: Run the exact test to verify it fails**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml --lib tianyi_local_state`

Expected: FAIL because no provider-specific cleanup exists.

- [ ] **Step 3: Implement preflight then deletion**

```rust
let tasks = store.list()?.into_iter().filter(is_tianyi_task).collect::<Vec<_>>();
cloud.preflight_tianyi_persisted_state()?;
validate_tianyi_payload_paths(store, &tasks)?;
remove_tianyi_payload_paths(store, &tasks)?;
store.remove_task_records(&task_ids)?;
clear_tianyi_credentials_and_profiles(cloud)?;
```

Reuse the existing per-file deletion policy; keep all state when preflight cannot prove a file path safe. Delete share-plan links before deleting task records and remove Tianyi directory groups after their linked tasks are removed.

- [ ] **Step 4: Run the exact test to verify it passes**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml --lib tianyi_local_state`

Expected: PASS.

- [ ] **Step 5: Run the cleanup before engine initialization**

```rust
let cloud = Arc::new(CloudState::new(data_dir.join("cloud-sessions"))?);
cloud::commands::purge_tianyi_local_state(&store, &cloud)?;
```

Place it after state creation and before `runtime.initialize()`.

### Task 3: Verify public behavior and Windows build

**Files:**
- Test: `src/lib/cloudApi.test.ts`

- [ ] **Step 1: Add a catalog-facing regression test**

```ts
expect(await getCloudProviderCatalog()).not.toContainEqual(expect.objectContaining({ provider: 'tianyi' }));
```

- [ ] **Step 2: Run focused tests**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml --lib parser::tests tianyi_local_state`

Run: `pnpm test --run src/lib/cloudApi.test.ts --exclude '.worktrees/**'`

Expected: PASS.

- [ ] **Step 3: Build release artifacts**

Run: `pnpm run build`

Run: `scripts/build-windows.ps1`

Expected: both commands exit 0.
