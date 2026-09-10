export type ThemeMode = 'auto' | 'light' | 'dark';

type ResolvedTheme = Exclude<ThemeMode, 'auto'>;
type ThemeListener = () => void;

const STORAGE_KEY = 'theme-mode';
const DARK_MODE_QUERY = '(prefers-color-scheme: dark)';
const listeners = new Set<ThemeListener>();

let autoMediaQuery: MediaQueryList | null = null;

function isThemeMode(value: string | null): value is ThemeMode {
  return value === 'auto' || value === 'light' || value === 'dark';
}

export function loadThemeMode(): ThemeMode {
  try {
    const savedMode = window.localStorage.getItem(STORAGE_KEY);
    return isThemeMode(savedMode) ? savedMode : 'auto';
  } catch {
    return 'auto';
  }
}

let currentMode: ThemeMode = loadThemeMode();

function resolvedTheme(): ResolvedTheme {
  if (currentMode !== 'auto') return currentMode;
  return autoMediaQuery?.matches ? 'dark' : 'light';
}

function applyCurrentTheme(): void {
  document.documentElement.dataset.themeMode = currentMode;
  document.documentElement.dataset.theme = resolvedTheme();
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
  const nextMode = isThemeMode(event.newValue) ? event.newValue : 'auto';
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
