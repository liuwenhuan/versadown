import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

type SystemTheme = {
  setDark: (matches: boolean) => void;
};

function installSystemTheme(initiallyDark: boolean): SystemTheme {
  const listeners = new Set<(event: MediaQueryListEvent) => void>();
  let matches = initiallyDark;
  const mediaQuery = {
    get matches() {
      return matches;
    },
    media: '(prefers-color-scheme: dark)',
    onchange: null,
    addEventListener: (type: string, listener: (event: MediaQueryListEvent) => void) => {
      if (type === 'change') listeners.add(listener);
    },
    removeEventListener: (type: string, listener: (event: MediaQueryListEvent) => void) => {
      if (type === 'change') listeners.delete(listener);
    }
  } as MediaQueryList;

  vi.stubGlobal('matchMedia', vi.fn(() => mediaQuery));

  return {
    setDark(nextMatches) {
      matches = nextMatches;
      const event = { matches, media: mediaQuery.media } as MediaQueryListEvent;
      listeners.forEach((listener) => listener(event));
    }
  };
}

describe('theme state', () => {
  const disposers: Array<() => void> = [];

  async function loadTheme() {
    const theme = await import('./theme');
    disposers.push(theme.disposeTheme);
    return theme;
  }

  beforeEach(() => {
    vi.resetModules();
    localStorage.clear();
    delete document.documentElement.dataset.theme;
    delete document.documentElement.dataset.themeMode;
  });

  afterEach(() => {
    disposers.splice(0).forEach((dispose) => dispose());
    vi.unstubAllGlobals();
  });

  it('restores a valid saved mode and applies it to the document', async () => {
    installSystemTheme(true);
    localStorage.setItem('theme-mode', 'light');

    const theme = await loadTheme();

    expect(theme.loadThemeMode()).toBe('light');
    expect(theme.getThemeMode()).toBe('light');
    expect(document.documentElement.dataset.themeMode).toBe('light');
    expect(document.documentElement.dataset.theme).toBe('light');
  });

  it('falls back to auto for an invalid saved value', async () => {
    installSystemTheme(true);
    localStorage.setItem('theme-mode', 'sepia');

    const theme = await loadTheme();

    expect(theme.loadThemeMode()).toBe('auto');
    expect(theme.getThemeMode()).toBe('auto');
    expect(document.documentElement.dataset.themeMode).toBe('auto');
    expect(document.documentElement.dataset.theme).toBe('dark');
  });

  it('persists explicit changes and exposes a React-compatible subscription', async () => {
    installSystemTheme(false);
    const theme = await loadTheme();
    const onChange = vi.fn();
    const unsubscribe = theme.subscribeTheme(onChange);

    theme.setThemeMode('dark');

    expect(theme.getThemeMode()).toBe('dark');
    expect(localStorage.getItem('theme-mode')).toBe('dark');
    expect(document.documentElement.dataset.themeMode).toBe('dark');
    expect(document.documentElement.dataset.theme).toBe('dark');
    expect(onChange).toHaveBeenCalledTimes(1);

    unsubscribe();
    theme.setThemeMode('light');
    expect(onChange).toHaveBeenCalledTimes(1);
  });

  it('tracks system changes only while mode is auto', async () => {
    const systemTheme = installSystemTheme(false);
    const theme = await loadTheme();
    const onChange = vi.fn();
    theme.subscribeTheme(onChange);

    systemTheme.setDark(true);

    expect(document.documentElement.dataset.theme).toBe('dark');
    expect(onChange).toHaveBeenCalledTimes(1);

    theme.setThemeMode('light');
    onChange.mockClear();
    systemTheme.setDark(false);
    systemTheme.setDark(true);

    expect(document.documentElement.dataset.theme).toBe('light');
    expect(onChange).not.toHaveBeenCalled();
  });

  it('synchronizes a theme change made by another application window', async () => {
    installSystemTheme(false);
    const theme = await loadTheme();
    const onChange = vi.fn();
    theme.subscribeTheme(onChange);

    localStorage.setItem('theme-mode', 'dark');
    window.dispatchEvent(new StorageEvent('storage', { key: 'theme-mode', newValue: 'dark' }));

    expect(theme.getThemeMode()).toBe('dark');
    expect(document.documentElement.dataset.themeMode).toBe('dark');
    expect(document.documentElement.dataset.theme).toBe('dark');
    expect(onChange).toHaveBeenCalledTimes(1);
  });
});
