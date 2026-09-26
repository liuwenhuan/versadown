# Linux 迅雷 AXDL 私有桥接设计

## 目标与确认范围

在 Linux x86_64 上，VersaDown 通过已安装官方迅雷客户端的私有原生运行时，为普通 HTTP(S) 链接和其他网盘已解析出的 HTTPS 直链尝试迅雷个人会员加速。实现不使用迅雷公开下载 SDK，不启动 Electron/Node/官方 UI，也不把任务交给官方客户端 UI。

用户已选择沿用 Windows 的“研究官方客户端并建立隔离桥接”方向。成功的定义是：VersaDown 创建、显示和控制自己的任务；官方迅雷客户端可同时运行且其任务不被列举、读取、修改或接管；只有可观测的会员通道字节增长才显示已使用 SVIP 加速。

## 已确认运行时事实

本机 DEB 和玲珑安装均提供以下同版本运行时组合：

- `libxl_stat.so`
- `libxl_thunder_sdk.so`
- `resources/bin/ThunderKernel.node`

`ThunderKernel.node` 的 `DT_NEEDED` 直接依赖前两个库，且包含可由 Rust 动态加载的 AXDL 原生桥接实现。它导出或引用的关键边界包括：

- `AXDL_Init(TAG_XL_DOWNLOAD_LIB_INIT_PARAM*, const char*, callback, void*)`
- `AXDL_SetUserInfo(const char*, const char*)`
- `AXDL_CreateP2spTask(const char*, const char*, const char*, const char*, const char*, long*)`
- `AXDL_StartTask(long)`、`AXDL_StopTask(long)`、`AXDL_DeleteTask(long)`
- `AXDL_QueryTaskInfo(long, callback, void*)`
- `AXDL_EnableDcdnWithToken(long, int, const char*, const char*)`
- `AXDL_EnableDcdnWithVipCert(long, int, const char*)`
- `AXDL_EnableDcdnWithSession(long, int, const char*, const char*, const char*)`

这些是私有、未承诺稳定的 ABI。所有调用都必须同时匹配 `ThunderKernel.node`、`libxl_thunder_sdk.so`、`libxl_stat.so` 的联合 SHA-256 指纹和已观察到的参数布局；任一项不匹配即拒绝原生提交。

在当前已观察的 x86_64 Linux 版本中，动态装载器绑定的是 C++ 修饰后的 ELF 名称，例如 `_Z9AXDL_InitP30TAG_XL_DOWNLOAD_LIB_INIT_PARAMPKcPFiiPvES3_`、`_Z16AXDL_SetUserInfoPKcS0_`、`_Z19AXDL_CreateP2spTaskPKcS0_S0_S0_S0_Pl`、`_Z14AXDL_StartTaskl`、`_Z13AXDL_StopTaskl`、`_Z15AXDL_DeleteTaskl` 和 `_Z18AXDL_QueryTaskInfolPFiliiPK19TAG_XL_TASK_INFO_EXPvES2_`。它们不是稳定 C ABI；绑定表本身属于联合指纹门的一部分。`AXDL_*` 任务 ID 为 Linux 的 `long`，协议和公共句柄必须使用有符号 64 位值，不能沿用当前 worker 协议中的 `u32`。

## 安全与隔离约束

- 不读取、写入、解析或监听官方迅雷的 Cookie、账号、数据库、任务文件、配置或私有 Unix socket。
- 不连接正在运行官方客户端的 Electron IPC；VersaDown 只加载安装目录中的不可变库文件。
- 不执行 `.node` 的 Node/N-API 初始化；仅由 Rust worker 通过 `dlopen`/`dlsym` 调用已经验证的 AXDL 原生符号。
- 每个 VersaDown 进程最多有一个 worker。worker 使用 VersaDown 自己的配置根目录、临时目录和输出目录，绝不指向官方客户端数据目录。
- 敏感的 URL、请求头、用户 ID、peer ID、访问 token、会话参数及文件 token 仅存在于父子进程内存和权限为 `0600` 的现有 Unix socket；不得出现在日志、Debug、任务 source、环境变量或命令行。
- Windows 保留现有私有 proxy 后端；Linux 接入 AXDL；macOS 仅在已安装客户端提供同等、可指纹验证的 AXDL 运行时后复用同一 `NativeEngine` 契约，不复用 Linux 路径或 ABI。

## 运行时架构

`XunleiNativeBackend` 继续是跨平台门面。其公共层拥有输出路径预约、任务库更新、会话限定 GID、进度计量、暂停/继续/取消和终态清理。平台层只实现 `NativeEngine`：

```rust
trait NativeEngine {
    fn submit(&mut self, request: NativeSubmission) -> Result<NativeTaskHandle, NativeEngineError>;
    fn poll(&mut self, handle: NativeTaskHandle) -> Result<NativeProgress, NativeEngineError>;
    fn control(&mut self, handle: NativeTaskHandle, action: DownloadAction) -> Result<(), NativeEngineError>;
    fn release(&mut self, handle: NativeTaskHandle) -> Result<(), NativeEngineError>;
}
```

Linux 继续通过现有受 nonce 认证的 `LinuxWorkerEngine` 执行原生调用。Worker 先以 `RTLD_GLOBAL` 加载 `libxl_stat.so`，再加载 `libxl_thunder_sdk.so` 和 `ThunderKernel.node`。同一 worker 线程顺序执行所有 AXDL 命令；AXDL 的异步回调经过 worker 内部同步适配后才写回父进程协议。

指纹门只认可“库三元组 + 原生符号名 + 调用约定 + 初始化布局 + 查询回调布局”的完整组合。`libxl_thunder_sdk.so` 单独匹配不足以启用任务。

## 直链统一提交

新增内部的“原生候选提交”边界，使下列来源在直链已获得之后共享同一路径：

1. 用户添加的普通 HTTP(S) URL；
2. 任何网盘连接器解析出的 HTTP(S) URL；
3. 迅雷网盘文件解析出的 URL 和其文件级 acceleration token。

候选边界接收 URL、真实 HTTP 请求头、输出路径、名称、长度提示、VersaDown 保存的迅雷会话身份和可选文件 token。它不关心网盘提供方，也不把认证标记转交 aria2。

不能进入候选边界的协议（BT、磁力、ed2k、HLS、DASH、FTP）保持现有引擎。运行时缺失、指纹不匹配或直链不支持原生任务时，在任务创建前返回“未选择原生引擎”，使现有 aria2 提交流程正常接管；原生任务一旦创建，后续失败必须先停止和释放，禁止再静默投递 aria2。

## 会员授权与状态

迅雷网盘文件 token 是文件级凭据，优先通过已验证的 AXDL token/VIP-cert 路径使用。普通直链没有该 token，必须走 `AXDL_EnableDcdnWithSession` 的已验证会话路径。

应用保存的迅雷会话可作为候选输入，但 `AXDL_EnableDcdnWithSession` 三个字符串参数的语义尚未由公开头文件定义。实施顺序必须是：先从当前官方客户端二进制的实际调用路径得到参数来源、顺序和所需格式；再将该观察结果与联合指纹绑定；未得到该证据前，禁止把 access token、peer ID 或任意字符串猜测性地传入该函数。

状态机为：`未尝试` → `已创建原生任务` → `已请求会员加速` → `已确认会员加速` 或 `普通原生通道`。只有同时满足以下条件才进入“已确认会员加速”：

1. 会员调用已被当前运行时接受；
2. 后续查询回调返回可识别的会员/P2P/P2S/DCDN 通道计数；
3. 至少一个会员相关累计字节字段在任务运行期间增加。

速度、仅有 SVIP 账号、请求返回成功或 UI 图标均不是确认条件。资源不支持时保持普通原生通道，不伪报加速。

## 生命周期与恢复

任务句柄继续使用 `engine_session UUID + task_id` 的 GID，旧 worker 的数值 task ID 永远不能被新 worker 控制。暂停停止当前句柄；恢复重新解析可能过期的直链、重新获取适用的文件 token 或会话授权后创建新句柄；取消和完成均停止并释放原生任务。

worker 意外终止、运行时指纹变化、回调超时或返回未知任务时，所有该会话拥有的下载安全暂停，并向用户提示重新提交。普通直链的恢复不尝试把私有检查点转换为 aria2 `.aria2`。

## 验证策略

- 单元测试先覆盖联合指纹门、私有符号表完整性、原生候选来源筛选、敏感字段不进入 Debug、旧会话 GID 拒绝控制，以及创建前安全回退。
- fake AXDL transport 测试覆盖初始化、创建、异步查询、暂停、恢复、取消、worker 停止和“创建后失败先释放”。
- Linux 忽略集成测试只在运行时三元组和当前受支持指纹都存在时加载库并验证符号；不得接触官方客户端数据。
- 在用户明确授权后，使用公开、可删除的 HTTPS 测试资源和 VersaDown 专用临时输出目录，验证创建、非零进度、暂停、恢复、取消、输出清理及通道字节判定。测试不会创建或修改官方客户端任务。
- 真实会话加速调用只在参数语义已观察并记录、且用户保存的凭据有效时进行；结果如未出现会员通道字节，仅报告“未确认”，不以下载速度作推断。

## 非目标

- 不接入迅雷公开下载 SDK 或其 App ID/API Key 体系。
- 不共享或接管官方客户端后端、任务数据库和已存在任务。
- 不通过 Node/Electron、浏览器自动化或官方 UI 创建任务。
- 不支持未验证版本、32 位 Linux、BT、磁力、ed2k、FTP、HLS、DASH。
- 本轮不实现 macOS 私有 ABI；只保留可复用的公共契约与运行时发现扩展点。
