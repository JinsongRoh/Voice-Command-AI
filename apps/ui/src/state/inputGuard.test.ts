import { afterEach, describe, expect, it } from 'vitest';

import {
  inputBusy,
  isBusy,
  noteComposer,
  noteTyping,
  resetInputGuard,
  TYPING_WINDOW_MS,
} from './inputGuard';

const quiet = { hasText: false, dictating: false, lastInputAt: 0, modalOpen: false };

describe('input protection', () => {
  afterEach(resetInputGuard);

  it('is free when nothing is going on', () => {
    expect(isBusy(quiet, TYPING_WINDOW_MS)).toBe(false);
  });

  it('holds with text in the composer, dictation or a dialog', () => {
    expect(isBusy({ ...quiet, hasText: true }, 1e9)).toBe(true);
    expect(isBusy({ ...quiet, dictating: true }, 1e9)).toBe(true);
    expect(isBusy({ ...quiet, modalOpen: true }, 1e9)).toBe(true);
  });

  it('holds while keys were pressed just now', () => {
    expect(isBusy({ ...quiet, lastInputAt: 1000 }, 1000 + TYPING_WINDOW_MS - 1)).toBe(true);
    expect(isBusy({ ...quiet, lastInputAt: 1000 }, 1000 + TYPING_WINDOW_MS)).toBe(false);
  });

  it('follows what the composer reports', () => {
    expect(inputBusy(1e12)).toBe(false);
    noteComposer({ hasText: true, dictating: false });
    expect(inputBusy(1e12)).toBe(true);
    noteComposer({ hasText: false, dictating: false });
    noteTyping(1e12);
    expect(inputBusy(1e12 + 1)).toBe(true);
  });
});
