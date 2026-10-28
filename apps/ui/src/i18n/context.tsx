import { createContext, useContext, useMemo, type ReactNode } from 'react';

import {
  formatBytes,
  formatRate,
  formatRelative,
  translate,
  type Locale,
  type TranslateFn,
} from './i18n';

export interface I18n {
  locale: Locale;
  t: TranslateFn;
  relative: (at: number) => string;
  bytes: (n: number) => string;
  rate: (n: number) => string;
}

function make(locale: Locale): I18n {
  return {
    locale,
    t: (key, vars) => translate(locale, key, vars),
    relative: (at) => formatRelative(locale, at),
    bytes: (n) => formatBytes(locale, n),
    rate: (n) => formatRate(locale, n),
  };
}

const I18nContext = createContext<I18n>(make('en'));

export function I18nProvider({ locale, children }: { locale: Locale; children: ReactNode }) {
  const value = useMemo(() => make(locale), [locale]);
  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

// eslint-disable-next-line react-refresh/only-export-components
export function useI18n(): I18n {
  return useContext(I18nContext);
}
