import { screen, render, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { App } from './App';
import type { NativeServices } from './native/context';
import type { HostStatus } from './native/host';
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
    expect(screen.getAllByTestId(/^session-card-/)).toHaveLength(7);
    await userEvent.click(card);
    const sheet = await screen.findByRole('region', { name: 'Pending decision for payments-api' });
    await userEvent.click(within(sheet).getByRole('button', { name: 'Yes' }));
    expect(await screen.findByText('Chose: Yes')).toBeInTheDocument();
    expect(screen.queryByRole('region', { name: /Pending decision/ })).not.toBeInTheDocument();
  });

  it('on phones opens a session from the list and goes back to it', async () => {
    // No matchMedia in the test DOM: the narrow (phone) layout renders.
    renderApp();
    const card = await screen.findByTestId('session-card-wk-claude-api');
    // The list: the app header and the bottom menu.
    expect(screen.getByRole('navigation', { name: 'Main navigation' })).toBeInTheDocument();
    expect(screen.getByRole('banner')).toBeInTheDocument();
    await userEvent.click(card);
    // The session: one slim top bar with the compact card strip always under
    // it, no app header and no bottom menu.
    expect(await screen.findByRole('heading', { level: 2, name: 'payments-api' })).toBeVisible();
    const bar = screen.getByTestId('session-topbar');
    expect(screen.queryByRole('navigation', { name: 'Main navigation' })).toBeNull();
    expect(within(bar).getByRole('list', { name: 'Session cards' })).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Back to sessions' }));
    expect(await screen.findByTestId('session-card-wk-claude-api')).toBeInTheDocument();
    expect(screen.getByRole('navigation', { name: 'Main navigation' })).toBeInTheDocument();
    expect(screen.queryByTestId('session-topbar')).toBeNull();
  });

  it('switches the microphone between tap-to-toggle and hold in settings', async () => {
    renderApp();
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(screen.getAllByRole('button', { name: 'Settings' })[0]!);
    await userEvent.click(await screen.findByRole('tab', { name: 'Voice' }));
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

  it('shows the brand lockup in the header with the machine and connection state', async () => {
    renderApp();
    await screen.findByTestId('session-card-wk-claude-api');
    const banner = screen.getByRole('banner');
    const heading = within(banner).getByRole('heading', { level: 1, name: 'Voice Command AI' });
    // Horizontal lockup (wide) and the symbol alone (narrow), both decorative.
    const lockup = heading.querySelector('svg[data-logo="horizontal"]');
    const symbol = heading.querySelector('svg[data-logo="symbol"]');
    expect(lockup).toHaveAttribute('aria-hidden', 'true');
    expect(lockup?.querySelectorAll('[data-part="symbol"] path')).toHaveLength(5);
    expect(lockup?.querySelector('[data-part="wordmark"]')).not.toBeNull();
    expect(symbol?.querySelector('[data-part="wordmark"]')).toBeNull();
    // Portrait phones show a short decorative "VCA" next to the symbol (CSS hides it on
    // wider screens); it stays out of the heading's accessible name.
    expect(within(banner).getByText('VCA')).toHaveAttribute('aria-hidden', 'true');
    expect(within(banner).getByRole('combobox', { name: 'Machine' })).toHaveValue('m1');
    expect(within(banner).getByRole('status')).toHaveTextContent(/Connect/);
  });
});

describe('App setup (no machine paired)', () => {
  it('shows the brand hero with the slogan on the pairing screen', () => {
    render(
      <App
        machines={[]}
        speech={{
          transcribe: async () => ({ text: '', corrections: [] }),
          createRecorder: () => ({
            start: async () => {},
            stop: async () => new Blob(),
            cancel: () => {},
          }),
          fallbackWithoutMic: false,
        }}
        settings={{ ...defaultSettings(), language: 'ko' }}
      />,
    );
    const logo = screen.getByRole('img', { name: 'Voice Command AI' });
    expect(logo).toHaveAttribute('data-logo', 'vertical');
    expect(screen.getByText('말로 지휘하고, 흐름을 관리하다.')).toBeInTheDocument();
  });

  it('desktop: leads with starting the server on this PC, pairing comes second', async () => {
    const status: HostStatus = {
      running: false,
      endpoint: 'pipe',
      server_name: null,
      version: null,
      remote: null,
      managed: false,
      can_start: true,
      run_server: false,
      log_path: null,
    };
    const host = {
      status: vi.fn(async () => status),
      start: vi.fn(async () => ({ ...status, running: true, managed: true })),
    };
    const native = {
      pair: vi.fn(),
      pairingCode: vi.fn(),
      forget: vi.fn(),
      platform: async () => 'windows',
      host,
    } as unknown as NativeServices;
    render(
      <App
        machines={[]}
        native={native}
        speech={{
          transcribe: async () => ({ text: '', corrections: [] }),
          createRecorder: () => ({
            start: async () => {},
            stop: async () => new Blob(),
            cancel: () => {},
          }),
          fallbackWithoutMic: false,
        }}
        settings={{ ...defaultSettings(), language: 'en' }}
      />,
    );
    const card = await screen.findByTestId('setup-this-pc');
    expect(within(card).getByRole('heading', { name: 'Use VCA on this PC' })).toBeInTheDocument();
    expect(card).toHaveTextContent('Pairing below is only for using another computer.');
    // First on the screen, before "Connect a phone" and pasting a link.
    const phone = screen.getByRole('heading', { name: 'Connect a phone' });
    expect(card.compareDocumentPosition(phone) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    await userEvent.click(
      within(card).getByRole('button', { name: 'Start the server on this PC' }),
    );
    expect(host.start).toHaveBeenCalledTimes(1);
    expect(await within(card).findByRole('status')).toHaveTextContent(
      'The VCA server on this PC is running. Connecting…',
    );
  });
});
