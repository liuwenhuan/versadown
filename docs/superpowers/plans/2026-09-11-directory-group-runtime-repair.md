# 云盘目录组运行时修复计划

## 根因

`list_tasks` 会将已关联的云盘叶子投影为一个递归目录根，但新建目录、叶子重试和 aria2 事件仍发送裸 `DownloadTaskPayload`。`App` 的通用 `upsertTask` 找不到尚未带子项的合成根或虚拟失败项对应的新 task ID 时，会把真实叶子插入顶层；根摘要也不会随叶子事件重新聚合。组根的 `retry` 没有调用已实现的单项重试，`open/recycle/purge` 仍把组 UUID 当作 `tasks` 表里的任务 ID。

## 目标和不变量

- 对所有云盘目录成员，目录组投影是新建、重试、任务事件和组操作的唯一外部表示；前端始终收到并替换同一个递归根。
- 非目录任务、BT/magnet 文件选择、凭据和签名 URL 边界保持不变。
- 组根重试会重提所有未关联失败叶子；单叶子重试仍然精确；已关联叶子不可重复提交。
- 目录组可打开其本地根目录；回收和彻底删除以成员为单位执行，并在永久删除后删除目录组记录及虚拟失败项。
- 所有修复测试先失败后通过；不新增第二套分组机制。

## Task 6：后端权威组快照和成员事件

**Files:** `src-tauri/src/commands.rs`, `src-tauri/src/cloud/commands.rs`, `src-tauri/src/lib.rs`, Rust tests.

1. 为一个目录组生成权威 `QueueTaskPayload` 根；完成目录提交后、虚拟叶子重试后和成员下载事件中都发出该根，而不是裸成员。
2. 为动作结果添加可选的组快照字段，保持普通任务结果兼容；组根 retry 重提未关联失败叶子并返回更新后的组快照。
3. 先写测试：目录提交后成员事件不能出现顶层叶子，重试失败叶子后仍只有根且根摘要更新；组根 retry 调用每个未关联失败项。

## Task 7：前端权威快照消费

**Files:** `src/lib/types.ts`, `src/lib/cloudTypes.ts`, `src/lib/cloudApi.ts`, `src/components/CloudManager.tsx`, `src/App.tsx`, corresponding tests.

1. 映射可选组快照；CloudManager 和 App 在收到时优先 upsert 组根。
2. 先写 App 集成测试：创建目录后接收成员事件、单失败叶子 retry 返回后，队列顶层仍只有一个根，进度/速度来自更新的叶子。
3. 普通任务和 BT 回归必须继续通过。

## Task 8：目录组打开、回收和永久删除

**Files:** `src-tauri/src/commands.rs`, `src-tauri/src/store.rs`, Rust tests.

1. 将 group UUID 的打开操作解析为安全的目录组输出根。
2. 回收动作遍历可回收成员；投影将整个已回收组放入回收站。永久删除先以原有安全逻辑清理实际成员文件/记录，再删除组及其虚拟失败项。
3. 先写目录组完成、回收和彻底删除测试；普通任务操作不变。

## Task 9：集成和标准 release

1. 运行目录 Rust 测试、commands 测试、App/CloudManager/DownloadQueue Vitest 及生产前端构建。
2. 仅在工作区干净时按 MSVC x64 构建 `src-tauri/target/release/versadown.exe`；不生成 installer。仅停止该精确路径的旧进程，再启动同一 EXE。
