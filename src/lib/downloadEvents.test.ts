import { describe, expect, it } from 'vitest';
import { formatBytes, formatEta, formatSpeed, upsertTask } from './downloadEvents';
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

  it('does not insert an unrelated snapshot into a parent task children', () => {
    const folder = task({ id: 'folder', children: [task({ id: 'child' })] });
    const result = upsertTask([folder], task({ id: 'task-2' }));
    expect(result.find((item) => item.id === 'folder')?.children).toEqual([task({ id: 'child' })]);
    expect(result).toHaveLength(2);
  });
});

describe('transfer formatters', () => {
  it('formats bytes, speed, and ETA from aria2 numeric values', () => {
    expect(formatBytes(1_572_864)).toBe('1.5 MB');
    expect(formatSpeed(524_288)).toBe('512 KB/s');
    expect(formatEta(65)).toBe('1 分 5 秒');
  });

  it('does not invent a percentage when aria2 reports an unknown total', () => {
    const unknownTotal = task({ total_length: undefined, progress: 0, completed_length: 2_048 });
    expect(formatBytes(unknownTotal.completed_length)).toBe('2 KB');
    expect(unknownTotal.total_length).toBeUndefined();
  });
});
