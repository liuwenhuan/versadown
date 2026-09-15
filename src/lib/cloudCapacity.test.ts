import { describe, expect, it, vi } from 'vitest';
import { CloudCapacityController, formatCloudCapacity } from './cloudCapacity';

const limited = {
  provider: 'quark' as const,
  accountId: 'account-1',
  capacity: { kind: 'limited' as const, totalBytes: 100 * 1024 ** 3, usedBytes: 12 * 1024 ** 3 },
  updatedAt: 1
};

describe('cloud capacity controller', () => {
  it('formats limited and unlimited capacity', () => {
    expect(formatCloudCapacity(limited.capacity)).toBe('已使用 12.0 GB / 100.0 GB');
    expect(formatCloudCapacity({ kind: 'unlimited', usedBytes: 3 * 1024 ** 3 })).toBe('已使用 3.0 GB / 不限容量');
  });

  it('retains the last successful value when a refresh fails', async () => {
    const controller = new CloudCapacityController();
    const publish = vi.fn();
    await controller.refresh('quark', 'account-1', vi.fn().mockResolvedValue(limited), publish);
    await controller.refresh('quark', 'account-1', vi.fn().mockRejectedValue(new Error('offline')), publish);
    expect(controller.read('quark', 'account-1')).toEqual({ loading: false, value: limited, refreshFailed: true });
  });

  it('discards mismatched responses and clears replaced accounts', async () => {
    const controller = new CloudCapacityController();
    await controller.refresh('quark', 'account-1', vi.fn().mockResolvedValue({ ...limited, accountId: 'other' }), vi.fn());
    expect(controller.read('quark', 'account-1')).toEqual({ loading: false, refreshFailed: true });
    await controller.refresh('quark', 'account-1', vi.fn().mockResolvedValue(limited), vi.fn());
    controller.clear('quark', 'account-1');
    expect(controller.read('quark', 'account-1')).toBeUndefined();
  });

  it('coalesces matching invalidations with a 400 ms trailing debounce', async () => {
    vi.useFakeTimers();
    try {
      const controller = new CloudCapacityController();
      const load = vi.fn().mockResolvedValue(limited);
      const publish = vi.fn();
      controller.invalidate({ provider: 'quark', accountId: 'account-1', reason: 'upload' }, 'quark', 'account-1', load, publish);
      await vi.advanceTimersByTimeAsync(300);
      controller.invalidate({ provider: 'quark', accountId: 'account-1', reason: 'delete' }, 'quark', 'account-1', load, publish);
      await vi.advanceTimersByTimeAsync(399);
      expect(load).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(1);
      expect(load).toHaveBeenCalledTimes(1);
      controller.invalidate({ provider: 'baidu', accountId: 'account-1', reason: 'upload' }, 'quark', 'account-1', load, publish);
      await vi.advanceTimersByTimeAsync(400);
      expect(load).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });
});
