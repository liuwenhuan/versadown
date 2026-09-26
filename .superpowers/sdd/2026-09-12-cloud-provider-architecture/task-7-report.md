# Task 7 — 分享能力拆分与 SQLite 行存储

本项实现七家网盘分享能力拆分和旧加密计划文件迁移。命令层调用签名、前端目录契约、稳定来源字符串和下载/清理行为保持兼容。没有修改 Task 8 的会话锁、下载源锁，也没有修改 Task 9 上传。

## 实现

- `cloud/share.rs` 只保留模块入口、导出与兼容导出；逻辑位于 `share/capabilities.rs`、`share/repository.rs`、`share/service.rs`，内部数据模型在 `share/models.rs`，原有测试按原模块路径迁入独立测试文件。
- `ShareInspect`、`ShareResolve`、`ShareTransfer`、`RecordedTransferResolver` 分别承载协议。`ShareTransfer::transfer`、账号指纹和 recorded resolver 均为必需方法，没有默认返回 `ProviderUnsupported` 的协议方法。兼容组合只提供可选能力访问器。
- `ProviderRegistry::share_capabilities` 是唯一服务商选择位置，构造可选 `Arc<dyn Trait>`；注册过程无网络调用。七家均有 inspect/resolve；夸克、百度、UC、迅雷有 transfer/recorded；PikPak、天翼、阿里不实现缺少的协议。facade 在读取到 `None` 时拒绝操作，命令层未恢复 provider match。
- 生产 `CloudSharePlanStore::new(DpapiCredentialStore)` 总是使用 `SqliteSharePlanRepository`。旧整文档存储仅留在 `cfg(test)` 下作为既有服务层故障注入/时钟测试的 backing；生产旧文档只用于迁移读取。

## SQLite 与恢复语义

数据库为凭据目录内的 `cloud-share-plans.sqlite`：

| 表 | 行单位及公开列 | 加密载荷 |
| --- | --- | --- |
| `share_plans` | 每 provider/share fingerprint 一行；LRU order、创建/访问时间、pinned 独立列 | DPAPI 加密计划正文，不包含 transfer 列表 |
| `share_journals` | 每计划/journal ordinal 一行；entry/owner fingerprint、owner scheme、cleanup state 独立列 | DPAPI 加密 journal，包含个人文件 ID |
| `share_metadata` | sequence、legacy_migrated | 无秘密 |

清理日志以外键关联计划。一次保存用 SQLite immediate transaction 提交增删改，仅重新加密/更新发生变化的计划或 journal；无关计划的 ciphertext 保持不变。沿用最多 256 计划的有界内存 mutation 视图和既有保留/固定策略；本项没有把所有读取改写为单计划查询。

迁移先验证并解密旧 `cloud-share-plans-v1.bin`，在一个事务内插入全部计划、journal 和迁移标记。失败回滚，旧文件不变；成功 commit 后才改名为 `cloud-share-plans-v1.bin.migrated-backup`。若 commit 后改名失败，下一次打开根据标记跳过重新导入，仅重试可恢复备份。备份同名已存在时使用独立后缀，避免覆盖已有备份。

保留 account-v2 owner fingerprint 与 legacy unknown 的隔离、已转存个人文件优先刷新、context rotation 在 URL 移交前持久化、cleanup claim 一次性/可重试状态以及任务 reservation saga。`stable_share_source` 和严格字面解析算法仅移动模块，字节格式不变。签名 URL 仍只移交下载引擎，未加入任何持久化模型。

## RED / GREEN 记录

1. 实现 SQLite 前运行 `cargo test --manifest-path src-tauri/Cargo.toml --lib cloud::share::tests::sqlite_`，3 条行为失败：首次旧文件读取没有创建 SQLite、全新计划仍写 bin、第二条迁移写入失败没有触发事务失败。随后核心实现使三条全部通过。
2. 能力矩阵测试先运行 `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract share_capabilities`，当时 registry 尚无 `share_capabilities`，得到 E0599。此处是接口缺失的编译 RED，不能冒称行为 RED。
3. 收尾用单次 mutation 验证能力矩阵：临时去掉夸克 recorded capability，运行 `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract share_capabilities_report_actual`，1 条明确行为失败（Quark `false != true`）；立即恢复真实注册，再跑两个指定 integration，33 条全部通过。这个行为 RED 是收尾 mutation 验证，时间上晚于能力实现。
4. 核心 SQLite GREEN 后补充两条实际数据库恢复验证：Windows 文件句柄禁止旧文件改名时，数据库已经提交且重试不重新解析旧文档；journal UPDATE 被 SQLite trigger 拒绝时，先发生的 plan 更新也回滚，释放故障后 cleanup 可重试且重开不重复 claim。均通过。
5. 原 DPAPI 磁盘保密测试首次仍读取旧 bin，得到文件不存在；调整为检查真实 SQLite 原始字节后通过。保留 access code、cookie、token、个人文件 ID 不出现明文的断言。

## 最终验证

Rust 使用 `C:/Users/deepin/.cargo/bin/cargo.exe` 和 `CARGO_TARGET_DIR=.tools/test-target`。依据 AGENTS.md，属于分享/连接器相关 Rust 变更，验证两个指定 integration、分享相关 Rust 用例以及发布构建。前端契约未改，没有运行前端测试或全量 Rust 测试。

| 实际命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test cloud_provider_contract --test cloud_directory_download` | 最后恢复 mutation 后 17 + 16 = 33 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib cloud::share` | 59 通过，277 未运行 |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib share` | 146 通过，190 未运行；包含七家分享协议、迁移、清理和命令层直接消费者；上行 59 条包含在其中 |
| `scripts/build-windows.ps1` | release 成功，1m37s；前端未改，复用 dist；生成主程序和 aria2-next sidecar |
| `git diff --check` | 通过 |

简报中在两个 integration 后附加 `cloud::share` 会过滤掉这两个 integration 的测试；实际分别无过滤运行指定 integration，并通过 `--lib cloud::share`/`--lib share` 运行分享测试，避免零测试假阳性。

构建仍有原有 browser/common/command helper dead-code 警告、链接器信息及本机路径 canonicalize 警告；没有新增 unused import 或编译错误。发布产物位于当前工作树 `artifacts/versadown.exe` 与 `artifacts/binaries/aria2-next-x86_64-pc-windows-msvc.exe`。

## 范围内自审

- 七家 provider 的既有方法体仅按能力归组；没有网络协议、授权恢复或上传行为变更。
- facade 缺少 transfer/recorded 时不访问网络，也不创建计划数据库。
- 真实 DPAPI/SQLite 测试覆盖两计划两 journal 恰一次迁移、原文件字节级备份、事务写失败、post-commit 改名失败恢复、无关行 ciphertext 不变以及原始数据库无秘密明文。
- 原稳定来源、owner fingerprint、优先刷新 recorded 文件、清理及 saga 测试保留并通过。
- 本项未加入锁范围/轮询/上传改造，未调用第三方线上网盘或发布到外部环境。

提交说明：`refactor(cloud): split share capabilities and persist plans by row`。
