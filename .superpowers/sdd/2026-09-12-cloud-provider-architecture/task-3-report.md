# 任务 3：安全的单账号凭据契约

- 工作树：`C:/Users/deepin/.codex/worktrees/c015/downloader/.worktrees/cloud-provider-architecture-v2`
- 分支：`codex/cloud-provider-architecture-v2`
- 基线：`09c15d7`
- 提交说明：`refactor(cloud): keep saved passwords behind the backend boundary`（包含本报告的提交即任务交付提交）。
- 结论：指定 Rust 23 项、前端 111 项、追加 Rust 19 项通过，前端与最终 Windows 发布构建通过。
- 未派生子代理，未运行全量前端或全量 Rust 测试。

## 契约与实现

`cloud_saved_password_credentials` 保留命令名，只返回 `SavedCloudCredentialInfo { username, hasSavedPassword }`。后端从已保存草稿或已验证密码记录计算元数据；响应结构根本没有 password 字段。前端读取类型同步去除 password，输入与输出类型分开。

`PasswordLoginRequest` 接受 provider、username、可选 password、useSavedPassword。Tauri 命令保留原扁平参数语义；旧调用提交 username/password 时，缺省的 useSavedPassword 为 false。Rust 只允许新密码或者绑定同一用户名的保存引用，拒绝冲突输入、空用户名、缺失密码和保存引用的用户名不匹配。空字符串密码按无新密码处理。有效用户名、密码和加载的保存凭据使用 Zeroizing；请求 Drop 会清空仍未被取走的敏感字段，避免排队失败或提前拒绝时留下请求明文。凭据持久化先完成可失败的读取，再复制密码。

CloudManager 只持有 `CloudAccount | null`，移除 initialAccounts、账号数组、activeAccountId、切换和账号 tablist。CloudWorkspace 继续拥有按动态 descriptor 排列的网盘标签；每个服务商只传一个 initialAccount。目录名称、根目录、授权模式与顺序继续来自 ProviderRegistry/catalog，没有重新引入前端固定服务商映射。

保存账号显示用户名；密码输入值为空、placeholder 为 `****`。密码未编辑时提交 `{ username, useSavedPassword: true }`；编辑后只提交 `{ username, password }`。改用户名不能套用另一用户名的保存引用。新密码请求结束后释放前端密码状态，并重新读取后端元数据决定是否显示保存占位符。

## 替换顺序与失败处理

密码登录：在 Rust 内解析/校验引用 → 比较已验证用户名、草稿和配置 owner → 如账号变化，清旧 session、password、draft、百度 profile owner 与 browser preference → 重置适用的密码 WebView 数据 → 保存本次草稿 → 验证候选账号与根目录 → 保存新 session 和已验证凭据。

网页授权：先验证候选账号及既有验证流程 → `select_password_credential_account` 在新 session 写入前清理旧账号数据 → 持久化新 session。百度、天翼与 PikPak 的调用顺序已同步调整。候选授权本身未通过时仍保留旧会话，不把“尚未完成网页登录”误判为已替换成功。

清理会尝试全部 owner 存储项；任何清理错误返回 Storage，阻断接受和后续新会话落盘。密码落盘失败或读取旧密码失败时也尝试清空 session/credential，避免部分提交。文件系统拒绝删除时不能承诺物理文件已经消失；本次授权会失败，且仍尝试其余清理项。

PikPak 使用直接密码登录，没有百度/天翼使用的密码 WebView profile。复用并收敛既有 profile 支持判断，账号数据清理后不为 PikPak 创建不支持的浏览器配置。

## RED 证据

首次后端命令：

```powershell
C:/Users/deepin/.cargo/bin/cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_session --test cloud_provider_contract
```

退出 1：cloud_session 4 通过、2 失败；cloud_provider_contract 10 通过。

- `saved_credential_response_never_serializes_the_saved_password`：实际 JSON 为 `{ "username": "alice", "password": "stored-secret" }`，预期只有 username/hasSavedPassword。首轮直接使用旧返回 DTO，GREEN 后改为通过实际命令复用的存储元数据函数验证。
- `replacing_browser_account_clears_previous_session_password_and_profile_owner`：选择 B 后读取 baidu 仍得到 A 的 session。

首次前端命令：

```powershell
node_modules/.bin/vitest.cmd run src/lib/cloudApi.test.ts src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx --exclude '.worktrees/**'
```

沙箱内首次启动被 esbuild 上级目录读取权限拦住，不作为 RED；获准在沙箱外重跑后退出 1，2 失败、107 通过。

- 保存元数据后密码 placeholder 仍为“密码”，没有 `****`。
- 已恢复账号仍产生服务商账号 tablist，违反单账号管理器约束。
- 明文输入编辑的已有行为作为兼容回归保留；其初始通过不记作新增行为 RED。

追加 RED：

- 指定后端集：`replacement_removes_old_session_even_when_password_persistence_fails` 因密码写入失败后 session 残留而失败；随后补全失败清理。
- `cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_session saved_password_handle_is_bound`：空字符串配合保存引用被错误拒绝，随后按需求将空字符串视为无新密码。
- `cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_session unreadable_old_credentials`：读取旧凭据失败提前返回而残留 session，随后清理并返回原错误。
- `cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_session replacing_pikpak`：错误请求 PikPak 不支持的密码浏览器 profile，随后修正 profile 重置决策。

## GREEN 与构建

最终指定后端命令退出 0：cloud_session 11 项、cloud_provider_contract 12 项，共 23 项通过。指定前端三文件通过 111 项（CloudManager 92、CloudWorkspace 15、cloudApi 4），无本次新增 React 警告；覆盖保存占位符、引用提交、新密码提交、单账号重新授权、工作区退出后换账号与原文件操作。

`pnpm build` 首轮发现独立视觉夹具还在使用移除的 initialAccounts/showAccountTabs；迁移为 initialAccount 后退出 0。构建 62 模块；已有 Tauri window 同时静态/动态导入的分块提示保留。

考虑网页授权落盘顺序的直接风险，追加而非全量执行：

```powershell
C:/Users/deepin/.cargo/bin/cargo.exe test --manifest-path src-tauri/Cargo.toml --lib cloud::session::tests
C:/Users/deepin/.cargo/bin/cargo.exe test --manifest-path src-tauri/Cargo.toml --lib baidu_web_authorization
C:/Users/deepin/.cargo/bin/cargo.exe test --manifest-path src-tauri/Cargo.toml --lib tianyi_web_authorization
```

分别退出 0：12、2、5 项通过。旧密码格式兼容、草稿清理、候选根目录校验失败保留旧会话、验证成功再保存等既有行为没有回归。

`./scripts/build-windows.ps1` 已多次退出 0；自审每次改变 Rust 之后都会重建，最终一轮退出 0（release 52.62 秒），产物路径为 `artifacts/versadown.exe` 与 `artifacts/binaries/aria2-next-x86_64-pc-windows-msvc.exe`。仅构建，不安装、不启动真实账号登录。

Rust 既有警告：两个无关的 dead_code、Windows linker 导出信息与 rustup 路径 canonicalize 提示；均未阻断编译或测试。`git diff --check` 通过，仅 Git 行尾提示。

## 文件与自审

- `src-tauri/src/cloud/types.rs`：安全返回 DTO、密码请求与 Drop 清理。
- `src-tauri/src/cloud/session.rs`：保存引用解析、元数据读取、单账号清理及失败封闭。
- `src-tauri/src/cloud/commands.rs`：命令兼容、解析边界与网页落盘顺序。
- `src-tauri/tests/cloud_session.rs`、`cloud_provider_contract.rs`：保密、保存引用、账号替换、清理失败、写/读失败与目录能力覆盖。
- `src/lib/cloudTypes.ts`、`cloudApi.ts`、`cloudApi.test.ts`：输入输出分离与桥接参数验证。
- `src/components/CloudManager.tsx`、`CloudWorkspace.tsx` 及各自测试：单账号状态、动态网盘工作区和占位符行为。
- `src/visual-tests/CloudAuthorizationVisual.tsx`：编译发现的必要调用方迁移。

自审关注点：前端返回类型不能携带密码；空白/冲突/跨用户名引用拒绝；网页流程不在清理前写新 session；密码保存失败不留下部分 session；各 owner 删除错误不短路其它删除；PikPak 不误走密码浏览器；工作区标签仍按 catalog；旧单账号记录仍可读。未运行真实服务商登录和 Windows WebView 交互验收，服务商行为由当前定向测试及发布编译验证。

## Fix round 1：持久重置义务与编辑版本保护

基线 `ef3532f`。本轮按审查意见逐条验证，不派生子代理。

### P1：失败的浏览器清理不能丢失重置义务

根因确认：旧实现先删除 session/password/draft/profile owner，再清理浏览器数据。清理失败后没有持久状态证明仍需清理；重新打开存储并提交同一新用户名时，旧 owner 已为空，代码会绕过重置。

RED 命令：

```powershell
C:/Users/deepin/.cargo/bin/cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_session failed_password_profile_reset
```

退出 1。`failed_password_profile_reset_is_required_again_after_reopening_encrypted_store` 在真正创建/关闭/重新创建 DpapiCredentialStore 后断言失败：`Baidu must not accept old-account after a failed profile reset and restart`。并非 mock 存储或源代码文本检查。

修复：统一安全凭据键契约新增 `cloud-password-profile-reset-baidu`、`cloud-password-profile-reset-tianyi`；仍通过现有 DPAPI 存储加密、临时文件写入与原子发布。`prepare_password_account_login` 必须先保存标志，才可清理旧 owner；后续准备登录始终重读该标志。`reset_password_profile_if_required` 先确保持久标志存在，再执行真实 WebView 清理回调；回调成功后使用存储的单文件删除操作原子删除标志。写标志、清理或删标志失败均返回错误，阻断保存登录草稿和接受旧浏览器账号。任何普通账号凭据清理都不删除此标志。

最终测试拆为百度和天翼两个独立重启用例，均验证：第一次清理失败 → 旧 session 已清理 → 重开同一加密存储 → 仍强制清理，无法到达接受 old-account 的分支 → 标志文件是密文 → 成功清理才删除标志，之后不再重复重置。另有标志写入失败保留旧 owner/不启动浏览器清理、标志删除失败保留重置义务并阻止登录的测试。标志只在 Rust 内使用，不加入前端 DTO，不明文写盘。

### P2：旧请求不能清空用户后来输入的密码

根因确认：原 finally 块无条件清密码，并异步恢复保存元数据。请求等待期间输入 P2，旧请求结束后会将 P2 清空。

RED 命令：

```powershell
node_modules/.bin/vitest.cmd run src/components/CloudManager.test.tsx --exclude '.worktrees/**' -t 'preserves a password edited'
```

退出 1：`preserves a password edited while an earlier login is pending and submits the newer value next` 预期输入值 P2，实际空串。输入保持可编辑，测试确实在延迟失败请求期间输入了新密码。

修复：用户名和密码编辑递增输入版本，提交时捕获版本快照。旧请求结束时仅在版本仍匹配时清本次输入；读取保存元数据后再次核对版本，防止这个异步阶段覆盖新编辑。未禁用输入，也没有仅靠字符串相等判断而忽略编辑后改回原值的情形。

GREEN 覆盖 P1 请求延迟失败期间输入 P2、P2 保留且下次确实提交 P2；同时覆盖元数据刷新期间编辑并清空密码，旧响应不能恢复 `****` 或偷偷提交保存引用。

### 本轮最终验证与自审

```powershell
C:/Users/deepin/.cargo/bin/cargo.exe test --manifest-path src-tauri/Cargo.toml --test cloud_session --test cloud_provider_contract
node_modules/.bin/vitest.cmd run src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx src/lib/cloudApi.test.ts --exclude '.worktrees/**'
pnpm build
./scripts/build-windows.ps1
git diff --check
```

Rust 指定集退出 0：cloud_session 15、cloud_provider_contract 12，共 27 项。前端指定集退出 0：CloudManager 94、CloudWorkspace 15、cloudApi 4，共 113 项。`pnpm build` 退出 0，62 模块。`./scripts/build-windows.ps1` 退出 0，release 用时 54.45 秒，已重新发布主程序与 aria2 sidecar。`git diff --check` 退出 0。

本轮改动仅涉及 credentials/session/commands、CloudManager 及相关测试和本报告。保留已有 Rust dead_code、链接信息和 Vite 分块提示，没有新增阻塞问题。自审确认持久标志写入先于 owner 删除，失败回调与失败删标志均不继续登录；同一 provider 的既有串行执行边界保持不变。真实 Windows DPAPI 重开与密文检查已执行；真实服务商登录和实际 WebView 清理仍未交互验收，失败回调边界通过定向测试注入。
