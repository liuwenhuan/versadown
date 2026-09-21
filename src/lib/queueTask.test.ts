import { describe, expect, it } from 'vitest';
import type { QueueTaskInput } from './types';
import { isPrivateTask, normalizeQueueTask, normalizeQueueTasks } from './queueTask';

function legacyTask(overrides: Partial<QueueTaskInput> = {}): QueueTaskInput {
  return {
    id: 'task-1',
    name: 'archive.zip',
    kind: 'file',
    status: 'downloading',
    progress: 25,
    ...overrides,
  };
}

describe('queue task normalization', () => {
  it('converts legacy wire aliases into the canonical task fields', () => {
    const normalized = normalizeQueueTask(legacyTask({
      speedBytes: 524_288,
      completedAt: 1_789_095_720,
      isPrivate: true,
    }));

    expect(normalized).toMatchObject({
      download_speed: 524_288,
      completed_at: 1_789_095_720,
      is_private: true,
    });
    expect(normalized).not.toHaveProperty('speedBytes');
    expect(normalized).not.toHaveProperty('completedAt');
    expect(normalized).not.toHaveProperty('isPrivate');
  });

  it('normalizes aliases in every child task', () => {
    const [normalized] = normalizeQueueTasks([legacyTask({
      children: [legacyTask({
        id: 'child-1',
        speedBytes: 131_072,
        completedAt: 1_789_095_720,
        isPrivate: true,
      })],
    })]);

    expect(normalized.children).toEqual([expect.objectContaining({
      id: 'child-1',
      download_speed: 131_072,
      completed_at: 1_789_095_720,
      is_private: true,
    })]);
    expect(normalized.children?.[0]).not.toHaveProperty('speedBytes');
  });

  it('uses the canonical privacy field for the shared privacy decision', () => {
    expect(isPrivateTask(normalizeQueueTask(legacyTask({ isPrivate: true })))).toBe(true);
    expect(isPrivateTask(normalizeQueueTask(legacyTask({ is_private: false })))).toBe(false);
  });
});
