import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { clearDiag, diagEntries, setDiagEnabled } from '../diag/log';
import { defaultSettings, parseSettings } from '../state/settings';
import {
  CHUNK_CHARS,
  createAlertVoice,
  fallbackReason,
  splitSpeech,
  type AlertAudio,
  type AlertVoice,
  type AlertVoiceDeps,
  type AlertVoicePrefs,
  type Synthesize,
} from './alertVoice';

const clip: AlertAudio = { format: 'mp3', data: new Uint8Array([1, 2, 3]) };

const flush = async () => {
  for (let i = 0; i < 50; i += 1) await Promise.resolve();
};

/** A promise the test settles by hand. */
function deferred<T>() {
  let resolve: (v: T) => void = () => {};
  let reject: (e: unknown) => void = () => {};
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function setup(overrides: Partial<AlertVoiceDeps> & { synthesize?: Synthesize } = {}) {
  const { synthesize: custom, ...deps } = overrides;
  const synthesize = vi.fn<Synthesize>(custom ?? (async () => clip));
  const playToEnd = vi.fn<NonNullable<AlertVoiceDeps['playToEnd']>>(async () => {});
  const speakDeviceToEnd = vi.fn<NonNullable<AlertVoiceDeps['speakDeviceToEnd']>>(async () => {});
  const stopAll = vi.fn();
  const created: AlertVoice = createAlertVoice({
    playToEnd,
    speakDeviceToEnd,
    stopAll,
    sleep: async () => {},
    ...deps,
  });
  const voice = {
    speak: (text: string, lang: string, prefs: AlertVoicePrefs, signal?: AbortSignal) =>
      created.speak(text, lang, prefs, synthesize, signal),
    warm: (texts: string[], lang: string, prefs: AlertVoicePrefs, signal?: AbortSignal) =>
      created.warm(texts, lang, prefs, synthesize, { signal }),
    stop: () => created.stop(),
    clear: () => created.clear(),
  };
  return { voice, synthesize, playToEnd, speakDeviceToEnd, stopAll };
}

const device = { serverEngine: null, voice: 'David', rate: 1.2 };
const server = { serverEngine: 'local-tts', voice: 'David', rate: 1.2 };

const voiceEvents = () =>
  diagEntries()
    .filter((e) => e.event === 'voice.out')
    .map((e) => e.data);

beforeEach(() => {
  setDiagEnabled(true);
  clearDiag();
});
afterEach(() => {
  clearDiag();
  setDiagEnabled(false);
});

describe('alert voice', () => {
  it('uses the device voice by default and never asks the PC', async () => {
    const { voice, synthesize, speakDeviceToEnd } = setup();
    await expect(voice.speak('Build failed', 'en', device)).resolves.toBe('device');
    expect(synthesize).not.toHaveBeenCalled();
    expect(speakDeviceToEnd).toHaveBeenCalledWith('Build failed', 'en', {
      voice: 'David',
      rate: 1.2,
    });
  });

  it('plays audio from the chosen PC engine', async () => {
    const { voice, synthesize, playToEnd, speakDeviceToEnd } = setup();
    await expect(voice.speak('Build failed', 'ko', server)).resolves.toBe('server');
    expect(synthesize).toHaveBeenCalledWith('Build failed', 'ko-KR', 'local-tts', null);
    expect(playToEnd).toHaveBeenCalledWith(clip);
    expect(speakDeviceToEnd).not.toHaveBeenCalled();
  });

  it('asks the PC for the chosen speaker', async () => {
    const { voice, synthesize } = setup();
    await voice.speak('Build failed', 'en', { ...server, serverVoice: 'M2' });
    expect(synthesize).toHaveBeenCalledWith('Build failed', 'en-US', 'local-tts', 'M2');
  });

  it('keeps clips of different speakers apart in the cache', async () => {
    const { voice, synthesize } = setup();
    const f1 = { ...server, serverVoice: 'F1' };
    await voice.speak('Build failed', 'en', f1);
    await voice.speak('Build failed', 'en', f1);
    expect(synthesize).toHaveBeenCalledTimes(1);
    await voice.speak('Build failed', 'en', { ...server, serverVoice: 'M1' });
    expect(synthesize).toHaveBeenCalledTimes(2);
    // No speaker is yet another key.
    await voice.speak('Build failed', 'en', server);
    expect(synthesize).toHaveBeenCalledTimes(3);
    await voice.speak('Build failed', 'en', { ...server, serverVoice: 'M1' });
    expect(synthesize).toHaveBeenCalledTimes(3);
  });

  it('falls back to the device voice when the PC fails, playback fails or is slow', async () => {
    const failing = setup({ synthesize: vi.fn(async () => Promise.reject(new Error('off'))) });
    await expect(failing.voice.speak('Done', 'en', server)).resolves.toBe('device');
    expect(failing.speakDeviceToEnd).toHaveBeenCalledWith('Done', 'en', {
      voice: 'David',
      rate: 1.2,
    });

    const blocked = setup({ playToEnd: vi.fn(async () => Promise.reject(new Error('autoplay'))) });
    await expect(blocked.voice.speak('Done', 'en', server)).resolves.toBe('device');
    expect(blocked.speakDeviceToEnd).toHaveBeenCalledOnce();

    const slow = setup({
      synthesize: () => new Promise<AlertAudio>(() => {}),
      timeoutMs: 5,
    });
    await expect(slow.voice.speak('Done', 'en', server)).resolves.toBe('device');
    expect(slow.speakDeviceToEnd).toHaveBeenCalledOnce();
  });

  it('caches audio by engine, language and text, but not failures', async () => {
    const { voice, synthesize, playToEnd } = setup();
    await voice.speak('Build failed', 'en', server);
    await voice.speak('Build failed', 'en', server);
    expect(synthesize).toHaveBeenCalledTimes(1);
    expect(playToEnd).toHaveBeenCalledTimes(2);
    await voice.speak('Build failed', 'ko', server);
    await voice.speak('Build failed', 'en', { ...server, serverEngine: 'other' });
    await voice.speak('Session finished', 'en', server);
    expect(synthesize).toHaveBeenCalledTimes(4);
    voice.clear();
    await voice.speak('Build failed', 'en', server);
    expect(synthesize).toHaveBeenCalledTimes(5);

    let fail = true;
    const flakySynthesize = vi.fn(async () => {
      if (fail) throw new Error('down');
      return clip;
    });
    const flaky = setup({ synthesize: flakySynthesize });
    expect(flaky.synthesize).not.toHaveBeenCalled();
    await expect(flaky.voice.speak('Done', 'en', server)).resolves.toBe('device');
    fail = false;
    await expect(flaky.voice.speak('Done', 'en', server)).resolves.toBe('server');
    expect(flaky.synthesize).toHaveBeenCalledTimes(2);
  });

  // (a) A stop while the PC is still making the clip: nothing plays later.
  it('plays nothing when stopped while the PC audio is still being made', async () => {
    const pending = deferred<AlertAudio>();
    const { voice, playToEnd, speakDeviceToEnd } = setup({ synthesize: () => pending.promise });
    const controller = new AbortController();
    const said = voice.speak('Build failed', 'en', server, controller.signal);
    await flush();
    controller.abort();
    await expect(said).resolves.toBe('stopped');
    pending.resolve(clip);
    await flush();
    expect(playToEnd).not.toHaveBeenCalled();
    expect(speakDeviceToEnd).not.toHaveBeenCalled();

    // `stop()` does the same for the run in progress.
    const late = deferred<AlertAudio>();
    const other = setup({ synthesize: () => late.promise });
    const run = other.voice.speak('Session finished', 'en', server);
    await flush();
    other.voice.stop();
    await expect(run).resolves.toBe('stopped');
    late.resolve(clip);
    await flush();
    expect(other.playToEnd).not.toHaveBeenCalled();
    expect(other.speakDeviceToEnd).not.toHaveBeenCalled();
  });

  // (b) The device voice stands in after a timeout; the late clip is never played.
  it('discards PC audio that arrives after the device voice took over', async () => {
    const pending = deferred<AlertAudio>();
    const { voice, playToEnd, speakDeviceToEnd } = setup({
      synthesize: () => pending.promise,
      timeoutMs: 5,
    });
    await expect(voice.speak('Done', 'en', server)).resolves.toBe('device');
    expect(speakDeviceToEnd).toHaveBeenCalledOnce();
    pending.resolve(clip);
    await flush();
    expect(playToEnd).not.toHaveBeenCalled();
  });

  it('a new run ends the one before it, also its sound', async () => {
    const first = deferred<void>();
    const { voice, playToEnd, stopAll } = setup();
    playToEnd.mockImplementationOnce(() => first.promise);
    const one = voice.speak('First alert', 'en', server);
    await flush();
    expect(playToEnd).toHaveBeenCalledOnce();
    const two = voice.speak('Second alert', 'en', server);
    // The first clip is silenced at once, not left playing under the second.
    expect(stopAll).toHaveBeenCalledOnce();
    await expect(one).resolves.toBe('stopped');
    await expect(two).resolves.toBe('server');
    expect(playToEnd).toHaveBeenCalledTimes(2);
  });

  // (g) Long text: sentence pieces in the PC voice, never the device voice.
  it('splits long text into sentences and plays them in the PC voice', async () => {
    const { voice, synthesize, playToEnd, speakDeviceToEnd } = setup();
    const sentences = ['a', 'b', 'c'].map((c) => `${c.repeat(120)}.`);
    await expect(voice.speak(sentences.join(' '), 'en', server)).resolves.toBe('server');
    expect(speakDeviceToEnd).not.toHaveBeenCalled();
    expect(synthesize.mock.calls.map(([said]) => said)).toEqual(sentences);
    expect(playToEnd).toHaveBeenCalledTimes(3);
  });

  it('never switches voices once the PC voice has spoken', async () => {
    let n = 0;
    const { voice, playToEnd, speakDeviceToEnd } = setup({
      synthesize: async () => {
        n += 1;
        if (n > 1)
          throw Object.assign(new Error('too many spoken alerts'), { code: 'unavailable' });
        return clip;
      },
    });
    const text = `${'b'.repeat(150)}. ${'c'.repeat(150)}.`;
    await expect(voice.speak(text, 'en', server)).resolves.toBe('server');
    expect(playToEnd).toHaveBeenCalledOnce();
    expect(speakDeviceToEnd).not.toHaveBeenCalled();
    expect(voiceEvents().at(-1)).toMatchObject({
      source: 'server',
      reason: 'rate_limited',
      played: 1,
      chunks: 2,
    });
  });

  // (f) Every outcome is logged with its source and reason, never the text.
  it('logs the voice source and the fallback reason', async () => {
    const fails = (err: Error) => setup({ synthesize: async () => Promise.reject(err) });
    const cases: [Error, string][] = [
      [
        Object.assign(new Error('too many spoken alerts; try again in a minute'), {
          code: 'unavailable',
        }),
        'rate_limited',
      ],
      [
        Object.assign(new Error('the text is 320 characters; the limit is 300'), {
          code: 'invalid_request',
        }),
        'too_long',
      ],
      [Object.assign(new Error('no TTS engine named "x"'), { code: 'not_found' }), 'no_engine'],
      [new Error('socket closed'), 'failed'],
    ];
    for (const [err, reason] of cases) {
      clearDiag();
      await fails(err).voice.speak('Secret build name', 'en', server);
      expect(voiceEvents()).toEqual([
        expect.objectContaining({ source: 'device', reason, played: 0 }),
      ]);
    }
    clearDiag();
    await setup({ synthesize: () => new Promise(() => {}), timeoutMs: 5 }).voice.speak(
      'Secret build name',
      'en',
      server,
    );
    expect(voiceEvents()).toEqual([
      expect.objectContaining({ source: 'device', reason: 'timeout' }),
    ]);
    clearDiag();
    await setup({ playToEnd: async () => Promise.reject(new Error('refused')) }).voice.speak(
      'Secret build name',
      'en',
      server,
    );
    expect(voiceEvents()).toEqual([
      expect.objectContaining({ source: 'device', reason: 'play_failed' }),
    ]);
    clearDiag();
    await setup().voice.speak('Secret build name', 'en', server);
    await setup().voice.speak('Secret build name', 'en', device);
    expect(voiceEvents()).toEqual([
      expect.objectContaining({ source: 'server', reason: 'none' }),
      expect.objectContaining({ source: 'device', reason: 'none' }),
    ]);
    expect(JSON.stringify(diagEntries())).not.toContain('Secret');
    expect(fallbackReason(new Error('x'))).toBe('failed');
  });

  it('warms fixed prompts one at a time and plays them from the cache', async () => {
    const sleep = vi.fn(async () => {});
    const { voice, synthesize, playToEnd } = setup({ sleep });
    const result = await voice.warm(['네', '보냈습니다.', '네'], 'ko', server);
    expect(result).toEqual({ cached: 1, synthesized: 2, failed: 0 });
    expect(synthesize).toHaveBeenCalledTimes(2);
    // Paced: one pause between the two syntheses.
    expect(sleep).toHaveBeenCalledOnce();
    await expect(voice.speak('네', 'ko', server)).resolves.toBe('server');
    expect(synthesize).toHaveBeenCalledTimes(2);
    expect(playToEnd).toHaveBeenCalledOnce();
    // Warming again asks for nothing.
    await voice.warm(['네', '보냈습니다.'], 'ko', server);
    expect(synthesize).toHaveBeenCalledTimes(2);
    // Warmed prompts survive a flood of other alerts.
    for (let i = 0; i < 80; i += 1) await voice.speak(`Alert ${i}`, 'ko', server);
    const before = synthesize.mock.calls.length;
    await voice.speak('네', 'ko', server);
    expect(synthesize).toHaveBeenCalledTimes(before);
  });

  it('stops warming at the rate limit, on a missing engine or when cancelled', async () => {
    const limited = setup({
      synthesize: async () =>
        Promise.reject(Object.assign(new Error('too many spoken alerts'), { code: 'unavailable' })),
    });
    await expect(limited.voice.warm(['a', 'b', 'c'], 'en', server)).resolves.toMatchObject({
      failed: 1,
      reason: 'rate_limited',
    });
    expect(limited.synthesize).toHaveBeenCalledOnce();

    const controller = new AbortController();
    controller.abort();
    const cancelled = setup();
    await cancelled.voice.warm(['a', 'b'], 'en', server, controller.signal);
    expect(cancelled.synthesize).not.toHaveBeenCalled();

    const none = setup();
    await none.voice.warm(['a'], 'en', device);
    expect(none.synthesize).not.toHaveBeenCalled();
  });

  it('stores the voice source with the device voice as the default', () => {
    const d = defaultSettings();
    expect(d.ttsServerEngine).toBeNull();
    expect(parseSettings(JSON.stringify({ ttsServerEngine: 'local-tts' }), d).ttsServerEngine).toBe(
      'local-tts',
    );
    expect(parseSettings(JSON.stringify({ ttsServerEngine: 42 }), d).ttsServerEngine).toBeNull();
    expect(parseSettings(JSON.stringify({ ttsServerEngine: '' }), d).ttsServerEngine).toBeNull();
  });

  it('stores the PC speaker, empty meaning the default', () => {
    const d = defaultSettings();
    expect(d.ttsServerVoice).toBe('');
    expect(parseSettings(JSON.stringify({ ttsServerVoice: 'M3' }), d).ttsServerVoice).toBe('M3');
    expect(parseSettings(JSON.stringify({ ttsServerVoice: 7 }), d).ttsServerVoice).toBe('');
  });
});

describe('splitSpeech', () => {
  it('keeps short text whole and cuts long text between sentences', () => {
    expect(splitSpeech('  Done.  ')).toEqual(['Done.']);
    expect(splitSpeech('')).toEqual([]);
    const a = `${'가'.repeat(150)}.`;
    const b = `${'나'.repeat(150)}?`;
    expect(splitSpeech(`${a} ${b}`)).toEqual([a, b]);
    // Short sentences are packed together.
    expect(splitSpeech('One. Two. Three.', 10)).toEqual(['One. Two.', 'Three.']);
  });

  it('cuts a long sentence at commas, then spaces, then anywhere', () => {
    expect(splitSpeech('alpha beta, gamma delta', 12)).toEqual(['alpha beta,', 'gamma delta']);
    expect(splitSpeech('alpha beta gamma', 11)).toEqual(['alpha beta', 'gamma']);
    expect(splitSpeech('x'.repeat(25), 10)).toEqual([
      'x'.repeat(10),
      'x'.repeat(10),
      'x'.repeat(5),
    ]);
    for (const piece of splitSpeech('word '.repeat(200))) {
      expect(Array.from(piece).length).toBeLessThanOrEqual(CHUNK_CHARS);
    }
  });
});
