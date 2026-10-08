import { useSyncExternalStore } from 'react';
import type { LocaleId, LocalePreference } from './locales';
import en from './messages/en';
import zhCN from './messages/zh-CN';
import { localeIds, resolveLocale } from './locales';

export { localeIds, localeMetadata, resolveLocale } from './locales';
export type { LocaleId, LocalePreference } from './locales';

const STORAGE_KEY = 'versadown-locale-preference';
const listeners = new Set<() => void>();
export type TranslationKey = keyof typeof en;
type MessageCatalog = Record<TranslationKey, string>;

type CatalogModule = { default: MessageCatalog };

// Only the fallback chain is bundled; every other catalog is a separate chunk,
// so each window's renderer parses just the language it displays.
const catalogLoaders: Record<Exclude<LocaleId, 'zh-CN' | 'en'>, () => Promise<CatalogModule>> = {
  'zh-TW': () => import('./messages/zh-TW'),
  ja: () => import('./messages/ja'),
  ko: () => import('./messages/ko'),
  es: () => import('./messages/es'),
  fr: () => import('./messages/fr'),
  de: () => import('./messages/de'),
  'pt-BR': () => import('./messages/pt-BR'),
  ru: () => import('./messages/ru'),
};

let dictionaries: Partial<Record<LocaleId, MessageCatalog>> = { 'zh-CN': zhCN, en };
let latestLocaleRequest = 0;
// The selection still loading; `preference` only changes once it is ready.
let pendingPreference: LocalePreference | null = null;

function isLocalePreference(value: string | null): value is LocalePreference {
  return value === 'auto' || (localeIds as readonly (string | null)[]).includes(value);
}

export function isLocaleLoaded(locale: LocaleId): boolean {
  return Object.hasOwn(dictionaries, locale);
}

async function loadLocale(locale: LocaleId): Promise<void> {
  if (isLocaleLoaded(locale) || locale === 'zh-CN' || locale === 'en') return;
  const catalog = (await catalogLoaders[locale]()).default;
  dictionaries = { ...dictionaries, [locale]: catalog };
}

/** Called before the first render so the initial paint is already localized. */
export async function loadActiveLocale(): Promise<void> {
  await loadLocale(activeLocale());
  updateDocumentLanguage();
}

function loadPreference(): LocalePreference {
  try {
    const value = window.localStorage.getItem(STORAGE_KEY);
    return isLocalePreference(value) ? value : 'auto';
  } catch {
    return 'auto';
  }
}

let preference = loadPreference();

function systemLanguages(): readonly string[] {
  if (typeof navigator === 'undefined') return [];
  return navigator.languages?.length ? navigator.languages : [navigator.language ?? ''];
}

function activeLocale(): LocaleId {
  return preference === 'auto' ? resolveLocale(systemLanguages()) : preference;
}

function updateDocumentLanguage(): void {
  if (typeof document !== 'undefined') {
    const locale = activeLocale();
    document.documentElement.lang = locale;
    document.title = t('app.title');
  }
}

function notify(): void {
  updateDocumentLanguage();
  listeners.forEach((listener) => listener());
}

updateDocumentLanguage();

if (typeof window !== 'undefined') {
  window.addEventListener('storage', (event) => {
    if (event.key !== STORAGE_KEY) return;
    void applyPreference(isLocalePreference(event.newValue) ? event.newValue : 'auto');
  });
  window.addEventListener('languagechange', () => {
    if ((pendingPreference ?? preference) === 'auto') void applyPreference('auto');
  });
}

export function getLocale(): LocaleId {
  return activeLocale();
}

export function getLocalePreference(): LocalePreference {
  return preference;
}

/**
 * Switches only once the catalog is ready, so no frame mixes languages.
 * Resolves `true` when this request became the active preference.
 */
async function applyPreference(next: LocalePreference): Promise<boolean> {
  const request = ++latestLocaleRequest;
  pendingPreference = next;
  const locale = next === 'auto' ? resolveLocale(systemLanguages()) : next;
  let loaded = true;
  try {
    await loadLocale(locale);
  } catch (error) {
    console.error('Unable to load locale catalog:', error);
    loaded = false;
  }
  // A newer selection may have finished first; never let a slower load win.
  if (request !== latestLocaleRequest) return false;
  pendingPreference = null;
  if (!loaded) return false;
  preference = next;
  notify();
  return true;
}

export async function setLocalePreference(locale: LocalePreference): Promise<void> {
  // Persist only an applied choice, so storage never names a catalog that
  // failed to load and both windows keep agreeing on the language.
  if (!(await applyPreference(locale))) return;
  try {
    window.localStorage.setItem(STORAGE_KEY, locale);
  } catch {
    // Apply the selection for this session even when browser storage is unavailable.
  }
}

export function subscribeLocale(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function t(key: TranslationKey, params?: Record<string, string | number>): string {
  const locale = activeLocale();
  const localized = dictionaries[locale]?.[key];
  const value = localized ?? zhCN[key] ?? en[key] ?? '未翻译的文本';
  if (!params) return value;
  return value.replace(/\{([\w.-]+)\}/g, (match, name: string) => (
    Object.hasOwn(params, name) ? String(params[name]) : match
  ));
}

export function useTranslation() {
  const locale = useSyncExternalStore(subscribeLocale, getLocale, getLocale);
  return { locale, localePreference: preference, t, setLocalePreference };
}
