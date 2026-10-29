import { screen, render, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { App } from './App';
import { MockTransport } from './protocol/mockTransport';
import { defaultSettings } from './state/settings';

function renderApp() {
  const transport = new MockTransport({ latencyMs: 0, autoTick: false });
  render(
    <App
      machines={[
        { id: 'm1', name: 'workstation', connectivity: 'LAN', transport },
        {
          id: 'm2',
          name: 'build-server',
          connectivity: 'Tailscale',
          transport: new MockTransport({ profile: 'build_server', latencyMs: 0, autoTick: false }),
        },
      ]}
      speech={{
        transcribe: async () => ({ text: '', corrections: [] }),
        createRecorder: () => ({
          start: async () => {},
          stop: async () => new Blob(),
          cancel: () => {},
        }),
        fallbackWithoutMic: false,
      }}
      settings={defaultSettings()}
    />,
  );
  return transport;
}

describe('App (with MockTransport)', () => {
  it('shows the deck, opens a session and answers its decision', async () => {
    renderApp();
    const card = await screen.findByTestId('session-card-wk-claude-api');
    expect(screen.getAllByTestId(/^session-card-/)).toHaveLength(6);
    await userEvent.click(card);
    const sheet = await screen.findByRole('region', { name: 'Pending decision for payments-api' });
    await userEvent.click(within(sheet).getByRole('button', { name: 'Yes' }));
    expect(await screen.findByText('Chose: Yes')).toBeInTheDocument();
    expect(screen.queryByRole('region', { name: /Pending decision/ })).not.toBeInTheDocument();
  });

  it('switches the microphone between tap-to-toggle and hold in settings', async () => {
    renderApp();
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(screen.getAllByRole('button', { name: 'Settings' })[0]!);
    const group = await screen.findByRole('group', { name: 'Microphone button' });
    const toggle = within(group).getByRole('button', { name: 'Tap to start / stop' });
    const hold = within(group).getByRole('button', { name: 'Hold to talk' });
    expect(toggle).toHaveAttribute('aria-pressed', 'true');
    await userEvent.click(hold);
    expect(hold).toHaveAttribute('aria-pressed', 'true');
    expect(toggle).toHaveAttribute('aria-pressed', 'false');
  });

  it('filters cloud sessions and switches machines', async () => {
    renderApp();
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(screen.getByRole('button', { name: 'Cloud' }));
    expect(screen.getAllByTestId(/^session-card-/)).toHaveLength(2);
    await userEvent.selectOptions(screen.getByRole('combobox', { name: 'Machine' }), 'm2');
    expect(await screen.findByTestId('session-card-bs-codex-mono')).toBeInTheDocument();
    expect(screen.queryByTestId('session-card-wk-claude-api')).not.toBeInTheDocument();
  });
});
