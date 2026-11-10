import { describe, expect, it } from 'vitest';

import en from './en.json';
import es from './es.json';
import ja from './ja.json';
import ko from './ko.json';
import { detectLocale, formatBytes, formatRelative, interpolate, translate } from './i18n';

describe('i18n', () => {
  it('translates with English fallback', () => {
    expect(translate('ja', 'nav.settings')).toBe('設定');
    expect(translate('ja', 'resources.queue')).toBe('実行待ちキュー');
    expect(translate('ja', 'no.such.key' as never)).toBe('no.such.key');
    expect(translate('es', 'card.unread', { count: 3 })).toBe('3 sin leer');
  });

  it('picks the singular variant by Intl.PluralRules where the language has one', () => {
    expect(translate('en', 'alerts.count', { count: 1 })).toBe('1 unhandled alert');
    expect(translate('en', 'alerts.count', { count: 0 })).toBe('0 unhandled alerts');
    expect(translate('en', 'alerts.count', { count: 3 })).toBe('3 unhandled alerts');
    expect(translate('es', 'windows.previewConfirm', { count: 1 })).toBe('Abrir 1 ventana');
    expect(translate('es', 'windows.previewConfirm', { count: 2 })).toBe('Abrir 2 ventanas');
    // No plural forms: the same text for every count.
    expect(translate('ko', 'alerts.count', { count: 1 })).toBe(
      translate('ko', 'alerts.count', { count: 2 }).replace('2', '1'),
    );
  });

  // A plural variant must say the same thing: same placeholders as its base key.
  it('keeps the placeholders of plural variants equal to their base key', () => {
    const names = (s: string) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
    for (const dict of [en, ko, ja, es] as Record<string, string>[]) {
      for (const key of Object.keys(dict).filter((k) => /_(zero|one|two|few|many)$/.test(k))) {
        const base = key.replace(/_(zero|one|two|few|many)$/, '');
        expect(dict).toHaveProperty(base);
        expect(names(dict[key]!)).toEqual(names(dict[base]!));
      }
    }
  });

  it('interpolates and leaves unknown placeholders alone', () => {
    expect(interpolate('{a} and {b}', { a: 1 })).toBe('1 and {b}');
  });

  it('only uses keys that exist in the English source', () => {
    for (const dict of [ja, ko, es]) {
      for (const key of Object.keys(dict)) expect(en).toHaveProperty(key);
    }
  });

  // No exceptions: every English key must be translated, otherwise the app
  // silently falls back to English for that language.
  it('has every English key in ko, ja and es', () => {
    const missing = (dict: Record<string, string>) =>
      Object.keys(en).filter((key) => !(key in dict));
    expect({ ko: missing(ko), ja: missing(ja), es: missing(es) }).toEqual({
      ko: [],
      ja: [],
      es: [],
    });
  });

  it('detects locales from BCP 47 tags', () => {
    expect(detectLocale(['ko-KR'])).toBe('ko');
    expect(detectLocale(['fr', 'es-MX'])).toBe('es');
    expect(detectLocale([])).toBe('en');
  });

  it('formats relative times and sizes', () => {
    const now = Date.UTC(2026, 0, 1);
    expect(formatRelative('en', now - 3 * 60_000, now)).toBe('3 minutes ago');
    expect(formatRelative('en', now - 2_000, now)).toBe('now');
    expect(formatBytes('en', 1536)).toBe('1.5 KiB');
    expect(formatBytes('en', 32 * 1024 ** 3)).toBe('32 GiB');
  });
});
