export type TaskStatus = 'draft' | 'parsing' | 'awaiting_user' | 'queued' | 'downloading' | 'processing' | 'completed' | 'failed' | 'cancelled' | 'paused';
export type TaskKind = 'file' | 'folder' | 'magnet' | 'torrent' | 'cloud';
export type DuplicateDownloadState = 'existing_active' | 'resume_available' | 'completed_files_present' | 'completed_files_missing' | 'overwrite_required';
export type TaskCommand = 'pause' | 'resume' | 'cancel' | 'retry' | 'open_directory' | 'copy_link' | 'details' | 'remove_record';

export interface QueueTask {
  id: string;
  name: string;
  kind: TaskKind;
  status: TaskStatus;
  progress: number;
  speed?: string;
  speedBytes?: number;
  size?: string;
  eta?: string;
  connections?: number;
  statusMessage?: string;
  source?: string;
  children?: readonly QueueTask[];
  error?: string;
}

export interface DuplicateDownload {
  task: QueueTask;
  state: DuplicateDownloadState;
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

export type ProxyProtocol = 'http' | 'https';

export interface ProxySettings {
  enabled: boolean;
  protocol: ProxyProtocol;
  host: string;
  port: number;
  username: string;
  password: string;
}

export interface AppSettings {
  debugReports: boolean;
  downloadDirectory: string;
  proxy: ProxySettings;
}
