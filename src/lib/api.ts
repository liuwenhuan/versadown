import { ConfirmResource, ParseResult, QueueTask } from './types';
import { invoke } from '@tauri-apps/api/core';

function fileNameFrom(value: string) {
  const path = value.split(/[?#]/)[0];
  return decodeURIComponent(path.split('/').filter(Boolean).pop() || '新下载任务');
}

function id() { return `local-${Date.now()}-${Math.random().toString(16).slice(2)}`; }

export function parseLocally(input: string): { task?: QueueTask; confirmation?: ParseResult } {
  const trimmed = input.trim();
  const isMulti = /^(magnet:|thunder:)/i.test(trimmed) || /\.torrent(?:\?|$)/i.test(trimmed) || /(?:pan\.quark\.cn|mypikpak\.com|pikpak\.me)/i.test(trimmed);
  if (isMulti) {
    const resources: ConfirmResource[] = [
      { id: `${id()}-1`, name: '下载内容 01', size: '待连接后获取', selected: true },
      { id: `${id()}-2`, name: '下载内容 02', size: '待连接后获取', selected: true }
    ];
    return { confirmation: { taskId: id(), requiresConfirmation: true, resources, processing: ['完成后按白名单检查后处理步骤'] } };
  }
  if (/^https?:\/\//i.test(trimmed)) {
    return { task: { id: id(), name: fileNameFrom(trimmed), kind: 'file', status: 'downloading', progress: 0, speed: '准备连接', source: trimmed } };
  }
  return { confirmation: { taskId: id(), requiresConfirmation: true, resources: [{ id: `${id()}-candidate`, name: '从文本中发现的候选下载项', size: '需要进一步解析', selected: true }], processing: ['将用本地规则提取链接；必要时请求脱敏 AI 分析'] } };
}

export function isTauriRuntime() {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

function confirmationFor(taskId: string): ParseResult {
  return {
    taskId, requiresConfirmation: true,
    resources: [{ id: `${taskId}-default`, name: '待连接后获取文件列表', size: '等待连接器/种子元数据', selected: true }],
    processing: ['文件选择确认后调用本地 aria2 下载']
  };
}

export async function parseDownload(input: string): Promise<{ task?: QueueTask; confirmation?: ParseResult }> {
  if (!isTauriRuntime()) return parseLocally(input);
  const result = await invoke<{ task: QueueTask; requires_confirmation: boolean }>('parse_download', { input });
  return result.requires_confirmation ? { confirmation: confirmationFor(result.task.id) } : { task: result.task };
}

export async function confirmDownload(taskId: string): Promise<QueueTask | undefined> {
  if (!isTauriRuntime()) return undefined;
  return invoke<QueueTask>('confirm_download_command', { taskId });
}

export async function listPersistedTasks(): Promise<QueueTask[]> {
  return isTauriRuntime() ? invoke<QueueTask[]>('list_tasks') : [];
}

export async function runTaskAction(taskId: string, action: 'pause' | 'resume' | 'cancel'): Promise<QueueTask | undefined> {
  return isTauriRuntime() ? invoke<QueueTask>('task_action', { taskId, action }) : undefined;
}
