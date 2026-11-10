/** Spoken alerts (NTF-04) through the Web Speech API, when available. */

import { speakableText } from './radioSpeech';

export const TTS_RATE_MIN = 0.8;
export const TTS_RATE_MAX = 1.3;
export const TTS_RATE_DEFAULT = 1;

/** Full BCP-47 tag per UI language; the base language is the fallback. */
const FULL_TAGS: Record<string, string> = {
  ko: 'ko-KR',
  en: 'en-US',
  ja: 'ja-JP',
  es: 'es-ES',
};

export interface TtsPrefs {
  /** `voiceURI` of the chosen voice; empty means "Auto (best available)". */
  voice: string;
  rate: number;
}

export function fullLangTag(lang: string): string {
  return FULL_TAGS[baseLang(lang)] ?? lang;
}

function baseLang(tag: string): string {
  return tag.toLowerCase().split(/[-_]/)[0] ?? '';
}

export function clampRate(rate: number): number {
  if (!Number.isFinite(rate)) return TTS_RATE_DEFAULT;
  return Math.min(TTS_RATE_MAX, Math.max(TTS_RATE_MIN, rate));
}

const QUALITY_NAME = /natural|neural|online|premium|enhanced/i;

/** True for voices that are clearly better than the legacy built-in ones. */
export function isHighQualityVoice(voice: SpeechSynthesisVoice): boolean {
  return QUALITY_NAME.test(voice.name);
}

function rank(voice: SpeechSynthesisVoice): number {
  if (isHighQualityVoice(voice)) return 0;
  return voice.localService ? 1 : 2;
}

/**
 * Voices usable for `lang`, best first. Voices matching the full tag
 * (ko-KR) come before voices that only match the base language (ko-*);
 * within a group: Natural/Neural/Online/Premium/Enhanced, then local, then rest.
 */
export function voicesForLang(
  voices: readonly SpeechSynthesisVoice[],
  lang: string,
): SpeechSynthesisVoice[] {
  const full = fullLangTag(lang).toLowerCase().replace('_', '-');
  const base = baseLang(lang);
  const group = (v: SpeechSynthesisVoice): number => {
    const tag = v.lang.toLowerCase().replace('_', '-');
    if (tag === full) return 0;
    return baseLang(tag) === base ? 1 : 2;
  };
  return voices
    .filter((v) => group(v) < 2)
    .map((v, i) => ({ v, i }))
    .sort((a, b) => group(a.v) - group(b.v) || rank(a.v) - rank(b.v) || a.i - b.i)
    .map((x) => x.v);
}

/** The voice to use: the saved one if it still exists for the language, else the best. */
export function pickVoice(
  voices: readonly SpeechSynthesisVoice[],
  lang: string,
  savedVoiceUri: string,
): SpeechSynthesisVoice | null {
  const list = voicesForLang(voices, lang);
  if (savedVoiceUri) {
    const saved = list.find((v) => v.voiceURI === savedVoiceUri);
    if (saved) return saved;
  }
  return list[0] ?? null;
}

/** True when the language has voices but none of them is a Natural/Neural/Online/Enhanced one. */
export function onlyLegacyVoices(voices: readonly SpeechSynthesisVoice[], lang: string): boolean {
  const list = voicesForLang(voices, lang);
  return list.length > 0 && !list.some(isHighQualityVoice);
}

export type TtsPlatform = 'windows' | 'apple' | 'android' | 'other';

export function detectTtsPlatform(
  userAgent: string = typeof navigator === 'undefined' ? '' : navigator.userAgent,
): TtsPlatform {
  if (/android/i.test(userAgent)) return 'android';
  if (/iphone|ipad|ipod|macintosh|mac os/i.test(userAgent)) return 'apple';
  if (/windows/i.test(userAgent)) return 'windows';
  return 'other';
}

function synth(): SpeechSynthesis | null {
  if (typeof window === 'undefined' || !('speechSynthesis' in window)) return null;
  return window.speechSynthesis;
}

export function ttsAvailable(): boolean {
  return synth() !== null;
}

function readVoices(): SpeechSynthesisVoice[] {
  try {
    return synth()?.getVoices() ?? [];
  } catch {
    return [];
  }
}

/**
 * Calls `listener` with the current voices now (possibly empty: they load
 * asynchronously) and again on every `voiceschanged`. Returns an unsubscribe.
 */
export function watchVoices(listener: (voices: SpeechSynthesisVoice[]) => void): () => void {
  const s = synth();
  if (!s) return () => {};
  const emit = () => listener(readVoices());
  s.addEventListener?.('voiceschanged', emit);
  emit();
  return () => s.removeEventListener?.('voiceschanged', emit);
}

function utteranceFor(
  text: string,
  lang: string,
  prefs: Partial<TtsPrefs>,
  voices: readonly SpeechSynthesisVoice[],
): SpeechSynthesisUtterance {
  const utterance = new SpeechSynthesisUtterance(speakableText(text, lang));
  utterance.lang = fullLangTag(lang);
  const voice = pickVoice(voices, lang, prefs.voice ?? '');
  if (voice) {
    utterance.voice = voice;
    utterance.lang = voice.lang || utterance.lang;
  }
  utterance.rate = clampRate(prefs.rate ?? TTS_RATE_DEFAULT);
  return utterance;
}

/**
 * Speaks `text` without interrupting anything and resolves when the device
 * voice reports the end (or an error). Used by the voice queue, which plays
 * one item at a time; it also bounds the wait.
 */
export function speakUntilEnd(
  text: string,
  lang: string,
  prefs: Partial<TtsPrefs> = {},
  voices: readonly SpeechSynthesisVoice[] = readVoices(),
): Promise<void> {
  const s = synth();
  if (!s) return Promise.resolve();
  return new Promise<void>((resolve) => {
    try {
      const utterance = utteranceFor(text, lang, prefs, voices);
      utterance.onend = () => resolve();
      utterance.onerror = () => resolve();
      s.speak(utterance);
    } catch {
      resolve();
    }
  });
}

/** Silences the device voice now. */
export function cancelSpeech(): void {
  try {
    synth()?.cancel();
  } catch {
    // Nothing is speaking.
  }
}

/**
 * Speaks `text` at once, cancelling whatever the device voice is saying
 * (previews and the hands-free read-back; queued alerts use
 * `speakUntilEnd`).
 */
export function speak(
  text: string,
  lang: string,
  prefs: Partial<TtsPrefs> = {},
  voices: readonly SpeechSynthesisVoice[] = readVoices(),
): void {
  const s = synth();
  if (!s) return;
  try {
    s.cancel();
    s.speak(utteranceFor(text, lang, prefs, voices));
  } catch {
    // Speech synthesis can be blocked (no voices, autoplay policy); alerts are still shown.
  }
}
