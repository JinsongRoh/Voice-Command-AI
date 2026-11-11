import { act, cleanup, fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { cloudSession, localSession } from '../test/fixtures';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { installFakeViewport, type FakeViewport } from '../test/fakeViewport';
import { renderWithApp } from '../test/render';
import { trackVisualViewport } from '../hooks/visualViewport';
import { clearDiag, diagEntries, setDiagEnabled } from '../diag/log';
import { originLabel } from '../protocol/helpers';
import { RequestError } from '../state/core';
import { useActions } from '../state/store';
import { MIC_STALL_MS, MIN_REQUEST_GAP_MS } from '../speech/dictation';
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
  // The log is opt-in; some tests check what it records.
  setDiagEnabled(true);
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

/** Sends a raw key the way the key bar does. */
function Probe() {
  const actions = useActions();
  return (
    <button type="button" onClick={() => void actions.sendRaw('s1', '1')}>
      raw
    </button>
  );
}

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
          input: { type: 'text', text: 'run the tests', submit: true, at_prompt: true },
        },
      ]),
    );
    await waitFor(() => expect(screen.getByTestId('delivery-word')).toHaveTextContent('Delivered'));
    // The dot also gets a short word for a moment (the status line says it too).
    expect(screen.getAllByText('Delivered')).toHaveLength(2);
    expect(input).toHaveValue('');
    // Sending plays the converging waveform once.
    expect(screen.getByTestId('voice-wave')).toHaveAttribute('data-mode', 'converge');
    await waitFor(() => expect(screen.queryByTestId('voice-wave')).not.toBeInTheDocument());
  });

  it('keeps an unsent draft per session across a remount and forgets it once sent', async () => {
    const first = renderWithApp(<Composer key="s1" session={localSession()} />);
    await userEvent.type(
      screen.getByRole('textbox', { name: 'Command for payments-api' }),
      'half a thought',
    );
    // Switching sessions remounts the composer (LiveView keys it by session id).
    first.unmount();
    const other = renderWithApp(
      <Composer key="s2" session={localSession({ id: 's2', display_name: 'web-app' })} />,
    );
    expect(screen.getByRole('textbox', { name: 'Command for web-app' })).toHaveValue('');
    other.unmount();

    renderWithApp(<Composer key="s1" session={localSession()} />);
    const input = screen.getByRole('textbox', { name: 'Command for payments-api' });
    expect(input).toHaveValue('half a thought');
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(input).toHaveValue(''));
    cleanup();

    renderWithApp(<Composer key="s1" session={localSession()} />);
    expect(screen.getByRole('textbox', { name: 'Command for payments-api' })).toHaveValue('');
  });

  it('a typed send may answer an open prompt: it says a person typed it', async () => {
    const waiting = localSession({
      state: { type: 'awaiting_decision' },
      pending_decision: {
        at: 100,
        decision: { id: 'd1', prompt: 'Allow?', options: [{ id: 'yes', label: 'Yes' }] },
      },
    });
    const { transport } = renderWithApp(<Composer session={waiting} />);
    await userEvent.type(screen.getByRole('textbox', { name: 'Command for payments-api' }), '2');
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() =>
      expect(transport.bodies('send_input')).toEqual([
        {
          type: 'send_input',
          session_id: 's1',
          input: { type: 'text', text: '2', submit: true, at_prompt: true },
        },
      ]),
    );
  });

  it('terminal keys a person taps may answer a prompt', async () => {
    const { transport } = renderWithApp(<Probe />);
    await userEvent.click(screen.getByRole('button', { name: 'raw' }));
    await waitFor(() =>
      expect(transport.bodies('send_input')).toEqual([
        {
          type: 'send_input',
          session_id: 's1',
          input: { type: 'text', text: '1', submit: false, at_prompt: true },
        },
      ]),
    );
  });

  it('names the target session in the text box (the selected card shows it visually)', () => {
    renderWithApp(<Composer session={localSession({ display_name: 'Checkout fix' })} />);
    const input = screen.getByRole('textbox', { name: 'Command for Checkout fix' });
    expect(input).toHaveAccessibleDescription(
      new RegExp(`^To Checkout fix · ${originLabel(localSession().origin)} · payments-api`),
    );
    expect(input).toHaveAttribute('placeholder', 'Speak or type to Checkout fix…');
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

  it('offers Command Center prompts instead of coding prompts to the conductor', async () => {
    renderWithApp(<Composer session={localSession({ conductor: true })} />);
    await userEvent.click(screen.getByRole('button', { name: 'Tell me what is waiting for me.' }));
    expect(screen.getByRole('textbox')).toHaveValue('Tell me what is waiting for me.');
    expect(
      screen.queryByRole('button', { name: 'Run the tests and fix any failures.' }),
    ).toBeNull();
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

  it('offers Undo when a quick prompt replaces a draft', async () => {
    renderWithApp(<Composer session={localSession()} />);
    const input = screen.getByRole('textbox');
    // An empty box is simply filled: nothing to undo.
    await userEvent.click(
      screen.getByRole('button', { name: 'Summarize what you changed so far.' }),
    );
    expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull();
    await userEvent.clear(input);
    await userEvent.type(input, 'my long dictated draft');
    await userEvent.click(
      screen.getByRole('button', { name: 'Run the tests and fix any failures.' }),
    );
    expect(input).toHaveValue('Run the tests and fix any failures.');
    expect(screen.getByText('Draft replaced.')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Undo' }));
    expect(input).toHaveValue('my long dictated draft');
    expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull();
    // Editing the inserted text drops the offer.
    await userEvent.click(
      screen.getByRole('button', { name: 'Run the tests and fix any failures.' }),
    );
    expect(screen.getByRole('button', { name: 'Undo' })).toBeInTheDocument();
    await userEvent.type(input, '!');
    expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull();
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
      expect(transcribe).toHaveBeenCalledWith(audio, expect.objectContaining({ sessionId: 's1' }));
      expect(
        screen.getByText(/Vocabulary corrections: “use effect” → useEffect/),
      ).toBeInTheDocument();
      expect(document.querySelector('mark')).toHaveTextContent('useEffect');
      expect(screen.getByRole('button', { name: 'Hold to talk' })).toHaveAttribute(
        'aria-pressed',
        'false',
      );
    });

    it('discards the recording on pointercancel, Escape or the page going hidden', async () => {
      const recorder = {
        start: vi.fn(async () => {}),
        stop: vi.fn(async () => new Blob(['a'])),
        cancel: vi.fn(),
      };
      const transcribe = vi.fn(async () => tr('should not appear'));
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe },
        settings: HOLD,
      });
      const input = screen.getByRole('textbox');
      const hold = async () => {
        const mic = screen.getByRole('button', { name: 'Hold to talk' });
        await act(async () => {
          fireEvent.pointerDown(mic, { pointerId: 1 });
        });
        expect(screen.getByRole('button', { name: /Recording/ })).toHaveAttribute(
          'aria-pressed',
          'true',
        );
        advance(1500);
        return mic;
      };
      const expectDiscarded = async (times: number) => {
        await waitFor(() =>
          expect(screen.getByRole('button', { name: 'Hold to talk' })).toHaveAttribute(
            'aria-pressed',
            'false',
          ),
        );
        expect(recorder.cancel).toHaveBeenCalledTimes(times);
        expect(transcribe).not.toHaveBeenCalled();
        expect(input).toHaveValue('');
      };

      // A call or a system gesture takes the pointer.
      let mic = await hold();
      await act(async () => {
        fireEvent.pointerCancel(mic, { pointerId: 1 });
      });
      await expectDiscarded(1);

      // Escape, wherever the focus is.
      mic = await hold();
      await act(async () => {
        fireEvent.keyDown(document.body, { key: 'Escape' });
      });
      await expectDiscarded(2);
      // The finger lifting afterwards does not transcribe anything.
      await act(async () => {
        fireEvent.pointerUp(mic, { pointerId: 1 });
      });

      // The phone locks or another app comes to the front.
      await hold();
      const visibility = vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden');
      await act(async () => {
        document.dispatchEvent(new Event('visibilitychange'));
      });
      visibility.mockRestore();
      await expectDiscarded(3);
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
    expect(await screen.findByText(/speech recognition is off there/)).toBeInTheDocument();
    // It points at the screen on the PC, not at config.toml.
    expect(screen.getByText(/Settings › Voice › Speech engines/)).toBeInTheDocument();
    expect(screen.queryByText(/config\.toml/)).not.toBeInTheDocument();
    // This device has no speech engine screen (not the desktop app on that PC).
    expect(screen.queryByRole('button', { name: 'Open speech engines' })).toBeNull();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('keeps the recording when the network fails it and sends it again on Retry', async () => {
    const transcribe = vi
      .fn<(audio: Blob) => Promise<Transcript>>()
      .mockRejectedValueOnce(new RequestError('transport', 'connection lost (transport_error)'))
      .mockResolvedValueOnce(tr('recovered words'));
    renderWithApp(<Composer session={localSession()} />, { speech: { transcribe } });
    const mic = screen.getByRole('button', { name: 'Tap to dictate' });
    await act(async () => {
      fireEvent.click(mic);
    });
    advance(1000);
    await act(async () => {
      fireEvent.click(mic);
    });
    expect(await screen.findByRole('alert')).toHaveTextContent(/network is weak.*Retry/);
    const firstAudio = transcribe.mock.calls[0]![0];
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    });
    await waitFor(() =>
      expect(screen.getByRole('textbox', { name: 'Command for payments-api' })).toHaveValue(
        'recovered words',
      ),
    );
    // The same audio went out again; nothing had to be said twice.
    expect(transcribe).toHaveBeenCalledTimes(2);
    expect(transcribe.mock.calls[1]![0]).toBe(firstAudio);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Retry' })).toBeNull();
  });

  it('keeps other transcription failures as errors', async () => {
    await talk(new RequestError('unavailable', 'speech is shutting down'));
    expect(await screen.findByRole('alert')).toHaveTextContent('speech is shutting down');
    expect(screen.queryByText(/speech recognition is off there/)).not.toBeInTheDocument();
  });

  describe('toggle mode (tap to dictate, the default)', () => {
    function simpleRecorder(audio: Blob = new Blob(['a'])) {
      return {
        start: vi.fn(async () => {}),
        stop: vi.fn(async () => audio),
        cancel: vi.fn(),
      };
    }

    it('shows the recording time beside the attach button on phones', async () => {
      const recorder = simpleRecorder();
      renderWithApp(<Composer session={localSession()} layout="phone" />, {
        speech: { createRecorder: () => recorder, transcribe: vi.fn(async () => tr('x')) },
      });
      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: 'Tap to dictate' }));
      });
      const row = screen.getByTestId('composer-bottom-row');
      // In the button row, not on a line of its own below it.
      expect(within(row).getByRole('timer', { name: 'Recording time' })).toHaveTextContent('00:00');
      expect(screen.getAllByRole('timer')).toHaveLength(1);
    });

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
      // The button shows the brand symbol (cyan-to-blue gradient, decorative) at rest.
      const logo = mic.querySelector('svg[data-logo="symbol"]');
      expect(logo).toHaveAttribute('aria-hidden', 'true');
      expect(logo?.querySelector('[data-part="symbol"]')?.getAttribute('fill')).toMatch(
        /^url\(#vca-logo-/,
      );
      expect(logo?.querySelectorAll('[data-part="symbol"] path')).toHaveLength(5);
      expect(mic).toHaveAttribute('data-phase', 'idle');
      await act(async () => {
        fireEvent.click(mic);
      });
      expect(recorder.start).toHaveBeenCalled();
      expect(mic).toHaveAccessibleName('Dictating… tap to stop');
      expect(mic).toHaveAttribute('aria-pressed', 'true');
      expect(mic).toHaveAttribute('data-phase', 'recording');
      expect(screen.getByRole('timer', { name: 'Recording time' })).toHaveTextContent('00:00');
      // The five-stroke waveform replaces the old pulsing dot.
      expect(screen.getByTestId('voice-wave')).toHaveAttribute('data-mode', 'live');
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
      expect(mic).toHaveAttribute('data-phase', 'idle');
      expect(screen.queryByRole('timer')).not.toBeInTheDocument();
      expect(screen.queryByTestId('voice-wave')).not.toBeInTheDocument();
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

    it('opens the microphone on the press, not when the finger comes up', async () => {
      const recorder = simpleRecorder();
      const transcribe = vi.fn(async () => tr('said'));
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe },
      });
      const mic = screen.getByRole('button', { name: 'Tap to dictate' });
      await act(async () => {
        fireEvent.pointerDown(mic, { pointerId: 1, button: 0 });
      });
      expect(recorder.start).toHaveBeenCalledTimes(1);
      // The click of that same tap does not stop what it started.
      await act(async () => {
        fireEvent.pointerUp(mic, { pointerId: 1 });
        fireEvent.click(mic);
      });
      expect(recorder.stop).not.toHaveBeenCalled();
      expect(mic).toHaveAttribute('aria-pressed', 'true');
      advance(1000);
      await act(async () => {
        fireEvent.pointerDown(mic, { pointerId: 1, button: 0 });
        fireEvent.pointerUp(mic, { pointerId: 1 });
        fireEvent.click(mic);
      });
      await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue('said'));
      expect(recorder.stop).toHaveBeenCalledTimes(1);
    });

    it('a press that became a scroll drops the recording it started', async () => {
      const recorder = simpleRecorder();
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe: vi.fn(async () => tr('x')) },
      });
      const mic = screen.getByRole('button', { name: 'Tap to dictate' });
      await act(async () => {
        fireEvent.pointerDown(mic, { pointerId: 1, button: 0 });
      });
      expect(recorder.start).toHaveBeenCalledTimes(1);
      await act(async () => {
        fireEvent.pointerCancel(mic, { pointerId: 1 });
      });
      expect(recorder.cancel).toHaveBeenCalled();
      expect(screen.getByRole('button', { name: 'Tap to dictate' })).toHaveAttribute(
        'aria-pressed',
        'false',
      );
    });

    it('a secondary button does not start a recording', async () => {
      const recorder = simpleRecorder();
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe: vi.fn(async () => tr('x')) },
      });
      await act(async () => {
        fireEvent.pointerDown(screen.getByRole('button', { name: 'Tap to dictate' }), {
          pointerId: 1,
          button: 2,
        });
      });
      expect(recorder.start).not.toHaveBeenCalled();
    });

    it('a press released off the button leaves the next tap able to stop', async () => {
      const recorder = simpleRecorder();
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe: vi.fn(async () => tr('x')) },
      });
      const mic = screen.getByRole('button', { name: 'Tap to dictate' });
      // No click follows this press.
      await act(async () => {
        fireEvent.pointerDown(mic, { pointerId: 1, button: 0 });
      });
      advance(1000);
      await act(async () => {
        fireEvent.pointerDown(mic, { pointerId: 2, button: 0 });
        fireEvent.click(mic);
      });
      await waitFor(() => expect(recorder.stop).toHaveBeenCalledTimes(1));
    });

    it('logs the time from the press to the first audio, in milliseconds only', async () => {
      clearDiag();
      const recorder = { ...simpleRecorder(), captured: vi.fn(async () => 160) };
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe: vi.fn(async () => tr('x')) },
      });
      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: 'Tap to dictate' }));
      });
      const timing = () => diagEntries().find((e) => e.event === 'composer.dictate.timing');
      await waitFor(() => expect(timing()).toBeDefined());
      const { data } = timing()!;
      expect(Object.keys(data).sort()).toEqual(['firstFrameMs', 'openMs', 'releaseMs']);
      expect(Object.values(data).every((v) => typeof v === 'number')).toBe(true);
    });

    it('tells the user when the microphone was interrupted and keeps the text', async () => {
      const recorder = {
        ...simpleRecorder(wav(2, 'after')),
        peek: vi.fn(async () => wav(2, 'peek')),
        split: vi.fn(async () => new Blob([], { type: 'audio/wav' })),
        captured: vi.fn(async () => 1000),
      };
      const transcribe = vi.fn(async () => tr('kept'));
      renderWithApp(<Composer session={localSession()} />, {
        speech: { createRecorder: () => recorder, transcribe },
      });
      await act(async () => {
        fireEvent.click(screen.getByRole('button', { name: 'Tap to dictate' }));
      });
      await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue('kept'));
      await act(async () => {
        advance(MIC_STALL_MS + 1);
        await new Promise((resolve) => setTimeout(resolve, MIN_REQUEST_GAP_MS + 100));
      });
      expect(await screen.findByText(/The microphone was interrupted/)).toBeInTheDocument();
      expect(recorder.start).toHaveBeenCalledTimes(2);
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

  describe('dictation topic', () => {
    it('marks the recording as final and shows the topic the server detected', async () => {
      const transcribe = vi.fn(
        async (_audio: Blob, options: { final?: boolean }): Promise<Transcript> => ({
          text: 'deploy with helm',
          corrections: [],
          ...(options.final ? { topic: 'Kubernetes deploy' } : {}),
        }),
      );
      renderWithApp(<Composer session={localSession()} />, { speech: { transcribe } });
      expect(screen.queryByTestId('composer-topic')).toBeNull();
      const mic = screen.getByRole('button', { name: 'Tap to dictate' });
      await act(async () => {
        fireEvent.click(mic);
      });
      advance(2000);
      await act(async () => {
        fireEvent.click(mic);
      });
      await waitFor(() =>
        expect(screen.getByTestId('composer-topic')).toHaveTextContent('Topic: Kubernetes deploy'),
      );
      // Without peek/split the whole recording is one final request.
      expect(transcribe).toHaveBeenCalledTimes(1);
      expect(transcribe.mock.calls[0]![1]).toMatchObject({ final: true, sessionId: 's1' });
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

    it('tags contextual (LLM) corrections with the context layer', async () => {
      await dictate({
        text: 'fix the submit handler',
        rawText: 'fix the sub mitt handler',
        corrections: [{ from: 'sub mitt handler', to: 'submit handler', layer: 'context' }],
      });
      await userEvent.click(screen.getByRole('button', { name: 'Show original' }));
      const panel = screen.getByRole('region', { name: 'Original transcription' });
      const term = panel.querySelector('mark')!;
      expect(term).toHaveAttribute('data-layer', 'context');
      expect(term).toHaveAttribute('title', 'Corrected to “submit handler” (Context (AI))');
      expect(term.firstChild).toHaveTextContent('sub mitt handler');
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
    expect(screen.getByText(/This session has ended/)).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toBeDisabled();
  });

  it('does not repeat the ended notice when the ended bar is already shown', () => {
    renderWithApp(
      <Composer session={localSession({ state: { type: 'exited', code: 0 } })} endedBarShown />,
    );
    expect(screen.queryByText(/This session has ended/)).not.toBeInTheDocument();
    expect(screen.getByRole('textbox')).toBeDisabled();
  });

  it('says why a watch-only session takes no input', () => {
    renderWithApp(<Composer session={localSession({ control: 'observe_only' })} />);
    expect(screen.getByText(/started in a terminal on the PC/)).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toBeDisabled();
  });

  describe('on-screen keyboard (phones)', () => {
    let viewport: FakeViewport;
    let stop: () => void;
    beforeEach(() => {
      viewport = installFakeViewport(390, 844);
      stop = trackVisualViewport(window, { touch: true });
    });
    afterEach(() => {
      stop();
      viewport.restore();
    });

    it('goes compact while the keyboard is open and the text box has focus', () => {
      renderWithApp(<Composer session={localSession()} />);
      const input = screen.getByRole('textbox', { name: 'Command for payments-api' });
      const composer = screen.getByRole('region', { name: 'Command for payments-api' });
      expect(composer).not.toHaveAttribute('data-keyboard-compact');

      act(() => {
        input.focus();
        viewport.keyboard(500);
      });
      expect(composer).toHaveAttribute('data-keyboard-compact');
      expect(composer.className).toMatch(/\bcompact\b/);
      // The text box takes the room above the keyboard.
      expect(input.parentElement?.className).toMatch(/\bcompact\b/);
      // The session cards are hidden: a strip still names the target.
      expect(screen.getByTestId('composer-target')).toHaveTextContent('Topayments-api');

      act(() => viewport.close());
      expect(composer).not.toHaveAttribute('data-keyboard-compact');
      expect(screen.queryByTestId('composer-target')).toBeNull();
      expect(input.parentElement?.className).not.toMatch(/\bcompact\b/);
    });

    it('leaves the composer as it is while another field has the keyboard', () => {
      renderWithApp(
        <>
          <input aria-label="Search" />
          <Composer session={localSession()} />
        </>,
      );
      act(() => {
        screen.getByRole('textbox', { name: 'Search' }).focus();
        viewport.keyboard(500);
      });
      const composer = screen.getByRole('region', { name: 'Command for payments-api' });
      expect(composer).not.toHaveAttribute('data-keyboard-compact');
    });

    it('leaves compact when the text box loses focus', () => {
      renderWithApp(<Composer session={localSession()} />);
      const input = screen.getByRole('textbox', { name: 'Command for payments-api' });
      const composer = screen.getByRole('region', { name: 'Command for payments-api' });
      act(() => {
        input.focus();
        viewport.keyboard(500);
      });
      expect(composer).toHaveAttribute('data-keyboard-compact');
      act(() => input.blur());
      expect(composer).not.toHaveAttribute('data-keyboard-compact');
    });

    it('names one target with the keyboard open: the @mention chip, not also this session', async () => {
      const other = localSession({ id: 'b2', display_name: 'web-frontend' });
      const transport = new FakeTransport((msg) =>
        msg.type === 'list_sessions'
          ? { type: 'sessions', sessions: [localSession(), other] }
          : defaultResponse(msg),
      );
      renderWithApp(<Composer session={localSession()} />, { transport });
      const input = screen.getByRole('textbox', { name: 'Command for payments-api' });
      act(() => {
        input.focus();
        viewport.keyboard(500);
      });
      expect(screen.getByTestId('composer-target')).toHaveTextContent('Topayments-api');
      await waitFor(() => {
        fireEvent.change(input, { target: { value: '@web-frontend hi' } });
        expect(screen.getByTestId('mention-chip')).toHaveTextContent('web-frontend');
      });
      expect(screen.queryByTestId('composer-target')).toBeNull();
    });
  });
});

describe('Composer messages that must not slip by', () => {
  const sessionsServer = (sessions: ReturnType<typeof localSession>[]) =>
    new FakeTransport((msg) =>
      msg.type === 'list_sessions' ? { type: 'sessions', sessions } : defaultResponse(msg),
    );

  it('shows why an @mention did not go in a cloud session too, until the text changes', async () => {
    const here = cloudSession();
    const waiting = localSession({
      id: 'b2',
      display_name: 'web-frontend',
      state: { type: 'awaiting_decision' },
    });
    renderWithApp(<Composer session={here} />, { transport: sessionsServer([here, waiting]) });
    const input = screen.getByRole('textbox');
    await waitFor(() => {
      fireEvent.change(input, { target: { value: '@web-frontend yes' } });
      expect(screen.getByTestId('mention-chip')).toBeInTheDocument();
    });
    await userEvent.click(screen.getByRole('button', { name: 'Send to web-frontend' }));
    const error = await screen.findByText(/waiting for an answer/);
    expect(error.closest('[role="alert"]')).not.toBeNull();
    // It does not fade out like a key bar note (3 s).
    await act(async () => {
      await new Promise((r) => setTimeout(r, 3300));
    });
    expect(screen.getByText(/waiting for an answer/)).toBeInTheDocument();
    // Editing the text clears it; so does its close button.
    fireEvent.change(input, { target: { value: '@web-frontend yes please' } });
    expect(screen.queryByText(/waiting for an answer/)).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Send to web-frontend' }));
    await screen.findByText(/waiting for an answer/);
    await userEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(screen.queryByText(/waiting for an answer/)).toBeNull();
  });

  it('names the target in the phone placeholder (the top bar may be scrolled away)', () => {
    renderWithApp(<Composer session={localSession()} layout="phone" />);
    expect(screen.getByRole('textbox')).toHaveAttribute(
      'placeholder',
      'Speak or type to payments-api…',
    );
  });

  it('leaves the floating reading-mode button for the full composer when the mic fails', async () => {
    renderWithApp(<Composer session={localSession()} layout="phone" reading />, {
      speech: {
        createRecorder: () => ({
          start: async () => {
            throw new Error('Permission denied');
          },
          stop: async () => new Blob(),
          cancel: () => {},
        }),
      },
    });
    const composer = screen.getByRole('region', { name: 'Command for payments-api' });
    expect(composer).toHaveAttribute('data-reading', 'true');
    await userEvent.click(screen.getByRole('button', { name: /Tap to dictate/ }));
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(/Permission denied/);
    // Not hidden behind reading mode: the composer shows the error.
    expect(composer).not.toHaveAttribute('data-reading');
  });
});
