# Duplicate Download Resolution Design

## Goal

When a user submits content that matches a prior task, reuse safely resumable data first, expose already completed content instead of downloading it again, and require an explicit overwrite confirmation only when reuse is unsafe.

## Task identity

`content_key` is persisted with every task and is the only duplicate lookup key. Magnet links use the lower-cased BTIH (`magnet:<hash>`), cloud downloads use their stable `cloud://provider/file-id` source, and other downloads use an absolute URL with its fragment removed. Tracker lists, display names, and directory names do not affect identity.

## Persisted download evidence

Each task persists a JSON manifest of aria2 payload files: absolute path and expected byte length. The manifest is refreshed from aria2 status while the task is active and is retained after completion. A completed task is considered present only if every manifest file exists and has the expected length. For a legacy record without a manifest, the saved task name must exist under its recorded directory before it is reported as complete.

## Decision flow

1. A new submission looks up the newest task with the same `content_key` before creating a record.
2. An active or queued matching task returns `existing_active`; the UI selects that task and does not create a duplicate.
3. A paused, failed, or cancelled matching task returns `resume_available` only when every known partial file and its aria2 control file exist. Confirming resumes the same task ID with `continue=true`.
4. A completed matching task whose full manifest is present returns `completed_files_present`. The dialog offers only `打开所在目录` and close.
5. A completed task with no manifest files left returns `completed_files_missing`. The dialog explicitly says the task was downloaded before but its files are gone, and offers `重新下载`.
6. A partial manifest, a nonterminal task without resumable control files, or a legacy task whose directory contains data returns `overwrite_required`. The dialog explains that existing files cannot be safely continued and requires `覆盖并重新下载`.
7. Restarting reuses the matching task record in its original configured directory. The aria2 request receives `allow-overwrite=true` only after the user chose the overwrite action; normal starts remain non-destructive.

## Interface and UI

`parse_download` returns either its existing parse outcome or a tagged duplicate-resolution outcome. The React client renders a focused `DuplicateDownloadDialog` and selects the matching queue item. Its only file-system action is the existing `open_task_directory` command, scoped to the matching task. The confirm command accepts `resume`, `restart_missing`, or `overwrite` and returns the task that should replace/select the matching queue item.

## Safety and compatibility

No path is deleted. An overwrite can only happen after an explicit modal confirmation. Existing records without a manifest are never silently treated as completed; they receive the missing-files or overwrite explanation based on whether their directory is empty. The prior task-directory isolation experiment is removed so a confirmed restart uses the user-selected download directory.

## Verification

Rust tests cover content-key normalization, manifest presence states, duplicate decision selection, resume/restart option propagation, and store migrations. React tests cover each dialog branch and opening the known completed task directory. Full Rust and Vitest suites, production frontend build, and Windows bundle build verify the release.
