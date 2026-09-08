import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { DownloadGlobalStat, QueueTask } from './types';

export function upsertTask(tasks: readonly QueueTask[], snapshot: QueueTask): QueueTask[] {
  const next = replaceExistingTask(tasks, snapshot);
  return next === tasks ? [snapshot, ...tasks] : [...next];
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
  const [stopTask, stopGlobal] = await Promise.all([
    listen<QueueTask>('download-task-updated', (event) => handlers.onTask(event.payload)),
    listen<DownloadGlobalStat>('download-global-stat-updated', (event) => handlers.onGlobalStat(event.payload))
  ]);
  return () => { stopTask(); stopGlobal(); };
}
