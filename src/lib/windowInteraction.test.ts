import { afterEach, expect, it, vi } from 'vitest';
import { connectWindowInteraction, createWindowInteraction, supportsNativeWindowDrag } from './windowInteraction';

afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

it('uses native dragging only on Windows with app-region support', () => {
  vi.spyOn(navigator, 'platform', 'get').mockReturnValue('Win32');
  vi.stubGlobal('CSS', { supports: (name: string, value: string) => name === 'app-region' && value === 'drag' });
  expect(supportsNativeWindowDrag(true)).toBe(true);
  expect(supportsNativeWindowDrag(false)).toBe(false);
  vi.spyOn(navigator, 'platform', 'get').mockReturnValue('MacIntel');
  expect(supportsNativeWindowDrag(true)).toBe(false);
  vi.spyOn(navigator, 'platform', 'get').mockReturnValue('Win32');
  vi.stubGlobal('CSS', { supports: () => false });
  expect(supportsNativeWindowDrag(true)).toBe(false);
});

it('synchronizes an interaction that began before the native listener was ready', async () => {
  vi.useFakeTimers();
  const interaction = createWindowInteraction();
  const applied: string[] = [];
  let onNative!: (active: boolean) => void;
  const stop = connectWindowInteraction(interaction, async (handler) => {
    onNative = handler; return () => undefined;
  }, async () => ({ active: true, nativeDrag: true }), () => undefined);
  await vi.advanceTimersByTimeAsync(0);
  interaction.schedule('focus', () => { applied.push('focus'); });
  await vi.advanceTimersByTimeAsync(500);
  expect(applied).toEqual([]);
  onNative(false);
  await vi.advanceTimersByTimeAsync(200);
  expect(applied).toEqual(['focus']);
  stop(); interaction.dispose();
});

it('does not overwrite a newer native exit event with a stale startup snapshot', async () => {
  vi.useFakeTimers();
  const interaction = createWindowInteraction();
  let onNative!: (active: boolean) => void;
  let resolveSnapshot!: (value: {active: boolean; nativeDrag: boolean}) => void;
  const snapshot = new Promise<{active: boolean; nativeDrag: boolean}>((resolve) => { resolveSnapshot = resolve; });
  const stop = connectWindowInteraction(interaction, async (handler) => {
    onNative = handler; return () => undefined;
  }, () => snapshot, () => undefined);
  await vi.advanceTimersByTimeAsync(0);
  onNative(true); onNative(false);
  resolveSnapshot({active: true, nativeDrag: true});
  await vi.advanceTimersByTimeAsync(0);
  const applied: string[] = [];
  interaction.schedule('focus', () => { applied.push('focus'); });
  await vi.advanceTimersByTimeAsync(200);
  expect(applied).toEqual(['focus']);
  stop(); interaction.dispose();
});

it('holds queued refreshes through a stationary native drag and coalesces them after release', async () => {
  vi.useFakeTimers();
  const interaction = createWindowInteraction();
  const applied: string[] = [];
  interaction.setActive(true);
  interaction.schedule('focus', () => { applied.push('old'); });
  interaction.schedule('focus', () => { applied.push('latest'); });
  await vi.advanceTimersByTimeAsync(1500);
  expect(applied).toEqual([]);
  interaction.setActive(false);
  await vi.advanceTimersByTimeAsync(200);
  expect(applied).toEqual(['latest']);
  interaction.dispose();
});

it('cancels a focus refresh when native interaction starts before the settling delay', async () => {
  vi.useFakeTimers();
  const interaction = createWindowInteraction();
  const applied: string[] = [];
  interaction.schedule('focus', () => { applied.push('focus'); });
  interaction.setActive(true);
  await vi.advanceTimersByTimeAsync(500);
  expect(applied).toEqual([]);
  interaction.setActive(false);
  await vi.advanceTimersByTimeAsync(200);
  expect(applied).toEqual(['focus']);
  interaction.dispose();
});

it('drops pending work on disposal and ignores late native events', async () => {
  vi.useFakeTimers();
  const interaction = createWindowInteraction();
  const applied: string[] = [];
  interaction.schedule('focus', () => { applied.push('focus'); });
  interaction.dispose();
  interaction.setActive(false);
  interaction.schedule('focus', () => { applied.push('late'); });
  await vi.advanceTimersByTimeAsync(500);
  expect(applied).toEqual([]);
});
