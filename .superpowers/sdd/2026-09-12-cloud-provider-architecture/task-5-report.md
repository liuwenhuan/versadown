# Task 5：Durable Authorization Supervisor

- 基线：`13760e4`；分支 `codex/cloud-provider-architecture-v2`。
- 实现者独立完成，没有派生子代理。
- 交付范围：后端长期授权维护、短会话临界区、读取至多重试一次、mutation 不重放；没有实现 Task 6 的 UI event 或数据库失败任务恢复，也没有实现上传流式切片/并发调度。

## 实现与接口

新增 `cloud/auth_supervisor.rs`，提供 `AuthSupervisor`、`AuthMaintenanceRecord`、`AuthHealth` 和 `SessionSnapshot`。记录的十个字段对应规格；不保存 password/cookie/token。维护记录通过现有 `CredentialStore` 保存，实际应用使用原有原子发布的 DPAPI store，没有新增明文秘密文件。

`AuthSupervisor::new` 注入秒级 clock 和 jitter。刷新时间为 80% 剩余 TTL 与到期前 5 分钟二者中较早者，再提前 30–120 秒；jitter 被限定在这个范围。Quark/Baidu/UC 永远没有定时 keepalive。失败在联系服务商前先写入尝试次数及 5 分钟、30 分钟、2 小时的下一期限；这使中途退出也不会在重启后立即重登。第三次失败后等待显式授权；Captcha 进入持久 RiskBlocked。成功和显式授权清零失败计数。

每 provider 使用独立 Tokio mutex。刷新、重登、同账号授权替换和 revision 检查保存使用该锁；普通命令获得自己的 session snapshot，网络请求不持有刷新锁。snapshot 的 session 修改只在 revision 与原始 session 都匹配时落盘，旧操作不能覆盖已切换账号或退出登录后的存储。同 provider 的失败请求共享一次恢复，不同 provider 独立执行。读取恢复后最多再运行一次 closure；mutation 恢复后返回既有 `session_renewed_retry_operation`，closure 不再运行。

`CloudState` 在初始化时恢复已有维护记录；七个独立后台循环每 30 秒检查本地状态，仅达到刷新期限时发网络请求。循环持 Weak 引用，状态释放后退出。已存在的失败记录原样沿用，不因创建新 supervisor 重置退避。

Task 4 的 `ProviderAuth` 增加默认 `refresh_session` seam，四个 token/App provider 适配后使用固定恢复顺序：refresh → 后端已保存密码 → 持久 profile 一次捕获 → 现有官方可见授权入口。非鉴权错误不尝试下一登录方式；普通 captcha 不自动重试。按需捕获窗口验证同一账号后才保存会话，完成或错误都会关闭窗口；没有常驻隐藏 WebView。七 provider 的显式网页授权采用各自稳定 profile，退出登录清理该 provider profile。

PikPak/Aliyun/Xunlei 快照消费者禁用客户端内置 token 刷新及失败请求重放，由管理器统一决定恢复。PikPak 保存密码重登沿用该账号 device ID。Aliyun 保留 `expires_in` 换算的绝对过期时间；网页登录已知绝对时间也被保留。Tianyi App 使用已有 access token 换取新的 SessionKey/SessionSecret，保留响应未提供的 token 字段，拒绝不一致的 loginName。

天翼协议核对来源：[AlistGo/alist 的 189pc refreshSession 实现](https://github.com/AlistGo/alist/blob/main/drivers/189pc/utils.go)。仅参考 App refresh 的请求/响应字段，没有复制其自动重放调用链。

七 provider 上传 adapter 改为在 session snapshot 上执行字节传输；PikPak 上传初始化的旧重放循环被移除。此调整是 Task 5 的 mutation/锁不变量所需，不改变上传 payload、切片或调度。原有 Tauri 导出、JSON 错误码、稳定下载来源格式和密码仅后端使用的契约保留。

## RED → GREEN 证据

所有 Rust 调用使用 `C:/Users/deepin/.cargo/bin/cargo.exe`，并设置 `CARGO_TARGET_DIR=.tools/test-target`。

1. 首先新增 `cloud_auth_supervisor` 行为用例。第一次编译因缺少模块失败，这只算接口缺失，不算行为 RED。加入最小可编译空实现后，同一命令实际运行 6 项：5 项断言失败、cookie 不 keepalive 基线通过。失败分别是调度 None、失败次数仍 0、状态仍 Disconnected、mutation 没有 RetryOperation、并发读取没有恢复。实现后 6 项全部通过。
2. `--test cloud_auth_supervisor native_snapshot_operation`：真实 native driver 下 PikPak 401 产生了 2 个 HTTP 请求，期望 1，RED；禁用 snapshot 内部自动恢复后 PikPak/Aliyun 两分支 GREEN。
3. `--test cloud_tianyi tianyi_supervisor_refreshes`：原默认 ProviderAuth 返回 false，RED；App refresh 实现后 GREEN。同文件补充无效 access token/不同账号响应均保留原 session。
4. `--test cloud_aliyun aliyun_refresh_preserves`：实际 refresh 成功但 store 没有过期时间，RED；保存绝对过期时间后 GREEN。
5. `--test cloud_sessions saved_password_recovery_reuses`：重登生成了新的随机 device ID，RED；恢复使用旧账号 device 后 GREEN。
6. `--test cloud_upload rejected_upload_initialization`：旧上传初始化恢复后仍重放 mutation，结果不是 RetryOperation，RED；移除重放循环后 GREEN。
7. `--test cloud_auth_supervisor actual_security_challenge`：captcha 后 health 仍 Ready，RED；持久 RiskBlocked 后 GREEN。
8. `--test cloud_sessions browser_token_capture_preserves`：输入已知 expiry 被改成 u64::MAX，RED；保留已知绝对 expiry 后 GREEN。
9. `--test cloud_xunlei supervised_xunlei_keeps_action_ticket_preflight_but_never_replays_a_rejected_mutation`：supervised fast path 跳过动作票据，实际 1 个请求而非预期 preflight + mutation 两个，RED；保留操作前 action ticket 获取、仍禁止实际 mutation 失败重放后 GREEN。拒绝分支最初误用旧协议 4121，改用既有现代 Web 映射 16/unauthenticated 后通过，没有改动服务错误映射。

新增测试也覆盖了：30/120 秒及越界 jitter；Ready→Refreshing→Ready；三段退避和第三次停止；重启 supervisor 不绕过退避；成功清零；同 provider singleflight；不同 provider 可进入维护；刷新持锁期间普通读取仍能完成；旧快照不能覆盖新账号；密码只从后端读取；网络故障不启动密码/profile；恢复旧 session 不清空已持久失败记录；真实 DPAPI store 关闭后重开仍遵守退避。新增并发/时钟测试使用 Barrier、注入 clock 和同步事件，没有通过 sleep 猜测执行顺序。

## 验证范围与实际命令

本次是跨网盘授权基础设施修改，按 AGENTS.md 执行相关 Rust 定向测试与 Windows 发布构建。额外选入 Aliyun/Xunlei/upload/provider_contract，是因为新增 refresh seam、快照行为和上传禁止重放直接影响这些模块；不触发全量测试。没有改前端契约，未运行 Vitest 或独立前端构建。

| 实际命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_auth_supervisor --test cloud_session --test cloud_coordination --test cloud_sessions --test cloud_tianyi` | 中间验收 10 + 15 + 2 + 27 + 31 = 85 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_auth_supervisor --test cloud_session --test cloud_coordination --test cloud_sessions --test cloud_tianyi --test cloud_aliyun --test cloud_xunlei --test cloud_upload --test cloud_provider_contract` | 最后组合复跑 13 + 15 + 2 + 29 + 33 + 19 + 35 + 6 + 16 = 168 通过，退出 0 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib cloud::commands` | 90 通过，241 未运行 |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_auth_supervisor` | 最终补充 DPAPI 重开用例后 13 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_xunlei --test cloud_auth_supervisor --test cloud_upload` | 最终补充 Xunlei preflight 用例后 35 + 13 + 6 = 54 通过，0 失败 |
| `./scripts/build-windows.ps1` | 首次退出 0，release 1 分 27 秒；最新生产代码重建退出 0，release 17.60 秒；前端未变化，脚本复用 dist；产物 artifacts/alldownloader.exe（24,935,424 字节）及 aria2 sidecar |
| `rustfmt --edition 2021 --config skip_children=true`（仅本次变更 Rust 文件） | 退出 0 |
| `git diff --check` | 提交前确认退出 0 |

中间编译反馈包括未引入的 helper/module，以及误用了仓库未依赖的 chrono（随即移除，未新增依赖）；均不计作行为 RED。输出保留现有 rustup 路径提示、Windows linker 信息和少量历史 helper 的 dead_code 提示。旧 PikPak 初始化刷新 helper 和已退出运行路径的 active-WebView 重试 helper 已清除；部分旧纯函数仍由原有回归用例覆盖，生产不再调用。

## 自审与已知边界

- checked：没有把 token/cookie/password 放进维护 envelope；真正的 session 继续 DPAPI 保存；回传前端的凭据接口不变。
- checked：refresh mutex 不包住普通文件网络请求或上传字节传输；新快照 CAS 同时核对 revision 和原 session，logout 的旧请求不会恢复凭据。
- checked：Tianyi App refresh 保留未轮换字段、拒绝换账号；保存密码重登及 profile 捕获都检查既有账号，PikPak device 稳定复用。
- checked：普通请求只在真实 AuthorizationExpired/NotAuthorized 才启动恢复，Captcha 不重放；后台流程不创建窗口；profile 按需验证后关闭。
- checked：Xunlei 的 action-specific captcha ticket 是请求前准备，保留在本次 session snapshot 中；它不是长效会话轮换，实际修改请求始终只发一次。
- checked：所有任务队列、数据库恢复、事件通信和 Task 6 UI 工作保持在后续任务；显式重新授权入口仅接入固定授权顺序和成功清零维护状态。
- 未做真实服务商账号 E2E、真实 Windows WebView profile 交互或长时间联网挂机验证。上述证据是协议 fixture、真实后端协调、DPAPI 和发布编译验证。
- 老版本 cookie/session 没有可靠 account owner 时，不会静默接受浏览器里可能不同的账号；需要一次显式授权建立绑定。可用的旧密码 credential 提供既有账号 owner。
- Tianyi App 当前协议没有可用 TTL 字段，因此按实际鉴权失败刷新，不编造短周期 keepalive。没有已知 expiry 的 token/session 同样按需恢复。
- 单进程单账号的 provider guard 与现有 DPAPI 单键原子发布没有扩展成跨键事务。网络成功后如果维护记录保存失败，会返回 Storage，并沿用事前已持久的退避记录；已轮换 session 仍由原凭据键保存。

## 独立审查修复 round 1/5

基线 `2d07407`。依照 receiving-code-review 与 TDD 技能，先验证四项审查意见，再实现；没有派生子代理，没有 Task 6 或前端扩展。

### 行为 RED 与修复

`cargo test --manifest-path src-tauri/Cargo.toml --test cloud_auth_supervisor review_` 实际运行 4 项，0 通过、4 失败：

1. `review_account_replacement_never_replays_an_old_read_on_the_new_account`：Barrier 等待 A 请求发出后显式替换 B，旧实现用 B 重跑 closure 并返回成功，授权错误断言失败。现在已发布 snapshot 包含同一版本的 session/revision/account ID 和仅后端使用的授权 lineage；恢复前后的 binding 必须相同，账号替换或退出返回 `NotAuthorized`，不在 B 执行第二次 closure。同账号正常 refresh 保持 lineage；退出后即使重新登录 A 也建立新 lineage。
2. `review_proactive_auth_failure_can_continue_to_profile_in_the_same_run`：已知 expiry 到期的 Aliyun 分支在主动 refresh 被拒绝后，普通读取确认真实 auth error，但结果仍为 `WebAuthorizationRequired`，profile 成功分支未达到。现在 `RecoveryPhase` 区分 Proactive/AuthFailure/ProfileOnly；当前 run 内的 proactive 恢复穷尽 refresh/password 后，保留一次性 continuation（绑定 revision、失败次数、持久 deadline、lineage）。实际读取再确认鉴权失效时只继续尚未执行的 profile，不重新调用 refresh/password，也不增加一次失败计数。该许可不跨 run、不持久化；后续调用和重启仍受退避约束。修复用例同时验证 Aliyun/Xunlei，refresh 恰好一次、profile 恰好一次、读取两次。
3. `review_captcha_on_the_second_read_persists_risk_blocked_without_a_third_read`：第二次读取返回 Captcha 时，旧状态仍 Ready。现在第二次结果同样记录 RiskBlocked，保留 Captcha 错误，closure 共两次；mutation 原有单次约束保留。
4. `review_refresh_publication_never_exposes_new_session_with_old_revision`：真实 maintenance closure 已写入 new-token、尚未发布新 revision 时通过 Barrier 暂停，普通读取实际拿到 new-token 而不是上一完整版本 old-token。现在刷新期间读取短锁复制上一已发布版本；完整凭据/revision 更新后才共同发布。没有持网络锁执行普通操作。用例使用 Barrier + channel，2 秒 recv_timeout 只作死锁故障上限，并在断言前释放屏障清理线程，没有 sleep 或概率竞态。

前三项相关修复后，首次复跑为 3 通过/1 失败（仅 profile 用例）；加入 continuation 后 supervisor 17/17 通过。随后补充两项边界覆盖：profile 继续失败后，后续调用和重启仍保持一次失败及原 deadline；A 退出再登录 A 不能复活旧 mutation。

扩大到原 upload 集时发现 3 个旧入口回归：测试通过 native store 写入授权，但新快照缓存仍为启动时的空版本。修复为在 provider `try_lock` 成功时短临界区协调未版本化的旧凭据更新；刷新持锁时完全不读取半发布存储。未版本化替换建立新的 lineage，不能给旧操作授予重试许可。原 upload 6/6 恢复通过。初始测试的两处 Rust 借用编译反馈不计作行为 RED。

### 本轮实际验证

继续使用 `CARGO_TARGET_DIR=.tools/test-target`，没有运行全量测试。

| 实际命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_auth_supervisor --test cloud_upload` | 19 + 6 = 25 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_auth_supervisor --test cloud_session --test cloud_coordination --test cloud_sessions --test cloud_tianyi --test cloud_aliyun --test cloud_xunlei --test cloud_upload --test cloud_provider_contract` | 19 + 15 + 2 + 29 + 33 + 19 + 35 + 6 + 16 = 174 通过，退出 0 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib cloud::commands` | 90 通过，241 未运行，退出 0 |
| `./scripts/build-windows.ps1` | 退出 0，release 1 分 26 秒；复用未变化的 dist，发布主程序及 aria2 sidecar |
| `rustfmt --edition 2021 --config skip_children=true`（本轮 3 个 Rust 文件） | 退出 0 |
| `git diff --check` | 退出 0 |

曾将上述 integration 目标和 `--lib cloud::commands` 合并运行；Cargo 将名称过滤器应用到所有 integration 目标，该次只有 90 项命令测试实际运行，integration 全部 0 项。因此随后去除名称过滤器单独重跑并获得上表 174 项通过，没有把 0 项运行计入验收。

自审：维护 envelope 字段未增加、没有秘密；session 副本和 lineage 仅在后端内存中；普通网络工作不持 publication 短锁，存储同步仅在 refresh guard 可立即获取时进行；固定恢复顺序和所有前端错误码保留。仍未做真实账号 E2E、真实 WebView 交互或长期挂机；协议与持久化边界风险沿用上一节。
