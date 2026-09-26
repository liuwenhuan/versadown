# 各网盘分享管理设计

## 目标

为夸克、PikPak、百度、天翼、阿里、迅雷和 UC 网盘提供统一的“创建分享链接、列出我的分享、取消分享”能力。用户从文件或目录右键菜单发起分享，填写提取码和有效期；网盘根目录提供“我的分享”虚拟文件夹，列出已分享的文件或目录及其链接信息。

## 边界

- 删除原有“文件详情”菜单项、详情弹窗和专用样式；不再以详情弹窗承载文件元数据。
- “我的分享”是本地导航虚拟目录，不会在真实网盘中新建文件夹。
- 目录内展示分享记录对应的原始文件/目录。它是管理视图，不把分享记录伪装成可下载的真实文件。
- 首版提供复制分享链接、取消分享，并保留创建时返回的提取码供用户立即复制；不实现修改既有分享参数。

## 统一契约

Rust 共享层新增 `ShareOps`：

- `create_share(file_ids, options) -> CloudShare`
- `list_shares() -> Vec<CloudShare>`
- `cancel_shares(share_ids)`

`CloudShareOptions` 只有跨平台语义：自定义提取码、请求由服务端生成提取码，以及规范化有效期（永久、1/7/30 天）。`CloudShare` 规范化返回分享 ID、链接、名称、源文件类型、创建/失效时间、可选提取码。

`ProviderCapabilities` 增加三项分享管理能力。原有 `share_inspect` / `share_transfer` 继续只表示“读取别人分享并转存”，不混用。

每个原生 provider driver 实现 `ShareOps`，调用该网盘自己的协议。命令层只处理会话恢复、参数校验和虚拟目录投影。

## 虚拟目录与 UI

- 固定虚拟 ID 为 `__versa_down_my_shares__`，与根目录中的回收站同级注入。
- 前端进入该 ID 时调用共享层的 `list_shares`，映射为不可下载的 `CloudFile` 投影；ID 带安全前缀且包含 share ID。
- UI 将其识别为“我的分享”，展示共享图标；在该目录中禁止下载、重命名、移动、复制、删除文件。
- 分享记录右键只提供“复制分享链接”和“取消分享”。取消后刷新该虚拟目录。
- 正常文件/目录右键以“分享”替代“文件详情”；批量选择也可创建一个包含多个条目的分享。
- 分享对话框复用现有确认弹窗、输入框和焦点管理样式，提供提取码（可留空）和有效期选择。创建成功后展示链接、提取码（若有）以及复制操作。

## 协议适配原则

- 阿里：`/v2/share_link/create`、`/v2/share_link/list`、`/v2/share_link/cancel`，以 session 中的 drive ID 和 access token 认证。
- 夸克 / UC：clouddrive 的 `share`、`share/password`、`share/list`（或 mypage detail）、`share/delete`，创建后轮询 task 并读取链接详情。
- PikPak / 迅雷：创建使用 `/drive/v1/share`；“我的分享”使用 `/drive/v1/share/list`；取消使用 `/drive/v1/share:batchDelete`。受保护链接由服务端生成提取码。
- 百度：网页会话的分享创建、分享记录和取消分享端点。
- 天翼：已登录 Web 会话的共享服务创建、我的分享列表与取消端点。

适配层负责端点差异、必要的任务轮询、时间转换和返回字段兼容；共享层不依赖任何 provider 专有请求字段。

## 验证

前端以 `CloudManager.test.tsx` 覆盖：详情移除、创建分享对话框、复制链接、根目录虚拟文件夹和取消分享。后端以共享驱动与虚拟目录投影的定向 Rust 测试覆盖契约、能力门控和参数验证。最后执行前端构建与 Windows 发布构建。
