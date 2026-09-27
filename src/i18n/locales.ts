export const localeIds = ['zh-CN', 'zh-TW', 'en', 'ja', 'ko', 'es', 'fr', 'de', 'pt-BR', 'ru'] as const;

export type LocaleId = typeof localeIds[number];
export type LocalePreference = LocaleId | 'auto';

export const localeMetadata: ReadonlyArray<{ id: LocaleId; label: string }> = [
  { id: 'zh-CN', label: '简体中文' },
  { id: 'zh-TW', label: '繁體中文' },
  { id: 'en', label: 'English' },
  { id: 'ja', label: '日本語' },
  { id: 'ko', label: '한국어' },
  { id: 'es', label: 'Español' },
  { id: 'fr', label: 'Français' },
  { id: 'de', label: 'Deutsch' },
  { id: 'pt-BR', label: 'Português (Brasil)' },
  { id: 'ru', label: 'Русский' },
];

const supportedLocales = new Set<string>(localeIds);

function normalizeLanguageTag(value: string): LocaleId | undefined {
  const normalized = value.trim().replaceAll('_', '-').toLowerCase();
  if (!normalized) return undefined;

  if (normalized === 'zh' || normalized.startsWith('zh-cn') || normalized.startsWith('zh-sg') || normalized.startsWith('zh-hans')) return 'zh-CN';
  if (normalized.startsWith('zh-tw') || normalized.startsWith('zh-hk') || normalized.startsWith('zh-mo') || normalized.startsWith('zh-hant')) return 'zh-TW';

  const exact = localeIds.find((locale) => locale.toLowerCase() === normalized);
  if (exact) return exact;
  const primary = normalized.split('-')[0];
  const baseLocale: Record<string, LocaleId> = {
    en: 'en', ja: 'ja', ko: 'ko', es: 'es', fr: 'fr', de: 'de', pt: 'pt-BR', ru: 'ru',
  };
  return baseLocale[primary];
}

export function resolveLocale(languages: readonly string[]): LocaleId {
  for (const language of languages) {
    const locale = normalizeLanguageTag(language);
    if (locale && supportedLocales.has(locale)) return locale;
  }
  return 'en';
}
