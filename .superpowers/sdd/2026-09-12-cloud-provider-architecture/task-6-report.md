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
| `./scripts/build-windows.ps1` | 两轮退出 0，release 分别 50.66 / 48.10 秒；最终 artifacts/alldownloader.exe 25,444,352 字节及 aria2 sidecar |
| `rustfmt --edition 2021 --config skip_children=true`（本次 Rust 文件） | 退出 0 |
| `git diff --check` | 当前退出 0 |

首次 Vitest 因 Windows sandbox 无法读取祖先目录而启动失败，随后自动批准沙箱外定向命令并正常执行；未把启动错误计为 RED。构建保留原有 dead_code/linker/rustup 路径提示、Vite 混合静态与动态 window 导入提示，无新增编译错误。

## 自审与边界

- 查询恢复以磁盘失败码和首次账号绑定为准，内存 pending 集与其方法已删除；无 token、cookie、password、临时签名 URL 加入事件或新表。
- 原有未知账号的旧任务不会自动恢复，需要用户在原账号下手动处理/重新添加；现有重复下载要求确认的任务仍保留原决策，不绕过覆盖保护。
- 新增表与任务创建是两个持久操作，进程在绑定前退出可能留下未知 owner；此类任务保守不自动恢复。该行为不会把任务绑定到错误账号。
- 已测试 provider/账号隔离、普通失败保留、重复 retry 不再次入队、父子目录投影、回收父任务排除，以及旧数据库重开。没有真实服务商账号、真实 WebView 页面或长时间联网 E2E。
- 完成事件 payload 按规格不含 attempt ID。当前活跃订阅会忽略其他 provider、非 Ready、重复、完成/卸载后的回调；极端跨两个独立授权尝试延迟投递的同 provider 旧事件无法单靠三字段判龄。后端完成缓存由 native 成功路径写入并重新核对当前维护状态，下载恢复另按数据库 owner 核验，事件不授予任务账号权限。
