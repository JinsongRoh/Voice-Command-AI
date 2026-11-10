import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { parseSettings, defaultSettings, speechLanguageParam } from '../state/settings';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { Composer } from './Composer';

let clock = 1_000_000;
beforeEach(() => {
  clock = 1_000_000;
  vi.spyOn(Date, 'now').mockImplementation(() => clock);
});
afterEach(() => vi.restoreAllMocks());

async function dictate(speechLanguage?: 'auto' | 'ko') {
  const transcribe = vi.fn(async () => ({ text: '안녕하세요', corrections: [] }));
  renderWithApp(<Composer session={localSession()} />, {
    speech: {
      transcribe,
      createRecorder: () => ({
        start: async () => {},
        stop: async () => new Blob(['a']),
        cancel: () => {},
      }),
    },
    settings: { micMode: 'hold', language: 'en', ...(speechLanguage ? { speechLanguage } : {}) },
  });
  const mic = screen.getByRole('button', { name: 'Hold to talk' });
  await act(async () => {
    fireEvent.pointerDown(mic, { pointerId: 1 });
  });
  clock += 1500;
  await act(async () => {
    fireEvent.pointerUp(mic, { pointerId: 1 });
  });
  await waitFor(() => expect(transcribe).toHaveBeenCalled());
  return transcribe;
}

describe('speech language', () => {
  it('does not force the UI language on the recognizer by default', async () => {
    const transcribe = await dictate();
    expect(transcribe).toHaveBeenCalledWith(
      expect.anything(),
      expect.objectContaining({ locale: 'en', language: null }),
    );
  });

  it('sends an explicitly chosen speech language', async () => {
    const transcribe = await dictate('ko');
    expect(transcribe).toHaveBeenCalledWith(
      expect.anything(),
      expect.objectContaining({ locale: 'en', language: 'ko' }),
    );
  });

  it('defaults to auto and keeps only known choices', () => {
    const defaults = defaultSettings(['en-US']);
    expect(defaults.speechLanguage).toBe('auto');
    expect(parseSettings('{"speechLanguage":"ja"}', defaults).speechLanguage).toBe('ja');
    expect(parseSettings('{"speechLanguage":"xx"}', defaults).speechLanguage).toBe('auto');
    expect(speechLanguageParam('auto')).toBeNull();
    expect(speechLanguageParam('es')).toBe('es');
  });
});
