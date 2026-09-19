# Quark Download Connection Budget Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 仅为夸克网盘下载实现每个任务的动态连接预算：优先让目录中的不同文件各用 1 个连接，在文件数不足系统“单任务并发连接数”时才给仍大于等于 1 MiB 的文件增加 Range 连接；任意时刻的目标连接和不超过该设置。

**Architecture:** 新建夸克插件私有协调器，纯函数先根据目录条目、任务状态与剩余字节计算确定性分配，再由一个仅在存在未结束夸克任务时运行的单飞轮询器调用 aria2 `changeOption`。通用下载命令只增加夸克专用的分流调用来获得首次提交选项并唤醒协调器；通用目录调度、注册表、其他提供商和引擎服务不承载策略。

**Tech Stack:** Rust、Tauri async runtime、现有 SQLite `TaskStore`、现有本机 aria2 JSON-RPC、Tokio、Cargo 定向测试。

**Spec:** `docs/superpowers/specs/2026-09-20-quark-download-connection-budget.md`

---

## 已确认的实现边界

1. 系统预算取 `AppSettings.aria2.max_connections_per_server`，记作 `B`，其合法范围已是 1–16。
2. “线程”指 aria2 单个 GID 的 `split` 与 `max-connection-per-server` 目标值，而非 CDN 最终实际建立的 TCP 数；后者可能低于目标，不能作为预算不变量。
3. 一个 `CloudDirectoryGroup` 是一个“单任务”连接池；普通夸克文件是只含一个文件的连接池。互不相关的根任务继续由现有最大并发下载/提供商准入控制，不相互借用预算。
4. 目录持久化在任何叶子 URL 提交前已经保存全部 `CloudDirectoryGroupItem`。未失败且未结束的未链接条目也要预留 1 个单位，避免首个大文件扩容后，下一文件提交时短暂超出 `B`。
5. `DownloadService` 已能使用 `aria2.changeOption`，但其底层 RPC 客户端并未公开。为保持策略不进入共享引擎，夸克插件将从当前设置的回环 RPC 端口和 token 构造短生命周期 `Aria2RpcClient`，仅发送已允许的两个可变选项。调用前后用 `TaskStore` 重新比对 GID，过期调用只丢弃并在下次协调重试。
6. 现有 `queue_cloud_dispatch`、目录成员入队和 `TauriDownloadEvents` 均在通用层；零行通用改动无法让插件在“提交前”预留新文件。因此本计划保留一个无策略的夸克分流入口，所有判断和状态仍在插件文件中。

## 分配定义

令某一任务池中未失败且未结束的文件数为 `N`。每个文件先占 1；若 `N < B`，仅将 `B-N` 的余量轮转给可信剩余 `>= 1_048_576` 的已提交文件。未链接正常目录条目预留 1；已失败、完成、取消或回收条目不占预算。小于 1 MiB、未知大小的已提交文件保持 1，允许预算闲置。

## 实施任务

### Task 1: 夸克私有预算算法

**Files:** Create `src-tauri/src/cloud/quark/download_scheduler.rs`; modify `src-tauri/src/cloud/quark.rs`。

先写失败单元测试，覆盖 `B=4,N=6`、`B=4,N=3`、`B=8,N=2`、未知大小、`<1 MiB`、`==1 MiB`、未链接条目预留、完成/失败释放。实现 `MIN_PARALLEL_REMAINING_BYTES`、`QuarkBudgetScope`、`QuarkBudgetMember` 和 `allocate_connections`；只为有 GID 的成员返回目标值。该算法不访问全局设置，不进入共享 `cloud/download_scheduler.rs` 或 `cloud/download_dispatch.rs`。

### Task 2: 插件协调器和初始提交选项

**Files:** Modify `src-tauri/src/cloud/quark/download_scheduler.rs` and `src-tauri/src/cloud/quark.rs`。

从 `TaskStore` 快照构建目录/单文件池，目录按 `item_index` 稳定排序。`QuarkDownloadCoordinator` 为插件私有单飞循环，首次提交、恢复或重试后被唤醒，在存在夸克未结束任务时每 500 ms 重新读取设置并重算。仅在 `(GID,目标值)` 变化时，使用短生命周期回环 `Aria2RpcClient` 的 `changeOption` 同时设置 `split` 与 `max-connection-per-server`。调用前后复核当前 GID；错误只退避重试，不改任务状态或 URL。

### Task 3: 最小分流接入与生命周期

**Files:** Modify `src-tauri/src/cloud/commands/download.rs`, `src-tauri/src/cloud/quark/download_scheduler.rs`, and `src-tauri/src/cloud/quark.rs`。

在 `queue_cloud_dispatch` 中仅对 `CloudProvider::Quark` 请求插件提供首次 `max_connections_per_server` 并唤醒协调器。非夸克目录仍为 1，非夸克单文件保留通用策略。夸克绕过注册表的每文件 4 线程裁剪，始终受 `B` 约束。普通/分享/目录/重试/URL 刷新和恢复路径均复用此入口。

### Task 4: 定向测试与兼容性验证

**Files:** Modify `src-tauri/src/cloud/quark/download_scheduler.rs` and `src-tauri/tests/aria2_live.rs`; use the existing `cloud_directory_download` fixture without exporting a test-only scheduler API。

在插件私有模块中增加 loopback RPC 客户端契约测试，验证只对当前夸克 GID 调用、两个 option 数字一致、无变化不重复、GID 替换失效、低于阈值不扩容、RPC 失败不重新提交。增加目录首次提交夹具，验证未链接成员的预留。若随附 aria2-next 可用，再以本地 Range HTTP fixture 验证活动 GID 的 `changeOption` 可增减这两个选项，不使用真实网盘数据或凭证。

### Task 5: 验证与提交边界

实施时运行：

```powershell
& 'C:\Users\deepin\.cargo\bin\cargo.exe' test cloud::quark::download_scheduler --lib -q
& 'C:\Users\deepin\.cargo\bin\cargo.exe' test --test cloud_directory_download scheduler_directory_pause_stops_linked_gidless_resolver_until_resume -q
& 'C:\Users\deepin\.cargo\bin\cargo.exe' test --test aria2_live bundled_aria2_next_changes_active_http_range_parallelism -q
```

不默认运行全量 Rust/Vitest；本请求没有生成二进制授权。提交仅包含夸克调度器、最小分流和相应定向测试，不包含其他工作树改动。
