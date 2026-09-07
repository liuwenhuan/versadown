export type TaskStatus = 'draft' | 'parsing' | 'awaiting_user' | 'queued' | 'downloading' | 'processing' | 'completed' | 'failed' | 'cancelled' | 'paused';
export type TaskKind = 'file' | 'folder' | 'magnet' | 'torrent' | 'cloud';

export interface QueueTask {
  id: string;
  name: string;
  kind: TaskKind;
  status: TaskStatus;
  progress: number;
  speed?: string;
  size?: string;
  source?: string;
  children?: readonly QueueTask[];
  error?: string;
}

export interface ConfirmResource {
  id: string;
  name: string;
  size: string;
  selected: boolean;
}

export interface ParseResult {
  taskId: string;
  requiresConfirmation: boolean;
  resources: ConfirmResource[];
  processing: string[];
  discoveredPassword?: string;
}

export type Provider = 'quark' | 'pikpak';

export interface CloudItem {
  id: string;
  name: string;
  kind: 'folder' | 'file';
  size?: string;
  modified: string;
}
