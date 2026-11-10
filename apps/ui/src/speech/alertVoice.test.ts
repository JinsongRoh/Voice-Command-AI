import { describe, expect, it, vi } from 'vitest';

import { defaultSettings, parseSettings } from '../state/settings';
import {
  createAlertVoice,
  type AlertAudio,
  type AlertVoice,
  type AlertVoiceDeps,
  type AlertVoicePrefs,
  type Synthesize,
} from './alertVoice';

const clip: AlertAudio = { format: 'mp3', data: new Uint8Array([1, 2, 3]) };

function setup(overrides: Partial<AlertVoiceDeps> & { synthesize?: Synthesize } = {}) {
  const { synthesize: custom, ...deps } = overrides;
  const synthesize = vi.fn<Synthesize>(custom ?? (async () => clip));
  const play = vi.fn<NonNullable<AlertVoiceDeps['play']>>(async () => {});
  const speakDevice = vi.fn<NonNullable<AlertVoiceDeps['speakDevice']>>();
  const created: AlertVoice = createAlertVoice({ play, speakDevice, ...deps });
  const voice = {
    speak: (text: string, lang: string, prefs: AlertVoicePrefs) =>
      created.speak(text, lang, prefs, synthesize),
    clear: () => created.clear(),
  };
  return { voice, synthesize, play, speakDevice };
}

const device = { serverEngine: null, voice: 'David', rate: 1.2 };
const server = { serverEngine: 'local-tts', voice: 'David', rate: 1.2 };

describe('alert voice', () => {
  it('uses the device voice by default and never asks the PC', async () => {
    const { voice, synthesize, speakDevice } = setup();
    await expect(voice.speak('Build failed', 'en', device)).resolves.toBe('device');
    expect(synthesize).not.toHaveBeenCalled();
    expect(speakDevice).toHaveBeenCalledWith('Build failed', 'en', { voice: 'David', rate: 1.2 });
    // Text over the PC's limit never leaves the device.
    await expect(voice.speak('a'.repeat(301), 'en', server)).resolves.toBe('device');
    expect(synthesize).not.toHaveBeenCalled();
  });

  it('plays audio from the chosen PC engine', async () => {
    const { voice, synthesize, play, speakDevice } = setup();
    await expect(voice.speak('Build failed', 'ko', server)).resolves.toBe('server');
    expect(synthesize).toHaveBeenCalledWith('Build failed', 'ko-KR', 'local-tts', null);
    expect(play).toHaveBeenCalledWith(clip);
    expect(speakDevice).not.toHaveBeenCalled();
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
    expect(failing.speakDevice).toHaveBeenCalledWith('Done', 'en', { voice: 'David', rate: 1.2 });

    const blocked = setup({ play: vi.fn(async () => Promise.reject(new Error('autoplay'))) });
    await expect(blocked.voice.speak('Done', 'en', server)).resolves.toBe('device');
    expect(blocked.speakDevice).toHaveBeenCalledOnce();

    const slow = setup({
      synthesize: () => new Promise<AlertAudio>(() => {}),
      timeoutMs: 5,
    });
    await expect(slow.voice.speak('Done', 'en', server)).resolves.toBe('device');
    expect(slow.speakDevice).toHaveBeenCalledOnce();
  });

  it('caches audio by engine, language and text, but not failures', async () => {
    const { voice, synthesize, play } = setup();
    await voice.speak('Build failed', 'en', server);
    await voice.speak('Build failed', 'en', server);
    expect(synthesize).toHaveBeenCalledTimes(1);
    expect(play).toHaveBeenCalledTimes(2);
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
