# AllDownloader 自绘标题栏与 aria2 RPC 完整接入设计

## 目标

本设计补充 `2026-09-07-alldownloader-windows-beta-design.md`，解决三个已确认问题：下载管理器仍使用 Windows 原生标题栏；现有 aria2 仅按任务启动进程，界面状态和控制不代表真实下载状态；关闭下载管理器会销毁 `main` 窗口，导致悬浮球无法再次打开。

“完整接入 aria2”在本阶段指：下载管理器展示的传输状态、进度和控制均以 aria2 1.37.0 JSON-RPC 为唯一真实来源，并覆盖 HTTP/HTTPS、磁力和种子任务所需的产品级能力。不在界面暴露 aria2 的全部低层配置项或任意 RPC 调试入口。

## 主窗口与生命周期

主窗口在 Tauri 配置中设置 `decorations: false`，保留 Windows 原生阴影并使用应用自绘标题栏。标题栏包含品牌、当前页面标题、最小化、最大化/还原和关闭按钮；除按钮与可交互区域外的标题栏区域可拖动窗口，双击切换最大化。

主窗口的关闭语义统一为“隐藏”：自绘关闭按钮、Alt+F4 和系统关闭请求均阻止销毁并调用 `hide()`。下载任务、aria2 sidecar 和悬浮球继续运行。悬浮球菜单的“打开下载管理器”和“设置”总能获取仍存活的 `main` 窗口，先取消最小化、显示并聚焦；“设置”额外发送内部导航事件。

只有悬浮球右键菜单中的“退出”结束应用。退出顺序为：停止接受新任务，调用 `aria2.saveSession`，调用 `aria2.shutdown`，等待最多 5 秒；仍未退出则结束本应用持有的 aria2 子进程；最后关闭 SQLite 并退出 AllDownloader。下次启动从 aria2 session 和本地任务映射恢复。

## aria2 进程模型与安全

应用启动时创建唯一的 `Aria2Service`，由它持有子进程、RPC 客户端、运行状态和关闭令牌。aria2 以单个常驻 sidecar 运行，而不是每个任务启动一个进程。

启动参数至少包含：

- `--enable-rpc=true`
- `--rpc-listen-all=false`
- `--rpc-allow-origin-all=false`
- 本机可用端口与每次启动随机生成的 256 位 `--rpc-secret`
- `--continue=true`
- `--input-file=<app-data>/aria2.session`
- `--save-session=<app-data>/aria2.session`
- `--save-session-interval=10`
- `--pause-metadata=true`
- 默认下载目录和既有的分片/连接数配置

RPC 只监听回环地址。密钥只保存在当前进程内存中，不写入 SQLite、配置、日志或调试报告。启动时若端口被抢占，服务重新选择端口并有限重试；sidecar 意外退出时所有活动任务进入明确的“下载引擎不可用”状态，自动重启一次并执行协调，重复失败后等待用户重试。

## RPC 能力与任务映射

Rust RPC 客户端使用回环地址上的 HTTP JSON-RPC 执行命令，并建立带相同 secret 的 WebSocket 连接接收 aria2 通知。客户端负责调用：

- 添加：`aria2.addUri`、`aria2.addTorrent`
- 状态：`aria2.tellStatus`、`aria2.tellActive`、`aria2.tellWaiting`、`aria2.tellStopped`、`aria2.getGlobalStat`
- 控制：`aria2.pause`、`aria2.unpause`、`aria2.remove`，必要时 `aria2.forceRemove`
- 选项：`aria2.changeOption`、`aria2.getOption`
- 生命周期：`aria2.saveSession`、`aria2.shutdown`

SQLite 任务表新增 `aria_gid`、`followed_by_gid`、`total_length`、`completed_length`、`download_speed`、`upload_speed`、`connections`、`eta_seconds`、`aria_error_code`、`updated_at`。文件表保存 GID 下的文件索引、路径、长度、完成量和选择状态。数据库迁移必须兼容现有任务，不能清空用户数据。

本地任务 ID 是应用稳定标识，aria2 GID 是传输实例标识。磁力元数据完成后 aria2 可能通过 `followedBy` 产生新 GID；服务必须更新映射并保持同一个本地父任务，不在列表中制造重复任务。

## 状态同步

活动下载每 500 毫秒批量查询，等待或暂停任务每 2 秒协调一次；aria2 完成、错误、暂停等通知用于立即唤醒协调，但不替代进度查询。Rust 将变化后的任务快照写入 SQLite，并通过 Tauri 事件向主窗口和悬浮球推送增量更新。

状态映射如下：

- aria2 `waiting` → 本地 `queued`
- aria2 `active` → 本地 `downloading`
- aria2 `paused` → 本地 `paused`
- aria2 `complete` → 本地 `processing`，无后处理时随后进入 `completed`
- aria2 `error` → 本地 `failed`，保存 errorCode/errorMessage
- aria2 `removed` → 本地 `cancelled`

进度使用 `completedLength / totalLength` 计算；未知总长度时显示已下载字节而不伪造百分比。ETA 仅在速度大于零且总长度已知时计算。前端不再自行推测暂停、继续或取消后的状态，也不在 RPC 失败时乐观修改任务。

## 下载与控制流程

标准 HTTP/HTTPS 任务创建后调用 `addUri`，成功获得 GID 才进入队列。连接器后续产生的 Cookie、请求头、文件名和临时 URL 作为经过白名单过滤的 aria2 选项传入，不写入普通日志。

`.torrent` 文件使用 `addTorrent`。磁力任务先获取元数据，`pause-metadata=true` 使元数据派生出的实际下载保持暂停；取得文件列表后展示多文件确认窗口，用户确认选择项后用 `select-file` 配置并启动。目录/多文件 HTTP 任务的每个文件使用独立 GID，父任务聚合子任务状态。

操作语义：

- 暂停/继续父任务：对其所有未完成 GID 调用 pause/unpause，全部成功后再更新界面。
- 取消：从 aria2 移除对应 GID并保留已下载部分和 `.aria2` 控制文件；删除本地文件必须是另一个明确操作。
- HTTP 多文件子项：可以独立暂停、继续、取消。
- 同一个 BT GID 内的文件共享一个传输会话。文件可通过 `select-file` 加入或排除，但 aria2 不支持对单个 BT 文件建立真正独立的暂停状态；界面必须呈现“选择/跳过”，不能伪装成独立暂停。
- RPC 超时或拒绝时维持 aria2 最后已知状态，显示操作失败并在下一次协调时校正。

## 前端结构

新增 `WindowTitleBar` 组件，标题栏按钮调用封装后的 Tauri 窗口操作；下载管理器内容从标题栏下方开始布局。最大化状态通过窗口 resize 事件同步，以正确切换最大化/还原图标和无边框窗口圆角。

任务数据由一次初始快照加后续 `download-task-updated` 事件驱动。应用重新显示时立即请求一次全量协调，避免隐藏期间事件丢失造成旧画面。任务列表展示真实速度、进度和状态；详情页展示 GID、总量、已完成量、连接数、ETA、错误码与文件状态。悬浮球汇总活动任务数量和全局下载速度。

## 错误与恢复

- aria2 启动失败：主界面显示“下载引擎不可用”，保留任务并提供重试，不把任务标记为完成。
- RPC 请求失败：有限重试后报告连接错误；控制按钮恢复可用，状态等待协调。
- 应用异常终止：下次启动读取 session，查询 aria2 当前任务，并用 GID 与 SQLite 协调。
- session 中存在本地未知 GID：导入为“恢复的下载任务”；SQLite 中存在但 aria2 不存在的活动 GID：根据输出和控制文件判断可恢复、已完成或失败。
- 关闭主窗口不会触发任何下载引擎关闭逻辑。
- 正常退出保存会话失败时仍尝试优雅 shutdown，并在退出前记录不含密钥和敏感 URL 的错误。

## 测试与验收

1. 前端组件测试覆盖自绘标题栏拖动区域、最小化、最大化/还原、关闭隐藏，以及 aria2 事件更新任务 UI。
2. Rust 单元测试覆盖 aria2 状态映射、长度/速度/ETA、GID 跟随、父子聚合、错误映射和退出状态机。
3. RPC 契约测试使用本地假服务器验证每个控制动作发送正确方法、secret 和参数，并验证失败时不乐观改状态。
4. 实际 aria2 集成测试使用临时目录和本地 HTTP 服务器，验证添加、进度、暂停、继续、完成、取消保留部分文件、session 保存与恢复。
5. BT 测试使用本地固定种子元数据验证文件清单、`followedBy` GID 更新和 `select-file`，不依赖公网资源。
6. 隐藏窗口集成测试验证关闭请求后 `main` 仍存在、悬浮球可重新显示它、设置导航有效、退出流程请求 aria2 保存和停止。
7. Windows 发布构建必须通过 EXE、MSI、NSIS 打包及无前台干扰的 WebView2 冒烟测试。

验收标准是：用户在 UI 中看到的活动状态、字节数、速度、进度和错误均能追溯到 aria2 RPC 返回值；暂停、继续、取消会改变 aria2 的真实状态；隐藏并重新打开下载管理器不会丢失窗口或状态；从悬浮球退出后 aria2 不再运行，重启应用可继续未完成任务。
