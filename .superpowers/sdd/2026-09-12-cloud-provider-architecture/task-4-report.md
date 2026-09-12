# 任务 4：Capability drivers 与 Tauri 命令职责拆分

- 基线：`d44b898`；工作树与分支沿用 `cloud-provider-architecture-v2`。
- 状态：实现、定向测试、前端构建、Windows 发布构建及自审已完成。
- 提交说明：`refactor(cloud): route commands through provider capabilities`（包含本报告的提交）。
- 未派生子代理，未执行全量前端或全量 Rust 测试。

## 交付边界

`ProviderRegistry` 保持原 catalog、URL 安全策略和全部 descriptor 契约，新增可注入 `ProviderDriverFactory`。对象安全的 `DriveOps`、`DownloadOps`、`ProviderAuth` 和 `UploadOps` 由 `ProviderDriver` 调用；操作 capability 禁用时先返回 `ProviderUnsupported`，缺省未实现操作同样返回此错误。密码登录不支持时继续使用既有 `WebAuthorizationRequired` 语义。授权访问器只暴露 `ProviderAuth`，不能绕过 facade 的文件操作 capability 检查。

唯一的七路运行时 provider 选择在 `driver/native.rs` factory，七个 adapter 各自保存所需 store/http 引用。借用转接完整保留 transport 的请求、隔离会话、cookie 管理与 URL cookie 导出方法。协议行为继续由既有 Quark/PikPak/Baidu/Tianyi/Aliyun/Xunlei/UC client 实现。

`ProviderAuth` 只使用 Cookie、选定 storage 字段、浏览器 context 快照、授权配方、用户名密码和恢复凭据。窗口创建、脚本执行、Tianyi 浏览器请求 transport、窗口关闭、密码网页登录回退都在命令 host。文件选择框仍在 `commands/upload.rs`；`UploadOps` 接收显式本地路径和协议 transport，`CloudState` 仅提供既有会话协调能力。

`CloudState` 的七个独立锁字段改为由 registry 初始化的 provider→mutex 集合，仍按同一 provider 串行，不扩大锁范围。PikPak 上传初始化/刷新才取会话锁，OSS 字节传输保持在锁外；迅雷操作失败仍保存已经轮换的 refresh token。百度分享清理的专用 `trash_transferred_files` 通过独立 driver 操作保留，其他 provider 默认使用其 trash 能力。

`QuarkError` 重命名为 `CloudError`，提供 `pub use CloudError as QuarkError`，错误文案与 JSON code 不变。`CloudFile.kind` 使用 `CloudFileKind::{File, Folder}`，JSON 仍为 `file`/`folder`。既有内部字面量 `.into()`、比较和 `as_str()` 保持兼容；阿里外部 `type` 字段走可失败解析，未知值返回 `InvalidResponse`。`ResolvedCloudDownload` 移到共享类型，同时保留 `commands::ResolvedCloudDownload` 导出。

## 文件边界与规模

没有把巨型文件换个名字。命令入口、队列协调、平台浏览器 host 和协议 helper 分开：

| 文件 | 行数 | 职责 |
| --- | ---: | --- |
| 原 `commands.rs` | 6724 | 原全部实现及内嵌测试 |
| 当前 `commands.rs` | 77 | CloudState、初始化、模块与兼容导出 |
| `commands/auth.rs` | 639 | 授权命令、账号替换与失败任务恢复协调 |
| `commands/browser.rs` | 701 | Windows WebView host、采集和网页登录回退 |
| `commands/drive.rs` | 176 | 账号、目录、文件操作命令 |
| `commands/download.rs` | 1075 | 下载计划、队列、目录重试与 stable source |
| `commands/share.rs` | 444 | 分享任务提交与清理协调 |
| `commands/upload.rs` | 112 | 文件选择和上传调度、PikPak 状态协调 |
| `commands/common.rs` | 307 | 命令运行器、锁/重试状态、路径与失败投影 |
| `cloud/common.rs` | 404 | 不依赖 Tauri 的共享协议/凭据 helper |
| `driver.rs` | 251 | 对象安全能力、facade 与借用转接 |
| `driver/native.rs` | 59 | 唯一七路 factory |
| 七个 `driver/native/*_driver.rs` | 149–264/文件 | 各 provider 的协议适配 |
| `commands/tests.rs` | 2092 | 原内嵌测试，完整保留 |

生产命令代码的 `match provider {` 从 20 个变为 0；新的 driver 目录总计 1 个，位于 factory。原 registry 的元数据选择边界保留，并接管浏览器/回收站策略，不把此内部策略序列化进 catalog。`commands/browser.rs` 与 `cloud/common.rs` 是对简报的文件边界补充：前者隔离平台代码，后者避免 driver 反向调用 Tauri 命令 helper。

原 public/pub(crate) 函数清单与拆分后函数进行了逐名比较，没有丢失。`lib.rs` 的 Tauri 注册名及调用路径无需修改，库测试和发布构建已编译实际注册宏。

## RED 与迁移反馈

首次先新增 recording factory/driver 的 facade 分发测试及 JSON/公开命令兼容测试，再运行简报指定命令：

```powershell
C:/Users/deepin/.cargo/bin/cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract --test cloud_coordination --test cloud_directory_download --test cloud_upload
```

退出 1：缺少 `cloud::driver`、`CloudError` 和 `CloudFileKind`，与尚无 capability 接口的预期一致。recording 测试最终覆盖账号、目录、rename、分享清理、下载、授权恢复、上传及参数；禁用 rename/upload 时，记录长度保持不变，未访问能力实现；未实现 copy 返回 `ProviderUnsupported`。

分享清理独立能力追加 RED：`cargo test ... --test cloud_provider_contract` 因 `DriveOps` 与 facade 尚无 `trash_transferred_files` 编译失败，再新增此方法及百度专用适配，保留清理协议。

迁移编译反馈包括静态脚本相对路径、public re-export、阿里 kind 解析、借用析构顺序；均在最终验证前修正。测试启动曾报 `0xc0000139`，不能记作行为 RED/GREEN。通过 `/imports` 和系统 DLL 导出核对，确认初版 auth vtable 错误地保留了 GUI `TaskDialogIndirect` 依赖。按协议/host 边界拆开后，原 integration test 不改 manifest 即可运行。新增 `native_capability_drivers_work_in_a_consumer_without_a_tauri_host` 验证七个 native driver 可在无 Tauri host 的进程中构造、读取配方、执行账号/目录能力且缺凭据不发生 HTTP。该测试同时保留目录缺凭据的历史差异：Quark/PikPak/Aliyun 为 `credentials_expired`，其余为 `not_authorized`；初版测试把它们误写成同一码，已依据既有 client 实现修正预期，没有更改生产错误行为。

## 最终 GREEN 与构建

所有最终 Rust 命令使用基线已有目标目录：

```powershell
$env:CARGO_TARGET_DIR = Join-Path (Get-Location) '.tools/test-target'
```

| 实际命令（cargo 路径均为 `C:/Users/deepin/.cargo/bin/cargo.exe`） | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract --test cloud_coordination --test cloud_directory_download --test cloud_upload` | 退出 0；15 + 2 + 13 + 5 = 35 项通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib cloud::commands` | 退出 0；78 项通过，241 项未运行 |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_session --test cloud_baidu --test cloud_tianyi --test cloud_aliyun --test cloud_xunlei --test cloud_uc --test cloud_quark` | 退出 0；15 + 31 + 31 + 18 + 34 + 11 + 16 = 156 项通过 |
| `node_modules/.bin/vitest.cmd run src/lib/cloudApi.test.ts src/components/CloudWorkspace.test.tsx src/components/CloudManager.test.tsx --exclude '.worktrees/**'` | 退出 0；113 项通过 |
| 通过仓库 `Get-AllDownloaderBuildTools` 解析 pnpm 后执行 `pnpm build` | 退出 0；62 模块 |
| `./scripts/build-windows.ps1` | 退出 0；release 1 分 13 秒，重新发布 exe 与 aria2 sidecar |
| `git diff --check` | 退出 0 |

最终合计 Rust 269 项、前端 113 项定向测试通过。扩大 Rust 定向范围的原因是七个 adapter 与授权保存顺序均被迁移，必须覆盖各 provider 既有协议及安全凭据用例；没有运行无关子系统或全量回归。

前端测试初次在沙箱内被 esbuild 父目录读取权限阻断，获准在沙箱外运行后通过；`pnpm exec vitest` 的 PATH 找不到 shim，改用仓库已有 `vitest.cmd`。这些启动错误均未记作功能 RED。最终仅保留基线两个 dead_code、Windows linker 导出提示、rustup 路径提示及 Vite 的已有静态/动态 window 导入提示。

发布产物：`artifacts/alldownloader.exe`（24,822,272 字节）和 `artifacts/binaries/aria2-next-x86_64-pc-windows-msvc.exe`。仅生成工作树产物，未安装或启动真实账号授权。

## 自审与疑虑

- checked：所有原命令导出、stable source、catalog JSON 和安全单账号契约保留；缺 capability 提前拒绝；auth accessor 不暴露未检查的 DriveOps/UploadOps。
- checked：HTTP 包装没有丢失 cookie/session hooks；回收站变更仍先校验记录归属；百度分享清理仍走专用协议；所有临时下载 URL、headers 和持久 stable source 边界不变。
- checked：网页登录候选先验证根目录，再替换会话；同账号密码恢复校验、browser profile 持久重置义务和失败清理由原 helper 继续执行；密码回退只保存一次最终凭据。
- checked：driver/common 没有 Tauri/WebView/对话框依赖；文件对话框在锁外；PikPak 上传锁边界及迅雷刷新落盘由现有测试覆盖。
- 限制：未执行真实服务商登录、实际 Windows WebView 交互或真实账号端到端上传下载；这部分继续依赖既有协议 fixture/HTTP 回归与发布编译。没有已知阻塞问题。
