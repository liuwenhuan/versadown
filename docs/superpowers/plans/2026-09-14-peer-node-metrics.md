# 下载任务节点指标 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在下载中的磁链、BT 和 ED2K 任务标题行右侧显示来自 aria2 的实时节点指标。

**Architecture:** 扩展 aria2 RPC 客户端以读取 BT `getPeers` 结果，下载服务仅在内存中以 `ip:port` 对节点去重，并将计数快照通过任务存储、Tauri payload 发送给 React。ED2K 不请求节点列表，只复用其原有实时连接数。React 根据任务种类与下载状态渲染不同文案，并用弹性布局保留标题省略与右侧对齐。

**Tech Stack:** Rust、SQLite/rusqlite、Tauri、React 18、TypeScript、Vitest、aria2-next JSON-RPC。

**Spec:** `docs/superpowers/specs/2026-09-14-peer-node-metrics-design.md`

## Global Constraints

- 仅 `downloading` 状态显示节点指标。
- 磁链与 BT 的 `已发现` 仅表示当前 aria2 进程会话中观察到的去重节点，不是互联网或 DHT 总量。
- ED2K 仅显示 `已连接 x`，不得由任务总速度伪造“有传输节点数”。
- 不持久化、发送或渲染节点 IP、端口和 peer ID。
- `aria2.getPeers` 失败不得中断 `tellStatus` 的进度同步。
- 仅运行相关定向 Rust/Vitest 测试、`npm run build` 和 `scripts/build-windows.ps1`；Vitest 必须带 `--exclude '.worktrees/**'`，不运行全量测试。

---

### Task 1: 添加受认证的 BT 节点 RPC

**Files:**
- Modify: `src-tauri/src/aria2/types.rs`
- Modify: `src-tauri/src/aria2/rpc.rs`
- Test: `src-tauri/tests/aria2_rpc_contract.rs`

**Interfaces:**
- Produces: `Aria2Peer { ip: String, port: String }`。
- Produces: `Aria2Api::get_peers(&self, gid: &str) -> Result<Vec<Aria2Peer>, Aria2Error>`。
- Produces: `Aria2RpcClient` 对 `aria2.getPeers` 的受 token 认证 JSON-RPC 调用。

- [ ] **Step 1: 写入会失败的 RPC 契约测试**

~~~rust
#[tokio::test]
async fn get_peers_sends_the_gid_and_decodes_peer_addresses() {
    let (url, calls, handle) = fixture(
        json!({"jsonrpc":"2.0","id":"1","result":[
            {"ip":"203.0.113.10","port":"51413"},
            {"ip":"2001:db8::7","port":"51414"}
        ]}),
        StatusCode::OK,
        Duration::ZERO,
    ).await;
    let client = Aria2RpcClient::new(url, RpcSecret::from_bytes([0xab; 32])).unwrap();

    let peers = client.get_peers("bt-gid").await.unwrap();
    handle.abort();

    assert_eq!(peers.len(), 2);
    assert_eq!(peers[0].ip, "203.0.113.10");
    assert_eq!(peers[1].port, "51414");
    let calls = calls.lock().unwrap();
    assert_eq!(calls[0]["method"], "aria2.getPeers");
    assert_eq!(calls[0]["params"][1], "bt-gid");
    assert!(calls[0]["params"][0].as_str().unwrap().starts_with("token:"));
}
~~~

- [ ] **Step 2: 运行测试，确认因缺少 `get_peers` 方法而失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_rpc_contract get_peers_sends_the_gid_and_decodes_peer_addresses`

Expected: 编译失败，指出 `Aria2RpcClient` 或 `Aria2Api` 尚无 `get_peers`。

- [ ] **Step 3: 写入最小 RPC 实现**

~~~rust
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Aria2Peer {
    pub ip: String,
    pub port: String,
}

async fn get_peers(&self, gid: &str) -> Result<Vec<Aria2Peer>, Aria2Error> {
    self.call("aria2.getPeers", vec![json!(gid)]).await
}
~~~

在 trait 中提供返回空列表的默认实现，令不关心节点统计的既有测试替身保持兼容；生产客户端覆盖该实现。

- [ ] **Step 4: 运行测试，确认 RPC 契约通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_rpc_contract get_peers_sends_the_gid_and_decodes_peer_addresses`

Expected: PASS，且断言 token、GID 与两个节点字段均正确。

- [ ] **Step 5: 提交本任务**

~~~bash
git add src-tauri/src/aria2/types.rs src-tauri/src/aria2/rpc.rs src-tauri/tests/aria2_rpc_contract.rs
git commit -m "feat: expose aria2 bittorrent peers"
~~~

### Task 2: 同步并发布发现节点计数

**Files:**
- Modify: `src-tauri/src/models.rs`
- Modify: `src-tauri/src/store.rs`
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/aria2/service.rs`
- Test: `src-tauri/tests/aria2_bt.rs`
- Test: `src-tauri/tests/store_aria2_migration.rs`

**Interfaces:**
- Produces: `DownloadTask.discovered_nodes: u32` 与 `QueueTaskPayload.discovered_nodes: u32`。
- Produces: SQLite `tasks.discovered_nodes INTEGER NOT NULL DEFAULT 0`，兼容已有数据库。
- Consumes: `Aria2Api::get_peers`。

- [ ] **Step 1: 写入服务层失败测试**

~~~rust
#[tokio::test]
async fn reconcile_tracks_unique_peers_for_an_active_torrent() {
    let (client, server) = fixture::server(|request| match request["method"].as_str().unwrap() {
        "aria2.tellStatus" => fixture::status("torrent-gid", "active", 0),
        "aria2.getPeers" => json!([
            {"ip":"203.0.113.10","port":"51413"},
            {"ip":"203.0.113.10","port":"51413"},
            {"ip":"2001:db8::7","port":"51414"}
        ]),
        "aria2.getGlobalStat" => fixture::global_stat(),
        other => panic!("unexpected method {other}"),
    }).await;
    let store = Arc::new(TaskStore::open_in_memory().unwrap());
    let task = store.create(NewTask {
        source: "magnet:?xt=urn:btih:test".into(),
        name: "test".into(),
        kind: "magnet".into(),
        is_private: false,
    }).unwrap();
    store.bind_gid(task.id, "torrent-gid").unwrap();
    let service = DownloadService::new(Arc::new(client), store.clone());

    service.reconcile_now().await.unwrap();

    assert_eq!(store.find(task.id).unwrap().discovered_nodes, 2);
    server.abort();
}
~~~

在迁移测试中从缺少该列的历史 tasks 表打开存储，断言字段默认值为 `0`。

- [ ] **Step 2: 运行测试，确认因尚未同步或保存发现数而失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_bt reconcile_tracks_unique_peers_for_an_active_torrent --test store_aria2_migration`

Expected: 断言失败或编译失败，显示 `discovered_nodes` 尚不存在，或计数仍为 `0`。

- [ ] **Step 3: 写入最小数据与同步实现**

~~~rust
if matches!(updated.status, TaskStatus::Downloading)
    && matches!(updated.kind.as_str(), "magnet" | "torrent") {
    if let Ok(peers) = self.api().get_peers(updated.aria_gid.as_deref().unwrap_or_default()).await {
        let seen = self.observed_peers.lock().unwrap().entry(updated.id).or_default();
        seen.extend(peers.into_iter().map(|peer| format!("{}:{}", peer.ip, peer.port)));
        let discovered_nodes = seen.len().try_into().unwrap_or(u32::MAX);
        if updated.discovered_nodes != discovered_nodes {
            updated.discovered_nodes = discovered_nodes;
            let updated = self.store.apply_transfer_snapshot(updated)?;
            self.publish_task(&updated)?;
        }
    }
}
~~~

将 `discovered_nodes` 加入模型、任务创建/重试重置、`TASK_SELECT`、行映射、快照 UPDATE、迁移、队列 payload 和普通任务 payload；服务内存映射只存格式化的去重键，并在任务不再下载时移除对应集合。

- [ ] **Step 4: 运行测试，确认节点去重、迁移与状态同步通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_bt reconcile_tracks_unique_peers_for_an_active_torrent --test store_aria2_migration`

Expected: PASS；两个重复/不同地址节点产生发现数 `2`，旧库任务读取 `0`，状态同步仍成功。

- [ ] **Step 5: 提交本任务**

~~~bash
git add src-tauri/src/models.rs src-tauri/src/store.rs src-tauri/src/commands.rs src-tauri/src/aria2/service.rs src-tauri/tests/aria2_bt.rs src-tauri/tests/store_aria2_migration.rs
git commit -m "feat: sync discovered bittorrent peer counts"
~~~

### Task 3: 在下载列表标题行渲染协议指标

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/components/DownloadQueue.tsx`
- Modify: `src/components/DownloadQueue.test.tsx`
- Modify: `src/styles.css`
- Modify: `src/visual-tests/DownloadQueueVisual.tsx`

**Interfaces:**
- Consumes: `QueueTask.connections`、`QueueTask.discovered_nodes`、`kind` 和 `status`。
- Produces: 磁链/BT `已连接 x / 已发现 y` 与 ED2K `已连接 x` 的标题行文字。

- [ ] **Step 1: 写入组件失败测试**

~~~tsx
it('shows live peer metrics only for downloading peer-to-peer tasks', () => {
  render(<DownloadQueue tasks={[
    { id: 'magnet', name: 'movie.mkv', kind: 'magnet', status: 'downloading', progress: 25, connections: 3, discovered_nodes: 8 },
    { id: 'torrent', name: 'bundle', kind: 'torrent', status: 'downloading', progress: 25, connections: 1, discovered_nodes: 4 },
    { id: 'ed2k', name: 'archive.zip', kind: 'ed2k', status: 'downloading', progress: 25, connections: 2 },
    { id: 'paused', name: 'paused.torrent', kind: 'torrent', status: 'paused', progress: 25, connections: 6, discovered_nodes: 9 },
    { id: 'file', name: 'file.iso', kind: 'file', status: 'downloading', progress: 25, connections: 99 },
  ]} selectedTaskId={null} onSelect={() => {}} onCommand={() => {}} />);

  expect(screen.getByText('已连接 3 / 已发现 8')).toBeVisible();
  expect(screen.getByText('已连接 1 / 已发现 4')).toBeVisible();
  expect(screen.getByText('已连接 2')).toBeVisible();
  expect(screen.queryByText('已连接 6 / 已发现 9')).not.toBeInTheDocument();
  expect(screen.queryByText('已连接 99')).not.toBeInTheDocument();
});
~~~

- [ ] **Step 2: 运行测试，确认新指标尚未渲染**

Run: `npm run test:run -- src/components/DownloadQueue.test.tsx --exclude '.worktrees/**'`

Expected: FAIL，测试找不到磁链、BT 和 ED2K 的节点指标文本。

- [ ] **Step 3: 写入最小前端实现**

~~~tsx
function taskPeerMetric(task: QueueTask): string | undefined {
  if (task.status !== 'downloading') return undefined;
  const connections = task.connections ?? 0;
  if (task.kind === 'magnet' || task.kind === 'torrent') {
    return '已连接 ' + connections + ' / 已发现 ' + (task.discovered_nodes ?? 0);
  }
  return task.kind === 'ed2k' ? '已连接 ' + connections : undefined;
}
~~~

在 `.task-copy` 内渲染 `task-peer-metric`。将标题设为可收缩的 flex 项，指标设为不可收缩并右对齐；窄屏仍优先保留指标且标题省略。视觉夹具给磁链任务设置 `discovered_nodes`，用于人工检查长标题不覆盖指标。

- [ ] **Step 4: 运行组件测试，确认协议筛选与文案通过**

Run: `npm run test:run -- src/components/DownloadQueue.test.tsx --exclude '.worktrees/**'`

Expected: PASS；普通 HTTP 任务和暂停种子均不显示节点指标。

- [ ] **Step 5: 提交本任务**

~~~bash
git add src/lib/types.ts src/components/DownloadQueue.tsx src/components/DownloadQueue.test.tsx src/styles.css src/visual-tests/DownloadQueueVisual.tsx
git commit -m "feat: show live peer metrics in download queue"
~~~

### Task 4: 进行范围匹配的构建验证

**Files:**
- Verify only: 本计划修改的 Rust、TypeScript 与 CSS 文件。

- [ ] **Step 1: 运行关联 Rust 测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test aria2_rpc_contract get_peers_sends_the_gid_and_decodes_peer_addresses; cargo test --manifest-path src-tauri/Cargo.toml --test aria2_bt reconcile_tracks_unique_peers_for_an_active_torrent; cargo test --manifest-path src-tauri/Cargo.toml --test store_aria2_migration`

Expected: 每个定向测试命令 PASS。

- [ ] **Step 2: 运行关联组件测试**

Run: `npm run test:run -- src/components/DownloadQueue.test.tsx --exclude '.worktrees/**'`

Expected: PASS。

- [ ] **Step 3: 运行前端构建**

Run: `npm run build`

Expected: PASS。

- [ ] **Step 4: 运行 Windows 发布构建**

Run: `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows.ps1`

Expected: PASS，生成 Windows 发布二进制。

- [ ] **Step 5: 检查工作树差异并提交验证后的改动**

~~~bash
git status --short
git diff --check
git add docs/superpowers/specs/2026-09-14-peer-node-metrics-design.md docs/superpowers/plans/2026-09-14-peer-node-metrics.md
git commit -m "docs: specify live peer metrics"
~~~
