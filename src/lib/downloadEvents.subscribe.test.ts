import { expect, it, vi } from 'vitest';

const state = vi.hoisted(() => ({ listen: vi.fn(), stopTask: vi.fn() }));

vi.mock('@tauri-apps/api/event', () => ({ listen: state.listen }));

import { subscribeToDownloadEvents } from './downloadEvents';

it('releases the task listener if subscribing to global stats fails', async () => {
  state.listen
    .mockResolvedValueOnce(state.stopTask)
    .mockRejectedValueOnce(new Error('global listener unavailable'));

  await expect(subscribeToDownloadEvents({ onTask: () => undefined, onGlobalStat: () => undefined }))
    .rejects.toThrow('global listener unavailable');

  expect(state.stopTask).toHaveBeenCalledOnce();
});
