# Task 6：授权事件与持久失败任务恢复

- 基线：`efef154`；分支 `codex/cloud-provider-architecture-v2`。
- 单一实现者完成，没有派生子代理。按 TDD 与 verification-before-completion 技能执行。
- 只处理授权完成、个人下载/目录任务恢复及其前端入口；没有分享计划 SQLite 迁移、上传流式或 Task 7 工作。

## 实现

`cloud-authorization-state` 只包含 `{ provider, state, account }`。`authorization_completion_event` 检查实际凭据存在、维护状态为 Ready、account ID 与维护记录一致。三个显式成功出口统一发布：网页完成（包含手动按钮及 WebView page-load）、密码登录（包含 native/App/browser fallback）、失败任务显式恢复（包含 profile/password 路径）。发布位于凭据和 supervisor 提交及相关 profile 元数据保存之后，随后关闭该 provider 官方窗口，异步启动任务恢复。Task 5 后台 refresh/普通读取静默恢复不调用发布函数，不会关闭或打开授权 UI。

同一 provider 的网页完成用独立 gate 串行处理，避免页面加载与手动/watchdog 同时验证。WebView 页面加载主动调用完成入口，前端不再负责短周期检测。CloudState 仅缓存每个 provider 最近一次已经成功的非秘密 CloudAccount，供成功窗口关闭后丢事件的 watchdog 返回结果。新网页授权在 completion gate 下先清空，logout 清空；读取会重新核对凭据、Ready 状态和 account ID。缓存不保存任务 ID 或失败任务集合，不是持久恢复的数据源，退出进程即销毁。

新增 `resume_reauthorized_provider_tasks(provider)` Tauri 命令及前端封装。每次从 TaskStore 查询 failed 且 failure_code 为 not_authorized / credentials_expired / web_auth_required 的个人任务，以及未关联 task 的授权失败目录 leaf。按 provider 和账号绑定筛选，排除普通失败、未知 owner、其他账号/网盘与回收目录关联任务。恢复运行在每 provider 单独 gate 下，个人任务复用 compare-and-resubmit，目录复用现有叶子 retry/source gate。任务操作前重新检查状态和失败码，沿用已有重复下载/文件覆盖决策，未自动批准覆盖。结果更新仍通过 download-task-updated 发布父子任务投影。

TaskStore 增加 `cloud_download_accounts(id PRIMARY KEY, provider, account_id)` 附属表。它在原有 migrate 成功后以 CREATE TABLE IF NOT EXISTS 创建，不修改原 task/group 数据形状、旧 user_version 或稳定 source。绑定只能首次写入，不能被新账号覆盖。个人任务在初次入队前绑定；目录在根任务发布/所有叶子提交前绑定。下载解析前后核对当前账号，已绑定任务重试也核对账号。旧数据库未知 owner 的任务保守排除自动恢复，不猜测它原本属于哪个账号。

`reauthorization_provider` 同时识别个人任务、虚拟 leaf、目录父 ID（包括已关联失败 task 的 leaf），普通错误不能触发重新授权。

前端 CloudManager 仅在网页授权活跃时订阅，事件立即接纳账号并退出等待；Workspace 通过现有 onAccountAuthorized 更新账号/标签，同时清除该 provider 旧恢复错误。App 下载队列有独立旧轮询入口，也改为同样事件驱动。cloudApi 共享一个原生 listener，按 provider 分发，最后一个订阅者退出时释放；异步注册晚于卸载也清理。完成 latch 忽略重复事件和晚到 watchdog 结果；watchdog 每 10 秒一次，前次请求结束后才安排下一次，完成或卸载停止。

## RED → GREEN

1. 首先写数据库关闭/重开后的个人任务和目录 leaf 用例。为运行行为断言加入最小空接口后，分别实际失败：找到 0 项、预期 3 项。没有使用 CloudState pending 集。实现查询后个人用例复用真实 DownloadService / compare-and-resubmit，录制引擎边界收到恰好 3 次 add_uri；每个旧快照提交第二次返回 None。目录三种授权码各重试一次，普通 network_error leaf 保留，第二次数据库发现为空。
2. 初始三个前端新用例均 RED：事件后文件树仍不存在、事件后 workspace tab 不存在、前 9999ms 旧 watchdog 调用 6 次而预期 0。watchdog 最初漏一次微任务 flush，修正后以上述真实旧行为重新确认 RED。实现后 GREEN。
3. App.live 两个授权用例 RED：事件后仍显示等待说明；前 9999ms 旧实现完成请求达 10 次。事件与 10 秒串行 watchdog 实现后，加原密码自动恢复用例共 3 项 GREEN。
4. 目录父 ID 用例 RED（返回 NotAuthorized），增加父/leaf 查找后 GREEN；之后补充回收父任务关联的 auth-failed task，旧查询仍返回任务而断言期望空，补排除条件后 GREEN。
5. Ready 事件资格用例 RED：凭据尚未持久化仍能构造成功事件。增加凭据/维护状态/owner 检查后 GREEN，序列化精确断言仅非秘密三字段。
6. 窗口关闭后完成结果缓存用例 RED：返回 None；缓存实现后 GREEN，并验证新尝试清空、logout 状态不能复用。
7. Workspace 补充用例 RED：新账号标签已出现但旧会话错误仍显示；清理对应 provider 错误后 GREEN。
8. 原生 listener 单例用例 RED：两个订阅实际注册 2 次，预期 1；共享 listener 后 GREEN，包含 provider 分发、最后订阅释放和重复清理幂等。

中间编译反馈（缺少 move 捕获、String/Uuid 类型、测试误用 CloudAccount 不存在的 expires_at 字段）与个人测试最初误用 Failed→Parsing 普通 transition 均已修正，不计为行为 RED。个人测试最终使用真实的现有 retry service。旧定向前端测试的 800ms/1800ms 时间断言改为 10 秒；自动完成场景改为显式投递原生事件，不通过延长真实时间等待掩盖行为。

## 实际验证

Rust 命令使用 `C:/Users/deepin/.cargo/bin/cargo.exe`，设置 `CARGO_TARGET_DIR=.tools/test-target`。这是跨前后端授权/任务契约改动，按 AGENTS.md 使用定向测试与发布构建；额外 App.live 和 cloudApi 是真实调用入口、共享 listener 的直接消费者，store 用例覆盖附属表创建对旧数据库启动的影响，没有运行全量测试。

| 命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_session --test cloud_directory_download` | 最后组合 18 + 14 = 32 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib cloud::commands` | 90 通过，241 未运行 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib store::tests` | 4 通过，327 未运行；包含 legacy version eight 数据库启动 |
| `node_modules/.bin/vitest.cmd run src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx src/lib/cloudApi.test.ts --exclude '.worktrees/**'` | 96 + 16 + 5 = 117 通过 |
| `node_modules/.bin/vitest.cmd run src/App.live.test.tsx --exclude '.worktrees/**' -t 'reauthorized Baidu\|automatic password renewal'` | 3 通过，50 未运行 |
| `pnpm build` | 两轮均退出 0；最终 62 modules，产出 dist |
| `./scripts/build-windows.ps1` | 两轮退出 0，release 分别 50.66 / 48.10 秒；最终 artifacts/versadown.exe 25,444,352 字节及 aria2 sidecar |
| `rustfmt --edition 2021 --config skip_children=true`（本次 Rust 文件） | 退出 0 |
| `git diff --check` | 当前退出 0 |

首次 Vitest 因 Windows sandbox 无法读取祖先目录而启动失败，随后自动批准沙箱外定向命令并正常执行；未把启动错误计为 RED。构建保留原有 dead_code/linker/rustup 路径提示、Vite 混合静态与动态 window 导入提示，无新增编译错误。

## 自审与边界

- 查询恢复以磁盘失败码和首次账号绑定为准，内存 pending 集与其方法已删除；无 token、cookie、password、临时签名 URL 加入事件或新表。
- 原有未知账号的旧任务不会自动恢复，需要用户在原账号下手动处理/重新添加；现有重复下载要求确认的任务仍保留原决策，不绕过覆盖保护。
- 新增表与任务创建是两个持久操作，进程在绑定前退出可能留下未知 owner；此类任务保守不自动恢复。该行为不会把任务绑定到错误账号。
- 已测试 provider/账号隔离、普通失败保留、重复 retry 不再次入队、父子目录投影、回收父任务排除，以及旧数据库重开。没有真实服务商账号、真实 WebView 页面或长时间联网 E2E。
- 完成事件 payload 按规格不含 attempt ID。当前活跃订阅会忽略其他 provider、非 Ready、重复、完成/卸载后的回调；极端跨两个独立授权尝试延迟投递的同 provider 旧事件无法单靠三字段判龄。后端完成缓存由 native 成功路径写入并重新核对当前维护状态，下载恢复另按数据库 owner 核验，事件不授予任务账号权限。

## 审查修复第 1 轮（基线 9423250）

仅修复审查确认的 3 项 P2，没有扩展 Task 7 或进行额外边缘重构。

### RED / GREEN

- 后端 2 项实际行为 RED：oneshot 可控解析返回前回收父目录，旧实现仍创建任务；两个可控 retry 同时观察未关联 leaf，后到 Duplicate 将成功 task_id 清成 None。修复后 2/2 GREEN，额外在同一用例断言迟到 failure 不清关联，以及 Parsing 已认领任务阻止回收。
- 前端 3 项实际行为 RED：CloudManager begin 延迟返回期间重复点击调用 2 次（预期 1）；旧事件直接接纳账号导致下一步完成按钮消失；App 队列重复重新授权调用 2 次（预期 1）。修复后 3/3 GREEN。CloudManager 用独立可控 completion-cache future 验证上一轮结果在新一轮 begin 后返回，不接纳旧账号、不清理新轮等待；随后当前完成事件只接纳新账号一次。
- App RED 首次漏切换失败 tab，修正测试 setup 后重新取得上述调用次数行为 RED。Rust 广覆盖首次失败是旧 fixture 直接回收仍有 Parsing 兄弟的目录；改为先将兄弟标为终态，符合真实命令已有的回收前置条件，再次通过。不将这两项 setup 调整计为产品行为 RED。

### 修复

- `claim_cloud_directory_leaf_task` 在同一 SQLite transaction / TaskStore connection lock 下完成父目录状态、provider/owner 和未关联检查，以及 task 创建、owner 写入、leaf 关联。URL 解析期间不持锁；如果回收已提交，认领返回 None，零任务创建、关联和引擎提交。反向顺序中，认领写入的 Parsing 任务是提交预约；回收在同一 connection lock 下拒绝有活跃 linked task 的父目录，不能插入认领与 engine submit 之间。
- 自动和手工 leaf retry 共用 TaskStore leaf gate，持锁后重新读取父状态与 task_id。link 使用未关联/同 task ID 条件；failure 只更新 task_id IS NULL 的虚拟 leaf，不再清空已有 task_id。稳定 source gate 继续保留。
- CloudManager 与 App 的活跃 begin 在 await 前同步 single-flight。CloudManager begin/complete/cache query/watchdog 绑定本地 generation，清理只影响匹配代次；App 同样防止 pending 和等待期重入，完成查询及 watchdog 用 generation 与 disposed 检查。
- 三字段事件现在仅作唤醒，不直接使用 event.account 完成 UI。新增只读 `cloud_completed_authorization` / `getCompletedCloudAuthorization`，读取后台已验证完成缓存，不启动 WebView 验证。CloudManager 在 begin 注册完成后才查缓存，之前事件仅记一次延迟唤醒。原生 listener 仍是 singleton，10 秒串行 watchdog 保留，完成/卸载释放。
- 缓存生命周期补充：仍是进程内每 provider 非秘密最近成功账号，读取仍校验凭据 / Ready / owner。真正创建新授权窗口时清空，复用已有窗口不清空，logout 清空，进程退出销毁。旧事件只能触发当前缓存查询；旧轮已在途的查询结果受 generation 限制，不能终止新轮。此项取代上文初次实现关于旧事件被直接消费的边界说明。
- 本轮没有新增表或迁移；初次 owner 附属表兼容规则不变。目录 task 创建、账号绑定和 leaf 关联现在一起提交；个人 task 的已有保守未知 owner 行为不变。

### 最终实际验证

Rust 仍使用 `CARGO_TARGET_DIR=.tools/test-target` 及本地 Cargo 路径。除下列定向范围外，没有运行全量测试。

| 命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_directory_download review_` | RED 2 失败 → GREEN 2 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_directory_download --test cloud_session` | 16 + 18 = 34 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib cloud::commands` | 90 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib store::tests` | 4 通过（包含旧数据库启动） |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib commands::tests::cloud_directory_group_` | 4 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib commands::tests::virtual_failed_directory_group_moves_to_recycle_without_local_members` | 1 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib commands::tests::failed_share_directory_stays_one_root_after_moving_to_recycle` | 1 通过 |
| `pnpm exec vitest run src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx src/lib/cloudApi.test.ts --exclude '.worktrees/**'` | 98 + 16 + 5 = 119 通过 |
| `pnpm exec vitest run src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx src/App.live.test.tsx --exclude '.worktrees/**' -t 'review \|completion event\|authorization events\|reauthorized Baidu'` | 7 通过 |
| `pnpm exec vitest run src/App.live.test.tsx --exclude '.worktrees/**' -t 'review shares\|reauthorized Baidu\|automatic password renewal'` | 4 通过 |
| `pnpm build` | 通过，62 modules |
| `./scripts/build-windows.ps1` | 通过，release 1m 31s，产出主程序和 aria2 sidecar |
| `rustfmt --edition 2021 --config skip_children=true`（5 个本轮 Rust 文件）及 `git diff --check` | 通过 |

自审已核对最终 claim/recycle 的共用临界区、leaf gate 后重读、条件 failure/link 和三处 UI 的清理路径。本轮没有真实服务商 / WebView 联网 E2E；仍保留原有编译提示，无新增编译错误。没有其他阻塞项。
