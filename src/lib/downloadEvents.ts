import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { DownloadGlobalStat, QueueTask } from './types';

export function upsertTask(tasks: readonly QueueTask[], snapshot: QueueTask): QueueTask[] {
  const next = replaceExistingTask(tasks, snapshot);
  return next === tasks ? [snapshot, ...tasks] : [...next];
}

export function mergeInitialTasks(current: readonly QueueTask[], snapshots: readonly QueueTask[]): QueueTask[] {
  return snapshots.reduceRight<QueueTask[]>((merged, snapshot) => (
    containsTask(merged, snapshot.id) ? merged : [snapshot, ...merged]
  ), [...current]);
}

export function mergeTaskSnapshot(
  current: readonly QueueTask[],
  snapshots: readonly QueueTask[],
  protectedIds: ReadonlySet<string> = new Set()
): QueueTask[] {
  return snapshots.reduce<QueueTask[]>((merged, snapshot) => (
    protectedIds.has(snapshot.id) ? merged : upsertTask(merged, snapshot)
  ), [...current]);
}

function containsTask(tasks: readonly QueueTask[], id: string): boolean {
  return tasks.some((task) => task.id === id || (task.children ? containsTask(task.children, id) : false));
}

function replaceExistingTask(tasks: readonly QueueTask[], snapshot: QueueTask): QueueTask[] | readonly QueueTask[] {
  let changed = false;
  const next = tasks.map((task) => {
    if (task.id === snapshot.id) {
      changed = true;
      return snapshot;
    }
    if (!task.children?.length) return task;
    const children = replaceExistingTask(task.children, snapshot);
    if (children === task.children) return task;
    changed = true;
    return { ...task, children };
  });
  return changed ? next : tasks;
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
