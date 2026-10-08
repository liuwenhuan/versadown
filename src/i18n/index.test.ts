import { beforeEach, describe, expect, it, vi } from 'vitest';
import ja from './messages/ja';
import ko from './messages/ko';

type I18nModule = typeof import('./index');

async function freshI18n(): Promise<I18nModule> {
  vi.resetModules();
  return import('./index');
}

describe('i18n locale loading', () => {
  beforeEach(() => {
    window.localStorage.clear();
    vi.spyOn(navigator, 'languages', 'get').mockReturnValue(['zh-CN']);
  });

  it('bundles only the fallback catalogs up front', async () => {
    const i18n = await freshI18n();

    expect(i18n.isLocaleLoaded('zh-CN')).toBe(true);
    expect(i18n.isLocaleLoaded('en')).toBe(true);
    expect(i18n.isLocaleLoaded('ja')).toBe(false);
  });

  it('switches only after the selected catalog has loaded', async () => {
    const i18n = await freshI18n();
    const listener = vi.fn();
    i18n.subscribeLocale(listener);

    const switching = i18n.setLocalePreference('ja');
    expect(i18n.getLocale()).toBe('zh-CN');
    expect(i18n.getLocalePreference()).toBe('auto');

    await switching;
    expect(i18n.getLocale()).toBe('ja');
    expect(i18n.t('app.brand')).toBe(ja['app.brand']);
    expect(window.localStorage.getItem('versadown-locale-preference')).toBe('ja');
    expect(document.documentElement.lang).toBe('ja');
    expect(listener).toHaveBeenCalledTimes(1);
  });

  it('keeps the latest selection when loads finish out of order', async () => {
    const i18n = await freshI18n();

    await Promise.all([i18n.setLocalePreference('ja'), i18n.setLocalePreference('ko')]);

    expect(i18n.getLocale()).toBe('ko');
    expect(i18n.t('app.brand')).toBe(ko['app.brand']);
  });

  it('preloads a saved preference before the first render', async () => {
    window.localStorage.setItem('versadown-locale-preference', 'ko');
    const i18n = await freshI18n();

    await i18n.loadActiveLocale();

    expect(i18n.isLocaleLoaded('ko')).toBe(true);
    expect(i18n.t('app.brand')).toBe(ko['app.brand']);
    expect(document.title).toBe(ko['app.title']);
  });

  it('keeps the stored preference unchanged when a catalog fails to load', async () => {
    vi.doMock('./messages/ja', () => {
      throw new Error('chunk unavailable');
    });
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const i18n = await freshI18n();

    await i18n.setLocalePreference('ja');

    expect(i18n.getLocale()).toBe('zh-CN');
    expect(window.localStorage.getItem('versadown-locale-preference')).toBeNull();
    vi.doUnmock('./messages/ja');
  });

  it('does not let a system language change cancel a pending selection', async () => {
    const i18n = await freshI18n();

    const switching = i18n.setLocalePreference('ja');
    window.dispatchEvent(new Event('languagechange'));
    await switching;

    expect(i18n.getLocale()).toBe('ja');
  });
});
