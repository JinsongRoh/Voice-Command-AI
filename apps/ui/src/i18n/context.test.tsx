import { render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { I18nProvider } from './context';

describe('I18nProvider', () => {
  afterEach(() => {
    document.documentElement.lang = 'en';
  });

  it('sets the page language to the UI language and follows changes', () => {
    const { rerender } = render(<I18nProvider locale="ko">x</I18nProvider>);
    expect(document.documentElement.lang).toBe('ko');
    rerender(<I18nProvider locale="ja">x</I18nProvider>);
    expect(document.documentElement.lang).toBe('ja');
    rerender(<I18nProvider locale="es">x</I18nProvider>);
    expect(document.documentElement.lang).toBe('es');
  });
});
