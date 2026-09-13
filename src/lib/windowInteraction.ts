/** WebView2's native caption path, not Tauri's JavaScript drag-region handler. */
export function supportsNativeWindowDrag(hostEnabled: boolean): boolean {
  return hostEnabled && navigator.platform.startsWith('Win')
    && typeof CSS !== 'undefined' && CSS.supports('app-region', 'drag');
}

export type WindowInteractionSnapshot = { active: boolean; nativeDrag: boolean };

/** Subscribe first; never let a stale startup snapshot overwrite newer edges. */
export function connectWindowInteraction(
  interaction: ReturnType<typeof createWindowInteraction>,
  listen: (handler: (active: boolean) => void) => Promise<() => void>,
  snapshot: () => Promise<WindowInteractionSnapshot>,
  onNativeDrag: (enabled: boolean) => void,
): () => void {
  let disposed = false;
  let stop: (() => void) | undefined;
  let eventEpoch = 0;
  void listen((active) => {
    eventEpoch += 1;
    if (!disposed) interaction.setActive(active);
  }).then(async (unlisten) => {
    if (disposed) { unlisten(); return; }
    stop = unlisten;
    const epoch = eventEpoch;
    const current = await snapshot();
    if (disposed) return;
    onNativeDrag(current.nativeDrag);
    if (epoch === eventEpoch) interaction.setActive(current.active);
  }).catch(() => undefined); // Keep the drag and resize fallbacks on older hosts.
  return () => { disposed = true; stop?.(); };
}

/** Only optional snapshot work is queued. Download events are never suspended. */
export function createWindowInteraction() {
  let active = false;
  let disposed = false;
  let timer: number | undefined;
  const pending = new Map<string, () => void>();
  const listeners = new Set<(active: boolean) => void>();
  const cancelTimer = () => {
    if (timer !== undefined) window.clearTimeout(timer);
    timer = undefined;
  };
  const flushAfterSettling = () => {
    cancelTimer();
    if (active || disposed || pending.size === 0) return;
    timer = window.setTimeout(() => {
      timer = undefined;
      const work = [...pending.values()];
      pending.clear();
      for (const run of work) run();
    }, 160);
  };
  return {
    setActive(value: boolean) {
      if (disposed || active === value) return;
      active = value;
      for (const listener of listeners) listener(active);
      flushAfterSettling();
    },
    subscribe(listener: (active: boolean) => void) {
      listeners.add(listener);
      listener(active);
      return () => { listeners.delete(listener); };
    },
    schedule(key: string, run: () => void) {
      if (disposed) return;
      pending.set(key, run);
      flushAfterSettling();
    },
    dispose() {
      disposed = true;
      cancelTimer();
      pending.clear();
      listeners.clear();
    },
  };
}
