# Full aria2, Cloud, and Duplicate-Download Integration Design

## Goal

Integrate the stashed cloud-drive and duplicate-download work into `master` without replacing the merged full aria2 runtime, recovery service, or window lifecycle.

## Decisions

- `src-tauri/src/aria2/` and `EngineRuntime` remain the only process, RPC, recovery, and task-control implementation. The legacy `aria2_rpc.rs` engine is not compiled or started.
- Cloud downloads persist only a stable `cloud://<provider>/<file-id>` source. A signed URL, browser cookie, request headers, and filename are resolved immediately before every aria2 submission and are never stored in `TaskStore`.
- The cloud adapter calls `DownloadService::submit_uri` with explicit per-task options. The service owns aria2 submission, task-state transitions, events, and recovery just as it does for ordinary HTTP downloads.
- Quark resolutions must carry its API/session cookies, desktop user agent, and referer to aria2. A 412 or expired signed URL is recovered by resolving the stable cloud source once more before a retry is marked failed.
- Duplicate detection is based on persisted output-file metadata and `.aria2` control files. Retrying a failed task must return a duplicate-resolution state before invoking aria2 whenever an existing payload could be overwritten.
- A completed existing payload opens its containing directory; a missing completed payload explicitly offers a fresh download. An overwrite requires an explicit resolution action.

## Backend Boundaries

- `cloud/*` owns credentials, provider APIs, download URL resolution, and cloud commands. It does not start aria2 or mutate a task after resolving the cloud file.
- `commands.rs` owns Tauri command orchestration and translates duplicate/refresh outcomes into UI payloads.
- `aria2/service.rs` owns submissions and controls. It gains only typed, per-download URI options needed for cloud headers, output name, directory, proxy, and controlled overwrite behavior.
- `store.rs` and `models.rs` retain task/file persistence. They gain only metadata required to compute duplicate state and preserve a stable cloud source.

## Frontend Boundaries

- `CloudWorkspace` and `cloudApi` remain responsible for cloud authorization and file browsing.
- `DownloadQueue` receives ordinary task payloads only; it surfaces a duplicate resolution dialog through `App` when a task action or cloud submission reports a duplicate state.
- `DuplicateDownloadDialog` presents the three explicit paths: resume, open existing completed files, or confirm restart/overwrite. It never invokes aria2 directly.

## Error Handling

- Cloud resolution errors are classified into authorization-required, expired/retryable link, request-rejected, and provider failure messages. No credentials or signed URLs appear in task records or error text.
- aria2 failures preserve the engine error code and concise provider context. A refresh is attempted only for cloud sources and at most once per task action.
- If a payload exists without an `.aria2` control file, retry stops with `OverwriteRequired`; aria2 is not allowed to truncate it.

## Verification

- Backend tests cover cloud URL/header forwarding, Quark response-cookie forwarding, expired-link refresh, duplicate preflight on retry, and overwrite/resume actions through the full `DownloadService` adapter.
- Frontend tests cover duplicate dialog choices and task-action responses carrying a duplicate state.
- Run scoped red/green tests for each migration, then `vitest run --dir src`, `cargo test`, and `tauri build --no-bundle` for the final Windows executable.
