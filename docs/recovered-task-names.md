# 恢复下载任务名称修复（2026-09-13）

## 原因与修复

aria2 会话中的未关联 GID 原先统一导入为“恢复的下载任务”，后续状态同步不更新名称。

- 已保存的真实任务名原样保留，不用文件名覆盖自定义名称。
- 新导入任务优先使用 BT 元数据中的原始名称，否则从输出文件/共同目录取得名称。
- 旧版 `aria2:` 任务的占位名称在恢复或后续同步时补齐；状态暂缺文件信息时使用已保存的文件记录。
- 完全缺少元数据时暂显带 GID 的“等待获取文件名”，文件信息到达后自动更新，不伪造原名。
- 名称随传输快照持久化后发布更新，不改变任务 ID、下载内容或进度。

## 验证

TDD 首次运行复现三个名称断言失败；修复后通过以下定向测试：

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --test aria2_recovery recovery_ -- --skip replacement_client_remains_available_when_recovery_persistence_fails
cargo test --manifest-path src-tauri/Cargo.toml --test aria2_sync --test aria2_types --test aria2_recovery_redaction --test downloader
```

第一组 16 项通过；第二组同步 10、状态解析 8、恢复脱敏 2、下载控制 18 项通过，共 54 项。只读审查无阻塞问题。

未执行全量测试或前端 Vitest：本次不改前端及其数据契约。Rust 格式化与定向 `git diff --check` 完成。

## 既有测试限制

初次恢复测试中的 `replacement_client_remains_available_when_recovery_persistence_fails` 仍硬编码引擎版本 `1.37.0`，实际捆绑引擎返回 `2.7.5`。该失败在名称实现前已存在，未修改其断言，后续定向验证显式跳过。

未访问或修改用户真实下载数据库；真实任务恢复效果需测试版人工验证。

## 发布测试版

`scripts/build-windows.ps1` 成功，前端未变化而复用已构建资源，Rust release 构建成功（已有非阻塞警告）。

测试程序：`artifacts/VersaDown-recovered-names-20260913-134822/alldownloader.exe`，同目录附带 `binaries/aria2-next-x86_64-pc-windows-msvc.exe`。复制后主程序与下载引擎哈希分别与发布源一致。

主程序 SHA-256：`BEFCA345FE06CD6DED455262586929D7F49EC76C245B7777C1A809D74610A22F`。
