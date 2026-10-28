import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { cloudSession, localSession } from '../test/fixtures';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { renderWithApp } from '../test/render';
import { Composer } from './Composer';

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

  it('records with push-to-talk and fills the transcript with corrections', async () => {
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
    });
    const mic = screen.getByRole('button', { name: 'Hold to talk' });
    await act(async () => {
      fireEvent.pointerDown(mic, { pointerId: 1 });
    });
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Microphone unavailable: Permission denied',
    );
    await act(async () => {
      fireEvent.pointerUp(mic, { pointerId: 1 });
    });
    expect(transcribe).not.toHaveBeenCalled();
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
