import { AiSettings, AiSettingsInput, AppSettings, CloudShareAccessCodeRequired, ConfirmResource, DownloadEngineState, DownloadGlobalStat, DuplicateDownloadState, ParseDownloadResult, ParseResult, QueueTask, TaskActionResult, TaskCommand } from './types';
import { invoke } from '@tauri-apps/api/core';

function fileNameFrom(value: string) {
  const path = value.split(/[?#]/)[0];
  return decodeURIComponent(path.split('/').filter(Boolean).pop() || '新下载任务');
}

function id() { return `local-${Date.now()}-${Math.random().toString(16).slice(2)}`; }

export function parseLocally(input: string): ParseDownloadResult {
  const trimmed = input.trim();
  const isMulti = /^(magnet:|thunder:)/i.test(trimmed) || /\.torrent(?:\?|$)/i.test(trimmed) || /(?:pan\.quark\.cn|mypikpak\.com|pikpak\.me)/i.test(trimmed);
  if (isMulti) {
    const resources: ConfirmResource[] = [
      { id: `${id()}-1`, name: '下载内容 01', size: '待连接后获取', selected: true },
      { id: `${id()}-2`, name: '下载内容 02', size: '待连接后获取', selected: true }
    ];
    return { confirmation: { taskId: id(), title: 'BT 下载任务', requiresConfirmation: true, resources, processing: ['完成后按白名单检查后处理步骤'] } };
  }
  if (/^https?:\/\//i.test(trimmed)) {
    return { task: { id: id(), name: fileNameFrom(trimmed), kind: 'file', status: 'downloading', progress: 0, speed: '准备连接', source: trimmed } };
  }
  return { confirmation: { taskId: id(), title: '待确认的下载内容', requiresConfirmation: true, resources: [{ id: `${id()}-candidate`, name: '从文本中发现的候选下载项', size: '需要进一步解析', selected: true }], processing: ['将用本地规则提取链接；必要时请求脱敏 AI 分析'] } };
}

export function isTauriRuntime() {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

type NativeTaskOutcome = QueueTask & { duplicate?: DuplicateDownloadState };
type NativeTaskParseOutcome = {
  kind: 'task';
  task: QueueTask;
  requires_confirmation: boolean;
  resources?: ConfirmResource[];
  duplicate?: DuplicateDownloadState;
};

type NativeShareConfirmationOutcome = {
  kind: 'share_confirmation';
  plan_source: string;
  title: string;
  resources: ConfirmResource[];
};

type NativeAccessCodeRequiredOutcome = {
  kind: 'access_code_required';
  provider: CloudShareAccessCodeRequired['provider'];
  message: string;
};

type NativeParseOutcome = NativeTaskParseOutcome | NativeShareConfirmationOutcome | NativeAccessCodeRequiredOutcome;

function duplicateResult(result: NativeTaskOutcome): TaskActionResult {
  return result.duplicate
    ? { duplicate: { task: result, state: result.duplicate } }
    : { task: result };
}

export async function parseDownload(input: string): Promise<ParseDownloadResult> {
  if (!isTauriRuntime()) return parseLocally(input);
  const result = await invoke<NativeParseOutcome>('parse_download', { input });
  if (result.kind === 'share_confirmation') {
    return {
      confirmation: {
        taskId: result.plan_source,
        title: result.title,
        requiresConfirmation: true,
        resources: result.resources,
        processing: ['确认后开始网盘下载']
      }
    };
  }
  if (result.kind === 'access_code_required') {
    return {
      shareAccessCodeRequired: {
        provider: result.provider,
        message: result.message
      }
    };
  }
  if (result.duplicate) return { duplicate: { task: result.task, state: result.duplicate } };
  if (!result.requires_confirmation) return { task: result.task };
  return {
    confirmation: {
      taskId: result.task.id,
      title: result.task.name,
      requiresConfirmation: true,
      resources: result.resources ?? [],
      processing: ['文件选择确认后调用本地 aria2 下载'],
      isBitTorrent: result.task.kind === 'magnet' || result.task.kind === 'torrent'
    }
  };
}

export async function confirmDownload(taskId: string, selectedIds: string[] = [], destinationDirectory?: string): Promise<QueueTask | undefined> {
  if (!isTauriRuntime()) return undefined;
  return invoke<QueueTask>('confirm_download_command', { taskId, selectedIds, destinationDirectory });
}

export async function listPersistedTasks(): Promise<QueueTask[]> {
  return isTauriRuntime() ? invoke<QueueTask[]>('list_tasks') : [];
}

export async function runTaskAction(taskId: string, action: Extract<TaskCommand, 'pause' | 'resume' | 'cancel' | 'retry'>): Promise<TaskActionResult> {
  if (!isTauriRuntime()) return {};
  return duplicateResult(await invoke<NativeTaskOutcome>('task_action', { taskId, action }));
}

export async function openTaskDirectory(taskId: string): Promise<void> {
  if (isTauriRuntime()) await invoke('open_task_directory', { taskId });
}

export async function playTaskVideos(taskId: string): Promise<void> {
  if (isTauriRuntime()) await invoke('play_task_videos', { taskId });
}

export async function resolveDuplicateDownload(taskId: string, action: 'resume' | 'restart_missing' | 'overwrite'): Promise<QueueTask | undefined> {
  return isTauriRuntime() ? invoke<QueueTask>('resolve_duplicate_download', { taskId, action }) : undefined;
}

export async function removeTaskRecord(taskId: string): Promise<void> {
  if (isTauriRuntime()) await invoke('remove_task_record', { taskId });
}

export async function moveTaskToRecycle(taskId: string): Promise<QueueTask | undefined> {
  return isTauriRuntime() ? invoke<QueueTask>('recycle_task', { taskId }) : undefined;
}

export async function purgeRecycledTasks(taskIds: string[], deleteFiles: boolean): Promise<void> {
  if (isTauriRuntime()) await invoke('purge_recycled_tasks', { taskIds, deleteFiles });
}

export async function selectDownloadFiles(taskId: string, indices: number[]): Promise<QueueTask | undefined> {
  return isTauriRuntime() ? invoke<QueueTask>('select_files', { taskId, indices }) : undefined;
}

export async function getDownloadEngineState(): Promise<DownloadEngineState> {
  return isTauriRuntime()
    ? invoke<DownloadEngineState>('get_download_engine_state')
    : { available: true, retrying: false };
}

export async function retryDownloadEngine(): Promise<DownloadEngineState> {
  return isTauriRuntime()
    ? invoke<DownloadEngineState>('retry_download_engine')
    : { available: true, retrying: false };
}

export async function getDownloadGlobalStat(): Promise<DownloadGlobalStat> {
  return isTauriRuntime()
    ? invoke<DownloadGlobalStat>('get_download_global_stat')
    : { download_speed: 0, upload_speed: 0, num_active: 0 };
}

export const defaultSettings: AppSettings = {
  debugReports: false,
  downloadDirectory: '',
  launcherVisibility: 'always',
  proxy: { enabled: false, protocol: 'http', host: '', port: 7890, username: '', password: '' },
  ai: { enabled: false, baseUrl: '', model: '', apiKeyConfigured: false },
  magnetMetadata: { enabled: true, baseUrl: 'http://peer.aigutta.com:8080' }
};

export async function getSettings(): Promise<AppSettings> {
  return isTauriRuntime() ? invoke<AppSettings>('get_settings') : defaultSettings;
}

export async function updateSettings(settings: AppSettings): Promise<AppSettings> {
  return isTauriRuntime() ? invoke<AppSettings>('update_settings', { settings }) : settings;
}

export async function updateAiSettings(settings: AiSettingsInput): Promise<AiSettings> {
  if (!isTauriRuntime()) {
    return { enabled: settings.enabled, baseUrl: settings.baseUrl, model: settings.model, apiKeyConfigured: Boolean(settings.apiKey) };
  }
  return invoke<AiSettings>('update_ai_settings', { settings });
}

export async function listAiModels(settings: AiSettingsInput): Promise<string[]> {
  if (!isTauriRuntime()) return [];
  return invoke<string[]>('list_ai_models', { settings });
}
