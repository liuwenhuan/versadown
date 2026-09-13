import { afterEach, describe, expect, it, vi } from 'vitest';
import { installWindowResizePerformanceMode } from './windowResizePerformance';

describe('window resize performance mode', () => {
  afterEach(() => {
    vi.useRealTimers();
    document.documentElement.classList.remove('window-resizing');
  });

  it('keeps expensive visual effects paused through a continuous resize and restores them after it becomes idle', () => {
    vi.useFakeTimers();
    const dispose = installWindowResizePerformanceMode();

    window.dispatchEvent(new Event('resize'));
    expect(document.documentElement).toHaveClass('window-resizing');

    vi.advanceTimersByTime(100);
    window.dispatchEvent(new Event('resize'));
    vi.advanceTimersByTime(119);
    expect(document.documentElement).toHaveClass('window-resizing');

    vi.advanceTimersByTime(1);
    expect(document.documentElement).not.toHaveClass('window-resizing');

    dispose();
  });

  it('does not restore effects while the mouse is held stationary in a native resize', () => {
    vi.useFakeTimers();
    let setActive!: (active: boolean) => void;
    const dispose = installWindowResizePerformanceMode((handler) => {
      setActive = handler;
      return () => undefined;
    });
    setActive(true);
    window.dispatchEvent(new Event('resize'));
    vi.advanceTimersByTime(1500);
    expect(document.documentElement).toHaveClass('window-resizing');
    setActive(false);
    expect(document.documentElement).not.toHaveClass('window-resizing');
    dispose();
  });
});
