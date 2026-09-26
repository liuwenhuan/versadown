# 迅雷断连重试与原生断点接续

## 本次变更

- 迅雷个人盘和分享下载提交时设置`max-tries=5`、`retry-wait=3`；由引擎在任务内部等待重试，暂停、取消仍然可中断。不改代理、并发上限或登录流程。
- 只读识别`aria2-next-state/stream/state.db`：核对旧GID、唯一文件路径、总长度和已完成区间。预分配文件长度不能作为下载进度；损坏、缺失或不匹配的断点不自动续传。
- 云盘排队期间保留原生断点GID，避免旧GID被清空后无法找到原生状态。提交时再次验证，并固定原输出路径。
- 提交刷新后的URL及原断点URL，满足引擎URI身份匹配；引擎从原URL恢复，失败后按原生镜像机制切换新URL。不写引擎状态库。
- 原生续传显式关闭`continue`的文件长度回退、覆盖及自动改名。该开关不影响引擎对有效原生checkpoint的恢复。传统`.aria2`断点保留原路径。

## 回归验证

以下均为定向验证，未运行全量测试：

| 实际命令 | 最终结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test downloader --test cloud_download_scheduler --test cloud_directory_download` | 37 + 5 + 29通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib duplicates::tests` | 6通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib xunlei` | 32通过 |
| `node node_modules/vitest/vitest.mjs run src/components/DownloadQueue.recovery.test.tsx src/components/DuplicateDownloadDialog.test.tsx --exclude '.worktrees/**'` | 7通过 |
| `node scripts/verify-native-resume.cjs` | 真实bundled引擎测试通过 |
| `powershell.exe -ExecutionPolicy Bypass -File scripts/build-windows.ps1` | 退出0，发布二进制及sidecar成功 |

回归测试先捕获缺陷：原生checkpoint被误判需覆盖、提交时仍使用危险的长度回退、云排队丢失原checkpoint身份。修复后通过。原来的传统续传测试补全了实际payload与`.aria2`夹具，继续验证传统行为。

真实引擎最终主线复验：空响应恰好5次，间隔3079/3197/3329/3206ms；等待期间暂停30ms、取消32ms生效。16MiB预分配文件只完成4MiB，旧URL403后从`bytes=4194304-8388607`开始切到新URL，最终SHA256完全一致。零新增字节再次失败仍保留4MiB区间并迁移至新GID；checkpoint删除则报错13、0次HTTP请求，原文件哈希不变、无改名。

## 真实迅雷后台验证范围

使用新发布二进制和独立数据目录/端口运行隐藏实例，保留原代理环境，不操作用户任务。此前用主动取消模拟网络断连的探测没有通过：精确引擎源码确认用户取消会主动删除checkpoint，所以取消不是断连的等价模拟。

最终后台探测通过，退出码0。执行命令：

```powershell
$env:VERSADOWN_TEST_BINARY='artifacts/versadown.exe'
$env:VERSADOWN_XUNLEI_PASSWORD_LIVE='1'
$env:VERSADOWN_XUNLEI_DOWNLOAD_LIVE='1'
$env:VERSADOWN_XUNLEI_RETRY_LIVE='1'
node scripts/verify-xunlei-auth-live.cjs
```

测试目录为`versadown-xunlei-auth-live-hfJQuY`。透明本地桥接仍转发到原代理，只关闭隔离引擎的桥接连接来模拟断连，不修改用户代理或原任务。此前只设置`all-proxy`的注入没有生效，不能算通过；最终同时设置隔离引擎的HTTP/HTTPS代理并确认桥接收到6次连接后才注入。

- 同一个真实迅雷文件，128543808字节。在266240字节暂停后恢复；日志显示从`266240-128543808`接续，空响应等待3057/3032/3049ms再试。
- 409600字节时断连，应用最终进入失败；经真实`task_action: retry`返回`resume_available`，再走`resolve_duplicate_download: resume`接续。
- 新GID下进度由409600增长至933888字节，新增512KiB；原有前256KiB的SHA256不变。RPC实际参数为`max-tries=5`、`retry-wait=3`、`continue=false`、`allow-overwrite=false`、`auto-file-renaming=false`。
- 结束时主动暂停隔离任务，原生状态保存为`0-950272`，没有误报完成；隐藏测试实例已退出。用户名登录、容量显示、授权窗口关闭、网盘菜单也通过脚本断言。

这是实际迅雷链路的短区间接续验证，不是完整下载122.6MB；完整文件哈希一致性由上面的16MiB真实引擎夹具验证。此次改动改善失败处理和接续，不会消除服务端间歇断连，也未提升普通直链的会员加速能力。

## 官方加速通道

见`xunlei-acceleration-assessment-2026-09-17.md`。本轮未接入SDK，不能承诺普通HTTP直链获得官方P2P/离线通道速度。

发布文件：`artifacts/versadown.exe`，SHA256 `295C9B344F06B962BA72DD59DD3F39A81702575889F915A239B60A39EA560650`。同目录`binaries`中的aria2 sidecar需保留。
