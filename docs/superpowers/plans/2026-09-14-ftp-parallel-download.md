# FTP 并发分片下载 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (\`- [ ]\`) syntax for tracking.

**Goal:** 让 FTP 下载按现有设置限制并发任务和单任务数据连接，并在服务器不支持安全分片时自动降级为单连接续传。

**Architecture:** \`FtpDownloadEngine\` 增加任务调度与每任务分片工作器。每个可分片任务将范围进度保存到 SQLite，工作器聚合这些范围进度为既有下载任务快照；无法确认 \`SIZE\`/非零 \`REST\` 时保留当前单流实现。

**Tech Stack:** Rust、Tokio、suppaftp、rusqlite、Tauri async runtime、受控本地 FTP 集成测试。

**Spec:** \`docs/superpowers/specs/2026-09-14-ftp-parallel-download-design.md\`

## Global Constraints

- FTP 继续使用进程内 Rust 引擎，不能提交给 aria2-next。
- \`max_concurrent_downloads\` 是 FTP 活跃任务上限；\`max_connections_per_server\` 是单任务连接上限；默认值为 5 和 8。
- 分片数为 \`min(max_connections_per_server, ceil(total / 4 MiB))\`，至少为 1。
- 无 \`SIZE\`、一个分片、非零 \`REST\` 被拒绝或分片初始化失败时，必须无感单流降级。
- 分片写入后失败必须保留确认进度、使任务可重试，不能改为覆盖式单流。
- 不泄露 FTP 凭据；HTTP/HTTPS、前端 UI 不改动。
- 只运行下列定向 Rust 测试和 \`scripts/build-windows.ps1\`，不运行全量测试。

---

## File Structure

- \`src-tauri/src/models.rs\`：\`FtpSegment\` 值对象。
- \`src-tauri/src/store.rs\`：\`ftp_segments\` 迁移和读写 API。
- \`src-tauri/src/ftp.rs\`：队列、分片、聚合、降级和恢复。
- \`src-tauri/src/engine_runtime.rs\`：将 aria2 设置同步给 FTP。
- \`src-tauri/tests/ftp_download.rs\`：端到端 FTP 行为。
- \`src-tauri/tests/window_config.rs\`：设置传播验证。

### Task 1: Durable segment state

**Files:**
- Modify: \`src-tauri/src/models.rs:359-421\`
- Modify: \`src-tauri/src/store.rs:178-196,1007-1078,1153-1232\`
- Test: \`src-tauri/tests/ftp_download.rs\`

**Interfaces:**
- Produces \`pub struct FtpSegment { pub index: u32, pub start_offset: u64, pub end_offset: u64, pub completed_length: u64 }\`, with an exclusive end offset.
- Produces \`TaskStore::{replace_ftp_segments, ftp_segments, update_ftp_segment, clear_ftp_segments}\`.

- [ ] **Step 1: Write the failing storage test**

\`\`\`rust
#[test]
fn ftp_segments_are_ordered_updated_and_deleted() {
    let store = TaskStore::open_in_memory().unwrap();
    let task = store.create(NewTask { source: "ftp://example.test/a.bin".into(), name: "a.bin".into(), kind: "ftp".into(), is_private: false }).unwrap();
    store.replace_ftp_segments(task.id, &[
        FtpSegment { index: 1, start_offset: 8, end_offset: 16, completed_length: 0 },
        FtpSegment { index: 0, start_offset: 0, end_offset: 8, completed_length: 3 },
    ]).unwrap();
    store.update_ftp_segment(task.id, 1, 4).unwrap();
    assert_eq!(store.ftp_segments(task.id).unwrap()[0].index, 0);
    store.remove(task.id).unwrap();
    assert!(store.ftp_segments(task.id).unwrap().is_empty());
}
\`\`\`

- [ ] **Step 2: Run it before implementation**

Run: \`cargo test --manifest-path src-tauri/Cargo.toml --test ftp_download ftp_segments_are_ordered_updated_and_deleted\`

Expected: compile failure because the state type and methods are absent.

- [ ] **Step 3: Implement the model, schema and transactions**

\`\`\`rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FtpSegment {
    pub index: u32,
    pub start_offset: u64,
    pub end_offset: u64,
    pub completed_length: u64,
}
\`\`\`

Add an \`ftp_segments(task_id, segment_index, start_offset, end_offset, completed_length)\` table with foreign-key cascade in migration version 10. Replacement deletes prior rows in the same transaction, then inserts validated rows. Reject invalid/overflowed ranges; reads are ordered by \`segment_index\`.

- [ ] **Step 4: Verify and commit**

Run: \`cargo test --manifest-path src-tauri/Cargo.toml --test ftp_download ftp_segments_are_ordered_updated_and_deleted\`

Expected: PASS.

\`\`\`bash
git add src-tauri/src/models.rs src-tauri/src/store.rs src-tauri/tests/ftp_download.rs
git commit -m "feat: persist FTP segment progress"
\`\`\`

### Task 2: Task scheduler and settings propagation

**Files:**
- Modify: \`src-tauri/src/ftp.rs:160-410\`
- Modify: \`src-tauri/src/engine_runtime.rs:210-229,552-562\`
- Test: \`src-tauri/tests/ftp_download.rs\`
- Test: \`src-tauri/tests/window_config.rs\`

**Interfaces:**
- Produces \`FtpDownloadLimits::new(max_active_tasks: u16, max_connections_per_task: u8)\`.
- Produces \`FtpDownloadEngine::set_limits(&self, max_active_tasks: u16, max_connections_per_task: u8)\`.
- \`submit_uri\`, resume and recovery enqueue task IDs; only \`pump_queue\` starts admitted workers.

- [ ] **Step 1: Write failing admission and setting tests**

\`\`\`rust
#[tokio::test]
async fn ftp_scheduler_never_starts_more_than_the_task_limit() {
    let fixture = spawn_blocking_ftp_server(vec![0; 8 * 1024 * 1024]).await;
    let engine = fixture.engine_with_limits(1, 4);
    let first = fixture.submit(&engine, "first.bin").await;
    let second = fixture.submit(&engine, "second.bin").await;
    fixture.wait_until_active(first.id).await;
    assert_eq!(fixture.store.find(second.id).unwrap().status, TaskStatus::Queued);
    fixture.release(first.id);
    fixture.wait_until_active(second.id).await;
}
\`\`\`

\`\`\`rust
#[test]
fn reconfigure_updates_ftp_limits() {
    let engine = ftp_fixture();
    engine.set_limits(2, 3);
    assert_eq!(engine.limits(), FtpDownloadLimits::new(2, 3));
}
\`\`\`

- [ ] **Step 2: Run both tests before implementation**

Run: \`cargo test --manifest-path src-tauri/Cargo.toml --test ftp_download ftp_scheduler_never_starts_more_than_the_task_limit\`

Run: \`cargo test --manifest-path src-tauri/Cargo.toml --test window_config reconfigure_updates_ftp_limits\`

Expected: both FAIL: workers start immediately and no FTP limits API exists.

- [ ] **Step 3: Implement queue admission**

\`\`\`rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FtpDownloadLimits {
    max_active_tasks: usize,
    max_connections_per_task: usize,
}
\`\`\`

Track queued task IDs and active task workers. On any completed, failed, paused or cancelled worker, remove the active record, release its slot and call \`pump_queue\`. Construct FTP limits from \`Aria2Settings\`; after successful \`reconfigure_aria2_settings\`, call \`ftp.set_limits\` with the new two values. Existing active tasks retain their current segment count.

- [ ] **Step 4: Verify and commit**

Run: both Task 2 test commands.

Expected: PASS.

\`\`\`bash
git add src-tauri/src/ftp.rs src-tauri/src/engine_runtime.rs src-tauri/tests/ftp_download.rs src-tauri/tests/window_config.rs
git commit -m "feat: limit concurrent FTP tasks"
\`\`\`

### Task 3: Segmented transfer and fallback

**Files:**
- Modify: \`src-tauri/src/ftp.rs:410-630\`
- Test: \`src-tauri/tests/ftp_download.rs\`

**Interfaces:**
- Produces \`plan_segments(total: u64, connection_limit: usize) -> Vec<FtpSegment>\`.
- Produces a range-bounded worker per active segment and aggregate task progress.

- [ ] **Step 1: Extend the local FTP fixture and write failing tests**

\`\`\`rust
#[tokio::test]
async fn ftp_uses_disjoint_ranges_up_to_the_connection_limit() {
    let payload = deterministic_payload(12 * 1024 * 1024);
    let fixture = spawn_ftp_server(FtpBehavior::supports_rest(payload.clone())).await;
    let completed = fixture.download_with_limits(5, 3).await;
    assert_eq!(std::fs::read(fixture.output()).unwrap(), payload);
    assert_eq!(fixture.max_active_data_connections(), 3);
    assert!(fixture.rest_offsets().iter().any(|offset| *offset > 0));
    assert_eq!(completed.connections, 0);
}
#[tokio::test]
async fn rejected_nonzero_rest_falls_back_to_one_stream() {
    let fixture = spawn_ftp_server(FtpBehavior::reject_nonzero_rest(deterministic_payload(8 * 1024 * 1024))).await;
    let completed = fixture.download_with_limits(5, 4).await;
    assert_eq!(completed.status, TaskStatus::Completed);
    assert_eq!(fixture.max_active_data_connections(), 1);
    assert!(fixture.store.ftp_segments(completed.id).unwrap().is_empty());
}
\`\`\`

- [ ] **Step 2: Run the tests before implementation**

Run: \`cargo test --manifest-path src-tauri/Cargo.toml --test ftp_download ftp_uses_disjoint_ranges_up_to_the_connection_limit\`

Run: \`cargo test --manifest-path src-tauri/Cargo.toml --test ftp_download rejected_nonzero_rest_falls_back_to_one_stream\`

Expected: both FAIL under the current one-stream engine.

- [ ] **Step 3: Implement planning, probing and bounded writes**

\`\`\`rust
fn plan_segments(total: u64, limit: usize) -> Vec<FtpSegment> {
    const MIN_SEGMENT_BYTES: u64 = 4 * 1024 * 1024;
    let count = limit.max(1).min(total.div_ceil(MIN_SEGMENT_BYTES) as usize).max(1);
    (0..count).map(|index| FtpSegment {
        index: index as u32,
        start_offset: total * index as u64 / count as u64,
        end_offset: total * (index as u64 + 1) / count as u64,
        completed_length: 0,
    }).collect()
}
\`\`\`

Probe nonzero \`REST\` using a disposable connection before persisting a plan or resizing the output. On success, set the file length once, persist the plan, and give every segment a distinct \`AsyncFtpStream\` plus a non-truncating, seeked file handle. A worker reads no more than its remaining range, persists its cursor after writing, then publishes aggregate bytes/speed/ETA/connections. If capability/setup fails before any data write, clear rows and use the existing single-stream worker. If a started segment fails, retain rows and fail the task.

- [ ] **Step 4: Verify and commit**

Run: both Task 3 test commands.

Expected: PASS with byte-for-byte output equality.

\`\`\`bash
git add src-tauri/src/ftp.rs src-tauri/tests/ftp_download.rs
git commit -m "feat: download FTP files with parallel ranges"
\`\`\`

### Task 4: Pause, retry and restart recovery

**Files:**
- Modify: \`src-tauri/src/ftp.rs:249-323,490-630\`
- Test: \`src-tauri/tests/ftp_download.rs\`

**Interfaces:**
- A task-level command channel reaches all of its segments.
- \`recover\` resumes each saved range at \`start_offset + completed_length\`; no rows uses legacy single-stream continuation.

- [ ] **Step 1: Write failing lifecycle tests**

\`\`\`rust
#[tokio::test]
async fn paused_parallel_ftp_transfer_resumes_without_holes() {
    let fixture = spawn_slow_ftp_server(deterministic_payload(12 * 1024 * 1024)).await;
    let engine = fixture.engine_with_limits(2, 3);
    let task = fixture.submit(&engine, "payload.bin").await;
    fixture.wait_for_segment_progress(task.id, 2).await;
    engine.control(task.id, DownloadAction::Pause).await.unwrap();
    assert!(fixture.store.ftp_segments(task.id).unwrap().iter().filter(|s| s.completed_length > 0).count() >= 2);
    engine.control(task.id, DownloadAction::Resume).await.unwrap();
    fixture.wait_for_completion(task.id).await;
    assert_eq!(std::fs::read(fixture.output()).unwrap(), fixture.payload());
}
#[tokio::test]
async fn restarted_parallel_ftp_transfer_resumes_saved_ranges() {
    let fixture = prepared_segmented_task_fixture().await;
    fixture.restart_engine().recover().await.unwrap();
    fixture.wait_for_completion(fixture.task_id()).await;
    assert_eq!(std::fs::read(fixture.output()).unwrap(), fixture.payload());
}
\`\`\`

- [ ] **Step 2: Run both tests before implementation**

Run: \`cargo test --manifest-path src-tauri/Cargo.toml --test ftp_download paused_parallel_ftp_transfer_resumes_without_holes\`

Run: \`cargo test --manifest-path src-tauri/Cargo.toml --test ftp_download restarted_parallel_ftp_transfer_resumes_saved_ranges\`

Expected: both FAIL.

- [ ] **Step 3: Implement fan-out lifecycle control**

\`\`\`rust
// TaskWorker stores a watch sender and JoinSet for all segment handles.
// signal_and_wait sends one WorkerCommand, joins all segments, then releases admission.
// completed, cancelled and successful overwrite clear rows only after all segments exit.
\`\`\`

Pause retains the rows and output. Cancel, completion and successful overwrite clear rows. Already complete ranges do not reconnect. A post-write failure keeps rows and uses the existing redacted task failure path, allowing retry to resume exactly from durable cursors.

- [ ] **Step 4: Verify and commit**

Run: both Task 4 test commands.

Expected: PASS.

\`\`\`bash
git add src-tauri/src/ftp.rs src-tauri/tests/ftp_download.rs
git commit -m "feat: resume parallel FTP transfers safely"
\`\`\`

### Task 5: Targeted verification and release build

**Files:**
- Modify: no production files; amend the specification only if a documented behavior is technically impossible.

**Interfaces:**
- Consumes all preceding interfaces.
- Produces a verified Windows release binary.

- [ ] **Step 1: Run FTP integration coverage**

Run: \`cargo test --manifest-path src-tauri/Cargo.toml --test ftp_download\`

Expected: PASS for legacy parsing/resume/failure/recovery plus new task cap, segmentation, fallback and lifecycle cases.

- [ ] **Step 2: Run configuration coverage**

Run: \`cargo test --manifest-path src-tauri/Cargo.toml --test window_config\`

Expected: PASS.

- [ ] **Step 3: Build the Windows release**

Run: \`powershell -ExecutionPolicy Bypass -File scripts/build-windows.ps1\`

Expected: exit code 0; this build validates packaging and does not replace the targeted tests.

- [ ] **Step 4: Inspect and commit only feature paths**

Run: \`git diff --check\` and \`git status --short\`

Expected: no whitespace errors and no staged user-owned changes.

\`\`\`bash
git add src-tauri/src/models.rs src-tauri/src/store.rs src-tauri/src/ftp.rs src-tauri/src/engine_runtime.rs src-tauri/tests/ftp_download.rs src-tauri/tests/window_config.rs docs/superpowers/specs/2026-09-14-ftp-parallel-download-design.md docs/superpowers/plans/2026-09-14-ftp-parallel-download.md
git commit -m "feat: support parallel FTP downloads"
\`\`\`

