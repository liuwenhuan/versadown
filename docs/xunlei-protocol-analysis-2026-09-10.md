# 迅雷云盘接入协议核查（2026-09-10）

## 结论与证据边界

当前连接器尚未完成真实账号接入。`7add766` 增加的根目录检查只能防止误报授权成功，不能修复请求协议；上一轮通过的模拟测试不应作为实机可用证明。

这次比对了运行中的本机官方客户端、当天在线的官方网页 JavaScript、OpenList 源码和本项目代码。证实存在协议混用、凭据选择错误及网页续期能力被错误限制。尚未捕获该账号本次登录后的实际云盘请求，因此不能断言每个问题都已在这个账号上触发，也不能保证改正后即可下载。

## 三种“迅雷”不能混为一谈

| 接入对象 | 依据 | 接入时的含义 |
| --- | --- | --- |
| `pan.xunlei.com` 网页云盘 | 官方当前 SSO、captcha、文件列表模块 | 应按网页 SSO 与操作验证流程实现 |
| 迅雷 Windows 客户端 | 本机运行版本 `25.0.92.1608`，包含独立云盘插件 | 可作为对照样本；其客户端身份和登录态不能直接假定等于网页 |
| OpenList `thunder_browser` | 包名 `com.xunlei.browser`，API 为 `x-api-pan.xunlei.com` | 是安卓“迅雷浏览器”产品，并非网页云盘驱动 |

参考：[OpenList 迅雷驱动说明](https://docs.openlist.team/zh/guide/drivers/thunder)、[thunder_browser 源码](https://github.com/OpenListTeam/OpenList/blob/main/drivers/thunder_browser/util.go)。

## 官方网页实际请求链

官方 `utils-initial` 的 `findFiles` 最终调用导出 `f`，参数路径是 `files`；该导出进入 `drive/v1/` 请求封装，再调用 captcha service，底层调用 SSO 的 `request`。

因此普通根目录请求为 `GET https://api-pan.xunlei.com/drive/v1/files`，根 `parent_id` 使用空字符串，后续通过 `page_token` 翻页。网页 done 过滤包括完成态与非回收站；all 过滤则不同。这里描述的是官方请求形状，不代表每个过滤参数都是服务端强制字段。

登录身份和操作验证是两个维度：

1. SSO 管理当前账号的 access token、refresh token 与用户身份；请求使用 `Authorization`。
2. 云盘请求携带对应网页客户端的 `x-device-id`、`x-client-id`；操作验证模块按 `METHOD:/path` 组织 action，在需要时调用 `/v1/shield/captcha/init` 并附加 `x-captcha-token`。
3. 验证模块使用匹配该客户端的版本、包名、时间戳等元信息。若服务要求交互验证，必须让用户在官方页面完成，再重试对应操作。

同一个公开 bundle 中也有旧式请求封装，会读取 `Peer-Id`、`Session-Id`、`Credit-Key` 等字段。它的存在不能证明这些字段就是现代文件列表接口的必需参数。

官方参考：[文件请求模块](https://static-pan.xunlei.com/_nuxt/dist/client/utils-initial.38a2added9b0686c9067.js)、[操作验证模块](https://static-pan.xunlei.com/_nuxt/dist/client/service-captcha.ac5c3d109dc206a227f9.js)、[SSO 模块](https://static-pan.xunlei.com/_nuxt/dist/client/service-sso.93351a71572962edfc08.js)。

## 当前代码的具体问题

| 位置 | 核查结果 | 后果 |
| --- | --- | --- |
| `xunlei.rs::is_complete`、`commands.rs::complete_xunlei_web_context` | 本地强制 `credit_key` 非空 | 即使已取得当前账号 bearer，也可能在发出目录请求前一直 Pending |
| `xunlei.rs::browser_protocol_headers/drive_url` | 把旧式会话头及 `_sessid/_uid/_h[]` 与 Bearer 云盘请求拼接 | 模拟测试验证了本地拼装，未验证其符合官方文件列表调用链 |
| `commands.rs::XUNLEI_WEB_TOKENS_JS` | 先遍历全部 storage，先找到的字段优先；通用 `id` 也被接受为 user_id | 旧记录或无关对象可抢占当前登录身份，甚至拼出不同记录的凭据组合 |
| `xunlei.rs::raw` | 只转发已有 `security_token`，没有实际操作验证令牌生命周期 | 网页完成登录后，独立后端仍可能缺少云盘操作所需验证 |
| `XunleiClientProfile::can_refresh` | 必須具备原生 `client_secret` 才允许 refresh | 把原生客户端限制强加给网页 SSO，导致可续期的网页会话被当成只能重登录 |
| `authorization_result` | 认证失效、需要网页验证等统一变成 Pending | 用户无法区分“还没登录”和“已经登录但后台验证/协议失败” |

网页 SSO 源码的默认 refresh 使用 `/v1/auth/token`。当前网页配置和传入 OAuth2Client 的选项没有 `clientSecret`；请求对象中的 undefined `client_secret` 会在 JSON 序列化时省略。因此，“网页没有原生 secret，所以不能 refresh”不是成立的协议结论。独立 Rust 续期是否被当前服务端接受，仍需真实账号验证。

当前公开网页 profile 是 client ID `Xqp0kJBXWhwaTpB6`、包名 `pan.xunlei.com`。这些是公开应用标识，不是用户 token。SSO SDK 还构造 `x-client-id`、`x-sdk-version`、`x-protocol-version` 与 `x-action` 等头；不能只实现三个 body 字段就宣称完成了续期适配。SDK 的 `withCredentials` 表示附加 OAuth 凭据，不能直接等同 Fetch 的 Cookie `credentials: include`。

### 已执行的可复现检查

直接提取本项目 `XUNLEI_WEB_TOKENS_JS`，在 Node VM 中执行，全部使用合成数据：

- 仅提供官方 `users.curUser` 形状：能取得当前用户、bearer、session、peer，未取得 credit key。这种候选会被现有非空门槛拒绝。
- 先放入带旧 `id/access_token` 的 storage 记录，再提供当前 `users.curUser`：输出的 user_id、access_token 均不是当前用户的值。这个优先级缺陷已经实际复现。

以上是脚本行为验证，不是账号认证成功测试。

## 本机官方客户端检查

确认运行文件为 `C:/Program Files/Thunder Network/Thunder/program/Thunder.exe`，产品版本 `25.0.92.1608`。安装资源包含 `resources/app/plugins/thunder-pan-plugin.asar` 和 `resources/app/out.asar`；两者使用非标准 ASAR 头，标准归档解析不能读取。检查的 9 月 8、9 日 xlaccount 日志为空，没有可用于对比的请求记录。

客户端的存在可以用于后续对比账号云盘目录是否可读、登录设备身份及下载请求，但目前没有证据表明它提供可直接调用的公开云盘 CLI 或导出授权接口。不能因为发现本地监听端口就认定它支持网盘控制，也不能把客户端私有登录态当成网页 Cookie 使用。

## 建议的纯 Rust 接入方案

优先完成网页 SSO 路线；授权窗口负责官方登录与人工验证，Rust 负责云盘请求和会话维护。

1. 把网页登录凭据建模为同一账号的一组数据，优先读取官方 SSO 当前账号；禁止混扫 storage 拼接凭据。设备 ID、网页 client ID、有效期和 refresh token 与账号绑定。
2. 单独实现网页协议客户端，按官方文件列表调用链构造请求；将旧接口兼容逻辑与原生客户端 profile 分开。
3. 实现网页 token 续期和按 action 维护的操作验证；保留服务返回的验证入口，验证完成后重新请求。公开客户端参数可版本化维护，不能靠换成百度 `netdisk` User-Agent 解决。
4. 授权状态明确区分未登录、已登录待验证、服务端拒绝、可读目录。只有后端用刚取得的会话真实读取目录成功，才保存并关闭授权窗口。
5. 用该账号比较官方成功的根目录请求与 Rust 请求，再验证非根目录、分页和续期。之后只在专用 `AllDownloader-IT-*` 目录测试上传、下载字节一致性、改名、移动和移入回收站。

原生客户端路线可作为后续独立适配器：需要确认客户端身份、登录/令牌交换、设备验证以及下载头的完整组合。不能把多个产品的 client ID、版本、secret、签名元信息和 User-Agent 混搭。

可参考 OpenList `thunder` 的安卓下载器流程：Core 登录取得 sessionid，经 `/v1/auth/signin/token` 使用 `access_end_point_token` 交换 OAuth 凭据，再以 Bearer 和操作验证令牌请求云盘。这是参考实现的证据，不是本机 Windows 25 客户端也使用完全相同登录链的证明。[参考 driver 源码](https://github.com/OpenListTeam/OpenList/blob/main/drivers/thunder/driver.go)

## 在线源码指纹

本次直接取得 HTTP 200 的公开资源：

- `service-sso.93351a71572962edfc08.js`，SHA-256 `5a3268da80466e6f132be51501540fd1fde498d408cdb2b932094eee5be4e64e`
- `service-captcha.ac5c3d109dc206a227f9.js`，SHA-256 `8704534f7e7314d318cc966438456560f017550853afff46bddcbf1643533315`

本轮是协议研究与诊断，未改动连接器生产代码、未发布新二进制，也未把公开源码分析或模拟响应当成实机通过。

### 复查入口

以下为上述公开文件的零基字符偏移，供后续复查调用关系；网站更新资源后应重新确认：

- `utils-initial`：`findFiles` 委托约 54909；module 2 的 `f` 导出约 165851；现代 drive helper 约 177625；旧式 `Credit-Key` helper 约 171853。
- `service-captcha`：`baseRequest=sso.request.bind(sso)` 约 26429；设备/client 头约 28262；captcha header 约 20408；shield 初始化约 21260。
- `service-sso`：Authorization 注入约 76147；默认 refresh 约 78438；SSO 构造 OAuth2Client 的配置约 211838。

## 追加核查：官方 SDK 凭据、交互验证与签名（2026-09-10）

本附录记录上文诊断之后的公开源码核对和桥接实现。上文“当前代码的具体问题”及“未改动生产代码”描述的是较早诊断阶段，不代表后续实现状态。本附录的入口、公式及合成向量来自公开官方 JavaScript；不代表真实账号已授权成功、Rust 请求已被服务端接受，或上传下载已完成实机验证。

### 当前 SDK 的真实入口

下表的 `require` 指官方页面当前 Webpack 4 运行时的模块加载函数。模块编号只适用于本文记录的资源版本，不是稳定公开 API；网页更新后必须重新核对。提取脚本通过临时 JSONP 模块取得该函数，并清理自己的临时模块，不改写官方业务模块。

| 入口 | 已核对行为 |
| --- | --- |
| `require(600).sso` | 当前网页 SSO 单例；`require(21).getSso()` 是其官方异步加载入口 |
| `sso.oAuth2Client.getCurrentSub()` | 异步返回当前账号 subject；应 `await`，不能把 Promise 当作账号字符串 |
| `sso.oAuth2Client.getCredentials()` | 异步返回当前账号整组 `{sub, access_token, refresh_token, token_type, expires_at}`；SDK 将日期解析为 `Date` |
| `require(32).default` | 当前域名对应的网页客户端配置，包括 `clientId`、`packageName`、`algVersion`、`funFile`、`funName` |
| `require(178).version` | 当前网页版本 `1.92.98` |
| `await (await require(142).a).deviceID` | 官方设备标识；不拿旧 peer ID 或随机值替代 |
| `require(16).preloadCaptchaRuntime()` | 官方 captcha 与签名运行时加载入口，避免把模块尚未加载误判为永久不可用 |
| `await require(16).getCaptchaClient()` | 返回 `{auth, captcha}`，其中 `captcha` 管理操作验证和交互窗口 |
| `await require(1618).a(timestamp)` | 调用官方网页签名实现；`timestamp` 是毫秒字符串 |

官方 SSO 的 storage key 是 `credentials_<clientId>`；多账号模式是 `credentials_<clientId>@<sub>`，当前账号指针为 `current_sub`。这些名称说明 SDK 如何选择账号，不授权提取器遍历所有记录并拼接字段。桥接实现优先读取 SDK，前后检查当前 subject 与凭据中的 subject 一致；仅当 SDK 不可用时，才使用当前 Vuex 账号的明确凭据字段，不接受通用 `id`。

来源：[SSO bundle](https://static-pan.xunlei.com/_nuxt/dist/client/service-sso.93351a71572962edfc08.js)、[页面初始工具 bundle](https://static-pan.xunlei.com/_nuxt/dist/client/utils-initial.38a2added9b0686c9067.js)、[页面异步工具 bundle](https://static-pan.xunlei.com/_nuxt/dist/client/utils-async.413fd3b1f17c7388b322.js)。

### 官方 forceRenew 与人工验证闭环

官方 captcha SDK 的请求入口是 `getCaptchaToken({forceRenew: true}, options)`。桥接使用以下结构，其中所有账号、设备及签名字段来自刚确认的当前会话；示例不包含真实用户凭据：

```javascript
const { captcha } = await require(16).getCaptchaClient();
const result = await captcha.getCaptchaToken({ forceRenew: true }, {
  action: 'GET:/drive/v1/files',
  withCaptchaMeta: {
    package_name: 'pan.xunlei.com',
    client_version,
    captcha_sign,
    timestamp,
    user_id,
  },
});
// result: { token, expires_at: Date字符串 }
```

调用前还核对 `captcha._config.clientId` 与 `originDeviceId`，确保与选定 profile、设备相符。`getCaptchaToken` 经 `fetchCaptchaToken`、`fetchShield` 发出 `POST https://xluser-ssl.xunlei.com/v1/shield/captcha/init`，JSON body 为 `{client_id, action, device_id, captcha_token, meta}`。`captcha_token` 可以是 SDK 先前缓存的令牌；`forceRenew: true` 保证执行新的初始化与验证流程，而不是直接把旧缓存作为本次结果返回。

服务返回验证 URL 时，SDK 创建官方浏览器窗口，挂载目标为 `document.getElementById('shield-app')`，设置 `redirect_uri=https://pan.xunlei.com/loading/` 与本次 state，等待页面加载回该入口，再保存新令牌并兑现 Promise。实际窗口能否显示以及用户能否完成验证，仍需要实机确认；本地提取脚本不自动解答验证。

SDK 存储键为 `captcha_<clientId>`，值为 JSON `{token, expires_at}`。`setExpiresAt` 用 `Date.now() + (expires_in - 30) * 1000` 计算日期字符串。该存储只有客户端维度，没有账号或 action 维度，因此不能把刚读到的旧 storage token直接归属于新登录账号，也不能仅凭这种存储结构断言服务端允许跨 action 复用。

桥接脚本等待官方 Promise 完成，宿主需跨轮询保留同一次任务，避免每次轮询重复 `forceRenew`。成功返回 `web_security.captcha_token`、`expires_at`（Unix 秒）及本次 `action`；过期、缺失结果或 SDK 拒绝只返回固定错误 `{authorization_error: 'captcha_verification_failed'}`，不转发可能含凭据的原始错误。所有异步阶段完成后再次检查账号，发生切换则丢弃整个候选。最终授权完成条件仍是 Rust 使用该候选真实读取根目录成功。

来源：[官方 captcha SDK 与 service 包装](https://static-pan.xunlei.com/_nuxt/dist/client/service-captcha.ac5c3d109dc206a227f9.js)。

### 可忠实移植的精确版本签名

本次核实的适用组合为：`client_id=Xqp0kJBXWhwaTpB6`、`client_version=1.92.98`、`package_name=pan.xunlei.com`、网页 host `pan.xunlei.com`、`algVersion=1`、`funFile=filters`、`funName=pub1iwjKbiXrwP9i86t19Ao0HtnZnkjCjZk`。不能把以下盐链应用于其他版本、域名或迅雷产品。

`utils-initial` 模块 16 用 `String(Date.now())` 为 captcha service 提供毫秒时间戳。`utils-async` 模块 1618 按下式生成签名输入，字段之间没有分隔符：

```text
value = client_id + client_version + location.host + device_id + timestamp
对以下 salt 顺序执行：value = lowercase_hex(MD5(UTF8(value + salt)))
captcha_sign = "1." + value
```

当前 `filters` 分支由 [service-pubkey 模块](https://static-pan.xunlei.com/_nuxt/dist/client/service-pubkey.5b794464b4eeb0a07b31.js) 的 `getPubKeySignUtil` 载入模块 25 导出函数，再跨官方工具模块执行以下 15 轮公开盐链：

```json
[
  "rgui",
  "GBbHjL+W3+IJcgUgydd8zuV2hrhnID9yPI",
  "KRDkNzr29JqPNUq2mxw+YFtueKYZlzmWJIaHkj3NSh/ogclHVm",
  "JMGo+UYkPo6/I4s1enZg",
  "ZcfXzUZq+yx9VKk1fV4xAQhq+TwyE42MP7JXORDQy",
  "Z",
  "WCvfOrvOIFJWrKDen7WBq17dX",
  "3J07h/7",
  "Im3qdyHl9DZH",
  "Jg",
  "Y+KS",
  "/1lcxUF8",
  "C",
  "GVEo",
  "8k4v5dntCyPof87pZ0gc/EixX3qv5RxoaGzZ"
]
```

为核对公式，执行了官方原始 `pub` 函数链和官方 `app` 模块 0 的 MD5 实现，并把每一轮结果与 Node `crypto` 的 MD5 逐轮对照。未执行的页面初始化依赖以隔离环境替代，签名函数与 MD5 函数使用公开 bundle 的原始实现。以下所有输入都是合成数据，不是用户设备 ID、账号 token 或实际请求记录：

| 输入（其余字段使用上述精确 profile） | 官方 JavaScript 结果 |
| --- | --- |
| `device_id=0123456789abcdef`，`timestamp=1788975000000` | `1.1b11bbf6b08458edaccf7acdaefbbb1e` |
| `device_id=synthetic-device`，`timestamp=1788975001000` | `1.124ca821e508ff39cb555e7012101bee` |
| 直接对字面量 `BASE` 运行盐链（不构成有效请求输入） | `1.fa4c4cc8ba79e1e1adfc3c539db3668e` |

这些向量证明本地公式与所检查的公开实现一致，不能证明服务端接受某个账号、设备或操作。算法本身不是人工验证的替代品，服务端返回交互验证时仍需进入官方流程。对未知版本应回退为当前网页捕获的签名，或要求重新核对 profile；不要假设旧盐链永久有效。先前提出的“五分钟签名窗口”是本地保守限制，并非这份公开源码证实的服务端有效期；官方页面本身在初始化时保存时间戳。

模块 1618 另有动态算法分支：仅在 `funFile` 为 `BEGIN_FUN_FILE_XUNLEI` 或 `BEGIN_FUN_FILE_XLPAN` 时，从 `https://risk-account-ssl.office.k8s.xunlei.cn/amaze/v1.0/algorithms` 获取 `algorithms[].salt`。该分支查询参数包含 client、版本、包名、秒级时间戳、算法版本及配置签名。当前 `pan.xunlei.com` 配置为 `filters`，不走该分支；本轮没有把动态接口可用性或返回值作为实现依据。

### 追加公开资源指纹与验证范围

2026-09-10 检查的官方首页仍引用以下命名资源。下列 SHA-256 来自本次读取的公开资源字节；相同版本字符串若对应不同 bundle，也应重新核对实现。

| 公开资源 | SHA-256 |
| --- | --- |
| [utils-initial.38a2added9b0686c9067.js](https://static-pan.xunlei.com/_nuxt/dist/client/utils-initial.38a2added9b0686c9067.js) | `5ed36f1bf4334ead97f1672b602af01cede09cf25a19cd21da0dabcc229d28c3` |
| [utils-async.413fd3b1f17c7388b322.js](https://static-pan.xunlei.com/_nuxt/dist/client/utils-async.413fd3b1f17c7388b322.js) | `919f314fc53c71cc0607050d2e8b54929011dfdeb6a0e975276715ef962ab6b4` |
| [app.cae34f44dce3e3fe0b98.js](https://static-pan.xunlei.com/_nuxt/dist/client/app.cae34f44dce3e3fe0b98.js) | `098359e35ad6308d85926cda170828b73e5c38d1b070819953ec7c3e76ca8682` |
| [service-pubkey.5b794464b4eeb0a07b31.js](https://static-pan.xunlei.com/_nuxt/dist/client/service-pubkey.5b794464b4eeb0a07b31.js) | `a9a2ee287a5ce64feb202e0c1d1d12b8b706f0212237353ee050ce1996ac64e8` |

桥接实现 `src-tauri/src/cloud/xunlei-web-session.js` 的定向验证命令为 `node --test scripts/test-xunlei-web-session.cjs`。截至本附录记录，18 项合成浏览器状态测试通过，覆盖旧 storage 抢占、泛 `id`、来源隔离、注销、SDK 当前账号一致性、签名失败、懒加载、captcha 续取、异步账号切换及 Vuex 默认哨兵；经历了实际失败后修复的 RED/GREEN 过程。另执行 `git diff --check`。这些检查均不接触真实账号，且不替代发布构建或实机验收。

追加安全复查发现并修复了一个边界：此前脚本把 Vuex `curUser.userId` 的字符串 `"0"` 视为真实旧账号，因此即使 SDK 的前后 subject 与完整凭据一致，也会提前返回 Pending。临时 Node VM 先复现 `"0" → null`，随后新增两项 RED/GREEN 回归。修复后将 Vuex 未就绪哨兵与真实其他账号区分，SDK 的 subject 强一致性检查和异步账号冲突检测继续保留；显式 `curUser: null` 仍表示已注销，即使 SDK 残留旧凭据也拒绝。合成测试确认默认 `"0"` 能完成同账号官方 captcha 流程，而显式注销不能恢复旧会话。`getCurrentSub` 为异步方法，脚本目前各调用点使用 `await` 的方式符合官方实现。

## 本轮集成验证与尚未完成的验收

代码位于 `master` 工作区。本轮真实执行结果：

- `node --test scripts/test-xunlei-web-capture.cjs scripts/test-xunlei-web-session.cjs`：22/22，通过。覆盖跨轮询等待人工验证，以及完成后、消费前切换账号的拒绝行为。
- `node node_modules/vitest/vitest.mjs run src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx --exclude '.worktrees/**'`：72/72，通过。
- `cargo test --manifest-path src-tauri/Cargo.toml --target-dir .cargo-target-xunlei-auth-completion --lib xunlei`：16/16，通过。
- 同 target 运行 `--test cloud_xunlei --test cloud_xunlei_upload --test cloud_upload --test live_pikpak_xunlei`：29+6+5+1 项通过，4 个真实账号测试明确 ignored，并非实机通过。
- `scripts/build-windows.ps1`：Release 编译成功。首次复制被旧进程占用；关闭已核实路径的旧 AllDownloader 进程后再次执行成功发布，未打包安装程序。
- 二进制：`artifacts/alldownloader.exe`；SHA-256 `801F36A3AE79D4494F02F4D387EC88C09BC66E8CAFF314857F154B9CBD9E2444`。

上传补齐了官方网页的 `resumable.supported_providers` 协商（仅声明已实现的 `PROVIDER_ALIYUN`）、OSS/AWS 分发与秒传完成分支，修复 GCID 在 128 MiB 等边界的分块判断。下载沿用当前浏览器的 User-Agent/Referer，不向 CDN 传送 API bearer、captcha 或 cookie。复制、移动、重命名与移入回收站继续走 Rust 接口。未实测的超大文件分片上限不在本轮成功声明范围内。

新二进制已通过实际 React 页面打开官方授权窗口；检查确认官方 captcha client/device 与捕获 profile 一致，主界面仍显示等待人工登录，尚无迅雷标签页。此处是**等待用户授权，不是成功接入的实机验收结论**。

后续实机只允许操作新建的 `AllDownloader-IT-*` 专用目录与其子项。`live_xunlei_directory_upload_rename_move_download_and_trash` 现覆盖上传、改名、移动、下载字节比对、复制、测试副本移入回收站及测试目录清理；需要 `ALLDOWNLOADER_LIVE_CLOUD=1` 显式启用。可通过 `ALLDOWNLOADER_TEST_DATA_DIR` 指向本轮隔离测试应用目录读取其加密凭证，不修改其他网盘账号。

## 人工授权后的实机验收（2026-09-10）

用户完成登录后，实际 React 授权轮询已关闭官方授权窗口、创建迅雷网盘标签页，并成功读取目录。测试使用独立应用数据目录中的加密凭据，没有覆盖正常应用的其他网盘账号。

实测发现并修复的上传协议差异：

- OSS 合约的 endpoint 是不带 scheme 的 DNS 主机名，且可能使用迅雷自定义域名。仅对 `PROVIDER_ALIYUN` 的合法裸 DNS 主机名补齐 HTTPS，仍拒绝 HTTP、用户信息、路径、查询参数和无效主机名。
- OSS multipart 完成不代表网盘文件已完成处理，立即改名会返回 `file_rename_uncompleted`。现代网页协议的上传现在等待文件进入 `PHASE_TYPE_COMPLETE`，对处理失败和超时返回明确提示。

`live_xunlei_directory_upload_rename_move_download_and_trash` 已显式解除 ignored 实际运行通过（1 passed，4 filtered），包含上传、改名、移动、下载内容字节比对、复制和移入回收站。新脚本 `scripts/test-xunlei-live-app.cjs` 进一步通过真实 Tauri 命令执行同类操作，并检查递归目录下载进入 aria2 后的两个文件与随机 32 KiB 上传内容逐字节一致，以及已完成下载的重复任务检测。应用链路有一次完整通过，但连续复测还发现间歇 `invalid_response`，故该次成功不作为下载稳定性的最终结论，继续定位后记录最终结果。

每次成功或失败的实机测试只清理自己新建的云盘测试目录（移入回收站，可恢复），不删除既有用户文件。正常运行的官方迅雷客户端不受影响。

集成复审还补齐了 `credentials_expired` 的重新授权入口：迅雷文件页保留已显示的账号与目录，直接显示“重新网页授权”，不再要求先退出账号。三个新增过期场景先失败再通过；主代理重新执行 CloudManager/CloudWorkspace 定向测试，75/75 通过。授权桥接测试重新执行，22/22 通过。

下载间歇失败的真实证据：使用新建的 32 KiB 随机内容，先复制再下载时，详情接口返回 `phase=PHASE_TYPE_COMPLETE`、`web_content_link=""`，没有备用 `download_url`。对同一对象下一次请求约 250 ms 后得到 HTTPS 链接；后续请求仍可能再次为空。诊断仅记录 phase 与链接缺失/空值/scheme 分类，不记录文件名、URL、签名或凭据。该证据表明“目录可见/文件 complete”与“本次请求取得下载链接”不是同一条件；修复应在每一次解析下载链接时处理，而不是只在上传后等待一次。

修复后的现代下载路径对已知 phase 的空链接进行 250 ms 间隔、30 s 截止的条件等待，超时给出“文件可见但下载链接仍在生成，请稍后重试。”。鉴权、HTTP、传输错误及非法链接立即退出；旧 native 路径保持原行为。截止检查放在每次 GET 之前，最后一次 sleep 不超过剩余时间；在途同步 HTTP 仍受传输层超时控制，不受此截止强制取消。真实 Rust 回归保留随机内容与先复制再下载的顺序，不使用测试层重试，最终 1/1 通过（7.60 s），观察到生产路径正确处理多次空链接后恢复。

最终主代理定向复核：`--lib xunlei` 17/17，`cloud_xunlei` 33/33，`cloud_xunlei_upload` 9/9，`cloud_upload` 5/5，`live_pikpak_xunlei` 默认脱敏用例 1/1（其余 4 项保持 ignored；上述真实 Xunlei 用例另行显式运行）。这些共 65 项，不包括前述 75 项界面和 22 项桥接测试。

发布后的重启验收观察到一次账号恢复 `网盘任务中断`，首次进程未重定向 stderr，因此不能断言该次 JoinError 的具体 panic 原因。随后源码检查确认 `app.state()` 在 CloudState 尚未注册时会 panic，独立启动测试确实复现了此路径。公共网盘入口现先用 `try_state()` 每 25 ms 异步等待，最多 10 s，之后才进入阻塞工作线程和 provider 锁；超时提供初始化提示，真实操作错误仍原样返回。新增三项启动测试覆盖七个 provider 延后注册恢复、超时不执行 work、业务错误只执行一次，先 RED 后 GREEN。`scripts/test-xunlei-app-startup.cjs` 提供显式开启的只读实机启动恢复检查。

### 最终发布验收

- 主代理重新执行启动测试 3/3、迅雷 lib 定向 17/17；定向独立用例共 68 项后端、75 项界面、22 项桥接通过，未为此运行无关全量套件。
- 最终 Release 构建成功，未打包安装程序。`artifacts/alldownloader.exe` 与正常构建产物 `.tools/release-target/release/alldownloader.exe` SHA-256 一致：`0CAC9547122C1CC5FFC63C30F13F69C42BBFBD6304B6A1452FDB87CE99B97453`。
- **最终二进制**启动恢复检查通过；真实应用随机 32 KiB 文件操作验收连续 **3/3** 通过：上传、改名、移动、复制、递归目录下载由 aria2 完成，两份下载逐字节一致且保存的父/子目录结构正确，完成任务的重复下载被识别；每轮自建测试根目录均移入回收站。
- 已先在内存中核对隔离测试与日常实例为同一迅雷账号；日常实例仍保存旧协议会话。备份旧加密凭据后，仅更新日常实例的 `xunlei.bin`，其他网盘凭据、任务库和设置不动。正常数据目录启动新二进制后实际出现迅雷标签，选择该标签能够渲染目录，无加载中或目录错误。最后去除测试调试参数重新启动日常实例。
- 上述结论限定于本次账号和基本操作验收，不包含超大文件/长时断网恢复压力测试；固定分片大小导致的超大文件分片上限仍是已知边界。删除验收采用移入回收站，不包含个人文件永久删除。
