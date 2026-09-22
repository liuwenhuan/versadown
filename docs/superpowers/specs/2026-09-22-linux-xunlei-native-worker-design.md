# Linux 迅雷原生 Worker 设计

## 目标

在 Linux 上把已安装的官方迅雷运行库接入 AllDownloader，使迅雷云盘解析流程能够像现有 Windows 原生后端一样：使用 AllDownloader 自己保存的迅雷凭据和短期加速 token 创建、控制和观察原生任务，而不经过官方迅雷 UI。

官方迅雷与 AllDownloader 必须能够同时运行。两者复用同一套已安装的官方 SDK 文件，但任务、凭据、任务数据库和控制权完全隔离；AllDownloader 不显示、枚举、接管或修改官方迅雷的任务，官方迅雷也不接管 AllDownloader 的任务。

Windows 和 Linux 必须共享下载业务流程、任务状态语义、SVIP 判定和恢复安全规则。仅运行库发现、原生 ABI 和引擎承载方式按平台分离。

## 已确认的本机运行时

| 安装形式 | 发现方式 | 运行库根目录 | 版本 | 备注 |
| --- | --- | --- | --- | --- |
| DEB | `dpkg-query` 的 `com.xunlei.download` | `/opt/apps/com.xunlei.download/files` | 1.0.0.5 | 启动器为 `start.sh`，设置同目录的 `LD_LIBRARY_PATH`。 |
| 玲珑 | `ll-cli --json list` 的 `com.xunlei.download`，再读取 layer `info.json` | `/var/lib/linglong/layers/<commit>/files` | 1.0.0.6 | layer commit 不是稳定 API，代码不得持久化或硬编码。容器内仍映射为 `/opt/apps/com.xunlei.download/files`。 |

两种安装都需要 `libxl_thunder_sdk.so` 和同目录的 `libxl_stat.so`；`thunder`、两个 `.so` 在当前机器上的 SHA-256 相同。`ThunderKernel.node` 的指纹不同，因而不是本适配器的 ABI 依赖。

Linux SDK 对外导出了 `XLInit`、`XLUnInit`、`XLSetUserId`、`XLSetVipType`、`XLSetAccelerateToken`、`XLCreateP2spTask`、`XLStartTask`、`XLStopTask`、`XLReleaseTask`、`XLGetTaskInfo`、`XLGetTaskInfoEx`、`XLStartDcdn` 和 `XLStopDcdn`。它不是 Windows 的 `DownloadSDKProxy.dll`：没有 `DownloadSDKServer.exe` 形式的可复用 broker，函数名和 ABI 不能直接沿用 Windows 定义。

## 范围与非目标

本设计包括：Linux x86_64 的 DEB/玲珑发现、无界面 Rust worker、从 AllDownloader 凭据派生的身份和加速 token 传递、原生 P2SP 生命周期、进度同步、并行运行隔离，以及真实 SVIP 通道验收。

本设计不包括：读取迅雷的 Cookie、配置、账号、任务数据库或 VIP 证书；调用 Electron/Node UI；让两端显示或控制同一任务；反向修改迅雷安装文件；支持未验证 ABI 的未来 SDK；macOS 迅雷接入；BT、磁力和 ed2k 的 Linux 原生下载。

## 与 Windows 的共享边界

`XunleiNativeBackend` 成为跨平台门面，公共层保留并统一以下行为：

- 云盘连接器产生的内存标记：用户 ID、peer ID、文件 GCID/CID、短期下载地址和加速 token；
- 输出文件名预约、重复文件检查、任务库快照和前端任务事件；
- 原生任务与 aria2 GID 的区分、云盘调度槽释放和终态处理；
- `NativeProgress`、速度计算、暂停/继续/取消状态转换和 SVIP 状态机；
- 旧引擎会话或不确定原生 task ID 绝不再被控制的安全规则。

公共层以内部 `NativeEngine` 契约与平台实现交互：

```rust
trait NativeEngine {
    fn submit(&mut self, request: NativeSubmission) -> Result<NativeTaskHandle, NativeSubmitError>;
    fn poll(&mut self, handle: NativeTaskHandle) -> Result<NativeProgress, NativeSubmitError>;
    fn control(&mut self, handle: NativeTaskHandle, action: DownloadAction) -> Result<(), NativeSubmitError>;
    fn release(&mut self, handle: NativeTaskHandle) -> Result<(), NativeSubmitError>;
}

struct NativeTaskHandle {
    engine_session: uuid::Uuid,
    task_id: u32,
}
```

Windows 用一个 `WindowsNativeEngine` 包装现有 `WindowsApi`，保留其私有 proxy/broker 隔离逻辑。Linux 用 `LinuxWorkerEngine` 实现相同契约，代理到 worker。持久化原生 GID 必须包含 `engine_session` 和 `task_id`，而不是仅保存可被新 SDK 会话复用的数字 ID。

## Linux 组件

### `LinuxXunleiRuntimeDiscovery`

发现器返回按版本降序排列的 `LinuxXunleiRuntime`：安装形式、版本、SDK 路径、统计库路径和不可逆 SDK 指纹。

DEB 候选必须同时满足 DPKG 包存在、`/opt/apps/com.xunlei.download/files/libxl_thunder_sdk.so` 与 `libxl_stat.so` 是 x86_64 ELF 文件。玲珑候选必须同时满足 `ll-cli --json list` 报告该应用、某个可读 layer 的 `info.json` 精确声明 `id: com.xunlei.download`、`files/` 内存在同样的两个 x86_64 ELF 文件。运行 worker 前重新发现，不保存 layer commit；升级发生后自然选择新 layer。

若两个候选都可用，选择版本较高者；版本相同则选择稳定 DEB 路径。若候选无法读取、依赖不全、架构不匹配或 ABI 指纹未允许，原生引擎不可用。

### `LinuxWorkerEngine` 与 `xunlei-native-worker`

主 Tauri 进程不直接加载 Linux SDK。每个 AllDownloader 进程至多启动一个 `xunlei-native-worker`，它是随应用发行的 Rust 原生 sidecar，不启动官方 UI，也不启动第二个 `thunder` 客户端。

worker 在自己的工作目录运行，并以 `RTLD_GLOBAL` 先加载选定的 `libxl_stat.so`、后加载 `libxl_thunder_sdk.so`。它不复制、重命名、补丁或写入供应商运行库。所有 SDK 调用固定在 worker 的单一工作线程，避免 SDK 全局状态和 FFI 重入。

worker 的 Linux FFI 仅对已经通过 ABI 验证的 SDK 指纹开放。验证阶段从 `ThunderKernel.node` 的参数编组和本机受控任务中确定当前 SDK 的 C 函数签名、初始化结构和 `XLGetTaskInfo(Ex)` 字段布局；得到的签名与指纹作为一个原子兼容项测试。任何符号、调用约定或布局不匹配都会使候选不可用，绝不以猜测的指针或结构调用生产 SDK。

### 父子协议

父进程创建权限为 `0700` 的临时目录和权限为 `0600` 的 Unix socket。socket 路径和运行库描述可作为非敏感启动参数；一次性随机 nonce 经 worker 的继承标准输入传递，不能出现在进程命令行、环境变量或日志中。

协议命令为 `initialize`、`submit`、`poll`、`pause`、`resume`、`cancel` 和 `shutdown`。`initialize`/`submit` 的敏感字段只在内存消息中出现：AllDownloader 的用户 ID、peer ID、会员类型、签名下载地址、HTTP 请求头和加速 token。worker 返回的进度消息只包含会话绑定的任务句柄、长度、状态、错误码、连接数和通道统计。URL 签名、令牌、用户 ID 和 peer ID 不得进入 `Debug`、错误、持久化配置或可观测日志。

## 任务生命周期

1. 现有迅雷云盘流程从 AllDownloader 的系统凭据库刷新会话并解析文件，得到短期地址和加速 token。
2. 公共 `XunleiNativeBackend` 在保留现有输出路径预约规则后，构造 `NativeSubmission` 并提交给当前平台的 `NativeEngine`。
3. Linux worker 初始化 SDK，设置 AllDownloader 的用户身份和会员信息，设置当前任务的加速 token，创建 P2SP 任务并启动其加速通道；Windows 使用现有等价步骤。
4. 公共进度监控层轮询 `NativeEngine::poll`，更新任务库、文件进度和前端事件。仅 AllDownloader 创建的会话句柄可被轮询或控制。
5. 暂停停止该句柄；继续使用 AllDownloader 自己的凭据重新解析短期地址和 token 后恢复；取消停止并释放该句柄。任务完成后也释放 SDK 句柄。
6. worker 退出、崩溃、SDK 更新或 SDK 返回“未知任务”时，所有属于其会话的任务安全进入暂停态。恢复操作必须创建或重新绑定一个当前会话任务，不能向旧数字 ID 发送控制命令。

提交前发现运行库不可用或 ABI 不兼容时，返回“未选择原生引擎”，让既有 aria2 直链流程继续。原生任务已创建后的失败先释放其句柄并报告失败，不自动再投递到 aria2，避免两个引擎下载同一文件。

## SVIP 状态与验收

界面状态按 `未知`、`具备会员资格`、`已请求加速`、`已确认加速`、`未启用` 显示。会员登录态、会员类型、请求返回成功或下载速度都不能单独证明 SVIP 生效。

只有满足全部条件时才显示“正在使用迅雷 SVIP 加速”：

1. AllDownloader 自己的迅雷会话为当前任务取得加速 token；
2. Linux SDK 已成功接受 token 并报告 DCDN/VIP 通道启用；
3. 后续至少一次轮询显示 DCDN/VIP 通道累计接收字节增长。

真实验收使用用户指定、合法且可清理的资源，在独立临时输出目录创建任务，等待非零进度后暂停、恢复并检查上述三个条件。测试完成后取消/释放原生任务并删除测试输出与检查点。若资源没有触发服务端加速，结果只能报告“未确认加速”，不能以速度推断成功。

## 失败处理与用户提示

新提示必须使用 i18n 键，并至少提供简体中文与英文。需要覆盖：未安装或无权访问官方运行库、SDK 版本不兼容、原生 worker 意外停止、需要重新解析后继续、SVIP token 不可用、加速已请求但尚未确认，以及实际确认加速。

官方迅雷升级不会中断已加载 worker 的映射库；新 worker 仅在重新发现并完成 ABI 校验后使用新版本。AllDownloader 不干预官方迅雷进程、端口、socket、任务文件或用户数据，因此两个程序的并行运行不依赖官方客户端是否已经启动。

## 验证

- Rust 单测覆盖 DEB/玲珑候选发现、layer hash 不持久化、版本优先级、ELF/依赖验证、worker 会话 GID、旧会话 ID 拒绝控制和普通下载回退。
- fake worker 测试覆盖提交、轮询、暂停、恢复、取消、worker 崩溃、原生创建后失败和调度槽释放。
- Windows 定向测试验证 `WindowsNativeEngine` 仍遵守相同的共享 `NativeEngine` 契约。
- Linux 忽略集成测试在有运行库时验证动态加载、要求符号和当前 ABI 指纹；无 SDK 的 CI 不得失败。
- 真实 Linux 验收只在明确提供受权测试资源时运行，检查创建、进度、暂停/恢复、清理和 DCDN/VIP 字节增长。

不因本文档变更运行构建或测试。后续实施按项目的定向验证规则执行；生成 Linux 原生二进制仅在用户当次明确要求时进行。
