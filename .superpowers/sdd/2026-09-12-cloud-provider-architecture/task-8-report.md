# Task 8 — 会话锁收窄与下载源锁回收

本项仅完成 Task8。未修改前端契约、上传正文或调度器，未执行 Task9。使用 TDD 与 verification-before-completion；没有派生子代理。

## 实现与边界

- Task5 已把主分享解析和个人下载读取接入 AuthSupervisor 的已发布会话快照，因此原有分享轮询本身已不持 provider 刷新锁。本项保留该路径，并把仍使用独占 `run` 的分享清理改为 snapshot + Mutation。没有重复改造 `commands/download.rs` 的既有快照读取。
- 分享解析可能执行转存，整个组合操作按 Mutation 执行；鉴权恢复不会重放已经执行过的转存。分享检查仍是 Read。此策略也意味着该组合调用在转存前发生鉴权失败时，会提示用户重新执行。
- `AuthSupervisor::run_checked` 在工作前、工作完成后、实际写回临界区及进入恢复前检查取消状态。普通 Provider/Transport 错误仍保留服务端已经轮换的有效 session；显式取消或超时才终止此次快照提交和恢复。
- session 写回同时检查 maintenance revision/account、published revision/account/binding（lineage）以及原始凭据字节，拒绝 A→B→A 的旧快照覆盖。接受的凭据变化递增 revision，但不清空原有失败退避；返回本次提交后的 revision，使 action ticket 轮换后发生的鉴权失败仍可执行一次恢复。
- `ShareWorkControl` 对分享网络请求实施协作取消和 5 分钟整体期限。调用 future 被丢弃会设置取消标志；每个新请求前检查，最终结果写回前再次检查。已经收到的成功转存响应仍会解析并写入原有 owner/cleanup journal，不丢弃已知的个人文件 ID。
- 协作取消不会强制中断已经发送的 blocking HTTP 请求；它在该请求返回或原有 HTTP 超时之后生效，不再发起后续轮询。没有新增常驻任务、线上请求或上传改造。
- 迅雷已转存文件的个人链接刷新尊重 snapshot 的 `automatic_session_recovery=false`，把 token 更新交给 AuthSupervisor；原有 request action ticket preflight 保留，实际 mutation 不自动重放。
- `CloudDownloadSourceGate` 使用 `HashMap<String, Weak<tokio::sync::Mutex<()>>>`。acquire 在 map 锁内 upgrade/create 并清理失效项，离开 map 锁后才等待 source 锁。holder 和等待中的 acquire 均持 strong owner，因此 cleanup 不会把同一 source 分裂为两个活 mutex。公开 `cleanup()` 返回清理后数量，便于确定性回收；share 仍按稳定 plan 归一化互斥。

## RED / GREEN 证据

1. 源 gate 最初缺少 cleanup API（编译 RED）；补上沿用强引用存活判定的接口骨架后，`source_gate_cleanup...` 明确行为失败：清理 1,000 个已释放 source 后实际 1,000、预期 0。改为 Weak 后通过；同一测试确定性轮询 pending acquire，证明 holder 和 waiter 保留、最后释放后归零。
2. `snapshot_rotation_cannot_write_back_after_unversioned_account_round_trip` 先失败：`obsolete-rotation` 覆盖了 `original`；增加 lineage/account/revision CAS 后通过。
3. `failed_transfer_does_not_commit_its_private_session_rotation` 先失败：取消/超时后的私有轮换落入 durable store；最终使用独立完成检查解决。自审中补充普通失败保留有效轮换与 action ticket 变化后恢复两条测试，分别看到旧凭据未更新、refresh=0 的行为失败后修复。
4. `cancelled_or_expired_share_poll...` 先看到取消控制仍放行网络请求的行为失败，补上检查后通过。
5. 真实 Xunlei adapter 的 `/tasks/transfer-task` 被 channel 阻塞时，同 provider account 和 directory 必须在 release 前完成；随后 Barrier 并发失败读取只执行一次 refresh，转存 POST 仅一次。此行为已由 Task5 满足，是回归验证，不冒称本项首次实现或取得对应基线 RED。
6. 取消该真实 poll 并替换账号后，已返回的完成响应仍记录 `created-file`，旧 worker 不刷新、不覆盖 replacement account。测试无需 sleep。
7. 迅雷 recorded resolver 新测试先观察到 snapshot 自行访问 token refresh；修正测试 fixture 使用现代协议 `error_code=16` 后，supervised 模式回传鉴权错误且不访问 token endpoint。早期 fixture 把普通 provider code 401 当作现代鉴权码造成一次测试失败，已按现有协议修正；没有为测试修改协议映射。

## 最终实际验证

Rust 命令使用 `C:/Users/deepin/.cargo/bin/cargo.exe`，环境 `CARGO_TARGET_DIR=.tools/test-target`。按 AGENTS.md 的下载核心/连接器定向验证等级执行，没有全量 Rust 或前端 Vitest。

| 命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_coordination --test cloud_directory_download --test cloud_sessions --test cloud_auth_supervisor --test cloud_session` | 5 + 17 + 33 + 19 + 18 = 92 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib share` | 146 通过，191 未运行；包括七家分享、SQLite、owner/cleanup journal 与直接命令消费者 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib session` | 55 通过，282 未运行；包括新增迅雷 supervised recorded resolver |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_xunlei supervised_xunlei_keeps_action_ticket_preflight` | 1 通过，34 未运行 |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows.ps1` | 首次因沙箱内 esbuild 读取祖先目录 Access denied 退出 1；同脚本获准权限重试后退出 0，前端构建成功、Rust release 1m17s |
| `git diff --check` | 通过 |

发布产物：当前工作树 `artifacts/alldownloader.exe` 与 `artifacts/binaries/aria2-next-x86_64-pc-windows-msvc.exe`。保留已有 dead-code、linker/canonicalize 信息及前端 dynamic/static import 警告；未新增构建错误。发布构建本身没有触发全量测试。

自审限定本项 diff：source map/source mutex 锁顺序、snapshot CAS/取消边界、mutation 不重放、迅雷 preflight，以及原有 owner/cleanup journal 均有对应验证。提交说明：`perf(cloud): narrow session locks and reclaim source gates`。

## Fix round 1/5 — 承接同次 CAS 后的 ProfileContinuation

本轮仅修聚焦审查的一个 P2：due proactive 恢复失败建立 continuation 后，普通操作合法轮换 action ticket/cookie，使 revision 递增；旧 continuation 因版本不匹配而跳过 ProfileOnly。没有派生代理，也没有新增其他改造。

严格 TDD：先运行 `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_auth_supervisor rotated_ -- --nocapture`，3 条均行为失败：Read 与 Mutation 没有得到 profile session，attempt 变化的 Read 意外执行第二次。随后运行强化过的 `review_failed_profile_continuation`，观察到轮换后跳过 ProfileOnly、没有返回预期的 WebAuthorizationRequired。

修复只在本次 `persist_snapshot` 成功 CAS 返回新 revision，且 ticket 原 revision、account、lineage 都与原 snapshot 一致时，承接 ticket 的 revision。实际 ProfileOnly 之前仍在 provider 锁内重新检查 account、lineage、失败次数、next_allowed 和 RiskBlocked；版本承接本身不授予新的登录 attempt。其他调用的版本变化不继承该资格。

新增 Read/Mutation 正向测试证明恢复 phase 为 Proactive→ProfileOnly；Read 仅重试一次，Mutation closure 仍只执行一次并返回 SessionRenewedRetryOperation。表驱动负向测试覆盖账号替换、A→B→A lineage、持久 failure 次数/期限及 RiskBlocked；原跨调用/重启测试加入 action ticket 轮换，证明失败的 profile attempt 仍保留 5 分钟退避，下一调用及重启都无法重用 continuation。所有新增测试无需 sleep。

本轮最终实际验证（仍使用 `.tools/test-target`）：

| 命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_auth_supervisor` | 22 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_coordination --test cloud_session --test cloud_sessions` | 5 + 18 + 33 = 56 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib share` | 146 通过，191 未运行 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib session` | 55 通过，282 未运行 |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows.ps1` | 使用此前获准的构建权限，退出 0；复用前端 dist，release 1m09s，主程序与 sidecar 已生成 |
| `git diff --check` | 通过 |

本轮没有执行前端测试或任何全量测试。生产改动只在 `auth_supervisor.rs`；另更新对应集成测试与本报告。提交说明：`fix(cloud): preserve profile continuation across session rotation`。
