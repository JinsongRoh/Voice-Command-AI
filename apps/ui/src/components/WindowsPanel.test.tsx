import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { App } from '../App';
import { I18nProvider } from '../i18n/context';
import { mockRestoreLocal } from '../protocol/mockRestore';
import { MockTransport } from '../protocol/mockTransport';
import type { ClientMsg, Envelope, ServerMsg } from '../protocol/types';
import { resetInputGuard } from '../state/inputGuard';
import { defaultSettings } from '../state/settings';
import { AppProvider } from '../state/store';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { SessionDeck } from './SessionDeck';

const speech = {
  transcribe: async () => ({ text: '', corrections: [] }),
  createRecorder: () => ({
    start: async () => {},
    stop: async () => new Blob(),
    cancel: () => {},
  }),
  fallbackWithoutMic: false,
};

/** `onThisComputer`: the simulated desktop app of the computer itself. */
function renderApp(onThisComputer: boolean) {
  const transport = new MockTransport({ latencyMs: 0, autoTick: false });
  render(
    <App
      machines={[
        {
          id: 'm1',
          name: 'workstation',
          connectivity: 'LAN',
          transport,
          simulate: {
            drop: () => {},
            ...(onThisComputer ? { restoreLocal: mockRestoreLocal() } : {}),
          },
        },
      ]}
      speech={speech}
      settings={{ ...defaultSettings(), language: 'en', approvalMode: 'ask' }}
    />,
  );
  return transport;
}

/** The session list alone, with one local session named payments-api. */
function renderDeck(onThisComputer: boolean) {
  const session = localSession({ id: 's1', display_name: 'payments-api' });
  const transport = new FakeTransport((msg) =>
    msg.type === 'list_sessions' ? { type: 'sessions', sessions: [session] } : defaultResponse(msg),
  );
  render(
    <AppProvider
      machines={[
        {
          id: 'm',
          name: 'test machine',
          connectivity: 'LAN',
          transport,
          simulate: {
            drop: () => {},
            ...(onThisComputer ? { restoreLocal: mockRestoreLocal() } : {}),
          },
        },
      ]}
      settings={defaultSettings()}
      persistSettings={false}
    >
      <I18nProvider locale="en">
        <SessionDeck onOpen={() => {}} onNewSession={() => {}} />
      </I18nProvider>
    </AppProvider>,
  );
  return transport;
}

/** Records the focus requests and answers them with `reply`. */
function spyOnFocus(transport: MockTransport, reply?: ServerMsg) {
  const sent: string[] = [];
  const send = transport.send.bind(transport);
  vi.spyOn(transport, 'send').mockImplementation(async (env: Envelope<ClientMsg>) => {
    if (env.body.type === 'focus_terminal') {
      sent.push(env.body.session_id);
      if (reply) return reply;
    }
    return send(env);
  });
  return sent;
}

const nav = () => within(screen.getByRole('navigation', { name: 'Main navigation' }));

async function openMenu(name: RegExp) {
  await screen.findByTestId('session-card-wk-claude-api');
  await userEvent.click(nav().getByRole('button', { name }));
}

afterEach(() => {
  try {
    localStorage.removeItem('vca.windowsTab');
  } catch {
    // No storage in this environment.
  }
  resetInputGuard();
  vi.restoreAllMocks();
});

describe('window management tabs', () => {
  it('keeps saving windows and the session order on separate, explained tabs', async () => {
    try {
      localStorage.removeItem('vca.windowsTab');
    } catch {
      // No storage in this environment.
    }
    renderApp(false);
    await openMenu(/Windows/);
    const tabs = await screen.findByRole('tablist', { name: 'Window management' });
    const restore = within(tabs).getByRole('tab', { name: 'Save & restore windows' });
    const order = within(tabs).getByRole('tab', { name: 'Session order' });
    expect(restore).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByText(/does not change the order of the session list/)).toBeInTheDocument();
    expect(screen.getByRole('heading', { level: 3, name: /Saved windows/ })).toBeInTheDocument();

    await userEvent.click(order);
    expect(order).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByText(/does not move, open or save any window/)).toBeInTheDocument();
    expect(screen.queryByRole('heading', { level: 3, name: /Saved windows/ })).toBeNull();
    expect(screen.getByRole('tabpanel')).toHaveAccessibleName('Session order');
  });
});

describe('opened but not placed', () => {
  it('reports a window with no saved position as opened, not placed or failed', async () => {
    const transport = renderApp(false);
    await openMenu(/Windows/);
    await screen.findByRole('heading', { level: 3, name: /Saved windows/ });
    await userEvent.click(screen.getByRole('button', { name: 'Add' }));
    const add = await screen.findByRole('dialog', { name: 'Add a window' });
    await userEvent.type(within(add).getByRole('textbox', { name: 'Window title' }), 'Notes');
    await userEvent.type(within(add).getByRole('textbox', { name: 'Folder' }), 'E:\\notes');
    await userEvent.click(within(add).getByRole('button', { name: 'Save' }));
    await userEvent.click(await screen.findByRole('checkbox', { name: 'Select Notes' }));
    await userEvent.click(screen.getByRole('button', { name: /Restore selected/ }));
    const preview = await screen.findByRole('dialog', { name: 'Restore windows' });
    await userEvent.click(within(preview).getByRole('button', { name: 'Open 1 window' }));
    const totp = await screen.findByRole('dialog', { name: 'Confirm restore' });
    await userEvent.type(within(totp).getByRole('textbox'), '123456');
    await userEvent.click(within(totp).getByRole('button', { name: 'Restore' }));
    await screen.findByRole('heading', { name: /Restoring Work/ });
    for (let i = 0; i < 3; i++) act(() => transport.tick());
    expect(await screen.findByRole('heading', { name: /restore finished/ })).toBeInTheDocument();
    expect(screen.getByText('Opened, not placed')).toBeInTheDocument();
    expect(screen.getByText(/no position is saved/)).toBeInTheDocument();
    expect(screen.queryByText('Failed')).toBeNull();
    // Nothing to retry: it did open.
    expect(screen.queryByRole('button', { name: 'Retry' })).toBeNull();
  });
});

describe('bring to front', () => {
  it('is offered from an alert on the computer itself and sent on click only', async () => {
    const transport = renderApp(true);
    const sent = spyOnFocus(transport);
    await openMenu(/Alerts/);
    const button = await screen.findByRole('button', {
      name: 'Bring payments-api to the front',
    });
    // Nothing is brought forward by itself.
    expect(sent).toEqual([]);
    await userEvent.click(button);
    await waitFor(() => expect(sent).toEqual(['wk-claude-api']));
    expect(screen.queryByText(/Couldn't bring/)).toBeNull();
  });

  it('is offered from the session menu on the computer itself', async () => {
    const transport = renderDeck(true);
    await userEvent.click(
      await screen.findByRole('button', { name: 'More actions for payments-api' }),
    );
    expect(transport.bodies('focus_terminal')).toEqual([]);
    await userEvent.click(screen.getByRole('menuitem', { name: 'Bring to front' }));
    await waitFor(() =>
      expect(transport.bodies('focus_terminal')).toEqual([
        { type: 'focus_terminal', session_id: 's1' },
      ]),
    );
    expect(screen.queryByRole('menu')).toBeNull();
  });

  it('is not offered on another computer', async () => {
    renderApp(false);
    await openMenu(/Alerts/);
    await screen.findByRole('button', { name: 'Open payments-api at its request' });
    expect(screen.queryByRole('button', { name: /to the front/ })).toBeNull();
    cleanup();
    const transport = renderDeck(false);
    await userEvent.click(
      await screen.findByRole('button', { name: 'More actions for payments-api' }),
    );
    expect(screen.getByRole('menuitem', { name: 'Rename' })).toBeInTheDocument();
    expect(screen.queryByRole('menuitem', { name: 'Bring to front' })).toBeNull();
    expect(transport.bodies('focus_terminal')).toEqual([]);
  });

  it('says so when Windows keeps the focus where it is', async () => {
    const transport = renderApp(true);
    spyOnFocus(transport, {
      type: 'error',
      code: 'unavailable',
      message: 'Windows did not let VCA bring the window forward',
    });
    await openMenu(/Alerts/);
    await userEvent.click(
      await screen.findByRole('button', { name: 'Bring payments-api to the front' }),
    );
    expect(await screen.findByText("Couldn't bring payments-api forward")).toBeInTheDocument();
    expect(screen.getByText(/flashing taskbar button/)).toBeInTheDocument();
  });
});
