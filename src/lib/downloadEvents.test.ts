import { describe, expect, it } from 'vitest';
import { formatBytes, formatCompactSpeed, formatEta, formatSpeed, upsertTask, mergeInitialTasks, mergeTaskSnapshot, reconcileTaskSnapshot } from './downloadEvents';
import type { QueueTask } from './types';

function task(overrides: Partial<QueueTask> = {}): QueueTask {
  return {
    id: 'task-1', name: 'archive.iso', kind: 'file', status: 'downloading', progress: 10,
    completed_length: 1_048_576, total_length: 4_194_304, download_speed: 524_288,
    connections: 3, ...overrides
  };
}

describe('upsertTask', () => {
  it('replaces a task snapshot without duplication', () => {
    expect(upsertTask([task({ progress: 10 })], task({ progress: 40 })))
      .toEqual([task({ progress: 40 })]);
  });

  it('adds a new snapshot while retaining existing tasks', () => {
    expect(upsertTask([task()], task({ id: 'task-2', name: 'other.zip' }))).toHaveLength(2);
  });

  it('keeps known torrent files when a live update temporarily omits them', () => {
    const files = [{
      index: 1,
      path: 'D:\\Downloads\\movie.mkv',
      length: 500 * 1024 * 1024,
      completed_length: 42 * 1024 * 1024,
      selected: true,
    }];
    const current = task({ kind: 'magnet', files, progress: 8 });
    const snapshot = task({ kind: 'magnet', files: [], progress: 9, download_speed: 410_500 });

    expect(upsertTask([current], snapshot)).toEqual([{ ...snapshot, files }]);
  });

  it('does not insert an unrelated snapshot into a parent task children', () => {
    const folder = task({ id: 'folder', children: [task({ id: 'child' })] });
    const result = upsertTask([folder], task({ id: 'task-2' }));
    expect(result.find((item) => item.id === 'folder')?.children).toEqual([task({ id: 'child' })]);
    expect(result).toHaveLength(2);
  });

  it('absorbs a transient standalone member when its authoritative directory arrives', () => {
    const child = task({ id: 'member', progress: 60 });
    const root = task({ id: 'root', kind: 'folder', children: [task({ id: 'subdir', kind: 'folder', children: [child] })] });
    const unrelated = task({ id: 'unrelated' });
    const result = upsertTask([task({ id: 'member', progress: 0 }), unrelated], root);
    expect(result.map((item) => item.id)).toEqual(['root', 'unrelated']);
    expect(result[0].children![0].children).toEqual([child]);
    expect(upsertTask(result, { ...child, progress: 80 }).map((item) => item.id)).toEqual(['root', 'unrelated']);
  });

  it('removes stale top-level members when an existing directory switches from virtual to linked IDs', () => {
    const before = task({ id: 'root', kind: 'folder', children: [task({ id: 'virtual-member' })] });
    const after = { ...before, children: [task({ id: 'real-member' })] };
    const result = upsertTask([task({ id: 'real-member' }), before], after);
    expect(result).toEqual([after]);
  });

  it('preserves grouping for initial and refresh snapshots in either arrival order', () => {
    const child = task({ id: 'member' });
    const root = task({ id: 'root', kind: 'folder', children: [child] });
    expect(mergeInitialTasks([child], [root])).toEqual([root]);
    expect(mergeTaskSnapshot([child], [root])).toEqual([root]);
    expect(mergeTaskSnapshot([], [child, root])).toEqual([root]);
    expect(mergeTaskSnapshot([], [root, child])).toEqual([root]);
  });

  it.each(['downloading', 'paused', 'completed', 'failed', 'recycled'] as const)(
    'keeps members grouped in the %s state without hiding independent same-name files', (status) => {
      const child = task({ id: 'member', status });
      const independent = { ...child, id: 'independent' };
      const root = task({ id: 'root', kind: 'folder', status, children: [child] });
      expect(upsertTask([child, independent], root)).toEqual([root, independent]);
    }
  );

  it('does not overwrite a newer directory with the initial list', () => {
    const root = task({ id: 'root', kind: 'folder', children: [task({ id: 'member', progress: 90 })] });
    expect(mergeInitialTasks([root], [{ ...root, children: [task({ id: 'member', progress: 10 })] }]))
      .toEqual([root]);
  });

  it('moves a protected live member into a stale directory without losing its newer progress', () => {
    const child = task({ id: 'member', progress: 90 });
    const root = task({ id: 'root', kind: 'folder', children: [task({ id: 'subdir', children: [{ ...child, progress: 10 }] })] });
    const expected = { ...root, children: [{ ...root.children![0], children: [child] }] };
    expect(mergeInitialTasks([child], [root])).toEqual([expected]);
    expect(mergeTaskSnapshot([child], [root], new Set([child.id]))).toEqual([expected]);
  });
});

describe('reconcileTaskSnapshot', () => {
  it('removes an active task that no longer exists in the durable active snapshot', () => {
    const stale = task({ id: 'discarded-magnet', kind: 'magnet', status: 'awaiting_user' });

    expect(reconcileTaskSnapshot([stale], [], 'active')).toEqual([]);
  });

  it('keeps terminal tasks while reconciling the active scope', () => {
    const completed = task({ id: 'completed-task', status: 'completed', progress: 100 });

    expect(reconcileTaskSnapshot([completed], [], 'active')).toEqual([completed]);
  });

  it('keeps a task updated after the active snapshot request began', () => {
    const live = task({ id: 'live-task', status: 'awaiting_user' });

    expect(reconcileTaskSnapshot([live], [], 'active', new Set([live.id]))).toEqual([live]);
  });
});

describe('transfer formatters', () => {
  it('formats bytes, speed, and ETA from aria2 numeric values', () => {
    expect(formatBytes(1_572_864)).toBe('1.5 MB');
    expect(formatSpeed(524_288)).toBe('512 KB/s');
    expect(formatEta(65)).toBe('1 分 5 秒');
  });

  it('formats a speed value compactly enough for the 48px launcher', () => {
    expect(formatCompactSpeed(870_400)).toBe('850K');
    expect(formatCompactSpeed(1_258_291)).toBe('1.2M');
    expect(formatCompactSpeed(1024 ** 3)).toBe('1.0G');
    expect(formatCompactSpeed(1024 ** 4)).toBe('1.0T');
  });

  it('does not invent a percentage when aria2 reports an unknown total', () => {
    const unknownTotal = task({ total_length: undefined, progress: 0, completed_length: 2_048 });
    expect(formatBytes(unknownTotal.completed_length)).toBe('2 KB');
    expect(unknownTotal.total_length).toBeUndefined();
  });
});
