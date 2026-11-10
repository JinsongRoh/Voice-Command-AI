import { screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ClientMsg, ServerMsg, TtsEngineInfo } from '../protocol/types';
import type { Settings } from '../state/settings';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { renderWithApp } from '../test/render';
import { TtsSettings } from './TtsSettings';

function voice(name: string, lang: string, localService = true): SpeechSynthesisVoice {
  return { name, lang, localService, voiceURI: name, default: false };
}

class FakeUtterance {
  voice: SpeechSynthesisVoice | null = null;
  lang = '';
  rate = 1;
  constructor(public text: string) {}
}

describe('TtsSettings', () => {
  let voices: SpeechSynthesisVoice[];
  const synth = {
    speak: vi.fn(),
    cancel: vi.fn(),
    getVoices: vi.fn(() => voices),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  };

  beforeEach(() => {
    voices = [voice('Microsoft Zira - English', 'en-US'), voice('David', 'en-US')];
    vi.clearAllMocks();
    vi.stubGlobal('SpeechSynthesisUtterance', FakeUtterance);
    Object.defineProperty(window, 'speechSynthesis', { value: synth, configurable: true });
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    Reflect.deleteProperty(window, 'speechSynthesis');
  });

  it('lists Auto plus voices and hints when only legacy voices exist', () => {
    renderWithApp(<TtsSettings />, { settings: { language: 'en' } });
    const options = screen.getAllByRole('option').map((o) => o.textContent);
    expect(options).toEqual(['Auto (best available)', 'Microsoft Zira - English', 'David']);
    expect(screen.getByText(/Only basic voices are installed/)).toBeInTheDocument();
  });

  it('hides the hint when a Natural voice exists', () => {
    voices = [...voices, voice('Microsoft Aria Online (Natural)', 'en-US', false)];
    renderWithApp(<TtsSettings />, { settings: { language: 'en' } });
    expect(screen.queryByText(/Only basic voices are installed/)).toBeNull();
  });

  it('shows Auto when the saved voice has disappeared', () => {
    renderWithApp(<TtsSettings />, { settings: { language: 'en', ttsVoice: 'Gone Voice' } });
    expect(screen.getByRole('combobox')).toHaveValue('');
  });

  it('previews with the chosen voice and rate, without real audio', async () => {
    renderWithApp(<TtsSettings />, {
      settings: { language: 'en', ttsVoice: 'David', ttsRate: 1.2 },
    });
    await userEvent.click(screen.getByRole('button', { name: 'Preview' }));
    expect(synth.cancel).toHaveBeenCalled();
    const u = synth.speak.mock.calls[0]?.[0] as FakeUtterance;
    expect(u.voice?.name).toBe('David');
    expect(u.rate).toBe(1.2);
    expect(u.lang).toBe('en-US');
  });

  describe('with TTS engines on the PC', () => {
    const play = vi.fn(async () => {});

    const plain: TtsEngineInfo = { name: 'local-tts', languages: ['en'], default: true };
    const speakerEngine: TtsEngineInfo = {
      ...plain,
      speakers: [
        { id: 'F1', label: 'Female 1', gender: 'female' },
        { id: 'F2', label: 'Female 2', gender: 'female' },
        { id: 'M1', label: 'Male 1', gender: 'male' },
        { id: 'M2', label: 'Male 2', gender: 'male' },
        { id: 'M3', label: 'Male 3', gender: 'male' },
      ],
      disclosure: 'This voice is machine-generated.',
    };
    const audioReply: ServerMsg = {
      type: 'audio',
      format: 'mp3',
      data: new Uint8Array([73, 68, 51]),
    };

    function pcTransport(
      audio: (msg: Extract<ClientMsg, { type: 'synthesize' }>) => ServerMsg,
      engine: TtsEngineInfo = plain,
    ) {
      return new FakeTransport((msg) => {
        if (msg.type === 'list_tts_engines') {
          return { type: 'tts_engines', engines: [engine] };
        }
        if (msg.type === 'synthesize') return audio(msg);
        return defaultResponse(msg);
      });
    }

    beforeEach(() => {
      play.mockClear();
      // No real playback: the audio element is stubbed.
      vi.spyOn(HTMLMediaElement.prototype, 'play').mockImplementation(play);
      vi.spyOn(HTMLMediaElement.prototype, 'pause').mockImplementation(() => {});
      URL.createObjectURL = vi.fn(() => 'blob:alert');
      URL.revokeObjectURL = vi.fn();
    });
    afterEach(() => {
      vi.restoreAllMocks();
      Reflect.deleteProperty(URL, 'createObjectURL');
      Reflect.deleteProperty(URL, 'revokeObjectURL');
    });

    it('offers the device (default) and the PC engine as voice sources', async () => {
      renderWithApp(<TtsSettings />, {
        transport: pcTransport(() => ({ type: 'ok' })),
        settings: { language: 'en' },
      });
      const source = await screen.findByLabelText('Voice source');
      expect(source).toHaveValue('device:');
      expect(screen.getByRole('option', { name: 'PC server: local-tts' })).toBeInTheDocument();
      expect(screen.getByLabelText('Voice')).toBeInTheDocument();
      await userEvent.selectOptions(source, 'local-tts');
      expect(source).toHaveValue('local-tts');
      // Voice and speed belong to the device voice.
      expect(screen.queryByLabelText('Voice')).toBeNull();
      expect(screen.getByText(/this device's voice is used/)).toBeInTheDocument();
      await userEvent.selectOptions(source, 'device:');
      expect(screen.getByLabelText('Voice')).toBeInTheDocument();
    });

    it('previews through the PC engine and plays the returned audio', async () => {
      const transport = pcTransport(() => ({
        type: 'audio',
        format: 'mp3',
        data: new Uint8Array([73, 68, 51]),
      }));
      renderWithApp(<TtsSettings />, {
        transport,
        settings: { language: 'en', ttsServerEngine: 'local-tts' },
      });
      await screen.findByLabelText('Voice source');
      await userEvent.click(screen.getByRole('button', { name: 'Preview' }));
      await vi.waitFor(() => expect(play).toHaveBeenCalledOnce());
      expect(transport.bodies('synthesize')).toEqual([
        {
          type: 'synthesize',
          text: 'Spoken alert sample.',
          language: 'en-US',
          engine: 'local-tts',
          voice: null,
        },
      ]);
      expect(synth.speak).not.toHaveBeenCalled();
      // No speakers configured: no picker.
      expect(screen.queryByRole('group', { name: 'Gender' })).toBeNull();
      expect(screen.queryByLabelText('Speaker voice')).toBeNull();
    });

    describe('speakers', () => {
      const pc = (settings: Partial<Settings> = {}) => {
        const transport = pcTransport(() => audioReply, speakerEngine);
        renderWithApp(<TtsSettings />, {
          transport,
          settings: { language: 'en', ttsServerEngine: 'local-tts', ...settings },
        });
        return transport;
      };
      const optionNames = () =>
        within(screen.getByLabelText('Speaker voice'))
          .getAllByRole('option')
          .map((o) => o.textContent);

      it('starts on the first female speaker and shows the disclosure', async () => {
        pc();
        const voice = await screen.findByLabelText('Speaker voice');
        expect(voice).toHaveValue('F1');
        expect(screen.getByRole('button', { name: 'Female' })).toHaveAttribute(
          'aria-pressed',
          'true',
        );
        expect(screen.getByRole('button', { name: 'Male' })).toHaveAttribute(
          'aria-pressed',
          'false',
        );
        expect(optionNames()).toEqual(['Female 1', 'Female 2']);
        expect(screen.getByText('This voice is machine-generated.')).toBeInTheDocument();
      });

      it('filters the voices by gender and previews the chosen speaker', async () => {
        const transport = pc();
        await screen.findByLabelText('Speaker voice');
        await userEvent.click(screen.getByRole('button', { name: 'Male' }));
        expect(optionNames()).toEqual(['Male 1', 'Male 2', 'Male 3']);
        expect(screen.getByLabelText('Speaker voice')).toHaveValue('M1');
        await userEvent.selectOptions(screen.getByLabelText('Speaker voice'), 'M3');
        await userEvent.click(screen.getByRole('button', { name: 'Preview' }));
        await vi.waitFor(() => expect(play).toHaveBeenCalledOnce());
        expect(transport.bodies('synthesize')).toEqual([
          expect.objectContaining({ engine: 'local-tts', voice: 'M3' }),
        ]);
        // Switching back lists the female voices again.
        await userEvent.click(screen.getByRole('button', { name: 'Female' }));
        expect(optionNames()).toEqual(['Female 1', 'Female 2']);
        expect(screen.getByLabelText('Speaker voice')).toHaveValue('F1');
      });

      it('keeps a saved speaker that is still offered', async () => {
        pc({ ttsServerVoice: 'M2' });
        expect(await screen.findByLabelText('Speaker voice')).toHaveValue('M2');
        expect(screen.getByRole('button', { name: 'Male' })).toHaveAttribute(
          'aria-pressed',
          'true',
        );
      });

      it('falls back to the default when the saved speaker is gone', async () => {
        const transport = pc({ ttsServerVoice: 'X9' });
        expect(await screen.findByLabelText('Speaker voice')).toHaveValue('F1');
        await userEvent.click(screen.getByRole('button', { name: 'Preview' }));
        await vi.waitFor(() => expect(play).toHaveBeenCalledOnce());
        expect(transport.bodies('synthesize')).toEqual([expect.objectContaining({ voice: 'F1' })]);
      });

      it('has no picker for the device voice', async () => {
        const transport = pcTransport(() => audioReply, speakerEngine);
        renderWithApp(<TtsSettings />, { transport, settings: { language: 'en' } });
        await screen.findByLabelText('Voice source');
        expect(screen.queryByLabelText('Speaker voice')).toBeNull();
        expect(screen.queryByText('This voice is machine-generated.')).toBeNull();
      });
    });

    it('falls back to the device voice when the PC cannot speak', async () => {
      const transport = pcTransport(() => ({
        type: 'error',
        code: 'unavailable',
        message: 'too many spoken alerts; try again in a minute',
      }));
      renderWithApp(<TtsSettings />, {
        transport,
        settings: { language: 'en', ttsServerEngine: 'local-tts', ttsVoice: 'David' },
      });
      await screen.findByLabelText('Voice source');
      await userEvent.click(screen.getByRole('button', { name: 'Preview' }));
      await vi.waitFor(() => expect(synth.speak).toHaveBeenCalledOnce());
      const u = synth.speak.mock.calls[0]?.[0] as FakeUtterance;
      expect(u.text).toBe('Spoken alert sample.');
      expect(u.voice?.name).toBe('David');
      expect(play).not.toHaveBeenCalled();
    });
  });
});
