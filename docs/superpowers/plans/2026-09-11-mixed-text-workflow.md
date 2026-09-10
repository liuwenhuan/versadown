# 混合文本下载工作流 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让一次粘贴的混合下载文本成为可持久化、可恢复的父任务，自动并发下载、由脱敏云端 AI 规划受控后处理，并在 Windows 上可靠交付最终结果。

**Architecture:** 新增独立 `workflow` Rust 域，持久化父任务、资源出现位置、计划版本、产物和事件；现有 aria2 与云盘分享连接器继续承担实际传输。云端只返回经过 JSON/DAG 验证的受限动作，Windows 本地运行时负责暂存目录、动作依赖、归档预处理和结果发布；前端将父任务投影为现有下载队列中的树节点。

**Tech Stack:** Rust 2021、Tauri 2、rusqlite、reqwest、serde/serde_json、PowerShell 5.1、随应用打包的 7-Zip 命令行后端、React 18、TypeScript、Vitest。

**Spec:** `docs/superpowers/specs/2026-09-11-mixed-text-workflow-design.md`

## Global Constraints

- 第一阶段仅保证 Windows；计划语义、归档预处理接口和持久化格式不得绑定 PowerShell，后续 Linux/macOS 只新增执行后端。
- Windows 使用 PowerShell 5.1 与随应用固定版本、带许可证说明的工具；不得依赖用户安装 Python、Node.js 或第三方解压软件。
- 原始粘贴文本、当前计划和密码只保存在本机；向云端发送前必须以 `{{LOCAL_SECRET:archive_password:N}}` 代替每一个识别到的压缩密码，响应和错误日志不得写入明文密码。
- 云端输出不是任意 shell：只接受已注册的 DAG 动作、任务工作区内相对路径和已声明的产物引用；禁止网络、注册表、任意可执行文件和越界删除。
- 同一工作流内相同的规范化下载身份只提交一次，所有文本出现位置仍持久化；不同工作流不能因为相同链接复用到另一个任务的输出目录。
- 已确认资源在 AI 规划期间立即下载到工作流暂存目录；无后处理意图时下载文件就是最终结果。
- 成功并完成校验后永久删除中间件，不进回收站；失败、等待用户补充或取消时保留所有源文件、中间件和日志。
- 前端 Vitest 命令必须带 `--exclude '.worktrees/**'`；本功能属于下载核心、网盘连接器与跨端数据契约变更，每一阶段运行相关 Rust/前端定向测试，最终运行 `scripts/build-windows.ps1`。

---

## File Structure

- `src-tauri/src/workflow/model.rs` — 工作流状态、计划、资源、产物和安全 IPC 投影的 Rust 类型。
- `src-tauri/src/workflow/candidates.rs` — 带 UTF-8 字节范围的链接提取、提供方归类、访问码关联与工作流内去重。
- `src-tauri/src/workflow/plan.rs` — 密码令牌化、OpenAI 兼容规划请求、结构化计划反序列化与 DAG/路径/动作验证。
- `src-tauri/src/workflow/submission.rs` — 普通协议与网盘分享资源的工作流子任务提交，不复用全局同源任务。
- `src-tauri/src/workflow/runtime.rs` — 依赖调度、下载状态观察、等待密码、计划修订、结果发布、取消和恢复。
- `src-tauri/src/workflow/archive.rs` — 分卷/分片/伪装头探测、派生产物及可替换归档后端接口。
- `src-tauri/src/workflow/windows.rs` — Windows 白名单动作编译器、二进制合并和固定 7-Zip 后端调用。
- `src-tauri/src/workflow/commands.rs` — Tauri 命令与队列父任务投影。
- `src-tauri/tests/mixed_workflow_*.rs` — 数据库、解析、AI 脱敏、提交、执行、归档及恢复的定向集成测试。
- `src/lib/workflowTypes.ts`、`src/lib/workflowApi.ts` — 工作流 IPC 类型和调用，不把密码暴露到浏览器日志。
- `src/components/WorkflowDetails.tsx`、`src/components/WorkflowPasswordDialog.tsx` — 计划、分支、错误、补充说明和当前归档密码交互。
- `src/components/NewDownloadDialog.tsx`、`src/components/NewDownloadTab.tsx`、`src/components/DownloadQueue.tsx`、`src/App.tsx` — 入口、队列树、事件订阅与工作流操作接线。
- `src-tauri/binaries/7z.exe`、`THIRD_PARTY_NOTICES.md`、`src-tauri/tauri.conf.json` — Windows 归档后端资源、许可证告知和打包声明。

## Tasks

### Task 1: 建立工作流领域模型与 SQLite 持久化

**Files:**
- Create: `src-tauri/src/workflow/mod.rs`
- Create: `src-tauri/src/workflow/model.rs`
- Create: `src-tauri/tests/mixed_workflow_store.rs`
- Modify: `src-tauri/src/models.rs`
- Modify: `src-tauri/src/store.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces `MixedWorkflow`, `MixedWorkflowStatus`, `WorkflowSource`, `WorkflowOccurrence`, `WorkflowPlanRevision`, `WorkflowArtifact`, `WorkflowEvent`, `NewMixedWorkflow` and `WorkflowArtifactKind` from `workflow::model`.
- Produces `TaskStore::{create_mixed_workflow,find_mixed_workflow,list_mixed_workflows,replace_workflow_plan,insert_workflow_sources,record_workflow_artifact,record_workflow_event,update_mixed_workflow_status}`.
- Stores ordinary aria2 task ownership in `workflow_child_tasks(workflow_id, task_id, source_id)` rather than adding nullable workflow columns to every existing task query.

- [ ] **Step 1: Write the failing store tests**

```rust
#[test]
fn workflow_keeps_raw_text_all_occurrences_and_plan_history() {
    let store = TaskStore::open_in_memory().unwrap();
    let root = tempfile::tempdir().unwrap();
    let workflow = store.create_mixed_workflow(NewMixedWorkflow::new(
        "链接 https://a.test/a 两次 https://a.test/a",
        root.path().join("output"),
        root.path().join("staging"),
    )).unwrap();
    store.insert_workflow_sources(workflow.id, &[NewWorkflowSource::direct("https://a.test/a", "direct:https://a.test/a", &[(3, 19), (23, 39)])]).unwrap();
    store.replace_workflow_plan(workflow.id, NewWorkflowPlanRevision::new(1, "下载后直接交付", "{\"actions\":[]}", "https://ai.test/v1", "planner-a")).unwrap();
    store.replace_workflow_plan(workflow.id, NewWorkflowPlanRevision::new(2, "改名后交付", "{\"actions\":[]}", "https://ai.test/v1", "planner-b")).unwrap();

    assert_eq!(store.find_mixed_workflow(workflow.id).unwrap().raw_pasted_text, "链接 https://a.test/a 两次 https://a.test/a");
    assert_eq!(store.workflow_occurrences(workflow.id).unwrap().len(), 2);
    assert_eq!(store.workflow_plan_revisions(workflow.id).unwrap().len(), 2);
}
```

- [ ] **Step 2: Run the new test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test mixed_workflow_store workflow_keeps_raw_text_all_occurrences_and_plan_history`

Expected: FAIL because `create_mixed_workflow` and the workflow model types do not exist.

- [ ] **Step 3: Add the model, migration, and store methods**

```rust
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MixedWorkflowStatus {
    Parsing, AwaitingAccess, Downloading, Processing, AwaitingPassword,
    NeedsRevision, Verifying, Completed, Failed, Cancelled,
}

pub struct NewMixedWorkflow { pub raw_pasted_text: String, pub output_directory: PathBuf, pub staging_directory: PathBuf }

pub fn create_mixed_workflow(&self, value: NewMixedWorkflow) -> Result<MixedWorkflow, StoreError> {
    let workflow = MixedWorkflow::new(Uuid::new_v4(), value, current_timestamp());
    self.insert_mixed_workflow(&workflow)?;
    Ok(workflow)
}
```

Create migration version 7 with `mixed_text_workflows`, `workflow_sources`, `workflow_occurrences`, `workflow_plan_revisions`, `workflow_child_tasks`, `workflow_artifacts`, and `workflow_events`; use foreign keys with `ON DELETE CASCADE`, a unique `(workflow_id, canonical_identity)` source key, and ordered event/artifact timestamps. Export the module from `lib.rs` without changing existing `tasks` status transitions.

- [ ] **Step 4: Run focused persistence tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test mixed_workflow_store`

Expected: PASS, including restart persistence, source uniqueness only within one workflow, plan revision append-only behavior, and cascading record removal.

- [ ] **Step 5: Commit the persistence boundary**

```powershell
git add src-tauri/src/workflow/mod.rs src-tauri/src/workflow/model.rs src-tauri/src/models.rs src-tauri/src/store.rs src-tauri/src/lib.rs src-tauri/tests/mixed_workflow_store.rs
git commit -m "feat: persist mixed text workflows"
```

### Task 2: 提取带证据的混合文本资源并在任务内去重

**Files:**
- Create: `src-tauri/src/workflow/candidates.rs`
- Create: `src-tauri/tests/mixed_workflow_candidates.rs`
- Modify: `src-tauri/src/parser.rs`
- Modify: `src-tauri/src/workflow/mod.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces `extract_workflow_candidates(input: &str) -> Result<Vec<WorkflowCandidate>, WorkflowCandidateError>`.
- `WorkflowCandidate` contains `kind`, `canonical_identity`, `source`, `cloud_share: Option<CloudShareCandidate>`, `occurrences: Vec<ByteRange>`, and `access_code: Option<String>`.
- Consumes existing `CloudShareCandidate`, `cloud_provider_for_url` logic, `classify_input`, and Thunder decoding only for canonical identity; it does not start downloads.

- [ ] **Step 1: Write failing candidate tests for seven providers, spans, and deduplication**

```rust
#[test]
fn extracts_all_resources_without_losing_duplicate_occurrences() {
    let text = "百度 https://pan.baidu.com/s/a 提取码: abcd；磁力 magnet:?xt=urn:btih:ABC；再次 https://pan.baidu.com/s/a";
    let values = extract_workflow_candidates(text).unwrap();

    assert_eq!(values.len(), 2);
    assert_eq!(values[0].cloud_share.as_ref().unwrap().provider, CloudProvider::Baidu);
    assert_eq!(values[0].occurrences.len(), 2);
    assert_eq!(values[0].access_code.as_deref(), Some("abcd"));
    assert_eq!(values[1].canonical_identity, "magnet:btih:abc");
}
```

- [ ] **Step 2: Run the candidate test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test mixed_workflow_candidates extracts_all_resources_without_losing_duplicate_occurrences`

Expected: FAIL because the workflow candidate extractor does not exist.

- [ ] **Step 3: Implement conservative extraction and identity rules**

```rust
pub fn extract_workflow_candidates(input: &str) -> Result<Vec<WorkflowCandidate>, WorkflowCandidateError> {
    let extracted = extract_candidates_with_byte_ranges(input)?;
    let normalized = extracted.into_iter()
        .map(|item| WorkflowCandidate::normalize(item, input))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(WorkflowCandidate::merge_occurrences(normalized))
}
```

Recognize all seven existing cloud providers through one `CloudShareCandidate::from_candidate` path, preserve signed HTTP query parameters, lowercase only host/scheme and BTIH identity, strip URL fragments from identity, and reject an invalid adjacent access code without discarding other candidates. Keep existing `extract_cloud_share_candidate` behavior unchanged for the non-workflow single-share flow.

- [ ] **Step 4: Run parser and candidate regression tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test parser_policy --test mixed_workflow_candidates`

Expected: PASS; a single normal share still follows its existing path, while multiple clouds no longer raise `MultipleLinks` in the workflow extractor.

- [ ] **Step 5: Commit the extraction boundary**

```powershell
git add src-tauri/src/workflow/candidates.rs src-tauri/src/workflow/mod.rs src-tauri/src/parser.rs src-tauri/src/lib.rs src-tauri/tests/mixed_workflow_candidates.rs
git commit -m "feat: extract mixed workflow resources"
```

### Task 3: 添加密码令牌化、OpenAI 兼容规划客户端与计划验证

**Files:**
- Create: `src-tauri/src/workflow/plan.rs`
- Create: `src-tauri/tests/mixed_workflow_plan.rs`
- Modify: `src-tauri/src/ai.rs`
- Modify: `src-tauri/src/workflow/mod.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces `mask_archive_passwords(raw: &str) -> MaskedWorkflowText`, where `MaskedWorkflowText::cloud_text` never contains discovered plaintext and `local_secrets` maps token to plaintext.
- Produces `AiPlanner::plan(&self, request: WorkflowPlanningRequest) -> Result<PlannedWorkflow, WorkflowPlanError>` using `POST {base_url}/chat/completions`.
- Produces `validate_plan(template: WorkflowPlanTemplate, sources: &[WorkflowSource], secrets: &SecretMap) -> Result<ValidatedWorkflowPlan, WorkflowPlanError>`.
- Allowed action names are exactly `wait_for_artifact`, `rename`, `merge_parts`, `inspect_archive`, `test_archive`, `extract_archive`, `verify`, and `publish`.

- [ ] **Step 1: Write failing tests for secrecy and plan rejection**

```rust
#[tokio::test]
async fn planner_request_masks_password_and_rejects_untrusted_action() {
    let masked = mask_archive_passwords("用密码 p@ss-123 解压 https://a.test/a.7z");
    assert_eq!(masked.cloud_text, "用密码 {{LOCAL_SECRET:archive_password:1}} 解压 https://a.test/a.7z");

    let error = validate_plan(template_with_action("execute", "C:\\Windows\\System32\\cmd.exe"), &[], &masked.local_secrets).unwrap_err();
    assert!(error.to_string().contains("不允许"));
}
```

- [ ] **Step 2: Run the planner test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test mixed_workflow_plan planner_request_masks_password_and_rejects_untrusted_action`

Expected: FAIL because tokenization and `validate_plan` are undefined.

- [ ] **Step 3: Implement the compatible client and validator**

```rust
pub enum WorkflowActionKind { WaitForArtifact, Rename, MergeParts, InspectArchive, TestArchive, ExtractArchive, Verify, Publish }
pub struct WorkflowPlanningRequest { pub model: String, pub masked_text: String, pub sources: Vec<PlanningSource> }
pub struct WorkflowPlanTemplate { pub summary: String, pub actions: Vec<WorkflowActionTemplate> }

pub fn validate_plan(template: WorkflowPlanTemplate, sources: &[WorkflowSource], secrets: &SecretMap) -> Result<ValidatedWorkflowPlan, WorkflowPlanError> {
    let actions = ValidatedWorkflowAction::from_templates(template.actions, sources, secrets)?;
    ValidatedWorkflowAction::ensure_unique_ids(&actions)?;
    ValidatedWorkflowAction::ensure_topological_dependencies(&actions)?;
    Ok(ValidatedWorkflowPlan { summary: template.summary, actions })
}
```

Use a normal chat-completions request with a strict JSON-only system prompt and `temperature: 0`; parse only `choices[0].message.content` as `WorkflowPlanTemplate`. Load the API key through the existing DPAPI `CredentialStore`, zeroize the temporary key after the request, and map HTTP/body failures to generic messages without retaining response bodies. Persist only the masked request/response audit data; inject plaintext into the in-memory/local executable plan after validation.

- [ ] **Step 4: Run AI and plan contract tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml ai::tests::fetch_models_uses_the_compatible_models_route_and_bearer_key --test mixed_workflow_plan`

Expected: PASS, including mock-server assertions that request JSON, cloud-response error text, and stored cloud-audit events never contain `p@ss-123`.

- [ ] **Step 5: Commit the planning boundary**

```powershell
git add src-tauri/src/workflow/plan.rs src-tauri/src/workflow/mod.rs src-tauri/src/ai.rs src-tauri/src/lib.rs src-tauri/tests/mixed_workflow_plan.rs
git commit -m "feat: add masked workflow AI planning"
```

### Task 4: 提交普通协议工作流子任务并立即并发下载

**Files:**
- Create: `src-tauri/src/workflow/submission.rs`
- Create: `src-tauri/tests/mixed_workflow_submission.rs`
- Modify: `src-tauri/src/downloader.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/workflow/mod.rs`

**Interfaces:**
- Produces `submit_workflow_uri_child(workflow_id, source_id, candidate, staging_directory, store, service) -> Result<DownloadTask, WorkflowSubmissionError>`.
- Produces `WorkflowSubmissionBatch { started_task_ids: Vec<Uuid>, awaiting_access_source_ids: Vec<Uuid>, failures: Vec<WorkflowSourceFailure> }`.
- The helper accepts an injected `Arc<DownloadService>` in tests and always uses a child-specific staging directory; it never calls global `find_by_source` duplicate resolution.

- [ ] **Step 1: Write the failing submission isolation test**

```rust
#[tokio::test]
async fn same_direct_url_is_once_per_workflow_but_not_shared_across_workflows() {
    let first = submit_fixture_workflow("https://a.test/file.bin", "workflow-a").await;
    let repeated = submit_fixture_workflow("https://a.test/file.bin", "workflow-a").await;
    let second = submit_fixture_workflow("https://a.test/file.bin", "workflow-b").await;

    assert_eq!(first.child_task_ids, repeated.child_task_ids);
    assert_ne!(first.child_task_ids, second.child_task_ids);
    assert!(first.destination.ends_with("workflow-a/staging/downloads"));
}
```

- [ ] **Step 2: Run the submission test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test mixed_workflow_submission same_direct_url_is_once_per_workflow_but_not_shared_across_workflows`

Expected: FAIL because worktree-specific URI submission is absent.

- [ ] **Step 3: Implement explicit child submission and asynchronous planning launch**

```rust
pub async fn submit_workflow_uri_child(
    workflow_id: Uuid, source: &WorkflowSource, directory: &Path,
    store: &TaskStore, service: &DownloadService,
) -> Result<DownloadTask, WorkflowSubmissionError> {
    let task = store.create(NewTask::direct(candidate.download_source()))?;
    store.link_workflow_child(workflow_id, source.id, task.id)?;
    let options = UriSubmissionOptions::for_directory(directory.to_path_buf());
    Ok(service.submit_uri_with_options(task.id, vec![task.source], options).await?)
}
```

Add `commands::start_mixed_workflow_submission` as an internal async helper: persist sources first, begin ordinary HTTP/Thunder/torrent/magnet child submission for deduplicated candidates, record individual failures, then spawn masked AI planning without awaiting it. Keep the existing single-link `parse_download` behavior unchanged.

- [ ] **Step 4: Run direct-download and submission regressions**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test downloader --test mixed_workflow_submission`

Expected: PASS; current duplicate dialogs still work for normal downloads and workflow children have isolated destinations.

- [ ] **Step 5: Commit the ordinary submission boundary**

```powershell
git add src-tauri/src/workflow/submission.rs src-tauri/src/workflow/mod.rs src-tauri/src/downloader.rs src-tauri/src/commands.rs src-tauri/tests/mixed_workflow_submission.rs
git commit -m "feat: submit isolated workflow downloads"
```

### Task 5: 让七个网盘分享在工作流中自动提交全部条目

**Files:**
- Create: `src-tauri/tests/mixed_workflow_cloud.rs`
- Modify: `src-tauri/src/cloud/share.rs`
- Modify: `src-tauri/src/cloud/commands.rs`
- Modify: `src-tauri/src/workflow/submission.rs`
- Modify: `src-tauri/src/workflow/model.rs`

**Interfaces:**
- Produces `workflow_share_sources(store, plan_source) -> Result<Vec<String>, QuarkError>` that returns every durable source in inspection order.
- Produces `submit_workflow_cloud_share_file(app, workflow_id, source_id, stable_source, configured) -> Result<DownloadTask, QuarkError>`.
- Consumes existing `inspect_cloud_share`, `CloudSharePlanStore`, `resolve_stable_cloud_share_download`, and every registered provider adapter.

- [ ] **Step 1: Write the failing all-entry cloud test**

```rust
#[tokio::test]
async fn workflow_starts_every_inspected_share_entry_without_confirmation() {
    let plan = plan_with_entries(["one.zip", "folder/two.001"]);
    let sources = workflow_share_sources(&store, &plan.source_for("one")).unwrap();

    assert_eq!(sources.len(), 2);
    let submitted = submit_sources_into_workflow(sources).await;
    assert_eq!(submitted.started_task_ids.len(), 2);
    assert!(submitted.failures.is_empty());
}
```

- [ ] **Step 2: Run the cloud workflow test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test mixed_workflow_cloud workflow_starts_every_inspected_share_entry_without_confirmation`

Expected: FAIL because cloud shares currently require a selected resource list and enforce global same-source duplicate handling.

- [ ] **Step 3: Add workflow-specific cloud source enumeration and submission**

```rust
pub fn workflow_share_sources(store: &CloudSharePlanStore, plan_source: &str) -> Result<Vec<String>, QuarkError> {
    let plan = store.resolve_entry(plan_source)?.plan;
    Ok(plan.entries.iter().map(|entry| plan.source_for(&entry.entry_id)).collect())
}
```

Keep `confirm_cloud_share_download` unchanged for the existing UI. Add a second submission path that creates a fresh local child task, associates it with `workflow_child_tasks`, writes into the workflow staging directory, and retains existing transfer/cleanup journaling. Translate `AccessCodeRequired` to the parent’s `awaiting_access` state without blocking unrelated sources; every provider error becomes a durable source failure with its current user-facing message.

- [ ] **Step 4: Run cloud provider and workflow tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract --test cloud_directory_download --test mixed_workflow_cloud`

Expected: PASS; all seven adapters remain registered, normal share confirmation still selects user-chosen entries, and workflow inspection submits all entries.

- [ ] **Step 5: Commit cloud workflow integration**

```powershell
git add src-tauri/src/cloud/share.rs src-tauri/src/cloud/commands.rs src-tauri/src/workflow/submission.rs src-tauri/src/workflow/model.rs src-tauri/tests/mixed_workflow_cloud.rs
git commit -m "feat: download cloud shares in workflows"
```

### Task 6: 实现受限动作执行、派生产物与结果发布

**Files:**
- Create: `src-tauri/src/workflow/runtime.rs`
- Create: `src-tauri/src/workflow/windows.rs`
- Create: `src-tauri/tests/mixed_workflow_runtime.rs`
- Modify: `src-tauri/src/policy.rs`
- Modify: `src-tauri/src/processing.rs`
- Modify: `src-tauri/src/workflow/mod.rs`

**Interfaces:**
- Produces `WorkflowRuntime::advance(workflow_id) -> Result<MixedWorkflow, WorkflowRuntimeError>` and `WorkflowRuntime::observe_download_task(task: &DownloadTask)`.
- Produces `WindowsActionExecutor::execute(action, context) -> Result<Vec<WorkflowArtifact>, WorkflowExecutionError>`.
- `ExecutionContext` exposes only `staging_root`, `results_root`, declared artifact IDs, and the current archive-node password; it does not expose arbitrary filesystem roots or an environment command shell.

- [ ] **Step 1: Write failing execution and path-safety tests**

```rust
#[test]
fn runtime_publishes_declared_output_but_rejects_escape_path() {
    let plan = plan([rename("source:one", "release.zip"), publish("action:rename-1")]);
    let completed = run_plan_in_tempdir(plan).unwrap();
    assert!(completed.final_paths[0].ends_with("最终结果/release.zip"));

    let error = run_plan_in_tempdir(plan([rename("source:one", "..\\outside.zip")])).unwrap_err();
    assert!(error.to_string().contains("路径"));
}
```

- [ ] **Step 2: Run the runtime test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test mixed_workflow_runtime runtime_publishes_declared_output_but_rejects_escape_path`

Expected: FAIL because no runtime or Windows executor exists.

- [ ] **Step 3: Implement topological action execution and file primitives**

```rust
match action.kind {
    WorkflowActionKind::Rename => executor.rename(&input, &relative_output),
    WorkflowActionKind::MergeParts => executor.merge_binary(&ordered_inputs, &relative_output),
    WorkflowActionKind::Verify => executor.verify(&input, &contract),
    WorkflowActionKind::Publish => executor.publish(&input, &context.results_root),
    WorkflowActionKind::InspectArchive => executor.inspect_archive(&input),
    WorkflowActionKind::TestArchive => executor.test_archive(&input, action.password()),
    WorkflowActionKind::ExtractArchive => executor.extract_archive(&input, &relative_output, action.password()),
    WorkflowActionKind::WaitForArtifact => executor.wait_for_artifact(&input),
}
```

Implement merge through `std::io::copy` over opened binary files, use an exclusive new output file, and validate every input is a registered artifact under staging. Record an artifact row and an event row after every successful or failed node. Retire `ProcessAction::Execute` from all workflow use; retain old `policy.rs` API only until its callers are migrated, then make it a thin wrapper over the same action allow-list.

- [ ] **Step 4: Run processing and runtime tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test processing --test mixed_workflow_runtime`

Expected: PASS; arbitrary program execution remains rejected and a valid declared rename/merge/publish plan produces only workspace-scoped files.

- [ ] **Step 5: Commit the controlled executor**

```powershell
git add src-tauri/src/workflow/runtime.rs src-tauri/src/workflow/windows.rs src-tauri/src/workflow/mod.rs src-tauri/src/policy.rs src-tauri/src/processing.rs src-tauri/tests/mixed_workflow_runtime.rs
git commit -m "feat: execute validated workflow actions"
```

### Task 7: 构建多卷、分片、伪装头和递归归档预处理器

**Files:**
- Create: `src-tauri/src/workflow/archive.rs`
- Create: `src-tauri/tests/mixed_workflow_archive.rs`
- Create: `THIRD_PARTY_NOTICES.md`
- Add: `src-tauri/binaries/7z.exe`
- Modify: `src-tauri/src/workflow/runtime.rs`
- Modify: `src-tauri/src/workflow/windows.rs`
- Modify: `src-tauri/tauri.conf.json`

**Interfaces:**
- Produces `ArchivePreprocessor::inspect(input) -> Result<Vec<ArchiveCandidate>, ArchiveError>` and `ArchivePreprocessor::prepare(candidate) -> Result<WorkflowArtifact, ArchiveError>`.
- Produces `ArchiveBackend::{list,test,extract}`; production `SevenZipBackend` runs only the bundled binary, while tests inject `FakeArchiveBackend`.
- `ArchiveCandidate` contains `source_artifact_id`, `offset`, `format`, `volume_set`, and `confidence`; it becomes actionable only after backend `test` succeeds.

- [ ] **Step 1: Write failing archive-layout tests using a fake backend**

```rust
#[test]
fn prefixed_file_with_two_verified_archives_creates_two_ordered_derivatives() {
    let candidates = inspect_fixture("cover.jpg+one.7z+two.zip", FakeArchiveBackend::verifying([16, 96])).unwrap();
    assert_eq!(candidates.iter().map(|value| value.offset).collect::<Vec<_>>(), vec![16, 96]);

    let derivatives = candidates.into_iter().map(|value| prepare_fixture(value)).collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(derivatives.len(), 2);
    assert!(derivatives.iter().all(|value| value.path.starts_with("staging/derived")));
}

#[test]
fn missing_regular_volume_never_uses_byte_merge() {
    let error = inspect_fixture("movie.7z.001", FakeArchiveBackend::default()).unwrap_err();
    assert!(error.to_string().contains("缺少分卷"));
}
```

- [ ] **Step 2: Run the archive tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test mixed_workflow_archive`

Expected: FAIL because the archive preprocessor and backend abstraction do not exist.

- [ ] **Step 3: Implement detection, safe derivation, and backend packaging**

```rust
pub trait ArchiveBackend: Send + Sync {
    fn list(&self, input: &Path, hint: ArchiveHint) -> Result<ArchiveListing, ArchiveError>;
    fn test(&self, input: &Path, hint: ArchiveHint, password: Option<&str>) -> Result<(), ArchiveError>;
    fn extract(&self, input: &Path, output: &Path, hint: ArchiveHint, password: Option<&str>) -> Result<Vec<PathBuf>, ArchiveError>;
}
```

Recognize formal multivolume naming before generic parts; require a continuous, complete set for formal volumes and call the backend on its canonical first volume. For generic numbered parts only, concatenate with `WindowsActionExecutor::merge_binary` into staging, then inspect/test the combined derivative. For prefixed content, scan within the configured byte limit, require a format parser candidate plus backend test, and copy from the verified offset into a new derivative without modifying the source. Sort multiple passing candidates by offset and create independent extraction output roots. Add `7z.exe` from the chosen fixed upstream distribution, include its exact version/license in `THIRD_PARTY_NOTICES.md`, and add `binaries/7z.exe` to Tauri `bundle.resources`.

- [ ] **Step 4: Run archive and packaging contract tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test mixed_workflow_archive --test processing`

Expected: PASS; false signatures do not produce derivatives, originals are never mutated, formal volumes are not generically concatenated, and only the fake backend runs during tests.

- [ ] **Step 5: Commit archive preprocessing**

```powershell
git add src-tauri/src/workflow/archive.rs src-tauri/src/workflow/runtime.rs src-tauri/src/workflow/windows.rs src-tauri/tests/mixed_workflow_archive.rs src-tauri/binaries/7z.exe src-tauri/tauri.conf.json THIRD_PARTY_NOTICES.md
git commit -m "feat: preprocess archives in workflows"
```

### Task 8: 增加递归调度、密码等待、修订、恢复和成功清理

**Files:**
- Create: `src-tauri/tests/mixed_workflow_lifecycle.rs`
- Modify: `src-tauri/src/workflow/runtime.rs`
- Modify: `src-tauri/src/workflow/model.rs`
- Modify: `src-tauri/src/store.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces `WorkflowRuntime::{provide_archive_password,skip_workflow_branch,revise_workflow,cancel_workflow,resume_incomplete_workflows}`.
- `provide_archive_password(workflow_id, archive_artifact_id, password)` applies the plaintext only to that archive node and its formal volume set.
- `advance` adds nested archive actions dynamically, deduplicated by `(content_hash, parent_artifact_id)`.

- [ ] **Step 1: Write failing lifecycle tests**

```rust
#[test]
fn unknown_password_pauses_one_branch_and_keeps_independent_branch_running() {
    let state = run_fixture_with_locked_and_plain_archives();
    assert_eq!(state.locked_branch.status, WorkflowNodeStatus::AwaitingPassword);
    assert_eq!(state.plain_branch.status, WorkflowNodeStatus::Completed);
    assert_eq!(state.workflow.status, MixedWorkflowStatus::AwaitingPassword);
}

#[test]
fn successful_workflow_keeps_final_and_logs_but_failed_workflow_keeps_staging() {
    assert!(!finish_fixture_workflow().staging_directory.exists());
    let failed = fail_fixture_workflow();
    assert!(failed.staging_directory.exists());
    assert!(failed.source_file.exists());
}
```

- [ ] **Step 2: Run lifecycle tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test mixed_workflow_lifecycle`

Expected: FAIL because the runtime does not yet persist waiting-password nodes, plan revisions, or cleanup decisions.

- [ ] **Step 3: Implement state aggregation and deterministic cleanup**

```rust
pub fn provide_archive_password(&self, workflow_id: Uuid, artifact_id: Uuid, password: String) -> Result<(), WorkflowRuntimeError> {
    self.store.record_workflow_secret_scope(workflow_id, artifact_id, password)?;
    self.advance(workflow_id)
}
```

On an encrypted archive without a node-scoped password, record `awaiting_password` with the backend’s original error and continue runnable independent nodes. A user revision appends a new plan revision and retains old actions/events; it never overwrites raw text. At successful final verification, atomically promote declared final artifacts into `最终结果`, retain `任务日志`, close all files, then recursively remove only the known workflow staging root. On `failed`, `needs_revision`, or `cancelled`, do not delete any artifact or staging directory. On app start, load non-terminal workflows and reschedule only nodes whose durable dependencies are satisfied.

- [ ] **Step 4: Run lifecycle and store recovery tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test mixed_workflow_lifecycle --test mixed_workflow_store --test store_transitions`

Expected: PASS; cancellation retains evidence, password scope does not leak into nested siblings, successful cleanup stays inside staging, and restart resumes only runnable work.

- [ ] **Step 5: Commit lifecycle behavior**

```powershell
git add src-tauri/src/workflow/runtime.rs src-tauri/src/workflow/model.rs src-tauri/src/store.rs src-tauri/src/lib.rs src-tauri/tests/mixed_workflow_lifecycle.rs
git commit -m "feat: recover and finalize workflows"
```

### Task 9: 暴露 Tauri 命令、工作流队列投影与实时事件

**Files:**
- Create: `src-tauri/src/workflow/commands.rs`
- Create: `src-tauri/tests/mixed_workflow_commands.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/models.rs`

**Interfaces:**
- Produces commands `start_mixed_text_workflow`, `get_mixed_workflow`, `revise_mixed_workflow`, `provide_workflow_archive_password`, `skip_workflow_branch`, and `cancel_mixed_workflow`.
- Produces `WorkflowQueuePayload` with ID `workflow:{uuid}`, kind `workflow`, child task projection, status message, plan summary, output directory, branch failures, and password-request metadata without a password value.
- `TauriDownloadEvents::task_updated` calls `WorkflowRuntime::observe_download_task` then emits its changed parent as `download-task-updated`.

- [ ] **Step 1: Write failing command projection tests**

```rust
#[tokio::test]
async fn start_command_returns_parent_before_ai_plan_and_emits_parent_updates() {
    let payload = start_workflow_with_fake_services("下载 https://a.test/a.zip 后改名为 release.zip").await.unwrap();
    assert!(payload.id.starts_with("workflow:"));
    assert_eq!(payload.kind, "workflow");
    assert_eq!(payload.status, "downloading");
    assert_eq!(payload.children.len(), 1);
}
```

- [ ] **Step 2: Run command tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test mixed_workflow_commands start_command_returns_parent_before_ai_plan_and_emits_parent_updates`

Expected: FAIL because no workflow Tauri commands or queue payload exist.

- [ ] **Step 3: Add commands and application-state wiring**

```rust
#[tauri::command]
pub async fn start_mixed_text_workflow(input: String, app: AppHandle, state: State<'_, AppState>) -> Result<WorkflowQueuePayload, String> {
    state.workflow_runtime.start(input, app).await.map_err(|error| error.to_string())
}
```

Manage one `Arc<WorkflowRuntime>` during `lib.rs::setup`, initialize it after `TaskStore` and settings are available, and call `resume_incomplete_workflows` after the download engine initializes. Extend `list_queue_tasks_for_scope` to include synthetic workflow parents and suppress their linked child tasks from the top level. Route `workflow:` action IDs before UUID parsing; retain existing `group:` and ordinary task actions unchanged.

- [ ] **Step 4: Run command and queue projection tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test mixed_workflow_commands --test cloud_directory_download`

Expected: PASS; workflow parent/child trees coexist with cloud directory groups and existing `task_action` behavior.

- [ ] **Step 5: Commit IPC and event projection**

```powershell
git add src-tauri/src/workflow/commands.rs src-tauri/src/commands.rs src-tauri/src/lib.rs src-tauri/src/models.rs src-tauri/tests/mixed_workflow_commands.rs
git commit -m "feat: expose mixed workflow commands"
```

### Task 10: 接入前端 IPC、混合文本入口与计划/分支详情

**Files:**
- Create: `src/lib/workflowTypes.ts`
- Create: `src/lib/workflowApi.ts`
- Create: `src/lib/workflowApi.test.ts`
- Create: `src/components/WorkflowDetails.tsx`
- Create: `src/components/WorkflowDetails.test.tsx`
- Modify: `src/lib/types.ts`
- Modify: `src/lib/api.ts`
- Modify: `src/components/NewDownloadDialog.tsx`
- Modify: `src/components/NewDownloadDialog.test.tsx`
- Modify: `src/components/NewDownloadTab.tsx`
- Modify: `src/App.tsx`

**Interfaces:**
- Produces TypeScript `WorkflowQueueTask`, `WorkflowDetailsPayload`, `WorkflowPasswordRequest`, and `WorkflowFailure` types; `QueueTask.kind` additionally accepts `'workflow'`.
- Produces `startMixedTextWorkflow`, `getMixedWorkflow`, `reviseMixedWorkflow`, `provideWorkflowArchivePassword`, `skipWorkflowBranch`, and `cancelMixedWorkflow` wrappers.
- `NewDownloadDialog` and `NewDownloadTab` send a text that contains multiple candidates or natural-language operations to `startMixedTextWorkflow`; a plain single-link text continues through `parseDownload`.

- [ ] **Step 1: Write failing API and entry tests**

```tsx
it('submits a mixed paste as one workflow instead of splitting lines', async () => {
  const user = userEvent.setup();
  render(<NewDownloadDialog open placement="modal" onParse={vi.fn()} onStartWorkflow={vi.fn().mockResolvedValue(workflowTask)} onClose={vi.fn()} />);
  await user.type(screen.getByLabelText('下载内容'), 'https://a.test/a.zip\nmagnet:?xt=urn:btih:abc\n解压后即可');
  await user.click(screen.getByRole('button', { name: '开始解析下载' }));
  expect(onStartWorkflow).toHaveBeenCalledOnce();
});
```

- [ ] **Step 2: Run frontend tests to verify they fail**

Run: `npm run test:run -- --exclude '.worktrees/**' src/lib/workflowApi.test.ts src/components/NewDownloadDialog.test.tsx src/components/WorkflowDetails.test.tsx`

Expected: FAIL because the workflow API, prop, and details component do not exist.

- [ ] **Step 3: Implement API normalization and workflow details**

```ts
export async function startMixedTextWorkflow(input: string): Promise<WorkflowQueueTask> {
  return invoke<WorkflowQueueTask>('start_mixed_text_workflow', { input });
}

export function isMixedWorkflowInput(value: string): boolean {
  return extractLinks(value).length > 1 || (extractLinks(value).length === 1 && removeLinkAndAccessCode(value).trim().length > 0);
}
```

Display original text, versioned human-readable plan summary, resource branches, completed steps, final artifacts, failure detail and the editable supplemental-instruction field in `WorkflowDetails`. Never put local password values, plan JSON, source access codes, or backend command strings into a browser console log, toast, `title`, or DOM data attribute.

- [ ] **Step 4: Run component tests and production type build**

Run: `npm run test:run -- --exclude '.worktrees/**' src/lib/workflowApi.test.ts src/components/NewDownloadDialog.test.tsx src/components/WorkflowDetails.test.tsx`

Run: `npm run build`

Expected: PASS; one mixed paste creates one UI task and current single-link and cloud-access-code tests remain green.

- [ ] **Step 5: Commit the mixed-input UI**

```powershell
git add src/lib/workflowTypes.ts src/lib/workflowApi.ts src/lib/workflowApi.test.ts src/lib/types.ts src/lib/api.ts src/components/WorkflowDetails.tsx src/components/WorkflowDetails.test.tsx src/components/NewDownloadDialog.tsx src/components/NewDownloadDialog.test.tsx src/components/NewDownloadTab.tsx src/App.tsx
git commit -m "feat: add mixed workflow input UI"
```

### Task 11: 增加密码弹窗、队列操作、隐私文案与最终验收

**Files:**
- Create: `src/components/WorkflowPasswordDialog.tsx`
- Create: `src/components/WorkflowPasswordDialog.test.tsx`
- Modify: `src/components/DownloadQueue.tsx`
- Modify: `src/components/TaskDetails.tsx`
- Modify: `src/components/SettingsPanel.tsx`
- Modify: `src/components/SettingsPanel.test.tsx`
- Modify: `src/App.tsx`
- Modify: `src/styles.css`

**Interfaces:**
- `WorkflowPasswordDialog` takes `{ request, onSubmit, onSkip, onCancelWorkflow }` and submits one password only for `request.archiveArtifactId`.
- Workflow row actions are `cancel`, `open_directory`, `details`, and `revise`; a waiting-password branch opens the password dialog while unrelated child rows remain live.
- Settings privacy copy states that AI receives the mixed text and resource metadata after archive passwords are locally replaced by tokens; it never receives plaintext passwords, local files, access codes, or the API key.

- [ ] **Step 1: Write failing modal and queue tests**

```tsx
it('submits a password only for the requested archive and keeps the queue visible', async () => {
  const user = userEvent.setup();
  render(<WorkflowPasswordDialog request={{ workflowId: 'w1', archiveArtifactId: 'a1', archiveName: 'locked.7z', message: '需要密码' }} onSubmit={onSubmit} onSkip={vi.fn()} onCancelWorkflow={vi.fn()} />);
  await user.type(screen.getByLabelText('当前归档密码'), 'local-only');
  await user.click(screen.getByRole('button', { name: '继续解压' }));
  expect(onSubmit).toHaveBeenCalledWith('w1', 'a1', 'local-only');
});
```

- [ ] **Step 2: Run UI tests to verify they fail**

Run: `npm run test:run -- --exclude '.worktrees/**' src/components/WorkflowPasswordDialog.test.tsx src/components/DownloadQueue.test.tsx src/components/SettingsPanel.test.tsx`

Expected: FAIL because no workflow password dialog or workflow queue actions exist.

- [ ] **Step 3: Implement non-blocking branch interaction and privacy copy**

```tsx
{passwordRequest && <WorkflowPasswordDialog
  request={passwordRequest}
  onSubmit={(workflowId, artifactId, password) => void provideWorkflowArchivePassword(workflowId, artifactId, password)}
  onSkip={(workflowId, artifactId) => void skipWorkflowBranch(workflowId, artifactId)}
  onCancelWorkflow={(workflowId) => void cancelMixedWorkflow(workflowId)}
/>}
```

Use an accessible modal with focus management and explicit “继续解压 / 跳过此分支 / 终止整个任务” actions. Do not mask the user-entered password in local task details or logs, but clear the React state immediately after the IPC promise settles. Show `needs_revision` failures with the original tool message, actual detected result, completed nodes, and an editable supplementary instruction control. Add workflow-aware status labels and row action guards without altering BT confirmation controls or cloud-directory tree behavior.

- [ ] **Step 4: Run targeted frontend validation and release build**

Run: `npm run test:run -- --exclude '.worktrees/**' src/components/WorkflowPasswordDialog.test.tsx src/components/WorkflowDetails.test.tsx src/components/NewDownloadDialog.test.tsx src/components/DownloadQueue.test.tsx src/components/SettingsPanel.test.tsx`

Run: `npm run build`

Run: `scripts/build-windows.ps1`

Expected: PASS; the Windows artifact includes aria2 and `7z.exe`, workflow password prompts do not halt independent branches, and privacy copy matches the tokenization boundary.

- [ ] **Step 5: Commit final UI and packaging work**

```powershell
git add src/components/WorkflowPasswordDialog.tsx src/components/WorkflowPasswordDialog.test.tsx src/components/DownloadQueue.tsx src/components/TaskDetails.tsx src/components/SettingsPanel.tsx src/components/SettingsPanel.test.tsx src/App.tsx src/styles.css
git commit -m "feat: complete workflow recovery UI"
```

## Plan Self-Review

- Spec coverage: Tasks 1–2 implement raw-text persistence, occurrence evidence, protocols and within-workflow deduplication. Tasks 3–5 implement cloud-model masking, fixed model snapshots, immediate direct/cloud downloads and per-source failures. Tasks 6–8 implement restricted scripts, Windows execution, formal volumes, generic split merging, prefix stripping, multiple verified archives, recursion, password scope, recovery and cleanup. Tasks 9–11 implement IPC, tree projection, user revision/password interaction, output directories and privacy disclosure.
- Safety coverage: Task 3 rejects unknown secret tokens and untrusted action types; Task 6 restricts all file mutations to declared workspace artifacts; Task 7 never mutates originals or byte-merges formal volumes; Task 8 deletes only known staging paths and only after final verification.
- Type consistency: `MixedWorkflowStatus`, `WorkflowSource`, `ValidatedWorkflowPlan`, `WorkflowArtifact`, `WorkflowRuntime`, `WorkflowQueuePayload`, `WorkflowQueueTask`, and `WorkflowPasswordRequest` are created before the tasks that consume them. The password path consistently uses `(workflow_id, archive_artifact_id, password)`.
- Placeholder scan: this plan contains no deferred implementation markers; every task names concrete files, public interfaces, a failing test, a focused command, an implementation boundary, a passing command and a commit.
