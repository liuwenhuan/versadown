# UC Personal Share 41026 Implementation Plan

> **For Codex:** Execute the tasks in order. Do not commit or merge this
> worktree: it already contains user-owned uncommitted share-management work.

**Goal:** Preserve UC's create-task `share_id` instead of replacing it with the
password-detail `pwd_id`, then prove the behaviour against a user-authorized
account with an isolated create/list/cancel/cleanup lifecycle.

**Architecture:** UC's asynchronous create task returns the management
`share_id`; the subsequent password-detail endpoint can return only a distinct
`pwd_id` for the public link. Keep the task identifier in `CloudShare.id`, since
the list and cancel APIs operate on it. Exercise the production native driver
through a deliberately opt-in integration test that only touches a newly
generated test directory.

**Tech Stack:** Rust, Tauri native cloud driver, reqwest, existing DPAPI
credential store, `cargo test`, Windows release build script.

## Global constraints

- Do not print credential bytes, cookie values, share URLs, or share IDs.
- Do not select, change, or remove any pre-existing user files or shares.
- The live test must use `ALLDOWNLOADER_LIVE_CLOUD=1`, create only an
  `AllDownloader-E2E-uc-*` directory, cancel only the share it created, and
  permanently clear only that test fixture from the recycle bin.
- Keep unrelated worktree edits intact. This narrowly changes UC personal-share
  identity handling and its tests; no frontend test suite is needed.

### Task 1: Add the real UC personal-share lifecycle regression

**Files:**
- Modify: `src-tauri/tests/live_aliyun_uc.rs`

1. Import `CloudShareOptions`.
2. Add an ignored `live_uc_personal_share_lifecycle` test that requires the
   existing opt-in environment flag and decrypted UC credential.
3. Create an isolated root and child folder; invoke the production UC driver's
   `create_share` for only that root; require a nonempty opaque ID and an HTTPS
   URL without logging either.
4. List shares and require the returned ID; cancel exactly that ID and require
   the server's successful acknowledgement. UC retains revoked links in the
   history list, so absence from that list is not a cancellation criterion.
   Always trash and permanently purge just the isolated root whether the share
   operation succeeds or fails.
5. Run the exact ignored test once before the production fix to reproduce the
   current live behavior and record only redacted transport observations.

### Task 2: Lock the UC share-identifier contract with a focused unit test

**Files:**
- Modify: `src-tauri/tests/cloud_uc.rs`

1. Add a create-share response fixture where the completed task supplies
   `share_id` and the password-detail response supplies only a different
   `pwd_id`.
2. Require the resulting `CloudShare.id` to equal the task `share_id`, while
   retaining the password-detail response's URL.
3. Run the focused library test and confirm it fails before the implementation
   change.

### Task 3: Make the minimal UC personal-share fix

**Files:**
- Modify: `src-tauri/src/cloud/uc.rs`

1. After parsing password details, overwrite `CloudShare.id` with the
   completed task's `share_id`.
2. Add a short code comment explaining why `pwd_id` must not replace the
   management identifier.
3. Run the focused UC tests until green.

### Task 4: Verify the user's account and build the changed worktree

1. Run the isolated live lifecycle test again using the existing encrypted UC
   credential and require create, list, server cancellation acknowledgement,
   and cleanup to succeed.
2. Once the lifecycle passes, run `scripts/build-windows.ps1` from this
   worktree. This is a release build check, not a replacement for the focused
   test.
3. Report only the commands that actually ran and whether the real share was
   created, listed, cancelled, and cleaned up.
