# 百度下载及授权恢复修复验证（2026-09-13）

## 已复现的根因

1. 当前保存的百度 Cookie 能访问账号、根目录，但旧 `xpan/multimedia filemetas` 下载请求返回 `errno=9019`。相同 Cookie 调用已签名的 PCS `locatedownload`，实际文件请求返回 HTTP 206。个人下载、分享转存后的下载和后续刷新已统一使用 Cookie 对应的原生 PCS 协议。
2. 显式“重新授权”曾先执行后台密码/会话恢复；根目录可读就提前返回成功，无法证明下载权限已恢复。百度的显式按钮现在直接打开官方授权窗口，后台自动恢复仍保留，不清除已保存的密码和会话。
3. 已有下载的响应同时含 `duplicate` 和 `queueTask`，前端优先接收队列投影，吞掉了需要用户选择的重复下载提示。个人下载和任务重试现在先处理重复决策，同时保留目录根任务投影。
4. 对整个 JSON 搜索 `verify`、`risk`、`captcha` 等词，会把合法文件名和签名 URL 误判为风控。改为读取明确的错误码和挑战字段；兼容 PCS `error_code`，不再把文件不存在的 `-9` 误当授权过期。
5. 根目录原本只读第一页 100 项。补全分页，避免目录下载、转存记录定位和清理漏项。
6. 独立审查发现 PCS 按路径下载存在旧路径被复用的风险。下载前现在从原父目录按持久化文件 ID 重新确认文件：同目录改名可追踪新路径；原 ID 消失时明确报错，不下载占用旧路径的其他文件。相关反例测试先失败、修复后通过，并再次取得真实 HTTP 206 响应。

## 授权与分享的边界

- Password/Web 会话最终均使用 BDUSS Cookie；登录模式标签不改变下载协议。
- 原有密码登录、网页 Cookie 验证、过期自动恢复、账号隔离和密码保存机制保留。没有将能读取目录等同于任何操作都具备权限。
- 分享优先直链；需转存时记录实际创建的文件 ID 和路径。刷新不重复转存；清理只匹配记录的精确 ID，不按分享原文件名删除。
- 转存目标当前为根目录；如果用户随后把转存文件移走，刷新报告“已移动或不存在”，不扫描整个个人盘或删除其他文件。
- 真实验证码、短信或平台风控仍需用户在官方页面完成；本次不会绕过平台验证或付费限制。

## 参考协议

- [BaiduPCS-Go](https://github.com/qjfoidnh/BaiduPCS-Go)：Cookie 身份、原生 PCS 下载、签名和 User-Agent；比照 `baidupcs/download.go`、登录入口实现。
- [baidupan-cli](https://github.com/yuexiaoliang/baidupan-cli)：OAuth access_token/refresh_token 及下载请求。当前应用没有以该项目的 OAuth 应用授权替代现有 Cookie 会话，不混用两套凭据语义。

## 实际执行的验证

工作目录为 `D:\projects\downloader`；Cargo 命令在 `src-tauri` 执行。

| 命令 | 结果 |
| --- | --- |
| `cargo test --quiet --lib baidu` | 40 通过，2 个需网络/显式输入的测试忽略 |
| `cargo test --quiet --test cloud_baidu` | 38 通过 |
| `cargo test --quiet --test cloud_session --test cloud_auth_supervisor --test cloud_saved_login` | 44 通过 |
| `pnpm.cmd run test:run src/App.live.test.tsx src/components/CloudManager.test.tsx --exclude '.worktrees/**' --exclude '.tmp-master-build-*/**'` | 196 通过 |
| 设置 `VERSADOWN_LIVE_CLOUD=1` 后 `cargo test --quiet --test live_baidu_tianyi live_baidu_personal_download_bytes -- --ignored --nocapture` | 使用已保存授权，个人文件 HTTP 206，实际收到前 1024 字节 |
| `powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows.ps1` | 正常 master 构建目录生成可运行 EXE；无安装包 |
| `git diff --check` | 通过 |

新测试首先复现错误，再确认修复：合法 URL 被误判风控、PCS 错误码丢失、目录第二页丢失、重复下载弹窗消失、原生授权错误显示为 `[object Object]`。

## 尚未声称通过的验证

- 桌面“重新授权”按钮实机点击：控制 VersaDown 的权限请求超时，未完成。已有组件/应用集成测试覆盖授权事件和兜底轮询后的任务恢复，但不能替代桌面实测。
- 用户问题分享链接的实际完整下载：本次未提供可用于该问题的百度分享链接，已验证分享直链、转存、刷新和清理的定向测试，但未声称该分享链接实测通过。
- 1024 字节只读探测验证下载链路，不等于完整文件下载/校验，也不代表所有账号或风控状态均可用。

## 二进制

- 路径：`D:\projects\downloader\artifacts\versadown.exe`
- 大小：26,301,952 字节（22:53:49）
- SHA256：`8169802B018785A08EBB5167594C81C98A1D9365E366217D8DD33C9F82713549`
- 前端包：`index-BUvbXf7q.js`
- 已停止占用该路径的旧下载器进程，未删除任务、下载文件或凭据。
