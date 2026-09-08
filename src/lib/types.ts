export type TaskStatus = 'draft' | 'parsing' | 'awaiting_user' | 'queued' | 'downloading' | 'processing' | 'completed' | 'failed' | 'cancelled' | 'paused';
export type TaskKind = 'file' | 'folder' | 'magnet' | 'torrent' | 'cloud';
export type DuplicateDownloadState = 'existing_active' | 'resume_available' | 'completed_files_present' | 'completed_files_missing' | 'overwrite_required';

export interface QueueTask {
  id: string;
  name: string;
  kind: TaskKind;
  status: TaskStatus;
  progress: number;
  speed?: string;
  eta?: string;
  size?: string;
  source?: string;
  children?: readonly QueueTask[];
  error?: string;
  aria_gid?: string;
  followed_by_gid?: string;
  total_length?: number;
  completed_length?: number;
  download_speed?: number;
  upload_speed?: number;
  connections?: number;
  eta_seconds?: number;
  aria_error_code?: string;
  files?: readonly DownloadFile[];
  file_index?: number;
  selected?: boolean;
}

export interface DownloadFile {
  index: number;
  path: string;
  length: number;
  completed_length: number;
  selected: boolean;
}

export interface DuplicateDownload {
  task: QueueTask;
  state: DuplicateDownloadState;
}

export interface TaskActionResult {
  task?: QueueTask;
  duplicate?: DuplicateDownload;
}

export interface DownloadGlobalStat {
  download_speed: number;
  upload_speed: number;
  num_active: number;
  num_waiting?: number;
  num_stopped?: number;
  num_stopped_total?: number;
}

export interface DownloadEngineState {
  available: boolean;
  retrying: boolean;
  error?: string;
}

export interface ConfirmResource {
  id: string;
  name: string;
  size: string;
  selected: boolean;
  index?: number;
}

export interface ParseResult {
  taskId: string;
  requiresConfirmation: boolean;
  resources: ConfirmResource[];
  processing: string[];
  discoveredPassword?: string;
  isBitTorrent?: boolean;
}

export type Provider = 'quark' | 'pikpak';

export interface CloudItem {
  id: string;
  name: string;
  kind: 'folder' | 'file';
  size?: string;
  modified: string;
}
