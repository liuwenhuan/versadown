# Task 7：目录组前端权威快照消费报告

## RED

- 新增 App 实时集成用例覆盖目录根接收后成员事件与成员动作结果：修复前，动作结果只会合并裸成员，父根进度停留在旧值。
- 新增虚拟失败叶子重试用例：修复前，重试返回的新真实成员会成为顶层任务，不能替换原失败叶子。

## GREEN

- `TaskActionResult`、通用任务动作映射和云盘下载映射保留可选 `queueTask` 字段。
- CloudManager 对目录提交优先将权威递归根交给应用；普通文件和重复任务路径不变。
- App 在单任务、批量任务、分享确认和网盘提交路径中优先合并 `queueTask`，避免将目录成员提升为顶层。
- 真实 App 用例确认目录根的成员事件/成员动作会更新父根进度，虚拟失败叶子的重试会以新根替换原叶子。

## Verification

```powershell
pnpm exec vitest run src/App.live.test.tsx --exclude '.worktrees/**' --reporter=verbose
pnpm exec vitest run src/components/CloudManager.test.tsx src/components/DownloadQueue.test.tsx src/lib/cloudApi.test.ts --exclude '.worktrees/**' --reporter=verbose
pnpm run build
git diff --check
```

结果：App 44/44，CloudManager/DownloadQueue/cloudApi 117/117，生产前端构建成功，差异检查通过。App 测试输出含既有 CloudWorkspace 的 React `act(...)` 警告；本任务未新增该警告。

## Sensitive-data review

本次只传递任务树快照；未增加凭据、Cookie、授权码、签名 URL 或下载链接到 UI/测试渲染。

## Commit

本报告与本 Task 的前端实现一并精确提交到 `master`。
