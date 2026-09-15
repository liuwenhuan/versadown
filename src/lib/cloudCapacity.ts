import type { CloudAccountCapacity, CloudCapacityInvalidated, CloudProvider, CloudStorageCapacity } from './cloudTypes';

export interface CloudCapacityViewState {
  loading: boolean;
  value?: CloudAccountCapacity;
  refreshFailed: boolean;
}

type CapacityLoader = () => Promise<CloudAccountCapacity>;
type CapacityPublisher = (state: CloudCapacityViewState) => void;

function key(provider: CloudProvider, accountId: string) {
  return `${provider}:${accountId}`;
}

function formatBytes(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KB', 'MB', 'GB', 'TB', 'PB'];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(1)} ${units[unit]}`;
}

export function formatCloudCapacity(capacity: CloudStorageCapacity) {
  const used = formatBytes(capacity.usedBytes);
  return capacity.kind === 'limited'
    ? `已使用 ${used} / ${formatBytes(capacity.totalBytes)}`
    : `已使用 ${used} / 不限容量`;
}

export class CloudCapacityController {
  private readonly states = new Map<string, CloudCapacityViewState>();
  private readonly generations = new Map<string, number>();
  private readonly timers = new Map<string, ReturnType<typeof setTimeout>>();

  read(provider: CloudProvider, accountId: string) {
    return this.states.get(key(provider, accountId));
  }

  async refresh(provider: CloudProvider, accountId: string, load: CapacityLoader, publish: CapacityPublisher) {
    const entryKey = key(provider, accountId);
    const previous = this.states.get(entryKey);
    const loading = { loading: true, ...(previous?.value ? { value: previous.value } : {}), refreshFailed: false };
    this.states.set(entryKey, loading);
    publish(loading);
    const generation = (this.generations.get(entryKey) ?? 0) + 1;
    this.generations.set(entryKey, generation);
    try {
      const value = await load();
      if (this.generations.get(entryKey) !== generation) return;
      if (value.provider !== provider || value.accountId !== accountId) throw new Error('capacity response account mismatch');
      const next = { loading: false, value, refreshFailed: false };
      this.states.set(entryKey, next);
      publish(next);
    } catch {
      if (this.generations.get(entryKey) !== generation) return;
      const next = { loading: false, ...(previous?.value ? { value: previous.value } : {}), refreshFailed: true };
      this.states.set(entryKey, next);
      publish(next);
    }
  }

  invalidate(event: CloudCapacityInvalidated, provider: CloudProvider, accountId: string, load: CapacityLoader, publish: CapacityPublisher) {
    if (event.provider !== provider || event.accountId !== accountId) return;
    const entryKey = key(provider, accountId);
    const pending = this.timers.get(entryKey);
    if (pending) clearTimeout(pending);
    this.timers.set(entryKey, setTimeout(() => {
      this.timers.delete(entryKey);
      void this.refresh(provider, accountId, load, publish);
    }, 400));
  }

  clear(provider: CloudProvider, accountId: string) {
    const entryKey = key(provider, accountId);
    const pending = this.timers.get(entryKey);
    if (pending) clearTimeout(pending);
    this.timers.delete(entryKey);
    this.states.delete(entryKey);
    this.generations.set(entryKey, (this.generations.get(entryKey) ?? 0) + 1);
  }

  dispose() {
    for (const pending of this.timers.values()) clearTimeout(pending);
    this.timers.clear();
  }
}
