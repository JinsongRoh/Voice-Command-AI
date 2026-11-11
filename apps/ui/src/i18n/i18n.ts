/**
 * Tiny dictionary-based i18n. English (`en.json`) is the source language and
 * the fallback for every missing key. Placeholders use `{name}` syntax.
 *
 * Plurals: with a numeric `{count}`, a key `<key>_one` (etc., by
 * `Intl.PluralRules`) of the same language is used when it exists, so
 * "1 unhandled alert" and "3 unhandled alerts" come from `alerts.count_one`
 * and `alerts.count`. Languages without plural forms (ko, ja) always use the
 * plain key; their `_one` entries only mirror it to keep the key sets equal.
 */
import en from './en.json';
import es from './es.json';
import ja from './ja.json';
import ko from './ko.json';

export type MessageKey = keyof typeof en;
export type Dictionary = Partial<Record<MessageKey, string>>;
export type Locale = 'en' | 'ja' | 'ko' | 'es';
export type Vars = Record<string, string | number>;

/** Locale list with native names (not translated on purpose). */
export const LOCALES: { code: Locale; name: string }[] = [
  { code: 'en', name: 'English' },
  { code: 'ja', name: '日本語' },
  { code: 'ko', name: '한국어' },
  { code: 'es', name: 'Español' },
];

const DICTIONARIES: Record<Locale, Dictionary> = {
  en,
  ja: ja as Dictionary,
  ko: ko as Dictionary,
  es: es as Dictionary,
};

export function isLocale(value: string): value is Locale {
  return value in DICTIONARIES;
}

/** Picks the best supported locale for a list of BCP 47 tags (e.g. `navigator.languages`). */
export function detectLocale(preferred: readonly string[]): Locale {
  for (const tag of preferred) {
    const base = tag.toLowerCase().split('-')[0] ?? '';
    if (isLocale(base)) return base;
  }
  return 'en';
}

export function interpolate(template: string, vars?: Vars): string {
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (match, name: string) =>
    name in vars ? String(vars[name]) : match,
  );
}

const pluralRules = new Map<Locale, Intl.PluralRules>();

/** The plural variant of `key` for `count` in this language, if it has one. */
function pluralTemplate(locale: Locale, key: MessageKey, count: number): string | undefined {
  let rules = pluralRules.get(locale);
  if (!rules) {
    rules = new Intl.PluralRules(locale);
    pluralRules.set(locale, rules);
  }
  const form = rules.select(count);
  if (form === 'other') return undefined;
  // Never another language's variant: a missing one falls back to the plain key.
  return DICTIONARIES[locale][`${key}_${form}` as MessageKey];
}

export function translate(locale: Locale, key: MessageKey, vars?: Vars): string {
  const count = vars?.count;
  const template =
    (typeof count === 'number' ? pluralTemplate(locale, key, count) : undefined) ??
    DICTIONARIES[locale][key] ??
    en[key] ??
    key;
  return interpolate(template, vars);
}

export type TranslateFn = (key: MessageKey, vars?: Vars) => string;

const UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ['year', 365 * 24 * 3600],
  ['month', 30 * 24 * 3600],
  ['week', 7 * 24 * 3600],
  ['day', 24 * 3600],
  ['hour', 3600],
  ['minute', 60],
  ['second', 1],
];

/** "3 minutes ago" in the given locale; `now` is injectable for tests. */
export function formatRelative(locale: Locale, at: number, now: number = Date.now()): string {
  const diffSec = Math.round((at - now) / 1000);
  const rtf = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' });
  if (Math.abs(diffSec) < 10) return rtf.format(0, 'second');
  for (const [unit, seconds] of UNITS) {
    if (Math.abs(diffSec) >= seconds) return rtf.format(Math.round(diffSec / seconds), unit);
  }
  return rtf.format(diffSec, 'second');
}

/** Binary byte size, e.g. "1.4 GiB". */
export function formatBytes(locale: Locale, bytes: number): string {
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let value = bytes;
  let i = 0;
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024;
    i += 1;
  }
  const nf = new Intl.NumberFormat(locale, { maximumFractionDigits: value < 10 && i > 0 ? 1 : 0 });
  return `${nf.format(value)} ${units[i]}`;
}

export function formatRate(locale: Locale, bytesPerSec: number): string {
  return `${formatBytes(locale, bytesPerSec)}/s`;
}
