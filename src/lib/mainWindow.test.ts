import { expect, it } from 'vitest';
import { mainWindowIsMaximized, runMainWindowAction } from './mainWindow';

function operations() {
  const calls: string[] = [];
  return {
    calls,
    minimize: async () => { calls.push('minimize'); },
    toggleMaximize: async () => { calls.push('toggle-maximize'); },
    hide: async () => { calls.push('hide'); },
    isMaximized: async () => false
  };
}

it('routes minimize only to the native minimize operation', async () => {
  const windowOperations = operations();

  await runMainWindowAction('minimize', windowOperations);

  expect(windowOperations.calls).toEqual(['minimize']);
});

it('routes maximize toggling only to the native toggle operation', async () => {
  const windowOperations = operations();

  await runMainWindowAction('toggle-maximize', windowOperations);

  expect(windowOperations.calls).toEqual(['toggle-maximize']);
});

it('maps close to hide instead of close', async () => {
  const windowOperations = operations();

  await runMainWindowAction('hide', windowOperations);

  expect(windowOperations.calls).toEqual(['hide']);
});

it('returns the native maximized state without deriving it from an action', async () => {
  const windowOperations = operations();
  windowOperations.isMaximized = async () => true;

  await expect(mainWindowIsMaximized(windowOperations)).resolves.toBe(true);
});
