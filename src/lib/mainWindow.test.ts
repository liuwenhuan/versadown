import { readFileSync } from 'node:fs';
import { beforeEach, expect, it, vi } from 'vitest';

const native = vi.hoisted(() => ({
  setMinSize: vi.fn().mockResolvedValue(undefined),
  setSize: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('@tauri-apps/api/window', () => ({
  PhysicalSize: class PhysicalSize {
    constructor(public width: number, public height: number) {}
  },
}));

import { applyMainWindowDpiSizing } from './mainWindow';

beforeEach(() => {
  native.setMinSize.mockReset().mockResolvedValue(undefined);
  native.setSize.mockReset().mockResolvedValue(undefined);
});

it('uses the WebView pixel ratio when Linux reports an unscaled native factor', async () => {
  const original = Object.getOwnPropertyDescriptor(window, 'devicePixelRatio');
  Object.defineProperty(window, 'devicePixelRatio', { configurable: true, value: 1.5 });

  try {
    await applyMainWindowDpiSizing(native, 1);

    expect(native.setMinSize).toHaveBeenCalledWith({ width: 1440, height: 960 });
    expect(native.setSize).toHaveBeenCalledWith({ width: 1440, height: 960 });
  } finally {
    if (original) Object.defineProperty(window, 'devicePixelRatio', original);
    else Reflect.deleteProperty(window, 'devicePixelRatio');
  }
});

it('authorizes the native minimum-size operation used by DPI sizing', () => {
  const capability = JSON.parse(readFileSync('src-tauri/capabilities/default.json', 'utf8')) as { permissions: string[] };

  expect(capability.permissions).toContain('core:window:allow-set-min-size');
});

it('passes a frontend signature to the Linux quick build so Tauri re-embeds changed assets', () => {
  const packageJson = JSON.parse(readFileSync('package.json', 'utf8')) as { scripts: Record<string, string> };

  expect(packageJson.scripts['build:linux:quick']).toContain('VERSADOWN_FRONTEND_SIGNATURE');
});
