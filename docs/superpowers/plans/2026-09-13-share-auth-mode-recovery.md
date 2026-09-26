# 多授权模式分享恢复 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让网盘分享按有效会话传输能力工作，支持天翼、百度和 PikPak 的账号密码与网页授权会话，而不将登录来源写死为单一路径。

**Architecture:** 分享检查继续通过 `AuthSupervisor` 的读操作恢复顺序运行，但使用中性名称表达其可使用当前已保存的任意授权方式。天翼适配器在安全分享 GET 被失效 Cookie 拒绝时，利用同一账号仍有效的 App 签名会话换取并持久化新 Cookie，最多重放该 GET 一次。百度工作由并行百度修复任务持有；合并前必须核验其 Password/Web Cookie 契约测试。

**Tech Stack:** Rust、Tauri、serde_json、现有 `CloudHttpTransport` mock、Cargo integration/unit tests、Vitest、Vite。

**Spec:** `docs/superpowers/specs/2026-09-13-share-auth-mode-recovery-design.md`

## Global Constraints

- `auth_method` 是账号登录来源元数据，分享适配器只能验证实际的 Cookie/token/App 会话。
- 仅 `AuthorizationExpired` 和 `NotAuthorized` 可触发恢复；访问码、验证码和风险验证不可触发密码重登。
- 天翼的 App 会话、Cookie、浏览器标识和 User-Agent 必须一并保留，且单次请求最多做一次 Cookie 换取重试。
- 不执行无关全量测试；Vitest 必须传入 `--exclude '.worktrees/**'`。
- Windows 发布验证使用 `scripts/build-windows.ps1`；如目标 exe 正在运行，不能覆盖或终止它。

---

### Task 1: 将分享读操作表述为通用会话恢复

**Files:**
- Modify: `src-tauri/src/cloud/commands/common.rs:180-188`
- Modify: `src-tauri/src/cloud/commands/share.rs:10-22`
- Test: `src-tauri/src/cloud/auth_supervisor.rs`（沿用 `recover_in_order` 单元测试模块）

**Interfaces:**
- Consumes: `run_session_work(AppHandle, CloudProvider, SessionWorkKind::Read, work)`。
- Produces: `run_share_session_read_work(AppHandle, CloudProvider, work) -> Result<T, QuarkError>`，其实现直接委托 `run_session_work`。

- [x] **Step 1: 审查现有恢复边界**

确认原 `run_web_session_read_work` 已直接委托 `run_session_work`，后者在 `AuthorizationExpired` 与 `NotAuthorized` 时使用 `AuthSupervisor` 的恢复顺序；访问码、验证码和风控均不满足这个入口条件。因此不新增会改变风险策略的全局重试。

- [x] **Step 2: 最小实现**

将 `run_web_session_read_work` 重命名为：

```rust
pub(crate) async fn run_share_session_read_work<T: Send + 'static>(
    app: AppHandle,
    provider: CloudProvider,
    work: impl FnMut(&SessionWorkContext<'_>, &ReqwestTransport) -> Result<T, QuarkError>
        + Send
        + 'static,
) -> Result<T, QuarkError> {
    run_session_work(app, provider, SessionWorkKind::Read, work).await
}
```

并把 `inspect_cloud_share` 的注释替换为：只读检查可以在真实认证失效后恢复并重试一次；访问码、验证码和风控不是认证恢复。调用新的中性函数名。

- [x] **Step 3: 通过服务回归编译并验证**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --lib tianyi`

Observed: PASS，38 项通过。

### Task 2: 让天翼失效的网页 Cookie 可由已保存 App 会话重建

**Files:**
- Modify: `src-tauri/src/cloud/tianyi.rs:161-229,1282-1400`
- Modify: `src-tauri/src/cloud/tianyi_app.rs:343-380`
- Test: `src-tauri/src/cloud/tianyi.rs:tests`

**Interfaces:**
- Consumes: `tianyi_app::refresh_web_cookie(&TianyiAppSession, &dyn CloudHttpTransport) -> Result<String, QuarkError>`。
- Produces: `TianyiShareSession::request(&mut self, path, query) -> Result<Value, QuarkError>`，在 `AuthorizationExpired` 时可利用内部 App session 更新 Cookie 并只重试一次。

- [x] **Step 1: 写失败测试**

新增 `tianyi_share_replaces_a_rejected_web_cookie_from_signed_app_session`。fixture 保存 `auth_method: password`、有效 App session 和 `COOKIE_LOGIN_USER=stale`；mock 依次返回：分享接口 `res_code: -1`、SSO 换取 Cookie 的成功响应和 `COOKIE_LOGIN_USER=recovered; JSESSIONID=share-session`、重试分享接口的有效文件响应。断言：

```rust
assert_eq!(plan.entries[0].name, "05_如何制作动感影集.mp4");
assert_eq!(requests.len(), 3);
assert!(requests[1].url.contains("/ssoLogin.action?sessionKey=app-key"));
assert!(stored.cookie.contains("COOKIE_LOGIN_USER=recovered"));
assert!(stored.cookie.contains("JSESSIONID=share-session"));
assert!(stored.app.is_some());
```

- [x] **Step 2: 运行失败测试，确认它因旧 Cookie 直接导致认证过期而失败**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --lib tianyi_share_replaces_a_rejected_web_cookie_from_signed_app_session`

Observed: FAIL，结果为 `AuthorizationExpired`，且不会出现 SSO 请求。

- [x] **Step 3: 最小实现**

让 `TianyiShareSession` 保存完整 `TianyiSession`、凭据 store 和 `renewed_cookie: bool`。将每次 GET 发送收敛为 `send_request`；`request` 在首次 `AuthorizationExpired`、`renewed_cookie == false` 且 `session.app` 存在时执行：

```rust
self.renewed_cookie = true;
let cookie = tianyi_app::refresh_web_cookie(app, self.http)?;
if cookie.trim().is_empty() {
    return Err(QuarkError::AuthorizationExpired);
}
self.session.cookie = cookie;
self.persist_session()?;
self.send_request(path, query)
```

非认证错误、第二次认证错误、缺少 App session 都返回原错误。创建 session 时 Cookie 为空仍沿用同一 `refresh_web_cookie` 和持久化逻辑；不额外重放副作用操作。

- [x] **Step 4: 运行天翼回归集**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --test cloud_tianyi`

Observed: PASS，`cloud_tianyi` 36 项、`--lib tianyi` 38 项均包含上述路径。

### Task 3: 核验百度与 PikPak 的双授权来源契约

**Files:**
- Modify: `src-tauri/src/cloud/baidu.rs`（由并行百度任务持有）
- Test: `src-tauri/tests/cloud_baidu.rs`（由并行百度任务持有）
- Test: `src-tauri/src/cloud/pikpak.rs:tests`

**Interfaces:**
- Consumes: `BaiduSession { cookie, auth_method }` 和 `PikPakSession { access_token, auth_method }`。
- Produces: 两个分享适配器均不读取 `auth_method` 作为会话准入条件；有效 Cookie/token 的 Password 与 Web session 产生相同分享检查结果。

- [x] **Step 1: 写双授权来源契约测试并审查百度适配器**

PikPak 测试以相同 bearer token 构造 `CloudAuthMethod::Password` 与 `CloudAuthMethod::Web`，断言两次 share inspect 都得到同一 entry id。审查百度适配器：其分享会话只依据 `BDUSS` Cookie，没有对 `auth_method` 的准入分支。

- [x] **Step 2: 确认已有实现并执行服务回归**

百度和 PikPak 适配器只检查 `BDUSS`/token 与必需设备信息；PikPak 原实现已满足，新增回归契约。百度修改由并行百度任务完成，本任务只读取和验证其 diff，避免覆盖。

- [x] **Step 3: 运行服务定向测试**

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --test cloud_baidu`

Run: `C:\Users\deepin\.cargo\bin\cargo.exe test --lib pikpak`

Observed: `cloud_baidu` 38 项通过；PikPak 12 项通过。

### Task 4: 端到端定向验证与 Windows 发布构建

**Files:**
- Test: `src/components/CloudManager.test.tsx`
- Test: `src/lib/cloudApi.test.ts`
- Build: `package.json`, `scripts/build-windows.ps1`

**Interfaces:**
- Consumes: 后端既有的 `cloud_failure_code` 与 Cloud Manager 错误提示。
- Produces: 不改变前端登录方式选择；通过更新后的后端错误语义引导必要的网页授权。

- [x] **Step 1: 运行前端定向测试**

Run: `pnpm test --run src/components/CloudManager.test.tsx src/lib/cloudApi.test.ts --exclude '.worktrees/**'`

Observed: 当前树的 Cloud Manager 128 项、API 10 项通过；Vitest 还发现一个既有 `.tmp-master-build` 副本，合计 244 项通过。

- [x] **Step 2: 运行前端构建**

Run: `pnpm run build`

Observed: exit code 0。

- [x] **Step 3: 检查 Rust 格式与差异空白**

Run: `C:\Users\deepin\.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin\rustfmt.exe --edition 2021 --check src\cloud\commands\common.rs src\cloud\commands\share.rs src\cloud\tianyi.rs src\cloud\tianyi_app.rs`

Run: `git diff --check`

Observed: 本次天翼与分享文件的 scoped rustfmt 检查成功，`git diff --check` 成功；PikPak 文件存在本任务之外的既有 rustfmt 差异，未作全文件格式化。

- [x] **Step 4: 运行 Windows 发布构建**

Run: `powershell -ExecutionPolicy Bypass -File scripts/build-windows.ps1`

Observed: `scripts/build-windows.ps1` exit 0；`artifacts/versadown.exe` 已更新。
