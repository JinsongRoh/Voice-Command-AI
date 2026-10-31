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

  it('interpolates and leaves unknown placeholders alone', () => {
    expect(interpolate('{a} and {b}', { a: 1 })).toBe('1 and {b}');
  });

  it('only uses keys that exist in the English source', () => {
    for (const dict of [ja, ko, es]) {
      for (const key of Object.keys(dict)) expect(en).toHaveProperty(key);
    }
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
