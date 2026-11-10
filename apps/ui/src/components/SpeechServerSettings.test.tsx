import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import type { ClientMsg, ServerMsg, SpeechProfileInfo } from '../protocol/types';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { renderWithApp } from '../test/render';
import { SpeechServerSettings } from './SpeechServerSettings';

const ENGINES: SpeechProfileInfo[] = [
  {
    id: 'whisper',
    label: 'Whisper (PC)',
    kind: 'openai_compatible',
    configured: true,
    default: true,
  },
  { id: 'cloud', label: 'OpenAI', kind: 'openai', configured: true, default: false },
  { id: 'google', label: 'Google', kind: 'google', configured: false, default: false },
];
const LLMS: SpeechProfileInfo[] = [
  {
    id: 'local-llm',
    label: 'Local LLM',
    kind: 'openai_compatible',
    configured: true,
    default: false,
  },
  { id: 'claude', label: 'Claude', kind: 'anthropic', configured: false, default: false },
];

function server(initial: { engine: string; llm: string | null }, engines = ENGINES) {
  const state = { ...initial };
  const options = (): ServerMsg => ({
    type: 'speech_options',
    engines,
    llms: LLMS,
    current_engine: state.engine,
    current_llm: state.llm,
  });
  return new FakeTransport((msg: ClientMsg) => {
    switch (msg.type) {
      case 'list_speech_options':
        return options();
      case 'set_speech_preferences':
        state.engine = msg.engine ?? 'whisper';
        state.llm = msg.llm === 'none' ? null : msg.llm;
        return options();
      default:
        return defaultResponse(msg);
    }
  });
}

describe('SpeechServerSettings', () => {
  it('fills both dropdowns and disables profiles without a key', async () => {
    renderWithApp(<SpeechServerSettings />, {
      transport: server({ engine: 'whisper', llm: null }),
    });
    const engine = await screen.findByRole('combobox', { name: 'Speech engine (on the PC)' });
    expect(engine).toHaveValue('whisper');
    const engineOptions = within(engine).getAllByRole('option');
    expect(engineOptions.map((o) => o.textContent)).toEqual([
      'Whisper (PC) (default)',
      'OpenAI',
      'Google (no key yet)',
    ]);
    expect(engineOptions[2]).toBeDisabled();
    expect(engineOptions[1]).toBeEnabled();

    const llm = screen.getByRole('combobox', { name: 'Transcript correction (LLM)' });
    expect(llm).toHaveValue('none');
    const llmOptions = within(llm).getAllByRole('option');
    expect(llmOptions.map((o) => o.getAttribute('value'))).toEqual(['none', 'local-llm', 'claude']);
    expect(llmOptions[2]).toBeDisabled();

    expect(screen.getByTestId('speech-hint-google')).toHaveTextContent(
      'Google: add the key on the PC with “vca speech login google”',
    );
    expect(screen.getByTestId('speech-hint-claude')).toHaveTextContent('vca speech login claude');
    expect(screen.queryByRole('button', { name: 'Save' })).toBeNull();
  });

  it('saves each choice with set_speech_preferences as soon as it is picked', async () => {
    const transport = server({ engine: 'whisper', llm: null });
    renderWithApp(<SpeechServerSettings />, { transport });
    const engine = await screen.findByRole('combobox', { name: 'Speech engine (on the PC)' });
    await userEvent.selectOptions(engine, 'cloud');
    await waitFor(() =>
      expect(transport.bodies('set_speech_preferences')).toEqual([
        { type: 'set_speech_preferences', engine: 'cloud', llm: 'none' },
      ]),
    );
    expect(await screen.findByText('Saved')).toBeInTheDocument();
    expect(engine).toHaveValue('cloud');
    const llm = screen.getByRole('combobox', { name: 'Transcript correction (LLM)' });
    await waitFor(() => expect(llm).toBeEnabled());
    await userEvent.selectOptions(llm, 'local-llm');
    await waitFor(() => expect(transport.bodies('set_speech_preferences')).toHaveLength(2));
    expect(transport.bodies('set_speech_preferences')[1]).toEqual({
      type: 'set_speech_preferences',
      engine: 'cloud',
      llm: 'local-llm',
    });
  });

  it('shows the server error when saving fails', async () => {
    const transport = new FakeTransport((msg: ClientMsg) => {
      if (msg.type === 'list_speech_options') {
        return {
          type: 'speech_options',
          engines: ENGINES,
          llms: LLMS,
          current_engine: 'whisper',
          current_llm: null,
        };
      }
      if (msg.type === 'set_speech_preferences') {
        return { type: 'error', code: 'forbidden', message: 'command scope required' };
      }
      return defaultResponse(msg);
    });
    renderWithApp(<SpeechServerSettings />, { transport });
    const engine = await screen.findByRole('combobox', { name: 'Speech engine (on the PC)' });
    await userEvent.selectOptions(engine, 'cloud');
    expect(await screen.findByRole('alert')).toHaveTextContent('command scope required');
    // The dropdown goes back to what the PC still uses.
    expect(engine).toHaveValue('whisper');
  });

  it('stays hidden when the server has no speech engines or does not answer', async () => {
    const transport = server({ engine: '', llm: null }, []);
    renderWithApp(<SpeechServerSettings />, { transport });
    await waitFor(() => expect(transport.bodies('list_speech_options')).toHaveLength(1));
    expect(screen.queryByTestId('speech-server-settings')).toBeNull();

    const old = new FakeTransport((msg: ClientMsg) =>
      msg.type === 'list_speech_options'
        ? { type: 'error', code: 'invalid_request', message: 'unknown message' }
        : defaultResponse(msg),
    );
    renderWithApp(<SpeechServerSettings />, { transport: old });
    await waitFor(() => expect(old.bodies('list_speech_options')).toHaveLength(1));
    expect(screen.queryByTestId('speech-server-settings')).toBeNull();
  });
});
