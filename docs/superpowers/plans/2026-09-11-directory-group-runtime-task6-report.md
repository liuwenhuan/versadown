# Task 6 运行时目录组投影报告

## ROOT CAUSE

`list_queue_tasks_for_scope` 已能把目录成员投影为递归根，但目录提交、虚拟叶子重试与 aria2 成员事件仍把裸 `DownloadTaskPayload` 交给前端。通用队列合并因此会把成员当作新的顶层任务，且根摘要不会随成员事件刷新。

## RED

- `directory_submission_persists_parent_links_success_and_keeps_provider_failure_retryable`：序列化结果中的 `queueTask` 为 `Null`，不能返回递归目录根。
- `resubmitting_a_persisted_failed_leaf_does_not_touch_linked_siblings`：虚拟失败叶子重试结果中的 `queueTask` 为 `Null`。

## GREEN

- `TaskActionOutcome` 新增可选、camelCase 的 `queueTask`；普通任务与 BT 结果不会序列化该字段，保持原有 JSON 兼容。
- 目录提交、单虚拟叶子重试和组重试都经 `list_queue_tasks_for_scope` 的 durable 投影返回完整 `QueueTaskPayload` 根。
- `TauriDownloadEvents` 对目录成员更新发同一个递归根；普通任务仍发原始 `DownloadTaskPayload`。
- 审查补充：本地 pause/resume/cancel/retry 的通用结果构造器同样按 durable 成员关系附加目录根，避免真实已关联子任务经本地动作路径重新出现在顶层。
- 组 retry 仅重提 `task_id` 为空的持久化失败项，逐项继续执行，并避免重新提交已经关联的成员。

## VERIFICATION

```powershell
& 'C:\Users\deepin\.cargo\bin\cargo.exe' test --manifest-path src-tauri/Cargo.toml --test cloud_directory_download
& 'C:\Users\deepin\.cargo\bin\cargo.exe' test --manifest-path src-tauri/Cargo.toml --lib commands::tests
git diff --check
```

结果：目录测试 10/10、命令相关库测试 67/67、差异检查通过。Cargo 会报告已有的未使用 helper 与 MSVC linker 提示；本 Task 未新增警告。

## TASK 7 CONTRACT

- `TaskActionOutcome.queueTask?: QueueTaskPayload`：只要目录提交/目录动作/虚拟叶子重试影响目录组，即存在并且是完整递归根。
- `download-task-updated`：目录成员事件直接发送 `QueueTaskPayload` 根；普通任务仍发送原 `DownloadTaskPayload`。
- 前端应在 `queueTask` 存在时优先 upsert 它，且把目录事件 payload 当作可直接 upsert 的根，不能把 `queueTask.children` 中的成员提升到顶层。

## COMMIT

See the Task 6 implementation commit reported to the coordinator.
