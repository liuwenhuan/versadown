import type { DownloadFile, QueueTask } from './types';

export type TaskFileTreeNode =
  | { readonly kind: 'file'; readonly key: string; readonly name: string; readonly file: DownloadFile; readonly bytes: number; readonly count: 1 }
  | { readonly kind: 'folder'; readonly key: string; readonly name: string; readonly children: readonly TaskFileTreeNode[]; readonly bytes: number; readonly count: number };

type TaskFileTreeInput = Pick<QueueTask, 'name' | 'children' | 'files'>;

function pathParts(path: string): string[] {
  const parts = path.trim().replace(/\\/g, '/').replace(/\/{2,}/g, '/').split('/').filter(Boolean);
  return /^[a-z]:$/i.test(parts[0] ?? '') ? parts.slice(1) : parts;
}

function rootPrefixLength(files: readonly DownloadFile[], rootName: string): number {
  const paths = files.map(file => pathParts(file.path));
  const rootPositions = paths.map(parts => parts.findIndex(part => part === rootName));
  return rootPositions[0] >= 0 && rootPositions.every(position => position === rootPositions[0]) ? rootPositions[0] + 1 : 0;
}

function fromFiles(files: readonly DownloadFile[], rootName: string): readonly TaskFileTreeNode[] {
  const prefixLength = rootPrefixLength(files, rootName);
  const entries = files.map(file => {
    const original = pathParts(file.path);
    const parts = original.slice(prefixLength);
    return { file, parts: parts.length ? parts : [original.at(-1) ?? file.path] };
  });
  const build = (current: readonly typeof entries[number][], parentKey: string): TaskFileTreeNode[] => {
    const directories = new Map<string, typeof entries>();
    const leaves: TaskFileTreeNode[] = [];
    current.forEach(entry => {
      if (entry.parts.length <= 1) {
        leaves.push({ kind: 'file', key: `${parentKey}/file:${entry.file.index}`, name: entry.parts[0], file: entry.file, bytes: entry.file.length, count: 1 });
        return;
      }
      const [directory, ...rest] = entry.parts;
      const children = directories.get(directory) ?? [];
      children.push({ file: entry.file, parts: rest });
      directories.set(directory, children);
    });
    return [
      ...Array.from(directories, ([name, children]) => {
        const key = `${parentKey}/${name}`;
        const descendants = build(children, key);
        return { kind: 'folder' as const, key, name, children: descendants, bytes: descendants.reduce((total, node) => total + node.bytes, 0), count: descendants.reduce((total, node) => total + node.count, 0) };
      }),
      ...leaves,
    ];
  };
  return build(entries, 'root');
}

function fromChildren(children: readonly QueueTask[], parentKey: string): readonly TaskFileTreeNode[] {
  return children.map((child, position) => {
    const key = `${parentKey}/${child.id || position}`;
    const nested = child.children;
    if (child.kind === 'folder' || Boolean(nested?.length)) {
      const descendants = fromChildren(nested ?? [], key);
      return { kind: 'folder' as const, key, name: child.name, children: descendants, bytes: descendants.reduce<number>((total, node) => total + node.bytes, 0), count: descendants.reduce<number>((total, node) => total + node.count, 0) };
    }
    const file: DownloadFile = {
      index: child.file_index ?? position,
      path: child.name,
      length: child.total_length ?? 0,
      completed_length: child.completed_length ?? 0,
      selected: child.selected ?? true,
    };
    return { kind: 'file' as const, key, name: child.name, file, bytes: file.length, count: 1 };
  });
}

export function buildTaskFileTree(task: TaskFileTreeInput): readonly TaskFileTreeNode[] {
  return task.children?.length ? fromChildren(task.children, 'root') : fromFiles(task.files ?? [], task.name);
}
