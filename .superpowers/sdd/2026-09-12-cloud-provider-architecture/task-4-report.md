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
| 通过仓库 `Get-VersaDownBuildTools` 解析 pnpm 后执行 `pnpm build` | 退出 0；62 模块 |
| `./scripts/build-windows.ps1` | 退出 0；release 1 分 13 秒，重新发布 exe 与 aria2 sidecar |
| `git diff --check` | 退出 0 |

最终合计 Rust 269 项、前端 113 项定向测试通过。扩大 Rust 定向范围的原因是七个 adapter 与授权保存顺序均被迁移，必须覆盖各 provider 既有协议及安全凭据用例；没有运行无关子系统或全量回归。

前端测试初次在沙箱内被 esbuild 父目录读取权限阻断，获准在沙箱外运行后通过；`pnpm exec vitest` 的 PATH 找不到 shim，改用仓库已有 `vitest.cmd`。这些启动错误均未记作功能 RED。最终仅保留基线两个 dead_code、Windows linker 导出提示、rustup 路径提示及 Vite 的已有静态/动态 window 导入提示。

发布产物：`artifacts/versadown.exe`（24,822,272 字节）和 `artifacts/binaries/aria2-next-x86_64-pc-windows-msvc.exe`。仅生成工作树产物，未安装或启动真实账号授权。

## 自审与疑虑

- checked：所有原命令导出、stable source、catalog JSON 和安全单账号契约保留；缺 capability 提前拒绝；auth accessor 不暴露未检查的 DriveOps/UploadOps。
- checked：HTTP 包装没有丢失 cookie/session hooks；回收站变更仍先校验记录归属；百度分享清理仍走专用协议；所有临时下载 URL、headers 和持久 stable source 边界不变。
- checked：网页登录候选先验证根目录，再替换会话；同账号密码恢复校验、browser profile 持久重置义务和失败清理由原 helper 继续执行；密码回退只保存一次最终凭据。
- checked：driver/common 没有 Tauri/WebView/对话框依赖；文件对话框在锁外；PikPak 上传锁边界及迅雷刷新落盘由现有测试覆盖。
- 限制：未执行真实服务商登录、实际 Windows WebView 交互或真实账号端到端上传下载；这部分继续依赖既有协议 fixture/HTTP 回归与发布编译。没有已知阻塞问题。

## Fix round 1：恢复密码登录阶段边界（2026-09-12）

### 发现与修复

复核确认原提交 `8642a48` 的回退范围回归：host 把整个 `ProviderAuth::login_password` 的错误作为浏览器候选，但驱动在底层原生登录成功后还有 account/root 验证。这会把第二阶段的 InvalidResponse、Provider、Captcha 等错误误当作登录回退，启动 WebView，并可能设置百度 browser-preferred。前一轮“密码回退保持行为”的自审结论因此不充分；本轮用阶段行为测试补上缺口。

- 协议边界新增 `NativeLoginOutcome::{Authenticated, NeedBrowserFallback}`。Authenticated 携带验证并保存成功的账户；NeedBrowserFallback 只由底层原生 login 结果中原有明确可回退错误产生。
- 百度、天翼共同使用纯协议阶段 helper：先判断原生 login 结果，再运行 account/root/save；后一阶段任何错误直接保留为 Err，不做回退分类。
- host 的 `password_login_with_browser` 只处理显式 NeedBrowserFallback。浏览器创建、验证码交互与回调留在 commands/auth，未引入 Tauri 到 driver。
- 百度清除 browser preference 的写操作移到 account/root 验证成功之后；验证失败不写入或删除任何会话/偏好。PikPak 仅把原有成功账户封装为 Authenticated，流程与错误不变。
- 原始底层客户端 login 方法内部的既有探测仍属于基线 login phase；本轮只纠正其返回成功之后的驱动验证阶段，没有额外改动服务商协议、超时或重试策略。

### RED → GREEN 与测试边界

先行为保持地提取可注入浏览器回调的 host seam，再添加百度/天翼 × account/root 四项 recording driver 测试。执行：

`cargo test --manifest-path src-tauri/Cargo.toml --lib post_login_`

RED：退出 1，0 passed / 4 failed；四项均在“浏览器 host 调用次数实际 1、预期 0”断言失败，确认真实行为失败，未把编译/链接/沙箱错误记作 RED。完成结果类型与阶段修复后，同一命令 GREEN：4 passed。

随后扩展为独立 `commands/password_login_tests.rs` 中 10 个行为测试，加 1 个 integration consumer：

- 四项 recording driver 分别循环 InvalidResponse、Provider、Captcha，验证 login 成功后 account/root 失败时 host 调用为 0、错误 code/message 原样、preference 不变。
- 三项真实 native driver HTTP 测试：百度第 8 次 account、第 9 次 root 请求，天翼第 6 次 root 请求注入上述三种错误，均保持旧 session，并通过记录 store 验证没有任何 save/remove。
- 原生登录的六种可回退错误仍调用浏览器一次并保留 captcha 标志；Storage、Transport、InvalidInput 等不可回退错误直接返回。
- 百度/天翼真实成功路径仍验证根目录、保存会话、返回 Password 账户，且不启动浏览器。
- 非 GUI integration consumer 通过公开 dyn ProviderAuth 构造并取得 NeedBrowserFallback；未依赖 Tauri host，也未添加测试 manifest 或链接规避。

天翼 App session 的 account() 在原生 login 成功后只读取已保证非空的 login_name，没有独立 HTTP 请求可注入 account 失败；该阶段用 recording ProviderAuth 和同一生产阶段 helper 覆盖，root 阶段另有真实 HTTP 覆盖。此限制没有通过制造不存在的协议请求掩盖。

### 本轮最终验证

Rust 均设置 `CARGO_TARGET_DIR=.tools/test-target` 并调用 `C:/Users/deepin/.cargo/bin/cargo.exe`。按授权边界影响范围选择定向测试，未执行全量测试。

| 实际命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib post_login_` | RED 4 失败后 GREEN 4 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib login_` | 中间扩展回归 22 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib cloud::commands` | 最终 88 通过，241 未运行 |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract --test cloud_session --test cloud_sessions --test cloud_baidu --test cloud_tianyi` | 16 + 15 + 27 + 31 + 31 = 120 通过 |
| `node_modules/.bin/vitest.cmd run src/lib/cloudApi.test.ts src/components/CloudWorkspace.test.tsx src/components/CloudManager.test.tsx --exclude '.worktrees/**'` | 113 通过 |
| 仓库工具解析后的 `pnpm build` | 退出 0，62 模块 |
| `./scripts/build-windows.ps1` | 退出 0，release 50.47 秒；重新发布 exe 与 aria2 sidecar |
| `git diff --check` | 退出 0 |

本轮最终验收 Rust 208 项、前端 113 项通过；中间定向运行不重复累加。前端测试与构建、Windows 发布构建按既有环境约束获准在沙箱外执行。仅有基线 dead_code、linker 导出提示、rustup 路径提示及 Vite window 静态/动态导入提示。

### 自审、规模与疑虑

- 本轮 commands.rs 仍 77 行；auth.rs 639 → 654；driver.rs 251 → 275；Baidu driver 264 → 262；Tianyi driver 264 → 263；新增测试模块 346 行。
- 生产 commands 的 provider match 仍为 0，native driver factory 仍为唯一 1 处；没有恢复巨型命令路由或新增 UI 依赖。
- checked：Tauri 命令名/参数、账户 JSON、file kind、stable sources 和安全单账号保存流程未变；只在成功账户返回后保存密码凭据，验证失败不会进入 browser host。
- checked：回退判定没有再包住 post-validation；百度偏好写入也不发生在失败的 account/root 之前；真实 fixture 明确证明底层 login 已完成才发生注入错误。
- 疑虑：未做真实账号/实际 WebView 交互；天翼 account 无独立网络阶段的测试限制见上。没有剩余已知阻塞问题。

## Fix round 2：session 保存失败不触碰浏览器偏好（2026-09-12）

复审确认前一轮仍遗漏 save 阶段：百度在 account/root 验证后先清除 preference，再保存 session；session 保存失败虽不回退浏览器，但已有 preference remove 副作用。本轮范围仅此顺序，不改协议、接口或 UI。

### RED → GREEN

先增加百度、天翼各一项真实 driver 保存失败用例，共同复用完整登录、account/root fixture，只在 CredentialStore 保存 session 的边界注入 Storage、带原始消息的 Provider 和 CaptchaRequired。断言原错误 JSON code/message、browser host 0 次、旧 session 未替换，以及 store 记录只能有一次失败的 session save，不能有 preference save/remove。

- RED 命令：`cargo test --manifest-path src-tauri/Cargo.toml --lib native_driver_session_save_failure`。
- RED 结果：1 failed（百度）/1 passed（天翼），百度实际记录 `[remove:cloud-password-baidu-browser-preferred, save:baidu]`，预期仅 `[save:baidu]`。失败前的错误、浏览器次数和旧会话断言均已通过；这是实际副作用失败，不是编译/环境错误。
- 最小实现：把百度 `set_browser_password_login_preferred(..., false)` 移至 `save_provider_session(...)?` 成功之后；天翼生产代码无需更改。
- GREEN：同一命令退出 0，2 passed；三种错误均保持原样且没有偏好副作用。

### 本轮验收与自审

本轮所有 Rust 命令仍使用 `C:/Users/deepin/.cargo/bin/cargo.exe`、`CARGO_TARGET_DIR=.tools/test-target`。

| 实际命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib cloud::commands` | 90 通过，241 未运行 |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract --test cloud_session` | 16 + 15 = 31 通过 |
| `./scripts/build-windows.ps1` | 退出 0；release 27.88 秒，重新发布 exe 和 aria2 sidecar |
| `git diff --check` | 退出 0 |

本轮实际重跑 Rust 121 项。按本轮指示，纯 Rust 顺序微调复用同一 Task 4 fix round 1 的前端 113 项与 `pnpm build`（62 模块）通过证据，未声称本轮重跑前端；发布脚本确认前端未变并复用 dist。没有执行全量测试，只有基线编译提示。

自审：生产 diff 仅一行重排；commands/driver 职责、唯一 factory provider match、Tauri 名称和前端契约均未改变。新增两项测试共 111 行，测试模块 346 → 457 行。确认 session save 的 Err 在到达偏好清除前即退出；天翼路径始终没有偏好写删。

风险边界：没有引入跨键事务；session 成功保存后若偏好清除本身失败，仍原样返回该错误，不回退浏览器。本轮只修复“session 保存失败时不得先修改偏好”，未扩大为通用凭据事务。未使用真实账号或实际 WebView；无剩余已知阻塞问题。
