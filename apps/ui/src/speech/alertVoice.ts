/**
 * Spoken alerts (NTF-04) from either source the user picked in Settings:
 * the device voice (Web Speech, the default) or a TTS engine on the PC,
 * whose audio is played with an `HTMLAudioElement`. Any failure of the PC
 * path (old server, engine down, rate limit, autoplay refused) falls back
 * to the device voice, so an alert is never silent because of the server.
 *
 * Callers pass only generic alert titles, never agent activity text.
 */
import { TTS_MAX_TEXT_CHARS } from '../protocol/types';
import { speakableText } from './radioSpeech';
import {
  cancelSpeech,
  fullLangTag,
  speak as speakDevice,
  speakUntilEnd,
  type TtsPrefs,
} from './tts';

/** Audio returned by the PC (`audio` reply). */
export interface AlertAudio {
  format: string;
  data: Uint8Array;
}

export interface AlertVoicePrefs extends Partial<TtsPrefs> {
  /** Engine name on the PC; `null` uses the device voice. */
  serverEngine: string | null;
  /** Speaker id of the PC engine; `null` or empty: the engine's configured voice. */
  serverVoice?: string | null;
}

/** Sends `synthesize` to the PC; rejects on any error reply. */
export type Synthesize = (
  text: string,
  language: string,
  engine: string | null,
  voice: string | null,
) => Promise<AlertAudio>;

export interface AlertVoiceDeps {
  /** Plays encoded audio; rejects when it cannot. Defaults to an `HTMLAudioElement`. */
  play?(audio: AlertAudio): Promise<void>;
  /** The device voice. Defaults to Web Speech (`tts.ts`). */
  speakDevice?(text: string, lang: string, prefs: Partial<TtsPrefs>): void;
  /** Plays encoded audio to its end (queued playback). */
  playToEnd?(audio: AlertAudio): Promise<void>;
  /** The device voice, resolving at the end of the utterance (queued playback). */
  speakDeviceToEnd?(text: string, lang: string, prefs: Partial<TtsPrefs>): Promise<void>;
  /** Silences both voices (queued playback is stopped). */
  stopAll?(): void;
  /** Longest wait for the PC before the device voice speaks instead. */
  timeoutMs?: number;
}

export interface AlertVoice {
  /** Speaks `text`; resolves once it was handed to a player. Never rejects. */
  speak(
    text: string,
    lang: string,
    prefs: AlertVoicePrefs,
    synthesize: Synthesize,
  ): Promise<'server' | 'device'>;
  /**
   * Like `speak`, but interrupts nothing and resolves only when the audio
   * has ended (the voice queue plays items with it). Never rejects.
   */
  play(
    text: string,
    lang: string,
    prefs: AlertVoicePrefs,
    synthesize: Synthesize,
  ): Promise<'server' | 'device'>;
  /** Stops whatever is playing (device voice and PC clip). */
  stop(): void;
  /** Forgets cached audio (another PC was selected). */
  clear(): void;
}

/** Synthesized clips kept in memory, so repeated alerts are not re-synthesized. */
const CACHE_ENTRIES = 32;
const TIMEOUT_MS = 10_000;

const MIME: Record<string, string> = {
  mp3: 'audio/mpeg',
  opus: 'audio/ogg',
  aac: 'audio/aac',
  flac: 'audio/flac',
  wav: 'audio/wav',
};

let playing: HTMLAudioElement | null = null;

/** True while a PC voice clip is playing (hands-free mode listens only after it). */
export function alertAudioPlaying(): boolean {
  return !!playing && !playing.paused && !playing.ended;
}

/** Stops the PC voice clip that is playing, if any (a tap stops a read-back). */
export function stopAlertAudio(): void {
  playing?.pause();
}

/**
 * Plays `audio` and resolves when it has ended, failed or was stopped
 * (`stopAlertAudio`). Used by the voice queue.
 */
export async function playAudioUntilEnd(audio: AlertAudio): Promise<void> {
  await playAudio(audio);
  const element = playing;
  if (!element || element.ended) return;
  await new Promise<void>((resolve) => {
    const done = () => resolve();
    element.addEventListener('ended', done, { once: true });
    element.addEventListener('error', done, { once: true });
    element.addEventListener('pause', done, { once: true });
  });
}

/** Plays `audio` with an audio element, stopping the previous alert first. */
export async function playAudio(audio: AlertAudio): Promise<void> {
  if (typeof Audio === 'undefined' || typeof URL.createObjectURL !== 'function') {
    throw new Error('audio playback is not available');
  }
  const blob = new Blob([audio.data as BlobPart], { type: MIME[audio.format] ?? 'audio/mpeg' });
  const url = URL.createObjectURL(blob);
  const element = new Audio(url);
  const release = () => URL.revokeObjectURL(url);
  element.addEventListener('ended', release, { once: true });
  element.addEventListener('error', release, { once: true });
  playing?.pause();
  playing = element;
  try {
    await element.play();
  } catch (err) {
    release();
    throw err;
  }
}

function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('timed out')), ms);
    promise.then(
      (v) => {
        clearTimeout(timer);
        resolve(v);
      },
      (e: unknown) => {
        clearTimeout(timer);
        reject(e instanceof Error ? e : new Error(String(e)));
      },
    );
  });
}

export function createAlertVoice(deps: AlertVoiceDeps = {}): AlertVoice {
  const play = deps.play ?? playAudio;
  const device = deps.speakDevice ?? ((text, lang, prefs) => speakDevice(text, lang, prefs));
  const timeoutMs = deps.timeoutMs ?? TIMEOUT_MS;
  const cache = new Map<string, Promise<AlertAudio>>();

  const audioFor = (
    synthesize: Synthesize,
    text: string,
    language: string,
    engine: string,
    voice: string,
  ): Promise<AlertAudio> => {
    // The speaker is part of the key: another voice is another clip.
    const key = JSON.stringify([engine, voice, language, text]);
    const hit = cache.get(key);
    if (hit) {
      // Most recently used last, so the oldest is evicted first.
      cache.delete(key);
      cache.set(key, hit);
      return hit;
    }
    const pending = synthesize(text, language, engine || null, voice || null);
    cache.set(key, pending);
    // Failures are not cached: the next alert asks again.
    pending.catch(() => {
      if (cache.get(key) === pending) cache.delete(key);
    });
    while (cache.size > CACHE_ENTRIES) {
      const oldest = cache.keys().next().value;
      if (oldest === undefined) break;
      cache.delete(oldest);
    }
    return pending;
  };

  const playToEnd = deps.playToEnd ?? playAudioUntilEnd;
  const deviceToEnd =
    deps.speakDeviceToEnd ?? ((text, lang, prefs) => speakUntilEnd(text, lang, prefs));
  const stopAll =
    deps.stopAll ??
    (() => {
      cancelSpeech();
      stopAlertAudio();
    });

  /** The PC voice when chosen and it works, else the device voice. */
  const run = async (
    text: string,
    lang: string,
    prefs: AlertVoicePrefs,
    synthesize: Synthesize,
    onAudio: (audio: AlertAudio) => Promise<void>,
    onDevice: (text: string, lang: string, prefs: Partial<TtsPrefs>) => unknown,
  ): Promise<'server' | 'device'> => {
    const devicePrefs: Partial<TtsPrefs> = { voice: prefs.voice, rate: prefs.rate };
    // The PC refuses longer text; alert titles are far shorter anyway.
    if (prefs.serverEngine === null || [...text].length > TTS_MAX_TEXT_CHARS) {
      await onDevice(text, lang, devicePrefs);
      return 'device';
    }
    let audio: AlertAudio;
    try {
      audio = await withTimeout(
        audioFor(
          synthesize,
          speakableText(text, lang),
          fullLangTag(lang),
          prefs.serverEngine,
          prefs.serverVoice ?? '',
        ),
        timeoutMs,
      );
    } catch {
      await onDevice(text, lang, devicePrefs);
      return 'device';
    }
    try {
      await onAudio(audio);
      return 'server';
    } catch {
      await onDevice(text, lang, devicePrefs);
      return 'device';
    }
  };

  return {
    speak(text, lang, prefs, synthesize) {
      return run(text, lang, prefs, synthesize, play, device).catch(() => 'device' as const);
    },
    play(text, lang, prefs, synthesize) {
      return run(text, lang, prefs, synthesize, playToEnd, deviceToEnd).catch(
        () => 'device' as const,
      );
    },
    stop() {
      stopAll();
    },
    clear() {
      cache.clear();
    },
  };
}
