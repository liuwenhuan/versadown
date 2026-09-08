import { expect, it } from 'vitest';
import { managerViewFromLauncherPayload } from './managerNavigation';

it('accepts only known launcher navigation destinations', () => {
  expect(managerViewFromLauncherPayload('settings')).toBe('settings');
  expect(managerViewFromLauncherPayload('downloads')).toBe('downloads');
  expect(managerViewFromLauncherPayload('unknown')).toBeNull();
  expect(managerViewFromLauncherPayload(null)).toBeNull();
});
