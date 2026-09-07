import { ConfirmResource, ParseResult, QueueTask } from './types';

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
