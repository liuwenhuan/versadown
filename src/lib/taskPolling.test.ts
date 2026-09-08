import { afterEach, describe, expect, it, vi } from 'vitest';
import { startTaskPolling } from './taskPolling';
import { QueueTask } from './types';

describe('task polling', () => {
  afterEach(() => vi.useRealTimers());

  it('refreshes persisted task state once per second and can be stopped', async () => {
    vi.useFakeTimers();
    let progress = 0;
    const updates: QueueTask[][] = [];
    const load = async (): Promise<QueueTask[]> => [{ id: '1', name: 'game.zip', kind: 'file', status: 'downloading', progress: ++progress }];
    const stop = startTaskPolling(load, (tasks) => updates.push(tasks));

    await vi.advanceTimersByTimeAsync(2100);
    expect(updates.map((tasks) => tasks[0].progress)).toEqual([1, 2, 3]);

    stop();
    await vi.advanceTimersByTimeAsync(1000);
    expect(updates).toHaveLength(3);
  });
});
