# 项目开发与验证规则

## 开发期兼容性与范围

- 开发过程中因接口、协议、模型或测试夹具变更造成旧数据不兼容时，默认直接更新当前实现和相关测试；**不**添加面向未发布旧数据的兼容分支、迁移代码、保护代码或冗余测试。
- 只有用户明确要求保留旧数据/旧协议兼容性时，才实现兼容或迁移。若用户明确说明仍处于开发期，视为默认不需要向前兼容；完成后说明跳过了哪些兼容工作。
- 修复或扩展某个网盘时，判断其他网盘是否走相同共享流程或存在明确相同风险：是则抽象共享能力或做一致修复；否则严格限制在当前网盘，不作猜测性扩展。

## 复杂度控制与临时例外

- 直接执行用户明确、范围清晰的需求；审核、设计与测试必须与实际风险相称。不要为几乎不可能的边界、竞态或未被影响的系统反复设计、审查或测试。
- 纯文字、静态布局等显而易见的简单改动直接修改；除非影响交互、响应式布局或用户明确要求，否则不做额外审核、单测或构建。
- 用户要求临时跳过本文件某项规则时，例外只覆盖当次明确操作。完成后询问用户是否永久调整规则；只有得到明确同意才能改变后续默认行为，否则立即恢复本文件全部约束。

## 验证与构建

每次修改前，按实际影响选择最小足够的验证集。提交、合并到 `master`、生成普通二进制都不自动触发全量测试。

| 变更范围 | 默认验证 | 不应默认执行 |
| --- | --- | --- |
| 纯文案、静态 CSS/布局 | 直接修改；仅在存在实际布局风险时做相关页面验证 | 审核、Vitest、Rust 测试、构建 |
| 单个前端交互组件 | 该组件及直接页面的定向 Vitest；`pnpm run build` | 全量 Vitest、Rust 测试 |
| 单个 Rust 模块 | 该模块的定向 `cargo test` | 不相关 Rust 模块、全量前端 Vitest、Release 构建 |
| 下载核心、网盘连接器、跨前后端契约或共享基础设施 | 相关前后端定向测试 | 全量测试；除非下列条件满足 |

- 默认使用增量和模块化构建：日常可运行验证使用 `pnpm run build:windows:quick`；正式二进制使用 `pnpm run build:windows:release`；安装包使用 `pnpm run build:windows:installer`。不要以验证、提交或合并为由反复执行完整 Release 构建。
- 全量前端或 Rust 测试仅在以下任一情况运行：用户明确要求全量测试/完整回归/发布前全量验收；改动横跨多个独立子系统且定向验证无法覆盖；或定向测试暴露共享基础设施风险。
- 前端 Vitest 必须排除历史工作树：`--exclude '.worktrees/**'`。
- 只有用户在**当前请求**明确要求“生成二进制”“编译二进制”“生成 `.exe`”或同等发布指令时，才可运行 `scripts/build-windows.ps1`、`cargo build --release`、`tauri build` 或其他原生发布构建。发布构建是构建验证，不替代定向测试。
- 汇报时只列出实际运行的命令与结果；扩大验证范围前，先说明具体风险和原因。

## 命令与工具链

- 默认通过 Git Bash 执行命令，避免 PowerShell 转码和临时命令拼接问题；不要编写或使用临时 PowerShell 构建命令。仓库内已提交的 `scripts/build-windows.ps1` 仅可在满足二进制构建开关时作为既定发布入口调用。
- 已知本机工具路径如下。优先直接使用这些路径，避免每次重新搜索；但不得把个人绝对路径硬编码进仓库构建脚本，应保持脚本的环境发现/相对路径兼容性。

| 工具 | 绝对路径 |
| --- | --- |
| Git Bash | `C:\Program Files\Git\bin\bash.exe` |
| Git | `C:\Program Files\Git\cmd\git.exe` |
| Cargo | `C:\Users\deepin\.cargo\bin\cargo.exe` |
| Node.js | `C:\Users\deepin\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\bin\node.exe` |
| pnpm | `C:\Users\deepin\.cache\codex-runtimes\codex-primary-runtime\dependencies\bin\fallback\pnpm.cmd` |
| MSVC 环境 | `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat` |
