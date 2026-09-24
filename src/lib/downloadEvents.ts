import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { DownloadGlobalStat, QueueTask, QueueTaskInput } from './types';

export type TaskSnapshotScope = 'active' | 'terminal';

// Match the backend's durable active/terminal snapshot scopes. Failed tasks
// remain visible in the Failed tab, not in the active snapshot.
const terminalTaskStatuses = new Set<QueueTask['status']>(['completed', 'failed', 'cancelled', 'recycled']);

/** One logical timeline for live updates and list/metadata requests. A snapshot
 * keeps its request version when it returns; receiving it is not a new event. */
export function createTaskUpdateMerger() {
  let version = 0;
  const taskVersions = new Map<string, number>();
  const scopeVersions = new Map<TaskSnapshotScope, number>();
  return {
    beginSnapshot: () => ++version,
    protectRemoved(ids: readonly string[]) {
      const removedVersion = ++version;
      for (const id of ids) taskVersions.set(id, removedVersion);
    },
    merge(
      current: readonly QueueTask[],
      snapshots: readonly QueueTask[],
      options: { requestVersion?: number; scope?: TaskSnapshotScope; protectedIds?: ReadonlySet<string> } = {},
    ): QueueTask[] {
      const incomingVersion = options.requestVersion ?? ++version;
      if (options.scope) {
        if ((scopeVersions.get(options.scope) ?? 0) > incomingVersion) return [...current];
        scopeVersions.set(options.scope, incomingVersion);
      }
      const protectedIds = new Set(options.protectedIds);
      for (const [id, appliedVersion] of taskVersions) {
        if (appliedVersion > incomingVersion) protectedIds.add(id);
      }
      if (options.requestVersion !== undefined && !options.scope) {
        // A single-task metadata response still belongs to a list scope. An
        // accepted newer list owns absence there, including IDs not seen yet.
        for (const task of snapshots) {
          const scope = taskMatchesSnapshotScope(task, 'active') ? 'active' : 'terminal';
          if ((scopeVersions.get(scope) ?? 0) > incomingVersion) protectedIds.add(task.id);
        }
      }
      const merged = options.scope
        ? reconcileTaskSnapshot(current, snapshots, options.scope, protectedIds)
        : mergeTaskSnapshot(current, snapshots, protectedIds);
      // Record concrete task updates globally. Absence belongs only to its
      // scope watermark: leaving active does not mean deletion from terminal.
      const record = (task: QueueTask) => {
        if (protectedIds.has(task.id)) return;
        taskVersions.set(task.id, incomingVersion);
        task.children?.forEach(record);
      };
      snapshots.forEach(record);
      return merged;
    },
  };
}

export function upsertTask(tasks: readonly QueueTask[], snapshot: QueueTask): QueueTask[] {
  // A directory snapshot owns its descendants. A list response may have observed
  // a member before it was linked; do not retain that stale standalone row.
  const memberIds = new Set<string>();
  collectDescendantIds(snapshot, memberIds);
  const roots = memberIds.size ? tasks.filter((task) => !memberIds.has(task.id)) : tasks;
  const next = replaceExistingTask(roots, snapshot);
  return next === roots ? [snapshot, ...roots] : [...next];
}

function collectDescendantIds(task: QueueTask, ids: Set<string>): void {
  for (const child of task.children ?? []) {
    ids.add(child.id);
    collectDescendantIds(child, ids);
  }
}

export function mergeTaskSnapshot(
  current: readonly QueueTask[],
  snapshots: readonly QueueTask[],
  protectedIds: ReadonlySet<string> = new Set()
): QueueTask[] {
  const currentTasks = protectedIds.size ? indexTasks(current) : new Map<string, QueueTask>();
  return snapshots.reduce<QueueTask[]>((merged, snapshot) => (
    protectedIds.has(snapshot.id) ? merged : upsertTask(merged, preserveLiveTasks(snapshot, currentTasks, protectedIds))
  ), [...current]);
}

/**
 * Reconcile one durable list scope instead of merely overlaying its rows. This
 * drops stale rows left by an event that raced with removal of a prepared task,
 * while retaining rows owned by the other list scope and newer live events.
 */
export function reconcileTaskSnapshot(
  current: readonly QueueTask[],
  snapshots: readonly QueueTask[],
  scope: TaskSnapshotScope,
  protectedIds: ReadonlySet<string> = new Set()
): QueueTask[] {
  const merged = mergeTaskSnapshot(current, snapshots, protectedIds);
  const durableIds = new Set(snapshots.map((task) => task.id));
  return merged.filter((task) => (
    !taskMatchesSnapshotScope(task, scope)
    || durableIds.has(task.id)
    || containsProtectedTask(task, protectedIds)
  ));
}

function taskMatchesSnapshotScope(task: QueueTask, scope: TaskSnapshotScope): boolean {
  const terminal = terminalTaskStatuses.has(task.status);
  return scope === 'terminal' ? terminal : !terminal;
}

function containsProtectedTask(task: QueueTask, protectedIds: ReadonlySet<string>): boolean {
  return protectedIds.has(task.id) || Boolean(task.children?.some((child) => containsProtectedTask(child, protectedIds)));
}

function indexTasks(tasks: readonly QueueTask[]): Map<string, QueueTask> {
  const indexed = new Map<string, QueueTask>();
  const visit = (items: readonly QueueTask[]) => {
    for (const task of items) {
      indexed.set(task.id, task);
      if (task.children) visit(task.children);
    }
  };
  visit(tasks);
  return indexed;
}

function preserveLiveTasks(
  snapshot: QueueTask,
  currentTasks: ReadonlyMap<string, QueueTask>,
  protectedIds: ReadonlySet<string>,
): QueueTask {
  const current = currentTasks.get(snapshot.id);
  if (current && protectedIds.has(snapshot.id)) return current;
  if (!currentTasks.size) return snapshot;
  const children = (snapshot.children ?? []).map((child) => preserveLiveTasks(child, currentTasks, protectedIds));
  const childIds = new Set(children.map((child) => child.id));
  for (const child of current?.children ?? []) {
    if (!childIds.has(child.id) && containsProtectedTask(child, protectedIds)) children.push(child);
  }
  return children.length !== (snapshot.children?.length ?? 0)
    || children.some((child, index) => child !== snapshot.children![index]) ? { ...snapshot, children } : snapshot;
}

function replaceExistingTask(tasks: readonly QueueTask[], snapshot: QueueTask): QueueTask[] | readonly QueueTask[] {
  let changed = false;
  const next = tasks.map((task) => {
    if (task.id === snapshot.id) {
      changed = true;
      return preserveKnownBitTorrentFiles(task, snapshot);
    }
    if (!task.children?.length) return task;
    const children = replaceExistingTask(task.children, snapshot);
    if (children === task.children) return task;
    changed = true;
    return { ...task, children };
  });
  return changed ? next : tasks;
}

function preserveKnownBitTorrentFiles(current: QueueTask, snapshot: QueueTask): QueueTask {
  const isBitTorrent = snapshot.kind === 'magnet' || snapshot.kind === 'torrent';
  if (!isBitTorrent || snapshot.files?.length || !current.files?.length) return snapshot;
  return { ...snapshot, files: current.files };
}

export function formatBytes(value: number | undefined): string {
  if (value === undefined || !Number.isFinite(value) || value < 0) return '未知';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let amount = value;
  let unit = 0;
  while (amount >= 1024 && unit < units.length - 1) { amount /= 1024; unit += 1; }
  return `${Number.isInteger(amount) ? amount : amount.toFixed(1)} ${units[unit]}`;
}

export function formatSpeed(value: number | undefined): string {
  return value === undefined ? '未知' : `${formatBytes(value)}/s`;
}

export function formatEta(seconds: number | undefined): string {
  if (seconds === undefined || !Number.isFinite(seconds) || seconds < 0) return '未知';
  const rounded = Math.floor(seconds);
  const hours = Math.floor(rounded / 3600);
  const minutes = Math.floor((rounded % 3600) / 60);
  const remainder = rounded % 60;
  if (hours) return `${hours} 小时 ${minutes} 分`;
  if (minutes) return `${minutes} 分 ${remainder} 秒`;
  return `${remainder} 秒`;
}

export function hasKnownTotal(task: QueueTask): boolean {
  return typeof task.total_length === 'number' && task.total_length > 0;
}

export function transferProgress(task: QueueTask): string {
  const completed = formatBytes(task.completed_length);
  return hasKnownTotal(task) ? `${completed} / ${formatBytes(task.total_length)}` : completed;
}

export async function subscribeToDownloadEvents(handlers: {
  onTask: (task: QueueTaskInput) => void;
  onGlobalStat: (stat: DownloadGlobalStat) => void;
}): Promise<UnlistenFn> {
  let stopTask: UnlistenFn | undefined;
  try {
    stopTask = await listen<QueueTaskInput>('download-task-updated', (event) => handlers.onTask(event.payload));
    const stopGlobal = await listen<DownloadGlobalStat>('download-global-stat-updated', (event) => handlers.onGlobalStat(event.payload));
    return () => { stopTask?.(); stopGlobal(); };
  } catch (error) {
    stopTask?.();
    throw error;
  }
}
