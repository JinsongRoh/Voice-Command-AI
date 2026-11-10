import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  clampRate,
  detectTtsPlatform,
  fullLangTag,
  onlyLegacyVoices,
  pickVoice,
  speak,
  voicesForLang,
  watchVoices,
} from './tts';

function voice(name: string, lang: string, localService = true): SpeechSynthesisVoice {
  return { name, lang, localService, voiceURI: name, default: false };
}

const HEAMI = voice('Microsoft Heami - Korean', 'ko-KR');
const SUNHI = voice('Microsoft SunHi Online (Natural) - Korean', 'ko-KR', false);
const KO_CLOUD = voice('Cloud Korean', 'ko-KR', false);
const KO_OTHER = voice('Odd Korean', 'ko');
const ZIRA = voice('Microsoft Zira - English (United States)', 'en-US');
const EN_GB = voice('Microsoft Hazel - English (Great Britain)', 'en-GB');

class FakeUtterance {
  voice: SpeechSynthesisVoice | null = null;
  lang = '';
  rate = 1;
  constructor(public text: string) {}
}

describe('tts', () => {
  let voices: SpeechSynthesisVoice[];
  let listeners: Array<() => void>;
  const synth = {
    speak: vi.fn(),
    cancel: vi.fn(),
    getVoices: vi.fn(() => voices),
    addEventListener: vi.fn((_: string, fn: () => void) => listeners.push(fn)),
    removeEventListener: vi.fn((_: string, fn: () => void) => {
      listeners = listeners.filter((l) => l !== fn);
    }),
  };

  beforeEach(() => {
    voices = [];
    listeners = [];
    vi.clearAllMocks();
    vi.stubGlobal('SpeechSynthesisUtterance', FakeUtterance);
    Object.defineProperty(window, 'speechSynthesis', { value: synth, configurable: true });
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    Reflect.deleteProperty(window, 'speechSynthesis');
  });

  it('maps UI languages to full tags', () => {
    expect(fullLangTag('ko')).toBe('ko-KR');
    expect(fullLangTag('en')).toBe('en-US');
    expect(fullLangTag('ja')).toBe('ja-JP');
    expect(fullLangTag('es')).toBe('es-ES');
  });

  it('ranks Natural/Online first, then local, then the rest; full tag before base', () => {
    const ranked = voicesForLang([KO_CLOUD, HEAMI, KO_OTHER, SUNHI, ZIRA], 'ko');
    expect(ranked.map((v) => v.name)).toEqual([
      SUNHI.name,
      HEAMI.name,
      KO_CLOUD.name,
      KO_OTHER.name,
    ]);
  });

  it('falls back to the base language when the full tag has no voice', () => {
    expect(voicesForLang([EN_GB, HEAMI], 'en')).toEqual([EN_GB]);
    expect(voicesForLang([HEAMI], 'ja')).toEqual([]);
  });

  it('detects when only legacy voices exist', () => {
    expect(onlyLegacyVoices([HEAMI, KO_CLOUD], 'ko')).toBe(true);
    expect(onlyLegacyVoices([HEAMI, SUNHI], 'ko')).toBe(false);
    expect(onlyLegacyVoices([HEAMI], 'ja')).toBe(false);
  });

  it('falls back to Auto when the saved voice is missing', () => {
    expect(pickVoice([HEAMI, SUNHI], 'ko', HEAMI.voiceURI)).toBe(HEAMI);
    expect(pickVoice([HEAMI, SUNHI], 'ko', 'Removed Voice')).toBe(SUNHI);
    expect(pickVoice([HEAMI, SUNHI], 'ko', '')).toBe(SUNHI);
    expect(pickVoice([], 'ko', '')).toBeNull();
  });

  it('waits for voiceschanged, then reports the loaded voices', () => {
    const seen: number[] = [];
    const stop = watchVoices((v) => seen.push(v.length));
    expect(seen).toEqual([0]);
    voices = [HEAMI, SUNHI];
    listeners.forEach((l) => l());
    expect(seen).toEqual([0, 2]);
    stop();
    expect(listeners).toHaveLength(0);
  });

  it('speaks with the best voice, full tag and clamped rate', () => {
    voices = [HEAMI, SUNHI];
    speak('Alert', 'ko', { voice: '', rate: 5 });
    const u = synth.speak.mock.calls[0]?.[0] as FakeUtterance;
    expect(u.text).toBe('Alert');
    expect(u.voice).toBe(SUNHI);
    expect(u.lang).toBe('ko-KR');
    expect(u.rate).toBe(1.3);
  });

  it('uses the full tag when no voice is available', () => {
    speak('Alert', 'en');
    const u = synth.speak.mock.calls[0]?.[0] as FakeUtterance;
    expect(u.voice).toBeNull();
    expect(u.lang).toBe('en-US');
    expect(u.rate).toBe(1);
  });

  it('never forces a voice of another language on Korean speech', () => {
    // iOS lists its voices late; a voice saved for another language does not count.
    voices = [ZIRA, EN_GB];
    speak('Orbit Ship 세션 완료', 'ko', { voice: ZIRA.voiceURI });
    const u = synth.speak.mock.calls[0]?.[0] as FakeUtterance;
    expect(u.voice).toBeNull();
    expect(u.lang).toBe('ko-KR');
    voices = [ZIRA, HEAMI];
    speak('세션 완료', 'ko', { voice: ZIRA.voiceURI });
    const v = synth.speak.mock.calls[1]?.[0] as FakeUtterance;
    expect(v.voice).toBe(HEAMI);
    expect(v.lang).toBe('ko-KR');
  });

  it('cancels queued utterances before speaking a new one', () => {
    speak('One', 'en');
    speak('Two', 'en');
    expect(synth.cancel).toHaveBeenCalledTimes(2);
    expect(synth.cancel.mock.invocationCallOrder[1]).toBeLessThan(
      synth.speak.mock.invocationCallOrder[1] ?? 0,
    );
  });

  it('clamps rates and detects platforms', () => {
    expect(clampRate(0.1)).toBe(0.8);
    expect(clampRate(NaN)).toBe(1);
    expect(detectTtsPlatform('Mozilla/5.0 (Windows NT 10.0) Edg/130')).toBe('windows');
    expect(detectTtsPlatform('Mozilla/5.0 (iPhone; CPU iPhone OS 17)')).toBe('apple');
    expect(detectTtsPlatform('Mozilla/5.0 (Linux; Android 14)')).toBe('android');
    expect(detectTtsPlatform('X11; Linux')).toBe('other');
  });
});
