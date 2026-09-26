# 统一混选上传实现计划

> **For agentic workers:** 按已批准的交互方案并行实现独立前后端任务；每项执行 TDD，集成后审查和定向验证。用户已要求直接实现，不再重复询问执行方式。

**Goal:** 一个普通样式的“上传”按钮，在同一窗口多选/混选文件与目录，确认后识别类型并去除父目录覆盖的重复项目。

**Architecture:** 新增本地只读目录枚举 IPC 和独立 React 选择弹窗。复用 `cloud_upload_files` 已有的文件/目录类型分派、递归上传及授权失败重试，不新增上传引擎。

**Tech Stack:** React / TypeScript / Tauri / Rust 标准文件系统 API。

**Spec:** 本任务对话已批准：单击勾选，双击目录进入，跨目录保留选择，多文件、多目录混选，确认上传，目录保留层级；父目录包含的已选项目去重；按钮无蓝色底色。

## Global Constraints

- 不改已有账号、网盘、上传凭据及用户数据。
- 枚举运行在阻塞工作线程；不递归扫描；排除符号链接、目录联接和非普通文件。
- 读取失败清晰显示，旧请求不覆盖新目录；取消不上传。
- 使用 `apply_patch` 编辑，保留所有无关脏改动，不提交或合并。
- Vitest 必须排除 `.worktrees/**` 和 `.pnpm-store/**`；不执行全量测试。

## Task 1：本地枚举接口

Files: 新增 `src-tauri/src/local_upload_picker.rs`，注册于 `src-tauri/src/lib.rs`。

Interface: `local_upload_entries(path: Option<String>) -> {path: string, parentPath: string|null, roots: {path,name,kind}[], entries: {path,name,kind}[]}`。kind 为 `file` / `directory`。path 缺省打开用户目录；roots 支持磁盘导航；path 为标准化绝对路径。

- [x] 先写真实临时目录测试：同时列出文件、空目录，目录优先；不读取文件内容；错误路径失败；不包含符号链接。
- [x] 运行 `cargo test --lib local_upload_picker` 确认失败。
- [x] 实现只读枚举及 spawn_blocking 命令，不注册其他权限或修改上传流程。
- [x] 重跑定向测试并报告证据。

## Task 2：统一弹窗和上传入口

Files: 新增 `src/lib/localUpload.ts`、`src/components/LocalUploadPicker.tsx` 及各自定向测试；修改 `CloudManager.tsx`、`CloudManager.test.tsx`、`cloudApi.ts`、`styles.css`。

Interface: `LocalUploadEntry {path:string; name:string; kind:'file'|'directory'}`；`LocalUploadListing {path:string; parentPath:string|null; roots:LocalUploadEntry[]; entries:LocalUploadEntry[]}`；`LocalUploadPicker({listEntries, onConfirm, onCancel})`；`onConfirm(paths:string[])` 接收去重后的完整路径。

- [x] 先测试勾选文件与目录、跨目录保留选择、选择父目录排除子项、取消零上传、错误目录可恢复及旧请求不覆盖新目录。
- [x] 运行对应 Vitest 观察失败，再实现窗口（原生 dialog 模态、键盘关闭、目录路径输入、磁盘导航、当前页全选、已选清单、确认按钮）。
- [x] 将工具栏/右键上传统一接入弹窗；确认调用已有 `uploadSelectedFiles(paths)`，保留授权重试的 inputIndex 映射；删除工具栏原文件/目录双按钮及蓝色类。
- [x] 修改旧测试为新的真实弹窗流程，不模拟掉选择组件。测试混选输出和上传目标目录/失败重试。

## Task 3：集成验证

- [x] 定向运行 LocalUploadPicker、localUpload、CloudManager、CloudWorkspace、App、cloudApi 测试；通过后运行 `scripts/build-windows.ps1`。
- [x] 只读审查交互、去重、异步生命周期及本地文件安全边界，修复并补测阻塞项。
- [x] 汇报实际命令和结果；若生成测试版，独立目录附带下载引擎并校验文件哈希。

## 验证记录（2026-09-13）

- 前端定向命令：`vitest run src/components/CloudManager.test.tsx src/components/LocalUploadPicker.test.tsx src/lib/localUpload.test.ts src/lib/cloudApi.test.ts src/components/CloudWorkspace.test.tsx src/App.test.tsx --exclude '.worktrees/**' --exclude '.pnpm-store/**'`，6 个文件、165 项通过。
- 后端定向命令：`cargo test --manifest-path D:\projects\downloader\src-tauri\Cargo.toml --lib local_upload_picker`，8 项通过，365 项过滤。
- `npm run build` 和 `scripts/build-windows.ps1` 均成功；存在已有非阻塞编译警告。
- 浏览器真实组件夹具验证：多文件/多目录混选、跨目录保留选择、父子去重、1280×720 浅色和 960×640 深色布局。
- 只读审查发现的磁盘枚举阻塞风险和全选半选状态重载问题已修复并补测。
- 独立测试版：`artifacts/VersaDown-mixed-upload-20260913-122047/versadown.exe`，附带 `binaries/aria2-next-x86_64-pc-windows-msvc.exe`。两文件与发布源的 SHA-256 分别一致。
- 主程序 SHA-256：`A0416E9D5E6EA99938270D5331F6F01B48FD1FAAF6753F63BCDC33A9A07BFF2B`。
- 验证边界：本次验证环境为 Windows；未向真实网盘上传用户文件，需人工测试真实授权账号的上传结果。未执行全量测试，未提交或合并。

## 上传选择窗口后续调整（2026-09-13）

按用户确认的新交互，删除说明文字、右侧已选面板、向上箭头和“前往”按钮。列表增加不可勾选的 `..` 返回行，以及大小、修改时间列；目录大小不递归统计。路径栏单击目录导航，双击编辑完整路径，Enter 提交、Escape 退出编辑。操作文字禁止选择，编辑态路径可选择复制。复用弹窗边框、关闭、底部主按钮样式，保留混选及父子去重。

- 前端定向：`vitest run src/components/LocalUploadPicker.test.tsx src/components/CloudManager.test.tsx src/lib/localUpload.test.ts src/lib/cloudApi.test.ts --exclude '.worktrees/**' --exclude '.pnpm-store/**'`，最终 4 个文件、141 项通过。
- 后端定向：`cargo test --manifest-path D:/projects/downloader/src-tauri/Cargo.toml --lib local_upload_picker`，最终 9 项通过。
- TDD：先观察缺失返回入口/元数据显示/路径栏行为失败；导航定时器竞态和非默认系统双击间隔测试分别先红后绿。
- 浏览器夹具：960×640 浅色/深色布局、列对齐、长文件名省略、关闭、单击路径跳转、双击原路径编辑、Escape 退出编辑、选中计数和主按钮悬停配色。未上传真实文件。
- 审查修正：按 Windows `GetDoubleClickTime` 判断双击；新导航取消旧单击定时器；表头与列表保留相同滚动条间隙；主按钮悬停不被普通按钮规则覆盖。
- 已知边界：沿用上版链接/reparse point 排除策略，因此 Windows 云占位文件仍不在选择范围；本次未扩大本地读取和递归上传安全边界。非 Windows 未实机验证。
- 最终 `scripts/build-windows.ps1` 成功（含前端构建与 Rust 发布构建，已有非阻塞警告）；独立测试版 `artifacts/VersaDown-upload-picker-20260913-132331/versadown.exe`，附带下载引擎。主程序 SHA-256：`891063C7E61645146C573E868C68F01C4A0F718B221DBCC56B4B9A5990D4192C`；主程序与引擎复制后哈希均与发布源一致。
