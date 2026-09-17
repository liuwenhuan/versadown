# 迅雷登录授权插件重写

## 边界

删除旧 `xunlei-web-session.js`、`xunlei-web-capture.js` 及其提取/完成函数和旧测试。通用密码脚本不再包含迅雷分支。保留网盘文件操作和下载驱动，不重写无关功能。

## 新流程

- 迅雷插件在注册信息中提供初始化、密码表单及会话快照脚本；共享宿主只负责浏览器生命周期、验证结果发布和持久化。
- 网页入口直接打开官方网盘；密码入口从同一网盘父页面进入官方登录 iframe，只向精确 HTTPS 官方来源注入一次表单，保留官方协议确认和安全验证。
- 被动观察官方页面自己的 fetch/XHR 响应，不读取私有 Webpack 模块、不调用内部签名函数、不主动请求验证码。
- 只接受官方成功根目录响应所用的同一账号/设备凭据；原生驱动再次验证根目录，成功后才保存会话、关闭窗口、通知菜单更新。失败保留原账号。
- 已登录但协议不可识别或密码表单不可兼容时，返回固定提示：`迅雷云盘插件需要更新`。网络失败、未登录和需要人工安全验证不伪装成成功。
- 不按网页版本号封锁；官方内部模块重排不会影响本实现。接口域名、公开客户端身份、响应字段或表单结构变化仍可能需要更新插件，不能保证任意未来版本兼容。

## 定向验证

- `node --test scripts/test-xunlei-auth-plugin.cjs scripts/test-password-login.cjs`
- `cargo test --manifest-path src-tauri/Cargo.toml --lib xunlei`
- `cargo test --manifest-path src-tauri/Cargo.toml --lib password_login`
- `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_shared_boundaries`
- `node node_modules/vitest/vitest.mjs run src/components/CloudManager.test.tsx --exclude '.worktrees/**'`
- `powershell.exe -ExecutionPolicy Bypass -File scripts/build-windows.ps1`

## 真实验收

`scripts/verify-xunlei-auth-live.cjs` 在隔离临时数据目录与隐藏窗口中点击实际界面按钮，断言根目录可读、账号可恢复、授权页关闭和迅雷菜单出现。网页模式复用隔离的已登录官方配置；密码模式仅复制本机已有的 DPAPI 加密密码，使用全新浏览器配置。不会输出密码、令牌、Cookie 或文件名，不改动用户原配置，不关闭前台实例。

密码模式：设置 `ALLDOWNLOADER_XUNLEI_PASSWORD_LIVE=1` 后执行脚本。网页模式可设置 `ALLDOWNLOADER_XUNLEI_REUSE_TEST_DATA` 指向既有隔离测试目录，不将其视为可提交的测试夹具。

## 本次执行结果（2026-09-17）

以上定向命令实际执行：Node 9 项通过；Rust `xunlei` 30 项通过、`password_login` 22 项通过（两组有重叠）；共享边界 1 项通过；CloudManager 146 项通过。发布构建成功，未运行全量测试。

最终发布二进制再次执行真实验收，两种模式均退出码 0，返回 `rootReadable=true`、`accountRestorable=true`、`authorizationWindowClosed=true`、`workspaceMenu=true`。网页验收使用已登录官方配置，密码验收使用全新浏览器配置与本机已有加密密码；不声称已覆盖所有账号、安全挑战和未来官方版本。

产物：工作树下 `artifacts/alldownloader.exe`。未替换或重启用户的前台实例。
