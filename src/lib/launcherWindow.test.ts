import { expect, it } from 'vitest';
import * as launcherWindow from './launcherWindow';

it('anchors the complete native window inside the monitor work area even when Windows enforces a wider size', () => {
  const position = (launcherWindow as unknown as {
    launcherCornerPosition(workArea: { x: number; y: number; width: number; height: number }, windowSize: { width: number; height: number }, scaleFactor: number): { x: number; y: number };
  }).launcherCornerPosition({ x: 100, y: 50, width: 1920, height: 1080 }, { width: 214, height: 86 }, 1.75);

  expect(position).toEqual({ x: 1764, y: 1002 });
});

it('resizes around the launcher bottom-right corner and clamps it to the work area', () => {
  const resized = (launcherWindow as unknown as {
    launcherResizedPosition(
      position: { x: number; y: number },
      currentSize: { width: number; height: number },
      nextSize: { width: number; height: number },
      workArea: { x: number; y: number; width: number; height: number }
    ): { x: number; y: number };
  }).launcherResizedPosition(
    { x: 1000, y: 800 },
    { width: 236, height: 98 },
    { width: 822, height: 525 },
    { x: 0, y: 0, width: 2560, height: 1516 }
  );

  expect(resized).toEqual({ x: 414, y: 373 });
});
