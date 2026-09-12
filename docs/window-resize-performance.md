# 窗口最小尺寸与缩放性能验证（2026-09-12）

## 当前尺寸约定

用户后续明确指定最小尺寸为 **960×640**。最终配置的 `width`、`height`、`minWidth`、`minHeight` 采用 960、640、960、640，不再追加截图推算的边框补偿。以下截图换算和性能数据记录的是此前测试版；其二进制及 SHA-256 不代表最终 master 版本。

诊断脚本的最小尺寸验收同步改为检查原生跟踪尺寸除以 DPI 后是否为 960×640（允许 1px 取整误差）。

960×640 入库验证：`cargo test --manifest-path D:\projects\downloader\src-tauri\Cargo.toml --test window_config`，2 项通过。定向运行 `src/App.test.tsx`、`src/App.live.test.tsx`、`src/components/DownloadQueue.test.tsx` 和 `src/lib/windowResizePerformance.test.ts`（两个历史目录均已排除）；主页面模拟缺失新隐私接口及新增参数断言，补齐后单独重跑 `src/App.live.test.tsx` 的 52 项通过，其余 49 项此前在同轮已通过。未修改隐私下载业务逻辑。

`scripts/build-windows.ps1` 完成前端与 Rust release 编译，默认输出仍被旧进程锁定。新的 960×640 二进制已复制并校验到 `artifacts/window-resize-960x640/alldownloader.exe`，SHA-256 为 `458412FBAF6B36D99249599ED24A77B18D9662F3E9F5C40ACAC9B321D32298A5`。本轮未重跑性能基准；上述最小尺寸诊断断言已同步到新约定。

## 前一版实现与测量

- 截图及当前窗口对应约 954×651 逻辑像素的内容区，系统 DPI 为 168（175%）。
- 当前 Tao 无边框窗口把最小值作为窗口跟踪尺寸，未补偿保留的原生边框。配置采用 968×659；启动尺寸也采用此值。
- 对最终程序发送只读 `WM_GETMINMAXINFO`，得到最小跟踪尺寸 1694×1153 物理像素。扣除实际边框，最小内容区为 954.286×650.857 逻辑像素，误差来自 DPI 取整。
- 网盘文件行启用 `content-visibility:auto`，内在内容高度为 53px，加上 1px 顶边框保持原来的 54px 行高。展开、选择、排序逻辑未改变。
- 对整页改用固定高度弹性布局的实验没有额外收益，因此没有纳入产品。

## 性能证据

`scripts/measure-window-resize.cjs` 启动独立数据和 WebView2 目录的临时实例，在真实 WebView2 中使用生产样式及无账号的模拟文件行。每轮通过原生窗口命令改变尺寸 80 次，间隔 8ms；同一实例交替关闭、开启文件行离屏渲染。

最终二进制两组重复对比：

| 文件行数 | 离屏渲染 | 调整耗时中位数 | P95 | 80 次累计布局耗时 | 列表滚动高度 |
| --- | --- | --- | --- | --- | --- |
| 1000 | 关闭 | 30.3ms / 29.9ms | 42.6ms / 42.2ms | 1.543s / 1.518s | 54042px |
| 1000 | 开启 | 14.7ms / 15.4ms | 22.0ms / 23.6ms | 0.578s / 0.568s | 54042px |

7 行场景中位数均约 6.6–6.8ms，没有显著改善。GPU 合成和硬件光栅化已启用。

这些是程序化原生缩放的结果，不是鼠标到屏幕的延迟或帧率，不能证明少量文件场景的鼠标卡顿已解决。截图场景的剩余卡顿需要结合具体拖动边缘和实时帧时间继续定位。检查时用户同时运行三个应用实例，测试没有终止这些实例，资源竞争仍是未排除因素。

## 验证命令和结果

通过已配置 Node 运行以下命令：

```text
vitest run src/components/CloudManager.test.tsx src/components/CloudWorkspace.test.tsx --exclude '.worktrees/**' --exclude '.pnpm-store/**'
```

101 项通过。

```text
cargo test --manifest-path D:\projects\downloader\src-tauri\Cargo.toml --test window_config
```

最终配置 2 项通过。

```text
./scripts/build-windows.ps1
```

前端类型检查、生产打包和 Rust release 编译完成。脚本最终复制步骤因用户旧版正在运行而失败；将新生成的程序复制到独立目录并核对 SHA-256，未覆盖或关闭用户旧版。

```text
node scripts/measure-window-resize.cjs artifacts/window-resize-v2/alldownloader.exe --verify-minimum
```

退出码 0，原生鼠标最小尺寸与性能对比均完成。Windows 的程序化 `set_size` 能绕过交互最小值，因此最小尺寸验证读取 `WM_GETMINMAXINFO`，不以程序化缩小为判据。

## 测试程序

`artifacts/window-resize-v2/alldownloader.exe`，配套引擎在同目录的 `binaries` 子目录。

SHA-256：`89F12496165807173B7C242E87A9651D0291847AF279304B5537F811F6B706DD`

保留既有编译警告：Tauri window 模块同时动态与静态导入，以及两个未使用 Rust 函数。未执行全量测试或完整 launcher 冒烟测试。
