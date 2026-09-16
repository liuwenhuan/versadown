export type ThemeId = 'clear-blue' | 'mint' | 'warm-gold' | 'lavender' | 'coral' | 'deep-ocean' | 'night-violet';
export type ThemeMode = 'auto' | ThemeId;
export type ResolvedTheme = 'light' | 'dark';

export interface ThemeOption {
  id: ThemeId;
  label: string;
  appearance: ResolvedTheme;
  preview: readonly [canvas: string, sidebar: string, accent: string];
}

type ThemeListener = () => void;

export const themeOptions: readonly ThemeOption[] = [
  { id: 'clear-blue', label: '清泉蓝', appearance: 'light', preview: ['#f4f7fc', '#edf3fd', '#4f7df3'] },
  { id: 'mint', label: '薄荷青', appearance: 'light', preview: ['#f5faf8', '#edf7f3', '#5d9f8b'] },
  { id: 'warm-gold', label: '暖阳金', appearance: 'light', preview: ['#fffdf5', '#fff7da', '#b58b00'] },
  { id: 'lavender', label: '丁香紫', appearance: 'light', preview: ['#faf8ff', '#f2edfb', '#8a6cbc'] },
  { id: 'coral', label: '珊瑚粉', appearance: 'light', preview: ['#fff9fa', '#fceef2', '#c8647b'] },
  { id: 'deep-ocean', label: '深海蓝', appearance: 'dark', preview: ['#101722', '#151f2d', '#65a6ff'] },
  { id: 'night-violet', label: '暮夜紫', appearance: 'dark', preview: ['#171421', '#211b2e', '#ae91f5'] }
] as const;

const STORAGE_KEY = 'theme-mode';
const DARK_MODE_QUERY = '(prefers-color-scheme: dark)';
const AUTO_LIGHT_THEME: ThemeId = 'mint';
const AUTO_DARK_THEME: ThemeId = 'deep-ocean';
const themeAppearances = new Map<ThemeId, ResolvedTheme>(themeOptions.map(({ id, appearance }) => [id, appearance]));
const listeners = new Set<ThemeListener>();

let autoMediaQuery: MediaQueryList | null = null;

function normalizeThemeMode(value: string | null): ThemeMode | null {
  if (value === 'auto') return 'auto';
  if (value === 'light') return AUTO_LIGHT_THEME;
  if (value === 'dark') return AUTO_DARK_THEME;
  return themeAppearances.has(value as ThemeId) ? value as ThemeId : null;
}

export function loadThemeMode(): ThemeMode {
  try {
    return normalizeThemeMode(window.localStorage.getItem(STORAGE_KEY)) ?? 'auto';
  } catch {
    return 'auto';
  }
}

let currentMode: ThemeMode = loadThemeMode();

function resolvedThemeId(): ThemeId {
  if (currentMode !== 'auto') return currentMode;
  return autoMediaQuery?.matches ? AUTO_DARK_THEME : AUTO_LIGHT_THEME;
}

function resolvedTheme(): ResolvedTheme {
  return themeAppearances.get(resolvedThemeId()) ?? 'light';
}

function applyCurrentTheme(): void {
  document.documentElement.dataset.themeMode = currentMode;
  document.documentElement.dataset.theme = resolvedThemeId();
}

function notifyListeners(): void {
  listeners.forEach((listener) => listener());
}

function handleSystemThemeChange(): void {
  if (currentMode !== 'auto') return;
  applyCurrentTheme();
  notifyListeners();
}

function stopTrackingSystemTheme(): void {
  if (!autoMediaQuery) return;
  autoMediaQuery.removeEventListener('change', handleSystemThemeChange);
  autoMediaQuery = null;
}

function syncSystemThemeTracking(): void {
  if (currentMode !== 'auto') {
    stopTrackingSystemTheme();
    return;
  }

  const nextMediaQuery = window.matchMedia?.(DARK_MODE_QUERY) ?? null;
  if (nextMediaQuery === autoMediaQuery) return;

  stopTrackingSystemTheme();
  autoMediaQuery = nextMediaQuery;
  autoMediaQuery?.addEventListener('change', handleSystemThemeChange);
}

export function getThemeMode(): ThemeMode {
  return currentMode;
}

export function getResolvedTheme(): ResolvedTheme {
  return resolvedTheme();
}

export function setThemeMode(mode: ThemeMode): void {
  currentMode = mode;
  try {
    window.localStorage.setItem(STORAGE_KEY, mode);
  } catch {
    // The selected mode still applies when storage is unavailable.
  }
  syncSystemThemeTracking();
  applyCurrentTheme();
  notifyListeners();
}

export function subscribeTheme(listener: ThemeListener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function handleStoredThemeChange(event: StorageEvent): void {
  if (event.key !== STORAGE_KEY) return;
  const nextMode = normalizeThemeMode(event.newValue) ?? 'auto';
  if (nextMode === currentMode) return;
  currentMode = nextMode;
  syncSystemThemeTracking();
  applyCurrentTheme();
  notifyListeners();
}

export function disposeTheme(): void {
  window.removeEventListener('storage', handleStoredThemeChange);
  stopTrackingSystemTheme();
  listeners.clear();
}

syncSystemThemeTracking();
applyCurrentTheme();
window.addEventListener('storage', handleStoredThemeChange);

const hot = (import.meta as ImportMeta & { hot?: { dispose(callback: () => void): void } }).hot;
hot?.dispose(disposeTheme);
