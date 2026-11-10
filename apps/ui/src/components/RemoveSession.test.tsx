import { act, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import ko from '../i18n/ko.json';
import type { ClientMsg, ServerMsg, SessionInfo } from '../protocol/types';
import { resetPinsCache } from '../state/sessionPins';
import { useActions, useAppState } from '../state/store';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { cloudSession, localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { LiveView } from './LiveView';
import { removeKind } from './RemoveSession';
import { SessionDeck } from './SessionDeck';
import { SessionOrderPanel } from './SessionOrderPanel';
import { ShellProvider } from './ShellProvider';

// xterm needs a real canvas; the screen itself is not under test here.
vi.mock('./TerminalView', () => ({
  default: ({ sessionId }: { sessionId: string }) => <div data-testid={`terminal-${sessionId}`} />,
}));

const managed = localSession({ id: 'm1', display_name: 'payments-api' });
const pcWindow = localSession({
  id: 'w2',
  display_name: 'pc-window',
  control: 'terminal_api',
  origin: { type: 'local', agent: { type: 'codex' }, project_dir: '/home/dev/web', pid: 7 },
});
const ended = localSession({
  id: 'e3',
  display_name: 'old-work',
  state: { type: 'exited', code: 0 },
});

const ALL_FEATURES = ['dismiss_session', 'dismiss_stop', 'session_order'];

/** A server with `sessions` and `features`; `scope` is this device's permission. */
function server(
  sessions: SessionInfo[],
  {
    features = ALL_FEATURES,
    scope,
    override = () => undefined,
  }: {
    features?: string[];
    scope?: 'view' | 'command' | 'admin';
    override?: (msg: ClientMsg) => ServerMsg | undefined;
  } = {},
) {
  return new FakeTransport((msg) => {
    const custom = override(msg);
    if (custom) return custom;
    if (msg.type === 'hello')
      return {
        type: 'welcome',
        protocol_version: 1,
        server_version: 't',
        server_name: 't',
        features,
      };
    if (msg.type === 'list_sessions') return { type: 'sessions', sessions };
    if (msg.type === 'get_session_order')
      return { type: 'session_order', session_ids: sessions.map((s) => s.id), custom: false };
    if (msg.type === 'list_devices' && scope)
      return {
        type: 'devices',
        devices: [
          {
            id: 'd',
            name: 'me',
            platform: 'ios',
            paired_at: 0,
            last_seen: 0,
            current: true,
            scope,
          },
        ],
      };
    return defaultResponse(msg);
  });
}

function renderDeck(transport: FakeTransport) {
  return renderWithApp(
    <ShellProvider>
      <SessionDeck onOpen={() => {}} onNewSession={() => {}} />
    </ShellProvider>,
    { transport },
  );
}

async function openCardMenu(id: string, name: string) {
  await screen.findByTestId(`session-card-${id}`);
  await userEvent.click(screen.getByRole('button', { name: `More actions for ${name}` }));
  return screen.getByRole('menu');
}

beforeEach(() => {
  localStorage.clear();
  resetPinsCache();
});

describe('what Remove from VCA does', () => {
  it('depends on whether VCA started the session and whether it still runs', () => {
    expect(removeKind(ended)).toBe('ended');
    expect(removeKind(managed)).toBe('stop');
    expect(removeKind(pcWindow)).toBe('pc_window');
    expect(removeKind(cloudSession())).toBe('cloud');
    expect(removeKind({ ...managed, conductor: true })).toBe('conductor');
  });

  it('is named the same in Korean everywhere', () => {
    expect(ko['remove.action']).toBe('VCA에서 제거');
    expect(ko['ended.remove']).toBe('VCA에서 제거');
    expect(ko['remove.stopConfirm']).toBe('중지하고 제거');
  });
});

describe('Remove from VCA in the card menu', () => {
  it('stops and removes a running session VCA started in one request, after one confirmation', async () => {
    const transport = server([managed, pcWindow]);
    renderDeck(transport);
    const menu = await openCardMenu('m1', 'payments-api');
    await userEvent.click(within(menu).getByRole('menuitem', { name: /Remove from VCA/ }));
    const dialog = await screen.findByRole('dialog', { name: 'Remove “payments-api” from VCA?' });
    expect(dialog).toHaveTextContent(/stops the agent process now/);
    expect(dialog).toHaveTextContent(/conversation history and the project folder stay on the PC/);
    expect(transport.bodies('dismiss_session')).toEqual([]);
    await userEvent.click(within(dialog).getByRole('button', { name: 'Stop and remove' }));
    await waitFor(() =>
      expect(transport.bodies('dismiss_session')).toEqual([
        { type: 'dismiss_session', session_id: 'm1', stop: true },
      ]),
    );
    // No separate Stop: the server does both.
    expect(transport.bodies('control')).toEqual([]);
    await waitFor(() => expect(screen.queryByTestId('session-card-m1')).not.toBeInTheDocument());
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('keeps the dialog open with the reason when the server refuses', async () => {
    const transport = server([managed], {
      override: (msg) =>
        msg.type === 'dismiss_session'
          ? { type: 'error', code: 'unavailable', message: 'the agent did not stop in time' }
          : undefined,
    });
    renderDeck(transport);
    const menu = await openCardMenu('m1', 'payments-api');
    await userEvent.click(within(menu).getByRole('menuitem', { name: /Remove from VCA/ }));
    const dialog = await screen.findByRole('dialog');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Stop and remove' }));
    expect(await within(dialog).findByRole('alert')).toBeInTheDocument();
    expect(screen.getByTestId('session-card-m1')).toBeInTheDocument();
  });

  it('on older servers stops first, waits for the exit, then removes', async () => {
    const transport = server([managed], { features: ['dismiss_session', 'session_order'] });
    renderDeck(transport);
    const menu = await openCardMenu('m1', 'payments-api');
    await userEvent.click(within(menu).getByRole('menuitem', { name: /Remove from VCA/ }));
    await userEvent.click(
      within(await screen.findByRole('dialog')).getByRole('button', { name: 'Stop and remove' }),
    );
    await waitFor(() =>
      expect(transport.bodies('control')).toEqual([
        { type: 'control', session_id: 'm1', action: 'stop' },
      ]),
    );
    expect(transport.bodies('dismiss_session')).toEqual([]);
    act(() =>
      transport.push({
        type: 'session_updated',
        session: { ...managed, state: { type: 'exited', code: 0 } },
      }),
    );
    await waitFor(() =>
      expect(transport.bodies('dismiss_session')).toEqual([
        { type: 'dismiss_session', session_id: 'm1' },
      ]),
    );
    await waitFor(() => expect(screen.queryByTestId('session-card-m1')).not.toBeInTheDocument());
  });

  it('explains what to do for a session in a PC terminal window, and sends nothing', async () => {
    const transport = server([pcWindow]);
    renderDeck(transport);
    const menu = await openCardMenu('w2', 'pc-window');
    await userEvent.click(within(menu).getByRole('menuitem', { name: /Remove from VCA/ }));
    const dialog = await screen.findByRole('dialog', { name: 'Close it on the PC first' });
    expect(dialog).toHaveTextContent(/Close that terminal window on the PC/);
    expect(within(dialog).queryByRole('button', { name: /Stop/ })).not.toBeInTheDocument();
    await userEvent.click(within(dialog).getByRole('button', { name: 'OK' }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(transport.bodies('dismiss_session')).toEqual([]);
    expect(transport.bodies('control')).toEqual([]);
  });

  it('removes an ended session at once, as before', async () => {
    const transport = server([ended]);
    renderDeck(transport);
    const menu = await openCardMenu('e3', 'old-work');
    await userEvent.click(within(menu).getByRole('menuitem', { name: /Remove from VCA/ }));
    await waitFor(() =>
      expect(transport.bodies('dismiss_session')).toEqual([
        { type: 'dismiss_session', session_id: 'e3' },
      ]),
    );
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('is not offered on a view-only device', async () => {
    const transport = server([managed, ended], { scope: 'view' });
    renderDeck(transport);
    await waitFor(() => expect(transport.bodies('list_devices').length).toBeGreaterThan(0));
    const menu = await openCardMenu('e3', 'old-work');
    await waitFor(() =>
      expect(within(menu).queryByRole('menuitem', { name: /Remove/ })).not.toBeInTheDocument(),
    );
  });
});

/** Selects the session (which subscribes it) and renders its live view. */
function Selected({ id }: { id: string }) {
  const actions = useActions();
  const selected = useAppState((s) => (s.selectedId ? s.sessions[s.selectedId] : undefined));
  const known = useAppState((s) => !!s.sessions[id]);
  useEffect(() => {
    if (known) actions.select(id);
  }, [actions, known, id]);
  return selected ? <LiveView key={selected.id} session={selected} /> : null;
}

describe('Remove from VCA in the session view', () => {
  it('sits with the session actions and asks first', async () => {
    const transport = server([managed]);
    renderWithApp(
      <ShellProvider>
        <Selected id="m1" />
      </ShellProvider>,
      { transport },
    );
    const actions = await screen.findByRole('group', { name: 'Session actions' });
    await userEvent.click(
      within(actions).getByRole('button', { name: 'Remove payments-api from VCA' }),
    );
    const dialog = await screen.findByRole('dialog', { name: 'Remove “payments-api” from VCA?' });
    await userEvent.click(within(dialog).getByRole('button', { name: 'Stop and remove' }));
    await waitFor(() =>
      expect(transport.bodies('dismiss_session')).toEqual([
        { type: 'dismiss_session', session_id: 'm1', stop: true },
      ]),
    );
  });
});

describe('Remove from VCA in the session order tab', () => {
  it('is on every row and works like the card menu', async () => {
    const transport = server([managed, pcWindow, ended]);
    renderWithApp(
      <ShellProvider>
        <SessionOrderPanel />
      </ShellProvider>,
      { transport },
    );
    await screen.findByTestId('order-row-e3');
    for (const id of ['m1', 'w2', 'e3']) {
      expect(screen.getByTestId(`order-remove-${id}`)).toBeInTheDocument();
    }
    await userEvent.click(screen.getByRole('button', { name: 'Remove old-work from VCA' }));
    await waitFor(() =>
      expect(transport.bodies('dismiss_session')).toEqual([
        { type: 'dismiss_session', session_id: 'e3' },
      ]),
    );
    await waitFor(() => expect(screen.queryByTestId('order-row-e3')).not.toBeInTheDocument());
    await userEvent.click(screen.getByTestId('order-remove-m1'));
    const dialog = await screen.findByRole('dialog', { name: 'Remove “payments-api” from VCA?' });
    await userEvent.click(within(dialog).getByRole('button', { name: 'Stop and remove' }));
    await waitFor(() => expect(screen.queryByTestId('order-row-m1')).not.toBeInTheDocument());
  });

  it('has no remove buttons on a view-only device', async () => {
    const transport = server([managed, ended], { scope: 'view' });
    renderWithApp(
      <ShellProvider>
        <SessionOrderPanel />
      </ShellProvider>,
      { transport },
    );
    await screen.findByTestId('order-row-e3');
    await waitFor(() => expect(screen.queryByTestId('order-remove-e3')).not.toBeInTheDocument());
    expect(screen.queryByTestId('order-remove-m1')).not.toBeInTheDocument();
  });
});
