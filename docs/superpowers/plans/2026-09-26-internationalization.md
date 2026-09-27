# Internationalization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a typed, persistent ten-language localization system, automatic system-language detection, and a language selector in Settings → General, then migrate visible UI copy to translation keys.

**Architecture:** Implement a small shared `src/i18n/` runtime with locale detection, persisted locale choice, typed dictionaries and a React hook. Integrate locale state at the application root so both main and launcher windows react to changes. Migrate UI copy by feature surface while keeping Chinese and English complete first; remaining locale dictionaries share the key schema and use English fallback until the user's later translation pass.

**Tech Stack:** React, TypeScript, Vite, localStorage, `navigator.languages`.

**Spec:** `docs/superpowers/specs/2026-09-21-internationalization-design.md`

## Global Constraints

- Support Simplified Chinese, Traditional Chinese, English, Japanese, Korean, Spanish, French, German, Brazilian Portuguese, and Russian.
- Auto mode matches `navigator.languages`, from the full language tag to the primary language tag; unsupported system languages fall back to English.
- Persist a user's fixed language choice and update all windows immediately.
- Missing translation keys fall back to Simplified Chinese during development; user-facing unsupported locales fall back to English.
- All new user-visible copy uses localization keys, with Simplified Chinese and English authored first.

## Review Focus

- `zh-Hant` and region-tagged system languages map to the intended locale; test exact-tag and base-tag selection.
- Unsupported locale and missing-key behavior never exposes internal keys; test English and Chinese fallback separately.
- A locale changed in the main window reaches the launcher window; test shared storage event synchronization.
- Repeated interpolation values and punctuation do not corrupt translated sentence order; test named parameters.
- A fixed locale survives restart while Auto re-detects the system language; test both persistence paths.

---

### Task 1: Typed localization runtime and locale dictionaries

**Files:**
- Create: `src/i18n/locales.ts`
- Create: `src/i18n/messages/zh-CN.ts`
- Create: `src/i18n/messages/en.ts`
- Create: `src/i18n/messages/zh-TW.ts`, `ja.ts`, `ko.ts`, `es.ts`, `fr.ts`, `de.ts`, `pt-BR.ts`, `ru.ts`
- Create: `src/i18n/index.ts`
- Create: `src/i18n/i18n.test.ts`

**Interfaces:**
- `LocaleId = 'zh-CN' | 'zh-TW' | 'en' | 'ja' | 'ko' | 'es' | 'fr' | 'de' | 'pt-BR' | 'ru'`
- `getLocale(): LocaleId`
- `getLocalePreference(): LocaleId | 'auto'`
- `setLocalePreference(locale: LocaleId | 'auto'): void`
- `subscribeLocale(listener: () => void): () => void`
- `t(key: TranslationKey, params?: Record<string, string | number>): string`
- `useTranslation(): { locale: LocaleId; localePreference: LocaleId | 'auto'; t: typeof t; setLocalePreference: typeof setLocalePreference }`
- Define complete dictionary keys from Chinese and English tables; other tables retain identical keys and are typed with `satisfies`.

- [ ] Add locale matching, fallback, persistence, subscription and interpolation tests.
- [ ] Run the focused localization test and confirm the expected missing-runtime failures.
- [ ] Implement locale runtime, document `lang` update, storage synchronization, ten locale tables and typed key fallback.
- [ ] Re-run the focused localization test.

### Task 2: Settings integration and cross-window language updates

**Files:**
- Modify: `src/components/SettingsPanel.tsx`
- Modify: `src/components/SettingsPanel.test.tsx`
- Modify: `src/components/LauncherWindow.tsx`
- Modify: `src/components/FloatingLauncher.tsx`
- Modify: `src/App.tsx`

**Interfaces:**
- Consume `useTranslation()` and `setLocalePreference()` from Task 1.
- Expose an “Automatic (system language)” selector option and native-language labels for all ten choices.

- [ ] Add Settings → General language selector and test fixed/automatic selection.
- [ ] Add locale subscription to both application roots and verify render updates across windows.
- [ ] Migrate app shell, navigation and Settings UI text to typed keys.

### Task 3: Download and shared dialogs localization

**Files:**
- Modify: `src/components/DownloadQueue.tsx`
- Modify: `src/components/NewDownloadDialog.tsx`
- Modify: `src/components/ParseConfirmDialog.tsx`
- Modify: `src/components/RemoveTaskDialog.tsx`
- Modify: `src/components/DuplicateDownloadDialog.tsx`
- Modify: `src/components/TaskDetails.tsx`
- Modify: `src/components/TaskDetailsDialog.tsx`
- Modify: `src/components/EngineStatusBanner.tsx`
- Modify: `src/lib/downloadNotifications.ts`

**Interfaces:**
- Use the shared `t()` and named parameter interpolation from Task 1.

- [ ] Move task tabs, sort menus, actions, statuses, dialog labels, validation and notification copy to localization keys.
- [ ] Preserve task names, provider labels, filenames and error payloads as dynamic values.

### Task 4: Cloud workspace and remaining screens localization

**Files:**
- Modify: `src/components/CloudWorkspace.tsx`
- Modify: `src/components/CloudNavigation.tsx`
- Modify: `src/components/CloudManager.tsx`
- Modify: `src/components/LocalUploadPicker.tsx`
- Modify: `src/components/LauncherContextMenu.tsx`
- Modify: `src/components/AboutPanel.tsx`
- Modify: remaining user-visible strings in `src/App.tsx` and components.

**Interfaces:**
- Reuse Task 1 translation runtime and typed message keys.

- [ ] Migrate cloud provider operations, share/recycle/upload UI, launcher menus, About screen, accessibility labels and user-facing error states.
- [ ] Ensure aria labels, tooltips, empty states and dynamic errors use translated templates.

### Task 5: Locale coverage and build integration

**Files:**
- Modify: all `src/i18n/messages/*.ts`
- Modify: `src/main.tsx`
- Modify: `src/lib/api.ts` only if locale preference belongs in shared settings rather than the dedicated locale storage key.

- [ ] Confirm all ten locale tables have the same keys and that missing non-English entries use English fallback without displaying keys.
- [ ] Run the focused application build and the selected localization/UI tests.
- [ ] Review remaining Chinese hardcoded text in shipped UI and migrate any missed user-facing strings; leave logs, fixtures and internal comments untranslated.
