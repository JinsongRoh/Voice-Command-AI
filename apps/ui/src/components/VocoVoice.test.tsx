import { act, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ClientMsg } from '../protocol/types';
import { micBorrowed } from '../speech/handsFree';
import { VocoTestProvider, type VocoListenerLike } from '../speech/useVoco';
import type { HeardUtterance, VocoListenerOptions } from '../speech/vocoListener';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { Composer } from './Composer';

class FakeUtterance {
  voice: SpeechSynthesisVoice | null = null;
  lang = '';
  rate = 1;
  onend: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(public text: string) {}
}

/**
 * The "네" ("Yes?") after the wake word uses the voice the user chose, like every
 * other prompt: with a PC engine chosen it is the PC voice, never the
 * phone's own.
 */
describe('Voco speaks in the chosen voice', () => {
  const synth = {
    speak: vi.fn((u: FakeUtterance) => queueMicrotask(() => u.onend?.())),
    cancel: vi.fn(),
    getVoices: vi.fn(() => []),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    speaking: false,
  };
  const played: string[] = [];

  beforeEach(() => {
    vi.clearAllMocks();
    played.length = 0;
    vi.stubGlobal('SpeechSynthesisUtterance', FakeUtterance);
    Object.defineProperty(window, 'speechSynthesis', { value: synth, configurable: true });
    vi.spyOn(HTMLMediaElement.prototype, 'play').mockImplementation(async function (
      this: HTMLMediaElement,
    ) {
      played.push(this.src);
    });
    vi.spyOn(HTMLMediaElement.prototype, 'pause').mockImplementation(() => {});
    URL.createObjectURL = vi.fn(() => 'blob:clip');
    URL.revokeObjectURL = vi.fn();
  });
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    Reflect.deleteProperty(window, 'speechSynthesis');
    Reflect.deleteProperty(URL, 'createObjectURL');
    Reflect.deleteProperty(URL, 'revokeObjectURL');
  });

  function render(settings: object) {
    const transport = new FakeTransport((msg: ClientMsg) => {
      if (msg.type === 'list_tts_engines') {
        return {
          type: 'tts_engines',
          engines: [{ name: 'local-tts', languages: ['ko'], default: true }],
        };
      }
      if (msg.type === 'synthesize') {
        return { type: 'audio', format: 'mp3', data: new Uint8Array([1, 2, 3]) };
      }
      return defaultResponse(msg);
    });
    let options: VocoListenerOptions | null = null;
    const listener = {
      start: vi.fn<() => void>(),
      pause: vi.fn<() => Promise<void>>(async () => {}),
      resume: vi.fn<() => void>(),
      stop: vi.fn<() => Promise<void>>(async () => {}),
    } satisfies VocoListenerLike;
    const names = new Map<Blob, string>();
    const transcribe = vi.fn(async (audio: Blob) => ({
      text: names.get(audio) ?? '',
      corrections: [],
    }));
    const rec = {
      start: vi.fn(async () => {}),
      stop: vi.fn(async () => new Blob(['a'])),
      cancel: vi.fn(),
    };
    renderWithApp(
      <VocoTestProvider
        hooks={{
          createListener: (o) => {
            options = o;
            return listener;
          },
          chime: () => {},
        }}
      >
        <Composer session={localSession()} />
      </VocoTestProvider>,
      {
        transport,
        speech: { createRecorder: () => rec, transcribe },
        settings: { handsFree: true, handsFreeNoteSeen: true, language: 'ko', ...settings },
      },
    );
    const say = async (text: string) => {
      const blob = new Blob([text]);
      names.set(blob, text);
      const u: HeardUtterance = {
        audio: blob,
        head: () => blob,
        speechMs: 1000,
        pausedBeforeLast: false,
        endedAt: Date.now(),
      };
      await act(async () => {
        options!.onSpeechStart();
        options!.onSpeechEnd(u);
      });
    };
    return { transport, say, listener };
  }

  it('says "네" with the PC engine when one is chosen', async () => {
    const { transport, say, listener } = render({ ttsServerEngine: 'local-tts' });
    await waitFor(() => expect(listener.start).toHaveBeenCalled());
    await say('보코');
    await waitFor(() =>
      expect(transport.bodies('synthesize')).toContainEqual(
        expect.objectContaining({ type: 'synthesize', text: '네', engine: 'local-tts' }),
      ),
    );
    await waitFor(() => expect(played).toEqual(['blob:clip']));
    expect(synth.speak).not.toHaveBeenCalled();
    // The prompt is over: the voice and the microphone are handed back.
    await waitFor(() => expect(micBorrowed()).toBe(false));
  });

  it('says "네" with the device voice when that is chosen', async () => {
    const { transport, say, listener } = render({ ttsServerEngine: null });
    await waitFor(() => expect(listener.start).toHaveBeenCalled());
    await say('보코');
    await waitFor(() => expect(synth.speak).toHaveBeenCalled());
    expect((synth.speak.mock.calls[0]![0] as FakeUtterance).text).toBe('네');
    expect(transport.bodies('synthesize')).toEqual([]);
    expect(played).toEqual([]);
  });
});
