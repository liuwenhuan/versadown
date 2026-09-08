import { expect, it } from 'vitest';
import * as launcherWindow from './launcherWindow';

it('anchors the compact launcher inside the monitor work area', () => {
  const position = (launcherWindow as unknown as {
    launcherCornerPosition(workArea: { x: number; y: number; width: number; height: number }, scaleFactor: number): { x: number; y: number };
  }).launcherCornerPosition({ x: 100, y: 50, width: 1920, height: 1080 }, 1.5);

  expect(position).toEqual({ x: 1912, y: 1022 });
});
