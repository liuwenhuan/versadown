import { QueueTask } from './types';

export function startTaskPolling(
  load: () => Promise<QueueTask[]>,
  onUpdate: (tasks: QueueTask[]) => void,
  intervalMs = 1000
): () => void {
  let stopped = false;
  let loading = false;

  async function refresh() {
    if (stopped || loading) return;
    loading = true;
    try {
      const tasks = await load();
      if (!stopped) onUpdate(tasks);
    } catch {
      // A temporary backend failure should not stop later refreshes.
    } finally {
      loading = false;
    }
  }

  void refresh();
  const timer = window.setInterval(() => void refresh(), intervalMs);
  return () => { stopped = true; window.clearInterval(timer); };
}
