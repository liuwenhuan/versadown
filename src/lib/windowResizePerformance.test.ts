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
});
