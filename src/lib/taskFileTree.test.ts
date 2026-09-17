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

  it('uses persisted files when the queue supplies an empty child collection', () => {
    const tree = buildTaskFileTree({
      name: 'release',
      children: [],
      files: [{ index: 1, path: 'release/_crack/setup.exe', length: 8, completed_length: 4, selected: true }],
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

  it('does not turn file children with an empty children array into folders', () => {
    const tree = buildTaskFileTree({
      name: '倩种1987',
      children: [
        {
          id: 'video', name: '씨받이[倩种].The.Surrogate.Woman.1986.web-1080P.mp4', kind: 'file', status: 'completed', progress: 100,
          total_length: 0, completed_length: 0, children: [],
        },
        {
          id: 'subtitles', name: '字幕', kind: 'folder', status: 'completed', progress: 100,
          children: [{ id: 'subtitle', name: '简体中字.srt', kind: 'file', status: 'completed', progress: 100, total_length: 0, completed_length: 0, children: [] }],
        },
      ],
    });

    expect(tree).toEqual([
      expect.objectContaining({ kind: 'file', name: '씨받이[倩种].The.Surrogate.Woman.1986.web-1080P.mp4' }),
      expect.objectContaining({ kind: 'folder', name: '字幕', count: 1 }),
    ]);
    expect((tree[1] as Extract<typeof tree[number], { kind: 'folder' }>).children).toEqual([
      expect.objectContaining({ kind: 'file', name: '简体中字.srt' }),
    ]);
  });
});
