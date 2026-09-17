# 迅雷个人网盘低速与续传诊断

## 已证实

- 用户报告任务：128543808 字节，失败时已写入3414400字节，aria错误码6，Empty reply from server。实际为VIP CDN、单连接、任务和全局均无下载限速。
- 原生状态库保存 `0-3414400`，不是用预分配文件大小推算；相邻 `.aria2` 不存在。未修改、重试或覆盖用户原任务。
- 保持现有代理环境，同一旧链接的非零Range获得206与正确Content-Range，30秒仅收到856064字节；从0读取、HEAD以及带If-Range的另几次探测发生无数据超时。现象不稳定，不能把HEAD、If-Range或代理单独认定为根因。
- 新的隐藏隔离实例曾4秒内报同样空响应；另一次连续下载约150秒，从1064960字节处暂停恢复，最终3903488字节，原生范围 `0-3903488`，速度约20–30KB/s。未达到预设的续传后增加4MiB阈值，测试以超时退出，不能宣称下载验收通过。
- 当前引擎确切bundled源码2521bb0c将空响应列入可重试网络错误；默认5次且retry-wait=0，连续失败可很快结束。正常同GID恢复采用Range GET而非HEAD。没有证据支持直接放宽单连接上限或替换引擎。

## 本次最小修复

后台复现重新获取直链会因文件详情路径变化而强制更新captcha，缺少签名元数据时在发请求前失败。同一会话直接GET官方文件详情则成功返回VIP `web_content_link`，无独立`download_url`。

将已有有效root票据复用范围从容量查询扩展到只读文件详情；其他操作和服务端拒绝处理保持不变。新增回归测试先失败、修改后通过；`cargo test --manifest-path src-tauri/Cargo.toml --lib xunlei` 32项通过。这修复了已证实的直链解析问题，不代表低速和间歇空响应已经解决。

`scripts/build-windows.ps1`编译成功，但原产物被用户运行中的程序占用，复制阶段失败。新编译文件及原sidecar另存 `artifacts/xunlei-download-fix/`，未关闭前台实例。

## 官方客户端对照及补充实测

- 用户补充官方客户端同文件截图：461.4 KB/s，已下载13.5MB，显示SVIP下载特权；用户说明速度仍在增长。不能再用账号无会员权限解释低速，但截图本身不能定位协议差异。
- `ALLDOWNLOADER_TEST_BINARY=artifacts/xunlei-download-fix/alldownloader.exe ALLDOWNLOADER_XUNLEI_PASSWORD_LIVE=1 ALLDOWNLOADER_XUNLEI_USAGE_PROBE=1 node scripts/verify-xunlei-auth-live.cjs`（PowerShell分别设置环境变量）完成隐藏隔离授权检查：登录、容量、关闭授权窗和网盘菜单通过。新增下载探测仅输出诊断，脚本退出0不代表下载成功。
- 同会话文件详情默认请求及`space=&usage=FETCH`均返回HTTP200、SVIP直链；1MiB Range探测均curl52空响应，未获取数据。`usage=DISPLAY`无下载链接。未修改生产请求参数。
- `links['application/octet-stream'].url`与`web_content_link`相同，不存在被忽略的另一条URL。但links对象有非空`token`及`token_type=TOKEN_TYPE_ACCELERATION`，令牌不同于账户access_token且不在URL中。当前插件未消费此令牌；其用法、与低速的因果关系尚未证实，不应直接当作HTTP Authorization发送。
- 官网公开脚本`102.a73ab5f326ac0106b25c.js`中的下载处理调用`callClientRetrieve`，传文件ID、种类、大小等给官方客户端，而非直接用网页直链下载。还不能据此声称必须使用官方客户端才能获得高速。
- 所有实测保留原代理环境；未操作用户前台客户端或原下载任务。本轮只增强诊断脚本和记录，没有新增生产代码修复，也未运行发布构建或全量测试。

## 仍未解决

低速和间歇空响应未解决。需要确认下载请求与官方加速流程的实际差异；不会盲目调整并发、伪造会员参数或将加速令牌当作账户凭据转发。

失败后重试入口仍存在只识别`.aria2`的兼容风险；当前任务实际保存的是Next原生状态。不可通过覆盖文件或按预分配大小恢复来规避，后续需要专门验证原生状态接续。

## 继续排查：官方通道统计与传输对照

### 速度差距已有直接证据

仅用 SQLite 只读模式查询已安装官方客户端 `Profiles/TaskDb.dat` 中名称为 `midysj.zip` 的任务，未启动/操作官方客户端、读取登录凭据或修改任务。将官方任务URL中的fileid与本应用cloud source比较，文件ID相同、大小同为128543808字节，排除仅同名文件混淆。

| 官方任务累计统计 | 字节 | 占有效接收量 |
| --- | ---: | ---: |
| 普通源站 OriginReceiveSize | 1105916 | 4.64% |
| P2P P2pReceiveSize | 19950966 | 83.78% |
| 离线 OfflineReceiveSize | 2755958 | 11.57% |
| 合计 TotalReceiveValidSize | 23812840 | 100% |

记录的DownloadingPeriod=55秒，累计总量/该时长约433KB/s，源站累计量/该时长约20KB/s。它们是整个任务时间内的平均值，不是某条连接瞬时速度；但足以证明截图中的主要吞吐来自P2P和离线通道，而非普通直链。官方源URL与本应用同属VIP CDN，OriginResourceThreadCount配置为5并不证明5条源站连接当时同时活跃。当前应用只走HTTP直链，没有接入这两个额外通道。

### 断连进一步缩小范围，尚未确认断开的具体原因

- 同一隔离会话、同一个新URL，依次测试原请求头+从0 Range、非零Range、无Referer、curl默认User-Agent、开放末尾Range，均出现curl52。代理CONNECT为200、TLS握手成功，未收到HTTP响应头。所有测试保留代理，不据此判断代理或源站哪一方主动断开。
- 新增 `scripts/probe-xunlei-transport.cjs` 使用Node内置net/tls/http/http2，通过显式现有HTTP代理CONNECT、正常证书校验进行比较。提供h2协商时CDN仍选http/1.1，不能宣称真正测试通过了HTTP/2。
- 后一组同URL比较中，无Range普通GET和HTTP/1.1非零Range均在TLS建立后ECONNRESET；第三次提供h2协商（实际仍http/1.1）的请求获得206和正确`bytes 3414400-4462975/128543808`，15秒收397312字节（约26KB/s）。说明断连是间歇性现象；单次成功不能证明ALPN选项具有修复作用。
- 该新链接剩余约21601秒有效期、没有HTML实体转义，排除本次因链接过期或`&amp;`损坏签名。
- 官方网页Mac分支将`links[].url/token`交给native任务。官方SDK头文件虽有`xl_dl_set_task_token`，但公开资料没有证明与`TOKEN_TYPE_ACCELERATION`等价，也没有其HTTP传输格式。不能把网盘加速token直接当Bearer或任意HTTP头发送。

### 本轮验证范围

无生产代码变更，仅诊断脚本与本文档。运行隐藏隔离授权+定向传输探测（`verify-xunlei-auth-live.cjs`配合`ALLDOWNLOADER_XUNLEI_TRANSPORT_PROBE=1`或`ALLDOWNLOADER_XUNLEI_PROTOCOL_PROBE=1`），并做脚本语法检查。不运行无关单元测试、全量回归或发布构建。

下一步应分开处理：HTTP断连后的有界重试与可靠接续；若要求接近官方客户端吞吐，则另行评估P2P/离线加速通道接入，不能以伪造会员标识、盲目增并发或未经证实的token请求头代替。
