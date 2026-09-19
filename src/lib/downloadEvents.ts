import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { DownloadGlobalStat, QueueTask } from './types';

export type TaskSnapshotScope = 'active' | 'terminal';

// Failed tasks remain in the active download list so users can retry or
// manage them without switching to a separate terminal scope.
const terminalTaskStatuses = new Set<QueueTask['status']>(['completed', 'cancelled', 'recycled']);

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

export function mergeInitialTasks(current: readonly QueueTask[], snapshots: readonly QueueTask[]): QueueTask[] {
  const liveTasks = indexTasks(current);
  return snapshots.reduceRight<QueueTask[]>((merged, snapshot) => (
    containsTask(merged, snapshot.id) ? merged : upsertTask(merged, preserveLiveTasks(snapshot, liveTasks))
  ), [...current]);
}

export function mergeTaskSnapshot(
  current: readonly QueueTask[],
  snapshots: readonly QueueTask[],
  protectedIds: ReadonlySet<string> = new Set()
): QueueTask[] {
  const liveTasks = protectedIds.size ? indexTasks(current, protectedIds) : new Map<string, QueueTask>();
  return snapshots.reduce<QueueTask[]>((merged, snapshot) => (
    protectedIds.has(snapshot.id) ? merged : upsertTask(merged, preserveLiveTasks(snapshot, liveTasks))
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

function indexTasks(tasks: readonly QueueTask[], ids?: ReadonlySet<string>): Map<string, QueueTask> {
  const indexed = new Map<string, QueueTask>();
  const visit = (items: readonly QueueTask[]) => {
    for (const task of items) {
      if (!ids || ids.has(task.id)) indexed.set(task.id, task);
      if (task.children) visit(task.children);
    }
  };
  visit(tasks);
  return indexed;
}

function preserveLiveTasks(snapshot: QueueTask, liveTasks: ReadonlyMap<string, QueueTask>): QueueTask {
  const live = liveTasks.get(snapshot.id);
  if (live) return live;
  if (!liveTasks.size || !snapshot.children?.length) return snapshot;
  const children = snapshot.children.map((child) => preserveLiveTasks(child, liveTasks));
  return children.some((child, index) => child !== snapshot.children![index]) ? { ...snapshot, children } : snapshot;
}

function containsTask(tasks: readonly QueueTask[], id: string): boolean {
  return tasks.some((task) => task.id === id || (task.children ? containsTask(task.children, id) : false));
}

function replaceExistingTask(tasks: readonly QueueTask[], snapshot: QueueTask): QueueTask[] | readonly QueueTask[] {
  let changed = false;
  const next = tasks.map((task) => {
    if (task.id === snapshot.id) {
      changed = true;
      return preserveKnownTaskState(task, snapshot);
    }
    if (!task.children?.length) return task;
    const children = replaceExistingTask(task.children, snapshot);
    if (children === task.children) return task;
    changed = true;
    return { ...task, children };
  });
  return changed ? next : tasks;
}

function preserveKnownTaskState(current: QueueTask, snapshot: QueueTask): QueueTask {
  const withFiles = preserveKnownBitTorrentFiles(current, snapshot);
  const keepsConfirmedXunleiSvip = current.statusMessage === '正在使用迅雷svip加速'
    && !withFiles.statusMessage
    && withFiles.status === 'downloading'
    && withFiles.aria_gid?.startsWith('xunlei-native:')
    && withFiles.aria_gid === current.aria_gid;
  return keepsConfirmedXunleiSvip
    ? { ...withFiles, statusMessage: current.statusMessage }
    : withFiles;
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

export function formatCompactSpeed(value: number): string {
  if (!Number.isFinite(value) || value < 0) return '—';
  if (value < 1024) return `${Math.round(value)}B`;
  if (value < 1024 * 1024) return `${Math.round(value / 1024)}K`;
  if (value < 1024 ** 3) return `${(value / (1024 ** 2)).toFixed(1)}M`;
  if (value < 1024 ** 4) return `${(value / (1024 ** 3)).toFixed(1)}G`;
  return `${(value / (1024 ** 4)).toFixed(1)}T`;
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
  onTask: (task: QueueTask) => void;
  onGlobalStat: (stat: DownloadGlobalStat) => void;
}): Promise<UnlistenFn> {
  let stopTask: UnlistenFn | undefined;
  try {
    stopTask = await listen<QueueTask>('download-task-updated', (event) => handlers.onTask(event.payload));
    const stopGlobal = await listen<DownloadGlobalStat>('download-global-stat-updated', (event) => handlers.onGlobalStat(event.payload));
    return () => { stopTask?.(); stopGlobal(); };
  } catch (error) {
    stopTask?.();
    throw error;
  }
}
