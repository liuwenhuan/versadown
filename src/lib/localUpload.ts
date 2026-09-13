export interface LocalUploadEntry {
  path: string;
  name: string;
  kind: 'file' | 'directory';
  size?: number | null;
  /** Milliseconds since Unix epoch; unavailable metadata is null. */
  modifiedAt?: number | null;
}

export interface LocalUploadListing {
  path: string;
  parentPath: string | null;
  roots: LocalUploadEntry[];
  entries: LocalUploadEntry[];
  doubleClickTimeMs?: number;
}

export function uploadPathSegments(path: string): { name: string; path: string }[] {
  if (!path) return [];
  const root = path.match(/^[a-z]:[\\/]/i)?.[0]
    ?? path.match(/^(?:\\\\|\/\/)[^\\/]+[\\/][^\\/]+/)?.[0]
    ?? (path.startsWith('/') ? '/' : '');
  if (!root) return [];
  const separator = root.includes('\\') ? '\\' : '/';
  const parts = path.slice(root.length).split(separator).filter(Boolean);
  const result = [{ name: root, path: root }];
  let current = root;
  for (const name of parts) {
    current = current.endsWith(separator) ? current + name : current + separator + name;
    result.push({ name, path: current });
  }
  return result;
}

export function formatUploadTime(value: number | null | undefined): string {
  if (value == null) return '—';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return '—';
  const pad = (number: number) => String(number).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

export function normalizeUploadSelection(entries: readonly LocalUploadEntry[]): LocalUploadEntry[] {
  const unique = new Map<string, LocalUploadEntry>();
  for (const entry of entries) {
    const key = uploadPathKey(entry.path);
    if (!unique.has(key)) unique.set(key, entry);
  }
  const directories = new Set([...unique].filter(([, entry]) => entry.kind === 'directory').map(([key]) => key));
  return [...unique].filter(([key]) => {
    for (let end = key.lastIndexOf('/'); end >= 0; end = key.lastIndexOf('/', end - 1)) {
      if (directories.has(key.slice(0, end) || '/')) return false;
      if (end === 0) break;
    }
    return true;
  }).map(([, entry]) => entry);
}

/** Listings return canonical absolute paths; fold case only for Windows paths. */
export function uploadPathKey(path: string): string {
  const windows = /^[a-z]:[\\/]/i.test(path) || path.startsWith('\\\\') || path.startsWith('//');
  const normalized = windows ? path.replaceAll('\\', '/').toLowerCase() : path;
  return normalized.replace(/\/+$/, '') || '/';
}
