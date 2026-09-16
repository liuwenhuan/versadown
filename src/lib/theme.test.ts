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

  it('restores a saved color theme and applies its light appearance', async () => {
    installSystemTheme(true);
    localStorage.setItem('theme-mode', 'warm-gold');

    const theme = await loadTheme();

    expect(theme.loadThemeMode()).toBe('warm-gold');
    expect(theme.getThemeMode()).toBe('warm-gold');
    expect(theme.getResolvedTheme()).toBe('light');
    expect(document.documentElement.dataset.themeMode).toBe('warm-gold');
    expect(document.documentElement.dataset.theme).toBe('warm-gold');
  });

  it('offers five light themes and two dark themes', async () => {
    installSystemTheme(false);

    const theme = await loadTheme();

    expect(theme.themeOptions.map(({ id, appearance }) => [id, appearance])).toEqual([
      ['clear-blue', 'light'],
      ['mint', 'light'],
      ['warm-gold', 'light'],
      ['lavender', 'light'],
      ['coral', 'light'],
      ['deep-ocean', 'dark'],
      ['night-violet', 'dark']
    ]);
  });

  it.each([
    ['light', 'mint', 'light'],
    ['dark', 'deep-ocean', 'dark']
  ] as const)('migrates the legacy %s mode to %s', async (storedMode, expectedTheme, expectedAppearance) => {
    installSystemTheme(false);
    localStorage.setItem('theme-mode', storedMode);

    const theme = await loadTheme();

    expect(theme.getThemeMode()).toBe(expectedTheme);
    expect(theme.getResolvedTheme()).toBe(expectedAppearance);
    expect(document.documentElement.dataset.theme).toBe(expectedTheme);
  });

  it('falls back to auto for an invalid saved value', async () => {
    installSystemTheme(true);
    localStorage.setItem('theme-mode', 'sepia');

    const theme = await loadTheme();

    expect(theme.loadThemeMode()).toBe('auto');
    expect(theme.getThemeMode()).toBe('auto');
    expect(document.documentElement.dataset.themeMode).toBe('auto');
    expect(document.documentElement.dataset.theme).toBe('deep-ocean');
  });

  it('persists explicit changes and exposes a React-compatible subscription', async () => {
    installSystemTheme(false);
    const theme = await loadTheme();
    const onChange = vi.fn();
    const unsubscribe = theme.subscribeTheme(onChange);

    theme.setThemeMode('night-violet');

    expect(theme.getThemeMode()).toBe('night-violet');
    expect(theme.getResolvedTheme()).toBe('dark');
    expect(localStorage.getItem('theme-mode')).toBe('night-violet');
    expect(document.documentElement.dataset.themeMode).toBe('night-violet');
    expect(document.documentElement.dataset.theme).toBe('night-violet');
    expect(onChange).toHaveBeenCalledTimes(1);

    unsubscribe();
    theme.setThemeMode('clear-blue');
    expect(onChange).toHaveBeenCalledTimes(1);
  });

  it('tracks system changes only while mode is auto', async () => {
    const systemTheme = installSystemTheme(false);
    const theme = await loadTheme();
    const onChange = vi.fn();
    theme.subscribeTheme(onChange);

    systemTheme.setDark(true);

    expect(document.documentElement.dataset.theme).toBe('deep-ocean');
    expect(onChange).toHaveBeenCalledTimes(1);

    theme.setThemeMode('coral');
    onChange.mockClear();
    systemTheme.setDark(false);
    systemTheme.setDark(true);

    expect(document.documentElement.dataset.theme).toBe('coral');
    expect(onChange).not.toHaveBeenCalled();
  });

  it('synchronizes a theme change made by another application window', async () => {
    installSystemTheme(false);
    const theme = await loadTheme();
    const onChange = vi.fn();
    theme.subscribeTheme(onChange);

    localStorage.setItem('theme-mode', 'lavender');
    window.dispatchEvent(new StorageEvent('storage', { key: 'theme-mode', newValue: 'lavender' }));

    expect(theme.getThemeMode()).toBe('lavender');
    expect(document.documentElement.dataset.themeMode).toBe('lavender');
    expect(document.documentElement.dataset.theme).toBe('lavender');
    expect(onChange).toHaveBeenCalledTimes(1);
  });
});
