# AllDownloader · Windows Beta

本地优先的 Windows 下载客户端。它把标准下载链接、磁力/种子、迅雷链接、网盘分享链接和复杂文本放入统一任务流程，并使用小型悬浮输入窗与下载管理器显示进度。

## 当前可运行能力

- Tauri 2 + WebView2 Windows 桌面壳；没有 Electron/Chromium 捆绑包。
- HTTP/HTTPS、FTP、ED2K、HLS `.m3u8`、MPEG-DASH `.mpd`、magnet、`.torrent`、Thunder 和网盘分享地址的规则分类；直链自动进入下载状态，多文件类型先进入确认窗口。
- 本地 SQLite 任务状态、恢复时读取的任务列表、暂停/继续/取消状态操作，以及目录父子任务界面。
- Aria2 Next Windows sidecar 已随包配置，负责 HTTP、BT、ED2K 以及原生 HLS/DASH 下载；任务状态、续传信息和媒体清单均由 aria2-next 统一持久化。FTP 仍由进程内纯 Rust 引擎下载并支持断点续传。
- 安全的后处理计划模型：只允许解压、合并、改名、校验；拒绝程序执行和路径穿越。页面中取得的密码仅存为受保护引用，不写入调试对象。
- 自绘的夸克/PikPak 文件管理界面、连接器能力契约与可测试 mock 连接器。账号登录、文件 API、上传与真实云端目录操作必须经过用户授权并受服务商接口/条款限制。
- 可选的 Beta 调试报告开关默认关闭；关闭时不应收集详细链路数据。

## 明确的安全边界

- 不共享用户账号、Cookie、token、快速下载链接或下载能力。
- 不绕过验证码、短信、扫码、DRM、真实支付、未知表单或解压密码。
- AI 仅在规则无法可靠理解文本/网页时接收当前任务的脱敏可读文本；绝不发送 Cookie、凭据、密码、二维码、短信或本地文件。
- 任何服务商拒绝嵌入式授权时应转为系统浏览器或服务商允许流程；不得抓取 OAuth token。

## 开发环境

Windows 10/11、WebView2 runtime、Node 24+、Rust stable MSVC、Visual Studio C++ Build Tools。项目构建时会使用 Tauri 的 Windows WiX 打包链。

```powershell
# 验证仓库内置的 Aria2 Next 原生媒体 sidecar
powershell -ExecutionPolicy Bypass -File scripts/download-sidecars.ps1

# 安装依赖，运行测试与 Web 构建
npm install
npm run test:run
npm run build

# 桌面开发模式 / Windows 安装包
npm run tauri:dev
npm run tauri:build

# 验证包已产出
powershell -ExecutionPolicy Bypass -File scripts/smoke-download.ps1
```

默认下载目录为当前 Windows 用户的 `Downloads`。任务数据库位于 Windows 应用数据目录中的 `AllDownloader/tasks.sqlite`；不要手动复制其中可能关联授权会话的信息。

## 关于夸克与 PikPak

应用的连接器边界与文件管理界面已经固定为“用户自身授权、凭据本机保护、应用自绘目录”。真实连接器不能依赖未审查的 Cookie 导入或规避服务商限制。下一步集成只能选择许可兼容、经安全审查的开源实现或服务商正式接口，并用用户本人完成的授权会话进行验证。没有真实账户授权时，自动化测试使用 mock 连接器而不是伪造云端成功结果。
