const resizeIdleDelayMs = 120;

/**
 * Keeps expensive visual effects out of the compositor while the native
 * window is being resized, then restores them once the pointer is released.
 */
export function installWindowResizePerformanceMode(
  subscribe?: (listener: (active: boolean) => void) => () => void,
): () => void {
  let idleTimer: number | undefined;
  let nativeInteraction = false;
  const root = document.documentElement;

  const finishResizing = () => {
    idleTimer = undefined;
    root.classList.remove('window-resizing');
  };

  const markResizing = () => {
    root.classList.add('window-resizing');
    if (idleTimer !== undefined) window.clearTimeout(idleTimer);
    idleTimer = nativeInteraction ? undefined : window.setTimeout(finishResizing, resizeIdleDelayMs);
  };

  const stop = subscribe?.((active) => {
    nativeInteraction = active;
    if (idleTimer !== undefined) window.clearTimeout(idleTimer);
    idleTimer = undefined;
    // Movement alone does not change layout, so don't invalidate every style
    // at mouse-down. The first resize applies the class; native exit removes it.
    if (!active) finishResizing();
  });

  window.addEventListener('resize', markResizing, { passive: true });
  return () => {
    stop?.();
    window.removeEventListener('resize', markResizing);
    if (idleTimer !== undefined) window.clearTimeout(idleTimer);
    root.classList.remove('window-resizing');
  };
}
