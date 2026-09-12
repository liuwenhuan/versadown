const resizeIdleDelayMs = 120;

/**
 * Keeps expensive visual effects out of the compositor while the native
 * window is being resized, then restores them once the pointer is released.
 */
export function installWindowResizePerformanceMode(): () => void {
  let idleTimer: number | undefined;
  const root = document.documentElement;

  const finishResizing = () => {
    idleTimer = undefined;
    root.classList.remove('window-resizing');
  };

  const markResizing = () => {
    root.classList.add('window-resizing');
    if (idleTimer !== undefined) window.clearTimeout(idleTimer);
    idleTimer = window.setTimeout(finishResizing, resizeIdleDelayMs);
  };

  window.addEventListener('resize', markResizing, { passive: true });
  return () => {
    window.removeEventListener('resize', markResizing);
    if (idleTimer !== undefined) window.clearTimeout(idleTimer);
    root.classList.remove('window-resizing');
  };
}
