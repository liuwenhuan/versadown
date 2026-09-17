import { describe, expect, it } from 'vitest';
import { buildTaskFileTree } from './taskFileTree';

describe('buildTaskFileTree', () => {
  it('uses child tasks before files and preserves folder aggregates', () => {
    const tree = buildTaskFileTree({
      name: 'release',
      files: [{ index: 1, path: 'release/ignored.bin', length: 1, completed_length: 0, selected: true }],
      children: [{
        id: 'folder', name: '_crack', kind: 'folder', status: 'downloading', progress: 0,
        children: [{ id: 'file', name: 'setup.exe', kind: 'file', status: 'downloading', progress: 50, total_length: 8, completed_length: 4 }],
      }],
    });

    expect(tree).toEqual([expect.objectContaining({ kind: 'folder', name: '_crack', bytes: 8, count: 1 })]);
  });

  it('normalizes slash styles and omits the task-root path segment', () => {
    const tree = buildTaskFileTree({
      name: 'release',
      files: [
        { index: 1, path: 'release\\_crack\\Win64\\patch.dll', length: 8, completed_length: 4, selected: true },
        { index: 2, path: 'release/readme.txt', length: 2, completed_length: 2, selected: true },
      ],
    });

    expect(tree.map(node => node.name)).toEqual(['_crack', 'readme.txt']);
    expect(tree[0]).toEqual(expect.objectContaining({ kind: 'folder', bytes: 8, count: 1 }));
  });
});
