export type ManagerView = 'downloads' | 'cloud' | 'settings';

export function managerViewFromLauncherPayload(payload: unknown): ManagerView | null {
  return payload === 'downloads' || payload === 'cloud' || payload === 'settings' ? payload : null;
}
