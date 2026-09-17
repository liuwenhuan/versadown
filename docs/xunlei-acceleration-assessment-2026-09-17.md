# 迅雷官方加速通道评估（2026-09-17）

结论：公开下载SDK仍只能作为独立的Windows HTTP(S)候选引擎，不能等同于用户现有网盘SVIP。后续本机对照已经确认：个人会员文件token由官方客户端的原生P2SP核心消费，主要字节来自P2P/离线资源，没有发现可交给aria2的加速HTTP URL。因此纯aria2接入不可行；若继续产品化，应采用单独的受支持原生后端，而不是给aria2追加猜测参数。

## 官方公开能力与条件

| 项目 | 已确认事实 | 对本项目的影响 |
| --- | --- | --- |
| 应用授权 | 注册迅雷开放平台、实名认证、创建应用，再取得 App ID / API Key；实名认证是创建 API Key 的前提。[接入流程](https://open.xunlei.com/doc?doc=access_flow) | 需要开发者应用授权，不能直接拿现有网盘登录信息替代。 |
| 登录凭据 | 服务端用 `X-API-Key` 调用 `/api/v1/sdk/login_token` 取得 SDK 登录 JWT，再交客户端调用 `xl_dl_login`；公开 scope 包括 `xtask.p2sp`、`xindex.query`。[签发实现](https://github.com/xunlei-open/xunlei-dlsdk/blob/main/xl-dl-node/packages/core/src/login-token.ts) | 生产分发需要安全的 token 签发边界。官方要求不要把 API Key 放进客户端代码、配置或安装包，推荐业务服务器保存密钥。[安全最佳实践](https://open.xunlei.com/doc?doc=api_key_security) |
| Windows 发布 | 官方提供 Windows x64 / x86 C++ SDK，运行库为 `dk.dll`，配套 `dk.lib` 和 C 接口头文件；文档要求运行时能找到 DLL。[官方 C++ SDK](https://github.com/xunlei-open/xunlei-dlsdk/tree/main/xl-dl-cpp) | 与本项目 Windows x64 架构匹配。Rust 调用 C ABI 是可行的工程方案，但本项目尚未编译、加载或验证兼容性；实际分发还需核对适用授权条款、额度与费用，不能由“公开下载”推定无限制使用。 |
| 协议 | 当前只开放 HTTP(S)，BT、磁力、ed2k 暂不支持。[快速接入](https://open.xunlei.com/doc?doc=quick_start) | 保留现有 aria2 能力；SDK 只适合作为部分 HTTP(S) 任务的可选引擎。 |
| 请求与任务控制 | 有 HTTP 头设置、创建、启动、停止、删除、状态查询、任务保存、并发数和限速接口。[官方头文件](https://github.com/xunlei-open/xunlei-dlsdk/blob/main/xl-dl-cpp/include/xl_dl/xl_dl_sdk.h) | 能覆盖基础任务生命周期；SDK 任务状态、恢复数据与 aria2 GID / `.aria2` 文件不能直接视为兼容。 |
| 代理 | 检查的官方 API 文档与 C 头文件没有代理设置接口，也未说明是否继承 `HTTPS_PROXY`、系统代理或是否代理 P2SP 流量。[API 文档](https://open.xunlei.com/doc?doc=api_doc) | 属于“未公开确认”，不是证明不支持。不能保证继承本项目的代理及代理认证；用户要求代理时，在验证前不能自动切换 SDK。 |

官方还提供动态链接 P2SP 开关，默认开启；文档提示极低概率存在下载文件不一致，应根据场景关闭或校验文件。因此接入验证必须包含内容完整性，不能只比较速度。[动态链接加速说明](https://open.xunlei.com/doc?doc=xl_dl_set_dynamic_link_acceleration)

## 网盘 SVIP 能否复用

**不能保证，且目前没有官方公开证据证明能复用。** SDK 的应用登录 JWT、网盘账号登录 token、文件 `TOKEN_TYPE_ACCELERATION` 是不同来源的凭据，不能因为都叫 token 就互换。

官方网盘网页把 `links["application/octet-stream"].url/token` 作为 `tasks[].url/token` 交给 Mac 原生 `createBatchGetBackTasks` / `createGetBackTask`。该证据只到原生边界，没有说明 token 在下载请求中如何使用。[官方网盘 JS，字符偏移约 398000](https://static-pan.xunlei.com/_nuxt/dist/client/102.a73ab5f326ac0106b25c.js)

官方 SDK 头文件虽声明 `xl_dl_set_task_token`，但未找到其接受网盘加速 token 的文档、调用示例或 SVIP 权益映射承诺。它不能作为复用会员的证明，也不能据此给 aria2 添加猜测的 Authorization、Cookie 或 URL 参数。[头文件第 168 行](https://github.com/xunlei-open/xunlei-dlsdk/blob/main/xl-dl-cpp/include/xl_dl/xl_dl_sdk.h#L168)

## 本机个人会员通道实测补充

在用户明确授权后，使用同一账号、同一文件创建了一个独立临时任务。官方云盘插件的实际顺序是：以签名URL创建P2SP任务，再把`TOKEN_TYPE_ACCELERATION`传给`enableDcdnWithVipCert(token, -1)`，最后启动任务。测试界面显示SVIP特权与加速图标，速度约584–946 KB/s；暂停时118,251,202字节中，P2P占92,959,484字节、离线资源占15,332,369字节、源站仅9,959,349字节。

这项实测把边界从“公开资料不足”推进到“当前aria2架构无法消费该通道”：token绑定GCID/hash并用于原生资源查询，原生核心没有向JS层返回peer URL列表或替代HTTP URL。TaskDb的`VipReceiveSize`和`FreeDcdnReceiveSize`均为0，但不能据此否定会员通道，因为界面已进入SVIP状态且实际提速字节主要记入P2P/离线类别。完整记录见`docs/xunlei-vip-channel-probe-2026-09-17.md`。

## 相对现有 aria2 的最小接入边界

以下为基于本仓库代码的工程判断，不是官方 SDK 的承诺。

当前网盘连接器产出 `ResolvedCloudDownload { url, name, headers }`；云盘调度在完成解析后，把 URL 和包含代理的选项交给 `DownloadService::submit_uri_with_options`，再转换为 aria2 RPC 参数。对应位置：`src-tauri/src/cloud/types.rs`、`src-tauri/src/cloud/commands/download.rs`、`src-tauri/src/aria2/service.rs`。Windows 发布当前仅打包 aria2 sidecar。

如果后续决定试接入，最小范围是一个 Windows SDK 适配器，在“网盘解析完成 → 提交下载引擎”处选择引擎，并保存任务所属引擎及 SDK task ID。复用现有列表和下载目录输入，补齐该引擎的进度、暂停/继续、取消、重启恢复和错误映射；分发时补入对应 DLL。无需先重构全部连接器或建立通用插件体系。已查阅的官方接口没有说明如何输出可交给 aria2 的加速直链，因此不能假定 SDK 只负责取得 URL 后仍由 aria2 传输。

继续实现的前提是得到官方对“第三方应用能否使用网盘文件加速token、能否复用个人SVIP、代理覆盖哪些流量、应用额度和分发条款”的明确答复，并取得稳定ABI。若官方只允许其客户端私有核心使用，该能力不能作为本应用可分发功能；普通HTTP(S)开发者SDK加速仍可作为独立目标验证，但不会继承个人网盘会员权益。

## 本次验证范围

只读官方公开文档、官方仓库源码及本仓库提交路径；官网为动态页面，正文同时核对了其公开文档资源 `https://open.xunlei.com/static/js/DevDoc-3da0900d.js`。未运行 SDK、未申请或使用新凭据、未访问本机迅雷客户端、未改生产代码；网络请求保留现有 `HTTPS_PROXY`。文档变更仅做空白错误检查和内容核对，未运行单元测试或发布构建。
