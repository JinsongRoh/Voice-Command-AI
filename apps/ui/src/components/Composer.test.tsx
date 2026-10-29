import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { cloudSession, localSession } from '../test/fixtures';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { renderWithApp } from '../test/render';
import { RequestError } from '../state/core';
import type { Transcript } from '../speech/transcribe';
import { Composer } from './Composer';

/** Controlled clock for recording durations (Date.now). */
let clock = 1_000_000;
const advance = (ms: number) => {
  clock += ms;
};

beforeEach(() => {
  clock = 1_000_000;
  vi.spyOn(Date, 'now').mockImplementation(() => clock);
});
afterEach(() => {
  vi.restoreAllMocks();
});

const tr = (text: string): Transcript => ({ text, corrections: [] });
const wav = (seconds: number, tag: string) => {
  const blob = new Blob([new Uint8Array(44 + Math.round(32_000 * seconds))], {
    type: 'audio/wav',
  });
  Object.defineProperty(blob, 'tag', { value: tag });
  return blob;
};
const tagOf = (b: Blob) => (b as Blob & { tag?: string }).tag;

function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

const HOLD = { micMode: 'hold' as const };

describe('Composer', () => {
  it('sends the typed command and shows the delivery status', async () => {
    const { transport } = renderWithApp(<Composer session={localSession()} />);
    const input = screen.getByRole('textbox', { name: 'Command for payments-api' });
    const send = screen.getByRole('button', { name: 'Send' });
    expect(send).toBeDisabled();
    await userEvent.type(input, 'run the tests');
    await userEvent.click(send);
    await waitFor(() =>
      expect(transport.bodies('send_input')).toEqual([
        {
          type: 'send_input',
          session_id: 's1',
          input: { type: 'text', text: 'run the tests', submit: true },
        },
      ]),
    );
    expect(await screen.findByText('Delivered')).toBeInTheDocument();
    expect(input).toHaveValue('');
  });

  it('shows queued delivery', async () => {
    const transport = new FakeTransport((msg) =>
      msg.type === 'send_input'
        ? { type: 'input_ack', session_id: 's1', status: 'queued' }
        : defaultResponse(msg),
    );
    renderWithApp(<Composer session={localSession()} />, { transport });
    await userEvent.type(screen.getByRole('textbox'), 'x');
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    expect(await screen.findByText(/Queued/)).toBeInTheDocument();
  });

  it('inserts a template and sends control keys', async () => {
    const { transport } = renderWithApp(<Composer session={localSession()} />);
    await userEvent.click(
      screen.getByRole('button', { name: 'Run the tests and fix any failures.' }),
    );
    expect(screen.getByRole('textbox')).toHaveValue('Run the tests and fix any failures.');
    await userEvent.click(screen.getByRole('button', { name: 'Interrupt' }));
    await waitFor(() =>
      expect(transport.bodies('send_input')).toContainEqual({
        type: 'send_input',
        session_id: 's1',
        input: { type: 'key', key: 'interrupt' },
      }),
    );
  });

  it('toggles the text box between auto-grow and expanded', async () => {
    renderWithApp(<Composer session={localSession()} />);
    const expand = screen.getByRole('button', { name: 'Expand text box' });
    expect(expand).toHaveAttribute('aria-pressed', 'false');
    await userEvent.click(expand);
    const collapse = screen.getByRole('button', { name: 'Shrink text box' });
    expect(collapse).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByRole('textbox').parentElement?.className).toMatch(/expanded/);
    await userEvent.click(collapse);
    expect(screen.getByRole('textbox').parentElement?.className).not.toMatch(/expanded/);
  });

  describe('hold mode (push-to-talk)', () => {
    it('records while held and fills the transcript with corrections', async () => {
      const audio = new Blob(['a']);
      const recorder = {
        start: vi.fn(async () => {}),
        stop: vi.fn(async () => audio),
        cancel: vi.fn(),
      };
      const transcribe = vi.fn(async () => ({
        text: 'fix the useEffect warning',
        corrections: [{ from: 'use effect', to: 'useEffect', layer: 'base' as const }],
      }));
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe },
        settings: HOLD,
      });
      const mic = screen.getByRole('button', { name: 'Hold to talk' });
      await act(async () => {
        fireEvent.pointerDown(mic, { pointerId: 1 });
      });
      expect(recorder.start).toHaveBeenCalled();
      expect(screen.getByRole('button', { name: /Recording/ })).toHaveAttribute(
        'aria-pressed',
        'true',
      );
      expect(screen.queryByRole('button', { name: 'Cancel recording' })).not.toBeInTheDocument();
      advance(1500);
      await act(async () => {
        fireEvent.pointerUp(mic, { pointerId: 1 });
      });
      await waitFor(() =>
        expect(screen.getByRole('textbox')).toHaveValue('fix the useEffect warning'),
      );
      expect(transcribe).toHaveBeenCalledWith(
        audio,
        expect.objectContaining({ engine: 'whisper_server', sessionId: 's1' }),
      );
      expect(
        screen.getByText(/Vocabulary corrections: “use effect” → useEffect/),
      ).toBeInTheDocument();
      expect(document.querySelector('mark')).toHaveTextContent('useEffect');
      expect(screen.getByRole('button', { name: 'Hold to talk' })).toHaveAttribute(
        'aria-pressed',
        'false',
      );
    });

    it('supports Space as press-and-release', async () => {
      const transcribe = vi.fn(async () => tr('spoken'));
      renderWithApp(<Composer session={localSession()} />, {
        speech: { transcribe },
        settings: HOLD,
      });
      const mic = screen.getByRole('button', { name: 'Hold to talk' });
      await act(async () => {
        fireEvent.keyDown(mic, { key: ' ' });
      });
      expect(mic).toHaveAttribute('aria-pressed', 'true');
      advance(800);
      await act(async () => {
        fireEvent.keyUp(mic, { key: ' ' });
      });
      await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue('spoken'));
    });

    it('discards an accidental tap with a hold hint', async () => {
      const recorder = {
        start: vi.fn(async () => {}),
        stop: vi.fn(async () => new Blob(['a'])),
        cancel: vi.fn(),
      };
      const transcribe = vi.fn(async () => tr('nope'));
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe },
        settings: HOLD,
      });
      const mic = screen.getByRole('button', { name: 'Hold to talk' });
      await act(async () => {
        fireEvent.pointerDown(mic, { pointerId: 1 });
      });
      advance(120);
      await act(async () => {
        fireEvent.pointerUp(mic, { pointerId: 1 });
      });
      expect(await screen.findByText('Too short — hold to talk')).toBeInTheDocument();
      expect(transcribe).not.toHaveBeenCalled();
      expect(recorder.cancel).toHaveBeenCalled();
      expect(recorder.stop).not.toHaveBeenCalled();
    });

    it('reports an unavailable microphone', async () => {
      const recorder = {
        start: vi.fn(async () => {
          throw new Error('Permission denied');
        }),
        stop: vi.fn(async () => new Blob()),
        cancel: vi.fn(),
      };
      const transcribe = vi.fn();
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe },
        settings: HOLD,
      });
      const mic = screen.getByRole('button', { name: 'Hold to talk' });
      await act(async () => {
        fireEvent.pointerDown(mic, { pointerId: 1 });
      });
      expect(await screen.findByRole('alert')).toHaveTextContent(
        'Microphone unavailable: Permission denied',
      );
      advance(1000);
      await act(async () => {
        fireEvent.pointerUp(mic, { pointerId: 1 });
      });
      expect(transcribe).not.toHaveBeenCalled();
    });
  });

  async function talk(error: Error) {
    const transcribe = vi.fn(async () => {
      throw error;
    });
    renderWithApp(<Composer session={localSession()} />, { speech: { transcribe } });
    const mic = screen.getByRole('button', { name: 'Tap to dictate' });
    await act(async () => {
      fireEvent.click(mic);
    });
    advance(1000);
    await act(async () => {
      fireEvent.click(mic);
    });
    await waitFor(() => expect(transcribe).toHaveBeenCalled());
  }

  it('shows a neutral notice when speech recognition is off on the server', async () => {
    await talk(
      new RequestError(
        'unsupported',
        'server-side transcription is off (set [speech] engine in config.toml)',
      ),
    );
    expect(
      await screen.findByText(/speech recognition is turned off on that machine/),
    ).toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('keeps other transcription failures as errors', async () => {
    await talk(new RequestError('unavailable', 'speech is shutting down'));
    expect(await screen.findByRole('alert')).toHaveTextContent('speech is shutting down');
    expect(screen.queryByText(/turned off on that machine/)).not.toBeInTheDocument();
  });

  describe('toggle mode (tap to dictate, the default)', () => {
    function simpleRecorder(audio: Blob = new Blob(['a'])) {
      return {
        start: vi.fn(async () => {}),
        stop: vi.fn(async () => audio),
        cancel: vi.fn(),
      };
    }

    it('taps to start, shows the recording state and taps to stop', async () => {
      const recorder = simpleRecorder();
      const transcribe = vi.fn(async () => tr('commit the changes'));
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe },
      });
      const input = screen.getByRole('textbox');
      await userEvent.type(input, 'Please');
      const mic = screen.getByRole('button', { name: 'Tap to dictate' });
      expect(mic).toHaveAttribute('aria-pressed', 'false');
      await act(async () => {
        fireEvent.click(mic);
      });
      expect(recorder.start).toHaveBeenCalled();
      expect(mic).toHaveAccessibleName('Dictating… tap to stop');
      expect(mic).toHaveAttribute('aria-pressed', 'true');
      expect(screen.getByRole('timer', { name: 'Recording time' })).toHaveTextContent('00:00');
      expect(screen.getByRole('button', { name: 'Cancel recording' })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: 'Send' })).toBeDisabled();

      advance(65_000);
      await waitFor(() => expect(screen.getByRole('timer')).toHaveTextContent('01:05'));

      await act(async () => {
        fireEvent.click(mic);
      });
      // Existing text is kept; dictation is appended after a space.
      await waitFor(() => expect(input).toHaveValue('Please commit the changes'));
      expect(transcribe).toHaveBeenCalledTimes(1);
      expect(transcribe).toHaveBeenCalledWith(
        expect.any(Blob),
        expect.objectContaining({ context: 'Please', sessionId: 's1' }),
      );
      expect(mic).toHaveAccessibleName('Tap to dictate');
      expect(screen.queryByRole('timer')).not.toBeInTheDocument();
      // Still editable after stop.
      await userEvent.type(input, '!');
      expect(input).toHaveValue('Please commit the changes!');
    });

    it('toggles with Enter and Space from the keyboard', async () => {
      const recorder = simpleRecorder();
      const transcribe = vi.fn(async () => tr('hello'));
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe },
      });
      const mic = screen.getByRole('button', { name: 'Tap to dictate' });
      mic.focus();
      await userEvent.keyboard('{Enter}');
      expect(mic).toHaveAttribute('aria-pressed', 'true');
      advance(2000);
      await userEvent.keyboard(' ');
      await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue('hello'));
      expect(mic).toHaveAttribute('aria-pressed', 'false');
    });

    it('cancel discards this run and restores the previous text', async () => {
      const recorder = {
        ...simpleRecorder(),
        peek: vi.fn(async () => wav(2, 'peek')),
        split: vi.fn(async () => new Blob([], { type: 'audio/wav' })),
      };
      const transcribe = vi.fn(async () => tr('half a thought'));
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe },
      });
      const input = screen.getByRole('textbox');
      await userEvent.type(input, 'Draft');
      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: 'Tap to dictate' }));
      });
      await waitFor(() => expect(input).toHaveValue('Draft half a thought'));
      await userEvent.click(screen.getByRole('button', { name: 'Cancel recording' }));
      expect(input).toHaveValue('Draft');
      expect(recorder.cancel).toHaveBeenCalled();
      expect(recorder.stop).not.toHaveBeenCalled();
      expect(screen.getByRole('button', { name: 'Tap to dictate' })).toHaveFocus();
    });

    it('discards taps shorter than 0.3 s without calling the server', async () => {
      const recorder = simpleRecorder();
      const transcribe = vi.fn(async () => tr('x'));
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe },
      });
      const mic = screen.getByRole('button', { name: 'Tap to dictate' });
      await act(async () => {
        fireEvent.click(mic);
      });
      advance(200);
      await act(async () => {
        fireEvent.click(mic);
      });
      expect(
        await screen.findByText('Too short — tap to start, tap again to stop'),
      ).toBeInTheDocument();
      expect(transcribe).not.toHaveBeenCalled();
      expect(recorder.cancel).toHaveBeenCalled();
    });

    it('discards a WAV clip with under 0.3 s of audio', async () => {
      const recorder = simpleRecorder(wav(0.1, 'tiny'));
      const transcribe = vi.fn(async () => tr('x'));
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe },
      });
      const mic = screen.getByRole('button', { name: 'Tap to dictate' });
      await act(async () => {
        fireEvent.click(mic);
      });
      advance(1000);
      await act(async () => {
        fireEvent.click(mic);
      });
      expect(await screen.findByText(/Too short/)).toBeInTheDocument();
      expect(transcribe).not.toHaveBeenCalled();
    });

    it('writes the live transcript into the box, dims the tail and finalizes on stop', async () => {
      const interim = deferred<Transcript>();
      const recorder = {
        ...simpleRecorder(wav(3, 'rest')),
        peek: vi.fn(async () => wav(2, 'peek')),
        split: vi.fn(async () => new Blob([], { type: 'audio/wav' })),
      };
      const transcribe = vi.fn((audio: Blob) =>
        tagOf(audio) === 'peek' ? interim.promise : Promise.resolve(tr('run the tests now')),
      );
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe },
      });
      const input = screen.getByRole('textbox');
      const mic = screen.getByRole('button', { name: 'Tap to dictate' });
      await act(async () => {
        fireEvent.click(mic);
      });
      await waitFor(() => expect(recorder.peek).toHaveBeenCalledTimes(1));
      // One interim request in flight: no second peek while it is pending.
      await act(async () => {
        await new Promise((r) => setTimeout(r, 900));
      });
      expect(recorder.peek).toHaveBeenCalledTimes(1);
      await act(async () => {
        interim.resolve(tr('run the'));
      });
      await waitFor(() => expect(input).toHaveValue('run the'));
      expect(input.parentElement?.className).toMatch(/live/);
      expect(document.querySelector('[class*="tail"]')).toHaveTextContent('run the');

      advance(5000);
      await act(async () => {
        fireEvent.click(mic);
      });
      await waitFor(() => expect(input).toHaveValue('run the tests now'));
      expect(input.parentElement?.className).not.toMatch(/live/);
      expect(document.querySelector('[class*="tail"]')).toBeNull();
      expect(transcribe).toHaveBeenLastCalledWith(
        expect.objectContaining({ tag: 'rest' }),
        expect.objectContaining({ context: null }),
      );
    });

    it('typing during dictation stops live updates and keeps the text', async () => {
      const late = deferred<Transcript>();
      const recorder = {
        ...simpleRecorder(),
        peek: vi.fn(async () => wav(2, 'peek')),
        split: vi.fn(async () => new Blob([], { type: 'audio/wav' })),
      };
      let calls = 0;
      const transcribe = vi.fn(() => (++calls === 1 ? Promise.resolve(tr('hello')) : late.promise));
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe },
      });
      const input = screen.getByRole('textbox');
      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: 'Tap to dictate' }));
      });
      await waitFor(() => expect(input).toHaveValue('hello'));
      await userEvent.type(input, ' world');
      expect(input).toHaveValue('hello world');
      expect(recorder.cancel).toHaveBeenCalled();
      expect(screen.getByRole('button', { name: 'Tap to dictate' })).toHaveAttribute(
        'aria-pressed',
        'false',
      );
      await act(async () => {
        late.resolve(tr('LATE'));
      });
      expect(input).toHaveValue('hello world');
    });
  });

  describe('original (raw) transcript', () => {
    const corrected: Transcript = {
      text: 'run pnpm test',
      rawText: 'um run p n p m test',
      corrections: [{ from: 'p n p m', to: 'pnpm', layer: 'base' }],
    };

    async function dictate(transcript: Transcript) {
      const transcribe = vi.fn(async () => transcript);
      renderWithApp(<Composer session={localSession()} />, { speech: { transcribe } });
      const input = screen.getByRole('textbox');
      await userEvent.type(input, 'Please');
      const mic = screen.getByRole('button', { name: 'Tap to dictate' });
      await act(async () => {
        fireEvent.click(mic);
      });
      advance(2000);
      await act(async () => {
        fireEvent.click(mic);
      });
      await waitFor(() => expect(input).toHaveValue(`Please ${transcript.text}`));
      return input;
    }

    it('shows the raw transcript with corrected terms tagged by layer', async () => {
      await dictate(corrected);
      const toggle = screen.getByRole('button', { name: 'Show original' });
      expect(toggle).toHaveAttribute('aria-expanded', 'false');
      await userEvent.click(toggle);
      expect(screen.getByRole('button', { name: 'Hide original' })).toHaveAttribute(
        'aria-expanded',
        'true',
      );
      const panel = screen.getByRole('region', { name: 'Original transcription' });
      const term = panel.querySelector('mark')!;
      expect(term).toHaveAttribute('data-layer', 'base');
      expect(term).toHaveAttribute('title', 'Corrected to “pnpm” (Built-in vocabulary)');
      expect(term.firstChild).toHaveTextContent('p n p m');
      expect(panel.querySelector('p')).toHaveTextContent(/^um run p n p m/);
      await userEvent.click(screen.getByRole('button', { name: 'Hide original' }));
      expect(screen.queryByRole('region', { name: 'Original transcription' })).toBeNull();
    });

    it('"Use original" swaps the dictated part for the raw text', async () => {
      const input = await dictate(corrected);
      await userEvent.click(screen.getByRole('button', { name: 'Show original' }));
      await userEvent.click(screen.getByRole('button', { name: 'Use original' }));
      expect(input).toHaveValue('Please um run p n p m test');
      expect(screen.queryByText(/Vocabulary corrections/)).not.toBeInTheDocument();
      expect(screen.queryByRole('button', { name: /original/i })).not.toBeInTheDocument();
    });

    it('hides "Use original" once the dictated text was edited away', async () => {
      const input = await dictate(corrected);
      await userEvent.clear(input);
      await userEvent.type(input, 'something else');
      await userEvent.click(screen.getByRole('button', { name: 'Show original' }));
      expect(screen.getByRole('region', { name: 'Original transcription' })).toBeInTheDocument();
      expect(screen.queryByRole('button', { name: 'Use original' })).not.toBeInTheDocument();
    });

    it('offers no toggle when nothing was corrected', async () => {
      await dictate({ text: 'plain words', rawText: 'plain words', corrections: [] });
      expect(screen.queryByRole('button', { name: 'Show original' })).not.toBeInTheDocument();
    });
  });

  it('hides terminal keys for cloud sessions and disables input for exited ones', () => {
    renderWithApp(<Composer session={cloudSession()} />);
    expect(screen.queryByRole('button', { name: 'Interrupt' })).not.toBeInTheDocument();
  });

  it('disables input for exited sessions', () => {
    renderWithApp(<Composer session={localSession({ state: { type: 'exited', code: 0 } })} />);
    expect(screen.getByText('This session cannot receive input.')).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toBeDisabled();
  });
});
