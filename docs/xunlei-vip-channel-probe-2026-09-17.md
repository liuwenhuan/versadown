# 现有个人会员加速通道：实测与原生边界

## 状态

已验证真实会员会话可以取得文件级加速令牌，并完成一次用户授权的官方客户端原生任务对照。结果确认会员通道由迅雷原生 P2SP 核心消费，**不是可直接交给 aria2 的另一条 HTTP 加速直链**。本轮没有修改生产连接器、引擎参数或原下载任务，不能将本记录当作 aria2 加速成功验收。

## 真实账号探测

`scripts/probe-xunlei-acceleration.ps1` 只接受本应用隔离授权测试目录，使用该目录的 DPAPI 会话，经现有代理 GET 官方文件详情。文件为用户报告的同一个 `midysj.zip`。令牌、签名 URL、用户 ID、IP 和解码后的完整数据均不输出。

旧隔离会话请求返回 HTTP 400，不能据此认定协议变化。重新运行隐藏授权实例后成功：

```powershell
$env:VERSADOWN_TEST_BINARY='artifacts/versadown.exe'
$env:VERSADOWN_XUNLEI_PASSWORD_LIVE='1'
$env:VERSADOWN_XUNLEI_ACCELERATION_PROBE='1'
node scripts/verify-xunlei-auth-live.cjs
```

结果退出0；隔离目录 `versadown-xunlei-auth-live-LPVK6I`。脚本同时断言登录、目录读取、容量显示、授权窗口关闭和网盘菜单正常。测试实例已退出。后续官方客户端对照使用了用户已运行并登录的官方客户端，不复用该隐藏实例的窗口。

随后执行：

```powershell
powershell.exe -ExecutionPolicy Bypass -File scripts/probe-xunlei-acceleration.ps1 -SessionDirectory 'C:/Users/deepin/AppData/Local/Temp/versadown-xunlei-auth-live-LPVK6I'
```

结果退出0，HTTP 200：

- `links.application/octet-stream` 含 `url/token/expire/type/token_type`；URL仍与`web_content_link`相同。
- `token_type=TOKEN_TYPE_ACCELERATION`，令牌为841字符、3段JWT，payload是JSON。
- 账号匹配当前隔离会话；`res_type=gcid`，`res_value`与文件hash完全相同、长度40，不是HTTP URL。
- `exp`距离查询时约43201秒。
- 数值字段：`vip_type=2`、`phub_level=9`、`dcdnhub_level=0`、`product_type=654`。仅记录服务端返回值，没有公开枚举映射；不能把`dcdnhub_level=0`直接解读为没有会员或作为低速根因。
- 还包含`peer_id/cip/jti/user_source/user_data`等字段。它们说明存在资源/身份上下文，但本次没有证明各字段的服务端绑定强度。

解码仅用于诊断，不代替签名验证；不修改JWT，不把账户Bearer或文件加速令牌试发给猜测的服务器/请求头。

## 本机官方核心的只读证据

仅检查`C:/Program Files/Thunder Network/Thunder/program`中的程序模块，未读取官方客户端Profiles或凭据、未运行/注入/修改模块。

| 模块 | 版本 | 有关符号 |
| --- | --- | --- |
| `SDK/DownloadSDK.dll` | `2.86.202.143` | `XL_EnableDcdnWithToken`、`XL_EnableDcdnWithVipCert`、`XL_SetTaskEquityToken`、`XL_SetTokenMode`、`XL_QueryAccelerateInfo`、`XL_CreateP2spTask` |
| `SDK/xl_thunder_sdk.dll` | `12.0728.960.264` | `XLSetAccelerateToken`、`XLSetEquityToken`、`XLSetTaskToken`、`XLStartDcdn`、`XLCreateP2spTask`、`XLAddBatchDcdnPeerRes` |
| `dk_addon.node` | 未作为版本判断依据 | `enableDcdnWithVipCert`、`updateDcdnWithVipCert`、`setTaskEquityToken` |

核心还包含`P2spTask::TryStartVipAccelerate`、`TokenVerifyFail`、`AccTokenPayload`、`phub.gateway.QueryReq/QueryResp`、`ResourceInfo/PeerRscInfo`、`TMD_ACC_TOKEN/TMD_BENEFITS_TOKEN`，以及PHUB、DCDN资源查询域名和`/thub/query-tresinfo`、`/thub/query-tserverres`路径字符串。

这些字符串和导出名支持以下工程推断：文件加速令牌输入到原生P2SP任务，核心向资源服务查询后调度下载。**它们不提供函数ABI、请求序列化格式或可供aria2重放的完整HTTP请求。** 未向这些域名发出携带用户令牌的探测。

安装程序目录未发现`.h/.proto/.idl/.lib`配套定义。官网公开代码也只提供Mac的URL+token原生桥接与Windows的文件ID原生桥接。当前证据仍不能确认hub响应中的资源是否可由普通HTTP客户端独立读取。

## 用户授权的官方客户端对照

对照只创建一个新任务，保存到隔离临时目录，名称为 `midysj-vip-probe.zip`。原有 `midysj.zip` 和其他任务均未启动、删除或覆盖；系统代理和证书未改动。测试结束后新任务已暂停，状态码为7、失败错误码为0。

普通“新建链接”界面即使输入刚刷新、12小时有效的 `web_content_link`，仍把它识别为 `index.html / 0 B`。这说明网盘任务不能只靠把签名URL粘贴成普通HTTP任务。随后按照官方云盘插件自己的调用顺序创建原生P2SP任务：指定真实文件名、大小和独立保存目录，设置源站URL，再调用 `enableDcdnWithVipCert(token, -1)` 后启动。

实测界面显示“已为您开启 SVIP 下载特权”和加速图标；瞬时速度观察到约584–946 KB/s。暂停后的持久化统计为：

| 项目 | 字节 | 占已接收数据 |
| --- | ---: | ---: |
| 总接收 | 118,251,202 | 100% |
| P2P | 92,959,484 | 78.61% |
| 离线资源 | 15,332,369 | 12.97% |
| 源站HTTP | 9,959,349 | 8.42% |
| P2S | 0 | 0% |
| `VipReceiveSize` | 0 | 0% |
| `FreeDcdnReceiveSize` | 0 | 0% |

`VipReceiveSize`和`FreeDcdnReceiveSize`为0不能反推“会员token没有作用”：同一任务界面明确进入SVIP特权状态，令牌payload的`phub_level=9`，而绝大多数实际字节记入P2P/离线资源。更准确的结论是：本次账号/文件的提速主体是PHUB/P2P与离线资源，而不是TaskDb单列的DCDN/VIP字节类别。

官方原生诊断JSON的可见schema含DCDN查询和peer统计，但也含token、IP和用户/peer标识；本次没有打印或保存完整诊断。它没有暴露可复用的peer URL列表或第二条HTTP下载URL。

## 下一步与边界

本次已经排除“把会员token换成一个HTTP URL后继续交给aria2”的实现路径。aria2只会取得源站HTTP字节，不能消费迅雷原生核心查询到的P2P/离线peer资源；增加HTTP分片数或猜测请求头也不会获得相同通道。

可实现边界是单独的迅雷原生下载后端：继续复用本应用的任务列表、目录、账号会话和共享调度抽象，但迅雷云盘文件不由aria2传输，而由受支持的原生接口负责创建、暂停、继续、恢复、进度和错误。现有安装目录只暴露私有原生符号而无稳定ABI或分发许可，因此不能直接把逆向得到的调用封装成生产功能。要产品化，需要迅雷提供个人会员token可用的受支持ABI/SDK，或明确允许依赖已安装官方客户端的桥接方式。

## 本轮验证范围

新增诊断探测及授权验证脚本的可选调用分支。已运行PowerShell语法解析、`node --check scripts/verify-xunlei-auth-live.cjs`、真实隔离探测及上述官方客户端对照。一次性签名URL和会员token仅在隔离临时文件中短暂传递，测试后文件与内存变量均已清除。没有运行Rust/Vitest测试或发布构建，因为本轮没有生产行为修改；上一轮二进制仍只是断连重试/原生续传修复版。
