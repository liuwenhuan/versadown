# Task 9 — 流式上传与有界文件并发

本项仅实现 Task9，没有派生子代理，没有进入 Task10。采用 test-driven-development 与 verification-before-completion 技能，按 AGENTS.md 的上传核心/网盘连接器范围运行定向 Rust 验证和 Windows 发布构建；前端未修改。

## 实现

- `UploadBody` 支持小型 `Bytes`、`FileSlice`、流式 multipart 和取消包装。文件切片只携带不可变快照、offset/length，不持有文件正文或打开的文件句柄。发送阶段 open、seek、`Read::take`，Reqwest 使用 `Body::sized` 和精确 Content-Length。百度/天翼 multipart 通过 prefix→文件 reader→suffix 串接，不收集为 Vec。
- `UploadFileSnapshot` 在散列和初始化之前捕获路径、长度、mtime 与真实文件 identity（Windows volume serial/file index；Unix dev/inode）。为读取 identity 短暂打开元数据句柄，不读取文件正文。reader 对路径与已打开文件进行读前/读后校验，并拒绝截断/替换；各独立 commit/finalize 前再次核验。天翼单请求 multipart 在发出结束 boundary 前会再次检查文件。
- PikPak、Quark/UC、百度、天翼、阿里、迅雷 OSS/AWS 都发送文件切片。PikPak GCID、百度分块 MD5、CloudDrive MD5/SHA1 等既有初始化语义保留。AWS 的请求签名需要提前散列正文，使用 64 KiB buffer 的 SHA256 预读，不收集文件切片。CloudDrive 的实际上传流继续累积 MD5/SHA1，防止同长度改写后恢复 mtime 绕过既有散列校验。
- `schedule_uploads` 使用 Tokio semaphore 和 blocking worker，在每批次按输入顺序入队。一个 permit 覆盖整个文件协议；默认并发来自已有 descriptor 的 2，可用 descriptor 降为 1。结果按输入顺序收集，单文件失败不取消其他排队文件。Tauri 仍保留原来的 `Result<Vec<CloudFile>, CloudError>` 契约：所有独立文件执行结束后返回首个输入顺序错误，没有改变前端返回格式。
- 取消标志检查覆盖队列准入、blocking worker 开始、每次协议请求、reader 推进、传输返回和 snapshot 提交。丢弃调度 future 会取消正在执行的 worker 并停止新文件准入。reader 的实际读取字节计数可由 `UploadControl::bytes_read` 获取。
- 每文件使用 Task5/8 的授权 snapshot；PikPak 也把完整文件协议置于单次 Mutation snapshot 内。`UploadHttpRef` 透传取消检查，取消后不发布旧 snapshot、不恢复、不重放实际上传 mutation。读文件与发数据均不持 refresh 锁。
- registry 已在 Task2 提供 `max_upload_concurrency=2`，本项直接消费，无重复元数据变更。UC/PikPak adapter 调用现有共同上传入口，不需要修改其无关协议代码。

## RED → GREEN

1. `changed_file_after_initialization...` 在旧 PikPak 实现中明确失败：初始化回调把 `hello` 改为更长内容，旧实现仍完成 multipart commit。加快照后通过。
2. 新 slice/scheduler 接口最初缺失造成编译失败，该次不计行为 RED。加入可编译骨架后，512 MiB 文件精确七字节切片测试实际读到 0（期望 7）；有界并发测试只有一个任务能在放行前启动（期望两个），分别观察到断言失败。实现 reader/semaphore 后通过。
3. 首轮组合测试取消用例发现并行 spawned admission 不能保证前两个输入先获得 permit。改为 FIFO 准入后，四输入中仅前两个启动、后两个取消，测试通过。
4. 自审发现仅以 metadata 替代 CloudDrive 原有实际上传字节散列会降低既有保护。先加入相同长度改写并恢复 mtime 的测试，观察错误的成功返回；再将 MD5/SHA1 累积移至流 reader，测试通过。

其他验证包括：Windows 显式标记 sparse 的 512 MiB fixture（超过 8 MiB 正文内存预算），fake transport 接收 FileSlice 并只用 32 字节 buffer 验证 offset/length；初始化后变化、最后一个分片后长度/mtime/identity 变化均不 commit；multipart 不发出变化后的结束 boundary；reader 取消；future 丢弃；真实本机 HTTP 验证精确 Content-Length=7、无 chunked、正文只有 PAYLOAD；AWS 固定已知 SHA256；原有上传授权锁/注销/禁止重放回归。并发测试使用 Barrier、Condvar、原子计数与 channel，3 秒 timeout 仅作死锁上限，没有 sleep 猜顺序。

## 实际验证记录

所有 Rust 测试使用 `C:/Users/deepin/.cargo/bin/cargo.exe`，`CARGO_TARGET_DIR=.tools/test-target`。

| 实际命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_upload --test cloud_xunlei_upload --test cloud_baidu --test cloud_tianyi --test cloud_aliyun --test cloud_uc --test cloud_quark` | 最终组合 14 + 10 + 31 + 33 + 19 + 11 + 17 = 135 通过，0 失败，退出 0 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib upload` | 3 通过（直接 HTTP 两项、上传选择路径一项），336 未运行，退出 0 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib cloud::http::upload_tests` | 中间验证 2 通过，337 未运行 |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_upload` | Windows sparse flag 补齐后最终复跑 14 通过，退出 0 |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_quark quark_streaming_preserves -- --nocapture` | 先观察 1 条行为 RED；stream digest 实现后 1 通过 |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows.ps1` | 退出 0；前端未变化、复用 dist；Rust release 1m38s；发布主程序及 aria2 sidecar |
| `rustfmt --edition 2021 --config skip_children=true`（本项变更 Rust 文件） | 退出 0 |
| `git diff --check` | 最终退出 0 |

发布路径：本工作树 `artifacts/versadown.exe` 与 `artifacts/binaries/aria2-next-x86_64-pc-windows-msvc.exe`。输出保留已有 dead-code、linker/canonicalize 和 CRLF 提示，没有新增构建失败。没有执行全量测试、前端 Vitest 或独立前端构建。

## 自审与边界

- 本项自审仅检查新增/修改的上传请求、文件校验、取消、授权 snapshot 和调度，不作额外架构扩展。
- 已发送的 blocking HTTP 在网络等待阶段仍受既有请求 timeout 约束；协作取消在下一次 reader/协议边界生效，不新增强制终止网络或前端取消 UI。
- 文件系统快照不是锁住外部写入者的事务。metadata 变化在读取与提交边界拒绝；CloudDrive 额外保留实际发送字节散列。没有声称可阻止恶意外部进程在所有校验之后修改文件。
- 初始化响应已经表示秒传完成时，遵循服务商语义返回其文件；发现本地变化会报错，无法回滚服务商已完成的秒传响应。
- 没有真实服务商账号 E2E、真实大文件公网传输或长时间性能压测；本项证据是协议 fixtures、稀疏文件、实际本机 HTTP、确定性协调与发布构建。
- 提交说明：`perf(cloud): stream uploads with bounded concurrency`。
