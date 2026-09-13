# 下载输入验证 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent malformed or unsupported download inputs from creating or submitting tasks, while giving mixed pasted input an ordered confirm-or-skip workflow.

**Architecture:** A Rust parser returns explicit valid or invalid input candidates and owns safe normalization. Creation, confirmation, retry, and engine submission revalidate stable task sources. The Tauri IPC exposes a scan result that React uses to drive an ordered dialog without creating tasks during the scan.

**Tech Stack:** Rust, Tauri IPC, url/regex crates, React 18, TypeScript, Vitest.

**Spec:** `docs/superpowers/specs/2026-09-14-input-validation-design.md`

## Global Constraints

- Invalid input must not create a task or invoke aria2.
- Do not decode, re-encode, or expose query strings, signed URLs, access codes, FTP passwords, magnet hashes, or RPC tokens.
- Keep IPC/RPC strings UTF-8; only normalize a queryless Unicode HTTP(S)/FTP path.
- Run only targeted Rust/Vitest tests; every Vitest command includes `--exclude '.worktrees/**'`.
- Run `npm run build`; run `scripts/build-windows.ps1` as the requested release build check.

---

### Task 1: Parse and validate every user-facing source

**Files:**
- Modify: `src-tauri/src/parser.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: `src-tauri/src/parser.rs`

**Interfaces:**
- Produces `validate_download_input(input: &str) -> Result<ValidatedCandidate, InputValidationError>`.
- Produces `scan_download_inputs(text: &str) -> Vec<ScannedInput>` where each item is either a valid source or an error and retains occurrence order.

- [ ] **Step 1: Write failing parser tests**

```rust
#[test]
fn rejects_a_short_btih_without_returning_a_magnet_candidate() {
    assert_eq!(
        validate_download_input("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef0123")
            .unwrap_err()
            .to_string(),
        "磁链格式无效"
    );
}

#[test]
fn rejects_a_registered_share_host_without_a_share_id() {
    assert_eq!(
        scan_download_inputs("https://pan.baidu.com/s/")[0].error_message(),
        Some("网盘分享链接格式无效")
    );
}
```

- [ ] **Step 2: Run the parser test target and verify RED**

Run: `cargo test --lib parser::tests::rejects_a_short_btih_without_returning_a_magnet_candidate`

Expected: FAIL because the unified validator does not exist.

- [ ] **Step 3: Implement the smallest validator**

```rust
pub fn validate_download_input(input: &str) -> Result<ValidatedCandidate, InputValidationError> {
    let raw = input.trim();
    let candidate = classify_and_normalize(raw)?;
    candidate.ensure_supported()
}
```

Add strict `btih`, ED2K, URL, FTP, torrent and Thunder-target validation; retain raw query values and require a non-empty, single-segment registered share ID.

- [ ] **Step 4: Run the parser test target and verify GREEN**

Run: `cargo test --lib parser::tests -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/parser.rs src-tauri/src/lib.rs
git commit -m "feat: validate download input candidates"
```

### Task 2: Enforce validation before task creation and all local submissions

**Files:**
- Modify: `src-tauri/src/downloader.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/aria2/service.rs`
- Test: `src-tauri/src/downloader.rs`
- Test: `src-tauri/src/commands.rs`

**Interfaces:**
- `prepare_download` consumes a `ValidatedCandidate` and is fallible before `TaskStore::create`.
- `validate_task_submission(task: &DownloadTask)` rejects durable local tasks with invalid sources before `addUri`.

- [ ] **Step 1: Write failing zero-side-effect tests**

```rust
#[test]
fn malformed_magnet_creates_no_task() {
    let store = TaskStore::open_in_memory().unwrap();
    assert!(prepare_download("magnet:?xt=urn:btih:short", &store).is_err());
    assert!(store.list().unwrap().is_empty());
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test --lib downloader::tests::malformed_magnet_creates_no_task`

Expected: FAIL because `prepare_download` accepts any `magnet:` prefix.

- [ ] **Step 3: Implement creation, confirm, retry and service defenses**

```rust
let validated = validate_download_input(input)?;
let source = validated.source().to_owned();
```

Use the same validator before `confirm_download_command`, `resubmit_task`, `submit_uri`, `resubmit_uri`, and metadata magnet submission. Mark an existing parsing task failed if a late validation or engine rejection occurs.

- [ ] **Step 4: Run targeted Rust tests and verify GREEN**

Run: `cargo test --lib downloader::tests::malformed_magnet_creates_no_task commands::tests -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/downloader.rs src-tauri/src/commands.rs src-tauri/src/aria2/service.rs
git commit -m "fix: guard download submission with input validation"
```

### Task 3: Expose a non-mutating scan endpoint and safe engine errors

**Files:**
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/aria2/types.rs`
- Modify: `src/lib/api.ts`
- Modify: `src/lib/types.ts`
- Test: `src-tauri/src/commands.rs`
- Test: `src/lib/api.test.ts`

**Interfaces:**
- Adds `scan_download_inputs(input: String) -> ScanDownloadOutcome` Tauri command.
- Adds `ScanDownloadResult` to the TypeScript API contract.
- Maps only URI/magnet parameter engine errors to `下载引擎拒绝该链接：链接格式无效`; generic 400 maps to `下载引擎拒绝请求（HTTP 400）`.

- [ ] **Step 1: Write failing contract tests**

```typescript
it('preserves an invalid batch item instead of invoking parseDownload', async () => {
  expect(await scanDownloadInputs('magnet:?xt=urn:btih:short')).toEqual([
    { valid: false, message: '磁链格式无效' }
  ]);
});
```

- [ ] **Step 2: Run the focused contract test and verify RED**

Run: `npm run test:run -- src/lib/api.test.ts --exclude '.worktrees/**'`

Expected: FAIL because the API has no scan call.

- [ ] **Step 3: Implement the IPC projection and error mapper**

```ts
export async function scanDownloadInputs(input: string): Promise<ScanDownloadResult[]> {
  return invoke<ScanDownloadResult[]>('scan_download_inputs', { input });
}
```

The projection contains only `valid`, `input`, `kind`, and `message`; it never exposes source secrets in errors.

- [ ] **Step 4: Run focused Rust and TypeScript contract tests and verify GREEN**

Run: `npm run test:run -- src/lib/api.test.ts --exclude '.worktrees/**'`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/aria2/types.rs src/lib/api.ts src/lib/types.ts src/lib/api.test.ts
git commit -m "feat: scan download inputs before task creation"
```

### Task 4: Add ordered mixed-input confirmation to the new-download dialog

**Files:**
- Modify: `src/components/NewDownloadDialog.tsx`
- Modify: `src/App.tsx`
- Modify: `src/components/ParseConfirmDialog.tsx`
- Test: `src/components/NewDownloadDialog.test.tsx`
- Test: `src/App.test.tsx`

**Interfaces:**
- Dialog consumes `onScan(input)` and processes scan entries serially.
- Valid batch entries call `onParse(entry.input, ...)` only after the user confirms the candidate; invalid entries expose `skip` and cannot call `onParse`. Existing magnet/torrent/share resource confirmations continue through the app-level confirmation flow after that candidate confirmation.

- [ ] **Step 1: Write failing dialog tests**

```tsx
it('shows an invalid second candidate as 2/2 without submitting it', async () => {
  render(<NewDownloadDialog open onScan={async () => [valid, invalid]} onParse={onParse} onClose={vi.fn()} />);
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));
  await user.click(screen.getByRole('button', { name: /确认并继续/ }));
  expect(screen.getByText('正在处理 2/2')).toBeVisible();
  expect(onParse).toHaveBeenCalledTimes(1);
});
```

- [ ] **Step 2: Run the dialog test and verify RED**

Run: `npm run test:run -- src/components/NewDownloadDialog.test.tsx --exclude '.worktrees/**'`

Expected: FAIL because the dialog has no scan session or skip action.

- [ ] **Step 3: Implement serial session state**

Keep a `ScanDownloadResult[]`, current index and completed state in the dialog. Reuse the existing confirmation view for valid entries. On cancel, close without advancing and retain already-created tasks. Keep a single valid direct link on the immediate parse route.

- [ ] **Step 4: Run focused UI tests and verify GREEN**

Run: `npm run test:run -- src/components/NewDownloadDialog.test.tsx src/App.test.tsx --exclude '.worktrees/**'`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/components/NewDownloadDialog.tsx src/components/NewDownloadDialog.test.tsx src/components/ParseConfirmDialog.tsx src/App.tsx src/App.test.tsx
git commit -m "feat: confirm mixed download input sequentially"
```

### Task 5: Verify the completed change at the required scope

**Files:**
- Modify: none

- [ ] **Step 1: Run relevant Rust targets**

Run: `cargo test --lib parser::tests downloader::tests commands::tests -- --nocapture`

Expected: PASS.

- [ ] **Step 2: Run relevant frontend targets**

Run: `npm run test:run -- src/lib/api.test.ts src/components/NewDownloadDialog.test.tsx src/App.test.tsx --exclude '.worktrees/**'`

Expected: PASS.

- [ ] **Step 3: Build the frontend**

Run: `npm run build`

Expected: PASS.

- [ ] **Step 4: Build the Windows release binary**

Run: `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows.ps1`

Expected: PASS.
