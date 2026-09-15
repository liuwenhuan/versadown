import { useCallback, useMemo, useRef, useState } from 'react';
import { ConfirmResource, ParseDownloadResult, ScanDownloadResult } from '../lib/types';

export type DownloadDraftStatus = 'parsing' | 'ready' | 'access_code_required' | 'failed';

export interface DownloadDraftItem {
  id: string;
  input: string;
  kind?: string;
  status: DownloadDraftStatus;
  selected: boolean;
  result?: ParseDownloadResult;
  error?: string;
  accessCode: string;
  expanded: boolean;
  submitted: boolean;
  generation: number;
}

interface Options {
  scan(input: string): Promise<ScanDownloadResult[]>;
  prepare(input: string, accessCode?: string): Promise<ParseDownloadResult>;
  discard(reference: string): Promise<void>;
  concurrency?: number;
}

type WorkItem = { id: string; generation: number };

function preparedReference(item: DownloadDraftItem): string | undefined {
  if (item.result?.confirmation) return item.result.confirmation.taskId;
  if (item.result?.task) return item.result.task.id;
  return undefined;
}

export function selectedResourceIds(item: DownloadDraftItem): string[] {
  return item.result?.confirmation?.resources
    .filter(resource => resource.selected)
    .map(resource => resource.id) ?? [];
}

export function useDownloadDraftBatch({ scan, prepare, discard, concurrency = 4 }: Options) {
  const [items, setItems] = useState<DownloadDraftItem[]>([]);
  const itemsRef = useRef<DownloadDraftItem[]>([]);
  const queueRef = useRef<WorkItem[]>([]);
  const activeRef = useRef(0);
  const sessionRef = useRef(0);

  const replaceItems = useCallback((update: (current: DownloadDraftItem[]) => DownloadDraftItem[]) => {
    const next = update(itemsRef.current);
    itemsRef.current = next;
    setItems(next);
  }, []);

  const pumpRef = useRef<() => void>(() => {});
  pumpRef.current = () => {
    while (activeRef.current < Math.max(1, concurrency) && queueRef.current.length) {
      const work = queueRef.current.shift()!;
      const item = itemsRef.current.find(candidate => candidate.id === work.id && candidate.generation === work.generation);
      if (!item) continue;
      activeRef.current += 1;
      void prepare(item.input, item.accessCode.trim() || undefined)
        .then(result => {
          replaceItems(current => current.map(candidate => {
            if (candidate.id !== work.id || candidate.generation !== work.generation) return candidate;
            if (result.shareAccessCodeRequired) {
              return { ...candidate, result, status: 'access_code_required', error: result.shareAccessCodeRequired.message };
            }
            return { ...candidate, result, status: 'ready', error: undefined };
          }));
        })
        .catch(reason => {
          replaceItems(current => current.map(candidate => candidate.id === work.id && candidate.generation === work.generation
            ? { ...candidate, status: 'failed', error: reason instanceof Error ? reason.message : String(reason) }
            : candidate));
        })
        .finally(() => {
          activeRef.current -= 1;
          pumpRef.current();
        });
    }
  };

  const start = useCallback(async (input: string) => {
    const session = ++sessionRef.current;
    const scanned = await scan(input);
    const candidates: ScanDownloadResult[] = scanned.length
      ? scanned
      : [{ input, valid: false, message: '未发现受支持的有效下载链接' }];
    const next = candidates.map<DownloadDraftItem>((entry, index) => ({
      id: `${session}-${index}`,
      input: entry.input,
      kind: entry.valid ? entry.kind : undefined,
      status: entry.valid ? 'parsing' : 'failed',
      selected: entry.valid,
      error: entry.valid ? undefined : entry.message,
      accessCode: '',
      expanded: false,
      submitted: false,
      generation: 0,
    }));
    itemsRef.current = next;
    setItems(next);
    queueRef.current = next.filter(item => item.status === 'parsing').map(item => ({ id: item.id, generation: item.generation }));
    pumpRef.current();
  }, [scan]);

  const retry = useCallback((id: string) => {
    let work: WorkItem | undefined;
    replaceItems(current => current.map(item => {
      if (item.id !== id) return item;
      const generation = item.generation + 1;
      work = { id, generation };
      return { ...item, status: 'parsing', error: undefined, result: undefined, generation };
    }));
    if (work) {
      queueRef.current.push(work);
      pumpRef.current();
    }
  }, [replaceItems]);

  const remove = useCallback((id: string) => {
    const item = itemsRef.current.find(candidate => candidate.id === id);
    const reference = item && !item.submitted ? preparedReference(item) : undefined;
    replaceItems(current => current.filter(candidate => candidate.id !== id));
    if (reference) void discard(reference).catch(() => undefined);
  }, [discard, replaceItems]);

  const toggleItem = useCallback((id: string) => {
    replaceItems(current => current.map(item => item.id === id ? { ...item, selected: !item.selected } : item));
  }, [replaceItems]);

  const setAccessCode = useCallback((id: string, accessCode: string) => {
    replaceItems(current => current.map(item => item.id === id ? { ...item, accessCode } : item));
  }, [replaceItems]);

  const toggleExpanded = useCallback((id: string) => {
    replaceItems(current => current.map(item => item.id === id ? { ...item, expanded: !item.expanded } : item));
  }, [replaceItems]);

  const toggleResource = useCallback((id: string, resourceId: string) => {
    replaceItems(current => current.map(item => {
      if (item.id !== id || !item.result?.confirmation) return item;
      return {
        ...item,
        result: {
          confirmation: {
            ...item.result.confirmation,
            resources: item.result.confirmation.resources.map(resource => resource.id === resourceId
              ? { ...resource, selected: !resource.selected }
              : resource),
          },
        },
      };
    }));
  }, [replaceItems]);

  const updateResources = useCallback((id: string, resources: readonly ConfirmResource[]) => {
    replaceItems(current => current.map(item => {
      if (item.id !== id || !item.result?.confirmation) return item;
      return {
        ...item,
        result: {
          confirmation: {
            ...item.result.confirmation,
            resources: resources.map(resource => ({
              ...resource,
              selected: item.result?.confirmation?.resources.find(existing => existing.index === resource.index || existing.id === resource.id)?.selected ?? resource.selected,
            })),
          },
        },
      };
    }));
  }, [replaceItems]);

  const markSubmitted = useCallback((ids: string[]) => {
    const submitted = new Set(ids);
    replaceItems(current => current.map(item => submitted.has(item.id) ? { ...item, submitted: true } : item));
  }, [replaceItems]);

  const discardAll = useCallback(async () => {
    const references = itemsRef.current
      .filter(item => !item.submitted)
      .map(preparedReference)
      .filter((reference): reference is string => Boolean(reference));
    await Promise.allSettled(Array.from(new Set(references)).map(reference => discard(reference)));
  }, [discard]);

  const canSubmit = useMemo(() => {
    const selected = items.filter(item => item.selected);
    return selected.length > 0 && selected.every(item => item.status === 'ready');
  }, [items]);

  return { items, canSubmit, start, retry, remove, toggleItem, setAccessCode, toggleExpanded, toggleResource, updateResources, markSubmitted, discardAll };
}
