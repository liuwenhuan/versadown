import { useSyncExternalStore } from 'react';
import type { LocaleId, LocalePreference } from './locales';
import en from './messages/en';
import zhCN from './messages/zh-CN';
import zhTW from './messages/zh-TW';
import ja from './messages/ja';
import ko from './messages/ko';
import es from './messages/es';
import fr from './messages/fr';
import de from './messages/de';
import ptBR from './messages/pt-BR';
import ru from './messages/ru';
import { resolveLocale } from './locales';

export { localeIds, localeMetadata, resolveLocale } from './locales';
export type { LocaleId, LocalePreference } from './locales';

const STORAGE_KEY = 'versadown-locale-preference';
const listeners = new Set<() => void>();
export type TranslationKey = keyof typeof en;
type MessageCatalog = Record<TranslationKey, string>;

const dictionaries: Record<LocaleId, MessageCatalog> = {
  'zh-CN': zhCN,
  'zh-TW': zhTW,
  en,
  ja,
  ko,
  es,
  fr,
  de,
  'pt-BR': ptBR,
  ru,
};

function isLocalePreference(value: string | null): value is LocalePreference {
  return value === 'auto' || Boolean(value && Object.hasOwn(dictionaries, value));
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
    document.title = dictionaries[locale]['app.title'];
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
    preference = isLocalePreference(event.newValue) ? event.newValue : 'auto';
    notify();
  });
  window.addEventListener('languagechange', () => {
    if (preference === 'auto') notify();
  });
}

export function getLocale(): LocaleId {
  return activeLocale();
}

export function getLocalePreference(): LocalePreference {
  return preference;
}

export function setLocalePreference(locale: LocalePreference): void {
  preference = locale;
  try {
    window.localStorage.setItem(STORAGE_KEY, locale);
  } catch {
    // Apply the selection for this session even when browser storage is unavailable.
  }
  notify();
}

export function subscribeLocale(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function t(key: TranslationKey, params?: Record<string, string | number>): string {
  const locale = activeLocale();
  const localized = dictionaries[locale][key];
  const value = localized ?? dictionaries['zh-CN'][key] ?? en[key] ?? '未翻译的文本';
  if (!params) return value;
  return value.replace(/\{([\w.-]+)\}/g, (match, name: string) => (
    Object.hasOwn(params, name) ? String(params[name]) : match
  ));
}

export function useTranslation() {
  const locale = useSyncExternalStore(subscribeLocale, getLocale, getLocale);
  return { locale, localePreference: preference, t, setLocalePreference };
}
