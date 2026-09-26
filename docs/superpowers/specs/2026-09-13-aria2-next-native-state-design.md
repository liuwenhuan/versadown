# Aria2 Next 原生状态统一设计

## 目标

VersaDown 只使用 Aria2 Next 2.7.5 的原生任务文件和 `--state-dir` 持久化状态。应用不再创建、读取或依据相邻 `.aria2` 控制文件判断完成、续传与覆盖，也不再导入、迁移或修复旧 aria2 会话。

## 状态归属

- `aria2-next.tasks` 是 Aria2 Next 的任务队列清单，由 `--input-file`、`--save-session` 和 `aria2.saveSession` 管理。
- `aria2-next-state` 是 Aria2 Next 的原生进度状态目录。HTTP、HTTPS、SFTP、Metalink、BitTorrent、ED2K 与媒体下载的恢复数据均归该目录所有。
- `tasks.sqlite` 只保存 VersaDown 的任务展示、目录分组、网盘来源、GID 映射和已完成历史，不保存字节区间或协议级断点状态。
- FTP 仍由进程内 Rust 引擎负责，依据目标文件长度续传，不进入 Aria2 Next 状态域。

## 启动与退出

- 启动参数改用 `<data-dir>/aria2-next.tasks`，不再引用 `<data-dir>/aria2.session`。
- 删除启动前对 stream `state.db` 的重号修补。原生任务文件和原生状态库必须作为同一代状态共同使用。
- 退出和设置重启前继续调用 `aria2.saveSession`，随后调用 `aria2.shutdown`。保存的是 Aria2 Next 当前任务文件，不是旧 `.aria2` 控制文件。
- 删除旧引擎一次性迁移标记、旧会话截断和旧任务迁移入口。旧 `aria2.session` 永远不读、不写。

## 严格恢复与冲突清理

启动后枚举 Aria2 Next 中 active、waiting、stopped 的任务，并与 `tasks.sqlite` 的 GID 映射严格核对：

1. 两边都存在且 GID 唯一：同步状态与文件列表。
2. VersaDown 存在未完成任务，但 Aria2 Next 不存在对应 GID：该任务不兼容，永久删除其任务记录、记录的局部下载文件和相邻遗留 `.aria2` 文件。
3. Aria2 Next 存在任务，但 VersaDown 没有对应 GID：该任务不受应用管理，从 Aria2 Next 永久移除；只删除位于配置下载根目录内的输出文件。
4. GID 映射冲突、元数据父子关系不唯一或恢复状态无法解析：相关应用任务和原生任务都按不兼容处理并删除，不创建“恢复的下载任务”，不猜测旧磁力父子关系。
5. 已完成的 VersaDown 历史及其完整文件保留；已完成历史不会因为引擎清理而重新下载。

清理必须限制在配置的下载根目录内。路径解析失败、越界路径、目录或非普通文件不得删除；对应任务记录仍可删除，错误记录为可诊断但不含凭据的启动错误。

## 续传、覆盖与取消

- 暂停和继续只操作当前 GID；Aria2 Next 负责恢复字节区间、BT fast-resume、ED2K 分块和媒体检查点。
- 失败任务只有在原生引擎仍识别当前 GID 时才可继续。当前 GID 不存在时，该任务按不兼容规则删除，用户需要重新新建下载。
- 覆盖或重新下载先永久移除旧 GID 的原生恢复数据，再删除用户确认覆盖的文件，再创建新任务/GID；不通过 `.aria2` 判断能否续传。
- 取消任务调用 Aria2 Next 原生 remove/forceRemove，保留或删除输出文件由现有用户操作语义决定；不创建或保留 `.aria2`。
- 用户彻底删除任务并勾选删除文件时，删除记录的输出文件，同时顺带删除同名遗留 `.aria2`，但该行为只用于清除历史垃圾，绝不参与状态判断。

## 原生媒体下载

- HLS 和 MPEG-DASH 不再由 `MediaDownloadEngine` 拆分成临时分片任务。
- `hls` 与 `dash` 任务直接通过 `aria2.addUri` 提交，分别附加 `media=hls` 或 `media=dash`；文件名提示通过 Aria2 Next 原生选项传递。
- 暂停、继续、取消、重启恢复和进度同步统一走普通 Aria2 Next GID 生命周期。
- 删除应用内媒体清单解析、分片工作目录、`.aria2` 分片判断和相关依赖测试；前端任务种类与图标保持不变。

## 错误处理

- 原生任务文件无法保存或引擎无法关闭时，退出报告保持失败状态，不能声称安全保存。
- 严格恢复中的单个冲突不能使整个下载引擎不可用；清理该冲突后继续处理其他任务。
- 删除越界文件时拒绝文件操作并保留诊断，绝不扩大删除范围。
- 所有错误继续脱敏，不输出网盘 Cookie、Authorization、分享码或完整签名 URL。

## 非目标

- 不迁移旧 `aria2.session`、旧 `.aria2` 或旧媒体分片工作目录。
- 不为已删除的不兼容任务提供撤销或自动重建。
- 不修改 FTP 引擎、网盘授权机制、任务列表 UI 或已完成历史展示。

## 验证

- 启动参数测试证明只使用 `aria2-next.tasks` 与 `aria2-next-state`。
- 严格恢复测试覆盖双边匹配、应用孤儿、引擎孤儿、冲突 GID、越界路径和已完成历史。
- 重复任务测试证明不再读取 `.aria2`。
- 本地 Aria2 Next 实测覆盖 HTTP 暂停重启续传、完成任务不重复下载以及 HLS/DASH 原生提交。
- 运行相关 Rust 定向测试与 `scripts/build-windows.ps1`；不因本次交付默认运行全量测试。
