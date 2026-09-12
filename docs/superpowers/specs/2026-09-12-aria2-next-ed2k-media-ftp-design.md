# Aria2 Next、ED2K、流媒体与 FTP 下载设计

## 目标

以 Aria2 Next 2.7.5 取代 aria2 1.37.0，为现有下载队列增加 ED2K、HLS `.m3u8` 和 MPEG-DASH `.mpd` 下载，并在 Aria2 Next 已移除 FTP 的前提下，以进程内纯 Rust FTP 引擎补齐 FTP 认证和断点续传。

## 引擎边界

- `aria2-next` 继续通过本机 loopback JSON-RPC 服务承载 HTTP、HTTPS、SFTP、Metalink、BitTorrent、Magnet 和 ED2K。
- 可执行文件固定为官方 Windows x86_64 发布资产，下载脚本固定版本并校验官方 SHA-256；Tauri 只打包新 sidecar。
- 启动参数增加 `--state-dir=<应用数据目录>/aria2-next-state`。既有 RPC 端口、密钥、并发和 BT 设置继续复用；不向远程网络开放 RPC。
- FTP URI 由 `FtpDownloadEngine` 截获，不提交给 Aria2 Next。该引擎使用 `suppaftp` 的 Tokio 实现，不引入第二个外部进程。
- HLS/DASH URI 由进程内 `MediaDownloadEngine` 截获。Rust 后端读取并解析清单，选择最高带宽变体，将初始化段和媒体分片交给 Aria2 Next 下载，最后按清单顺序合并为一个任务和一个输出文件。Aria2 Next 本身只承担分片传输。

## 输入与任务

- 识别 `ed2k://|file|名称|大小|MD4|/`，校验文件名、十进制大小和 32 位十六进制 MD4。合法 ED2K 文件链接自动开始，任务种类为 `ed2k`。
- 识别 `ftp://`，支持 URI 内用户名、密码和端口，默认匿名登录。密码不得进入错误或调试输出。
- `.m3u8` 和 `.mpd` URL 作为单个媒体任务自动提交给媒体编排层；任务种类分别为 `hls` 和 `dash`，队列仍显示为一个任务。HLS 支持主/媒体清单与初始化段；DASH 支持 `BaseURL`、`SegmentList` 以及常用的编号/时间线 `SegmentTemplate` 静态清单。不受支持的加密或字节范围清单会明确失败，不会把清单误报为媒体文件。
- 文本混合提取同时支持 ED2K、FTP、HLS 和 DASH，既有 HTTP、磁链、迅雷链接和网盘分享解析保持不变。

## FTP 行为

- 默认被动模式；下载前读取 SIZE，使用二进制传输。
- 目标文件存在且小于远端大小时，通过 FTP `REST` 从本地长度续传；相同大小直接完成；本地大于远端或用户明确覆盖时从零开始。
- 下载按流写入目标文件，周期性把总大小、已完成字节、速度、预计时间和连接数写回 `TaskStore` 并发送现有任务更新事件。
- 暂停和取消使用协作式取消信号：暂停保留文件，恢复时重新连接并从本地长度继续；取消保留与现有队列一致的状态语义；失败任务可重试并续传。
- 应用重启后，数据库中未完成的 FTP 任务重新排队，并依据目标文件长度续传。

## 旧引擎清理

- 以应用数据目录中的一次性迁移标记识别首次切换。
- 首次切换时，删除所有未完成的旧 aria2 任务记录、其数据库记录的局部下载文件以及相邻 `.aria2` 控制文件；路径必须解析后位于配置的下载目录内。
- 已完成任务及其完整文件保留。删除失败时记录可诊断错误并停止写入迁移标记，避免留下半完成但被误认为已迁移的状态。
- 清理完成后写入版本化标记，后续启动不重复删除。

## 错误与兼容

- Aria2 Next 启动后必须通过 RPC 版本/功能探针；缺少 ED2K 能力时下载引擎显示不可用，而不是静默退化。媒体清单解析由应用内 Rust 引擎负责。
- FTP 认证、DNS、TLS、服务器不支持 SIZE/REST、磁盘写入等错误映射为用户可读中文；敏感凭据始终脱敏。
- 既有云盘直链继续走 Aria2 Next HTTP，并保留 Cookie/Header、代理和文件名选项。
- Aria2 Next 使用 GPLv2；`suppaftp` 使用 MIT/Apache-2.0，补充第三方声明。

## 验证

- Rust 定向测试覆盖协议解析、ED2K 校验、启动参数、旧任务安全清理、FTP 匿名/认证下载、暂停后 REST 续传和恢复。
- 使用官方 Windows sidecar 实测 RPC 启动、版本、ED2K 添加和本地 HTTP 下载；用本地受控 FTP 服务器验证续传，不依赖公共 FTP 服务。
- 前端定向 Vitest 覆盖新任务种类、标题和图标；运行 `npm run build`。
- 最后运行 `scripts/build-windows.ps1` 生成普通可运行二进制，不打安装包。
