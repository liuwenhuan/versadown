import { expect, it } from 'vitest';
import * as launcherWindow from './launcherWindow';

it('anchors the complete native window inside the monitor work area even when Windows enforces a wider size', () => {
  const position = (launcherWindow as unknown as {
    launcherCornerPosition(workArea: { x: number; y: number; width: number; height: number }, windowSize: { width: number; height: number }, scaleFactor: number): { x: number; y: number };
  }).launcherCornerPosition({ x: 100, y: 50, width: 1920, height: 1080 }, { width: 214, height: 86 }, 1.75);

  expect(position).toEqual({ x: 1764, y: 1002 });
});
