# Batch New Download Dialog Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace sequential paste confirmation with one immediately opened, concurrently prepared batch dialog and persist one shared “解压密码和后处理” note on every selected top-level task.

**Architecture:** Reuse `parse_download` in forced-confirmation mode as the prepare boundary, add guarded draft/plan cleanup, and place the four-worker batch state in a focused React hook. A single mixed batch IPC command owns native and cloud submissions after the dialog closes and streams each created top-level task through a channel. Extend SQLite and queue payloads with an optional local-only post-processing note.

**Tech Stack:** React 18, TypeScript, Vitest/Testing Library, Tauri 2 IPC, Rust, rusqlite, aria2.

**Spec:** `docs/superpowers/specs/2026-09-15-batch-new-download-dialog-design.md`

## Global Constraints

- Pasting any non-empty download text opens the unified dialog immediately; no intermediate valid-link confirmation UI.
- Prepare at most 4 candidates concurrently and never start a normal download before submission.
- Submit is enabled only when every selected item is ready; parsing items can be deselected.
- Copy the shared note only to selected top-level tasks and never expose it to logs, errors, telemetry, or debug reports.
- Directory, BT, magnet, and cloud-share resources are expandable and selectable.
- Run related frontend/backend tests plus `pnpm run build`; do not run full suites.

---

### Task 1: Persist the post-processing note

**Files:**
- Modify: `src-tauri/src/models.rs`
- Modify: `src-tauri/src/store.rs`
- Modify: `src/lib/types.ts`
- Modify: `src/components/TaskDetails.tsx`
- Modify: `src/styles.css`
- Test: `src-tauri/src/store.rs`
- Test: `src/components/TaskDetails.test.tsx`

**Interfaces:**
- Produces: `DownloadTask.post_processing_note: Option<String>` and `QueueTask.post_processing_note?: string`.
- Produces: `TaskStore::set_post_processing_note(id: Uuid, note: Option<&str>) -> Result<DownloadTask, StoreError>`.

- [ ] **Step 1: Write failing persistence and detail tests**

```tsx
render(<TaskDetails task={{ ...task, post_processing_note: '密码：1234\n完成后拼接分卷' }} />);
expect(screen.getByText('解压密码和后处理')).toBeInTheDocument();
expect(screen.getByText(/完成后拼接分卷/)).toBeInTheDocument();
```

The Rust test creates a task, sets the exact multiline Unicode note, reads it back, then sets whitespace and expects `None`.

- [ ] **Step 2: Run tests and verify failure**

```powershell
.\node_modules\.bin\vitest.CMD run TaskDetails.test.tsx --dir src/components --exclude '.worktrees/**' --pool=forks --maxWorkers=1 --no-file-parallelism
$cargo=(Get-Command cargo -ErrorAction SilentlyContinue).Source; if(-not $cargo){$cargo='C:\Users\deepin\.cargo\bin\cargo.exe'}; & $cargo test --manifest-path src-tauri/Cargo.toml post_processing_note -- --nocapture
```

- [ ] **Step 3: Add model, migration, query, and setter**

Add nullable `post_processing_note TEXT`, increment the schema version, append it to `TASK_SELECT`/`task_from_row`, initialize new tasks with `None`, and implement:

```rust
pub fn set_post_processing_note(&self, id: Uuid, note: Option<&str>) -> Result<DownloadTask, StoreError> {
    let note = note.map(str::trim).filter(|value| !value.is_empty());
    let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
    let changed = connection.execute(
        "UPDATE tasks SET post_processing_note=?1, updated_at=?2 WHERE id=?3",
        params![note, current_timestamp(), id.to_string()],
    )?;
    if changed == 0 { return Err(StoreError::NotFound(id)); }
    find_with_connection(&connection, id)
}
```

- [ ] **Step 4: Render the non-empty note**

```tsx
{task.post_processing_note?.trim() && <section className="task-post-processing-note">
  <b>解压密码和后处理</b><p>{task.post_processing_note}</p>
</section>}
```

Style the paragraph with `white-space: pre-wrap`.

- [ ] **Step 5: Run the tests and commit**

Run Step 2 commands; expect PASS.

```powershell
git add src-tauri/src/models.rs src-tauri/src/store.rs src/lib/types.ts src/components/TaskDetails.tsx src/components/TaskDetails.test.tsx src/styles.css
git commit -m "feat: persist download post-processing notes"
```

### Task 2: Make preparation explicit and discardable

**Files:**
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/cloud/share/repository.rs`
- Modify: `src/lib/api.ts`
- Modify: `src/lib/api.test.ts`
- Test: `src-tauri/src/commands.rs`

**Interfaces:**
- Produces: `prepareDownload(input, accessCode?)` which calls `parse_download` with `requireConfirmation: true`.
- Produces: `discardPreparedDownload(reference)` and Tauri `discard_prepared_download` for either a native draft UUID or a `cloud-share://` plan source.
- Produces: `CloudSharePlanStore::discard_plan(plan_source)` that refuses to discard plans already linked to local tasks.

- [ ] **Step 1: Write failing API and command tests**

```ts
await prepareDownload('https://example.test/file.bin');
expect(native.invoke).toHaveBeenCalledWith('parse_download', {
  input: 'https://example.test/file.bin', requireConfirmation: true,
});
await discardPreparedDownload('task-1');
expect(native.invoke).toHaveBeenLastCalledWith('discard_prepared_download', { taskId: 'task-1' });
```

Rust tests prove an awaiting-user task is removed and a queued task is rejected.

- [ ] **Step 2: Run tests and verify failure**

```powershell
.\node_modules\.bin\vitest.CMD run api.test.ts --dir src/lib --exclude '.worktrees/**' --pool=forks --maxWorkers=1 --no-file-parallelism
$cargo=(Get-Command cargo -ErrorAction SilentlyContinue).Source; if(-not $cargo){$cargo='C:\Users\deepin\.cargo\bin\cargo.exe'}; & $cargo test --manifest-path src-tauri/Cargo.toml discard_prepared -- --nocapture
```

- [ ] **Step 3: Implement wrappers and guarded cleanup**

```ts
export const prepareDownload = (input: string, accessCode?: string) => parseDownload(input, accessCode, true);
export async function discardPreparedDownload(taskId: string) {
  if (isTauriRuntime()) await invoke('discard_prepared_download', { taskId });
}
```

The Rust command branches by reference type. For UUIDs it loads the task, rejects non-preparation states, removes a paused bound aria2 job when present, then deletes the task row. For `cloud-share://` values it calls `discard_plan`, which deletes only an unlinked prepared plan. Register it in `src-tauri/src/lib.rs`.

- [ ] **Step 4: Run tests and commit**

Run Step 2 commands; expect PASS.

```powershell
git add src-tauri/src/commands.rs src-tauri/src/lib.rs src-tauri/src/cloud/share/repository.rs src/lib/api.ts src/lib/api.test.ts
git commit -m "feat: prepare and discard download drafts"
```

### Task 3: Add the four-worker batch state hook

**Files:**
- Create: `src/components/useDownloadDraftBatch.ts`
- Create: `src/components/useDownloadDraftBatch.test.tsx`
- Modify: `src/lib/types.ts`

**Interfaces:**
- Produces: `DownloadDraftItem` with `id`, `input`, `kind`, `status`, `selected`, `result`, `error`, `accessCode`, `expanded`, and `selectedResourceIds`.
- Produces: `useDownloadDraftBatch({ scan, prepare, discard, concurrency: 4 })` with `start`, `retry`, `remove`, selection methods, `discardAll`, `markSubmitted`, `items`, and `canSubmit`.

- [ ] **Step 1: Write failing hook tests**

Use deferred promises to assert maximum concurrency is 4, a fifth starts after completion, items remain in scan order, parsing items may be deselected, access-code results stay on the item, retry generation ignores stale results, and close discards only native unsubmitted drafts.

```ts
expect(maxConcurrent).toBe(4);
act(() => result.current.toggleItem('item-5'));
expect(result.current.canSubmit).toBe(true);
```

- [ ] **Step 2: Run the test and verify failure**

```powershell
.\node_modules\.bin\vitest.CMD run useDownloadDraftBatch.test.tsx --dir src/components --exclude '.worktrees/**' --pool=forks --maxWorkers=1 --no-file-parallelism
```

- [ ] **Step 3: Implement reducer, workers, selection, and cleanup**

`start(text)` scans once, creates all rows immediately, and feeds valid entries to four promise workers. Every completion dispatch carries `{id, generation}`. `canSubmit` requires at least one selected item and all selected statuses equal `ready`. Resource changes immutably update `ParseResult.resources`; cleanup ignores submitted items.

- [ ] **Step 4: Run the test and commit**

Run Step 2; expect PASS.

```powershell
git add src/components/useDownloadDraftBatch.ts src/components/useDownloadDraftBatch.test.tsx src/lib/types.ts
git commit -m "feat: orchestrate concurrent download drafts"
```

### Task 4: Replace the sequential dialog with batch UI

**Files:**
- Modify: `src/components/NewDownloadDialog.tsx`
- Modify: `src/components/NewDownloadDialog.test.tsx`
- Modify: `src/styles.css`

**Interfaces:**
- Consumes: Task 3 hook.
- Produces: `onSubmitBatch(items, destinationDirectory, privateDownload, postProcessingNote)`.
- Produces: `downloadDirectory` and `browseDirectory` props matching the existing confirmation footer.

- [ ] **Step 1: Write failing interaction tests**

Cover immediate rows, “任务解析中”, parallel results preserving order, top-level selection, expandable children, inline access-code retry, item retry/remove, selected-parsing submit blocking, and deselect-to-unblock. Add:

```tsx
await user.click(screen.getByRole('button', { name: '解压密码和后处理' }));
await user.type(screen.getByRole('textbox', { name: '解压密码和后处理' }), '密码 88in\n完成后拼接');
await user.click(screen.getByRole('button', { name: '新建任务' }));
expect(onSubmitBatch).toHaveBeenCalledWith(expect.any(Array), undefined, false, '密码 88in\n完成后拼接');
```

- [ ] **Step 2: Run the test and verify failure**

```powershell
.\node_modules\.bin\vitest.CMD run NewDownloadDialog.test.tsx --dir src/components --exclude '.worktrees/**' --pool=forks --maxWorkers=1 --no-file-parallelism
```

- [ ] **Step 3: Implement list, resource tree, shared note, and footer**

Remove sequential `DownloadBatch.index`. Render rows in scan order, child checkboxes under expand controls, row-local access-code/errors, a default-collapsed shared textarea, destination/privacy controls, and one `新建任务` button governed by `canSubmit`.

- [ ] **Step 4: Implement focus and cleanup behavior**

Escape/backdrop/close awaits `discardAll` before `onClose`; submitting first calls `markSubmitted`. Retain focus restoration/trapping and include all new controls.

- [ ] **Step 5: Run the test and commit**

Run Step 2; expect PASS.

```powershell
git add src/components/NewDownloadDialog.tsx src/components/NewDownloadDialog.test.tsx src/styles.css
git commit -m "feat: show concurrent batch new download dialog"
```

### Task 5: Add the backend batch command and integrate all input routes

**Files:**
- Modify: `src/App.tsx`
- Modify: `src/App.test.tsx`
- Modify: `src/lib/api.ts`
- Modify: `src/lib/api.test.ts`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces: `submitPreparedDownloadBatch(items, destinationDirectory, privateDownload, postProcessingNote, onCreated)`.
- Produces: Tauri `submit_prepared_download_batch` accepting tagged `native` and `cloud_share` items and returning per-item outcomes/failures.

- [ ] **Step 1: Write failing App/API tests**

Assert single/multiline paste and Enter open the one batch dialog, multiple dropped torrents enter one batch, the batch IPC includes `postProcessingNote`, and App closes/navigates on the first `onCreated` channel event without waiting for the complete command result.

- [ ] **Step 2: Run tests and verify failure**

```powershell
.\node_modules\.bin\vitest.CMD run App.test.tsx api.test.ts --dir src --exclude '.worktrees/**' --pool=forks --maxWorkers=1 --no-file-parallelism
```

- [ ] **Step 3: Implement the mixed backend batch command**

Add the tagged request type:

```rust
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreparedBatchItem {
    Native { task_id: String, selected_ids: Vec<String> },
    CloudShare { plan_source: String, selected_ids: Vec<String> },
}
```

`submit_prepared_download_batch` validates common destination/privacy settings once, processes every selected item, sends each created top-level `QueueTaskPayload` through `on_created`, and accumulates an outcome or `{item_key, code, message}` failure without aborting siblings. Native submission stores the note before queueing. Cloud submission stores it on every selected top-level file task or directory root immediately after creation; blank input becomes `NULL`, children do not duplicate it. A cloud failure after preparation creates or retains a named failed top-level task for the Failed tab and retry action.

- [ ] **Step 4: Add the TypeScript batch IPC wrapper**

Create `PreparedBatchSubmissionItem` and one `submitPreparedDownloadBatch` wrapper that attaches `Channel<QueueTask>` for `onCreated`, maps native results through `duplicateResult`, and sends the shared note once at batch level.

- [ ] **Step 5: Route every input into the unified dialog**

Search paste/Enter, launcher parse, modal new-download, and torrent drops set a shared `newDownloadInput` and open the dialog. Remove sequential dropped-torrent parsing and paste confirmation handoffs; keep `ParseConfirmDialog` only for reopening legacy awaiting-user tasks.

- [ ] **Step 6: Close and navigate on the first created-task event**

Copy selected items and invoke the single batch command. On the first `onCreated` event, mark dialog items submitted, close the dialog, switch to `downloads/active`, select/scroll to that task, and upsert later channel events without reopening the window. When the command resolves, enqueue duplicates and surface accumulated failures through the existing notice while failed top-level tasks remain retryable in the Failed tab.

- [ ] **Step 7: Run related tests and commit**

```powershell
.\node_modules\.bin\vitest.CMD run App.test.tsx api.test.ts NewDownloadDialog.test.tsx TaskDetails.test.tsx useDownloadDraftBatch.test.tsx --dir src --exclude '.worktrees/**' --pool=forks --maxWorkers=1 --no-file-parallelism
$cargo=(Get-Command cargo -ErrorAction SilentlyContinue).Source; if(-not $cargo){$cargo='C:\Users\deepin\.cargo\bin\cargo.exe'}; & $cargo test --manifest-path src-tauri/Cargo.toml post_processing_note -- --nocapture
git add src/App.tsx src/App.test.tsx src/lib/api.ts src/lib/api.test.ts src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat: submit pasted downloads as one batch"
```

### Task 6: Focused regression and production frontend build

**Files:**
- Verify only.

**Interfaces:**
- Consumes: Tasks 1-5.
- Produces: focused test/build evidence.

- [ ] **Step 1: Run focused frontend tests**

```powershell
.\node_modules\.bin\vitest.CMD run App.test.tsx NewDownloadDialog.test.tsx useDownloadDraftBatch.test.tsx ParseConfirmDialog.test.tsx TaskDetails.test.tsx api.test.ts --dir src --exclude '.worktrees/**' --pool=forks --maxWorkers=1 --no-file-parallelism
```

- [ ] **Step 2: Run focused Rust tests**

```powershell
$cargo=(Get-Command cargo -ErrorAction SilentlyContinue).Source; if(-not $cargo){$cargo='C:\Users\deepin\.cargo\bin\cargo.exe'}; & $cargo test --manifest-path src-tauri/Cargo.toml discard_prepared -- --nocapture
$cargo=(Get-Command cargo -ErrorAction SilentlyContinue).Source; if(-not $cargo){$cargo='C:\Users\deepin\.cargo\bin\cargo.exe'}; & $cargo test --manifest-path src-tauri/Cargo.toml post_processing_note -- --nocapture
```

- [ ] **Step 3: Build frontend and inspect status**

```powershell
pnpm run build
git diff --check
git status --short
```

Expected: focused tests and frontend build pass; no Windows binary is built unless requested later.
