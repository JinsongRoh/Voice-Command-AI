import { act, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect } from 'react';
import { describe, expect, it, vi } from 'vitest';

import type { ServerMsg, SessionInfo } from '../protocol/types';
import { useActions, useAppState } from '../state/store';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { LiveView } from './LiveView';

// xterm needs a real canvas; the terminal tab is not under test here.
vi.mock('./TerminalView', () => ({ default: () => <div data-testid="terminal" /> }));

const enc = new TextEncoder();

const fakeAgent = localSession({
  origin: {
    type: 'local',
    agent: { type: 'generic', name: 'E:/dev/target/debug/vca-fake-agent.exe' },
    project_dir: 'E:/dev/demo',
    pid: 7,
  },
  display_name: 'demo',
});

/** Selects the session (which subscribes it) and renders its live view. */
function Selected({ session }: { session: SessionInfo }) {
  const actions = useActions();
  const selected = useAppState((s) => s.selectedId);
  const known = useAppState((s) => !!s.sessions[session.id]);
  useEffect(() => {
    if (known) actions.select(session.id);
  }, [actions, known, session.id]);
  return selected === session.id ? <LiveView session={session} /> : null;
}

describe('LiveView chat', () => {
  it('echoes sent prompts and shows the reply of an agent without a transcript', async () => {
    const transport = new FakeTransport((msg) =>
      msg.type === 'list_sessions'
        ? { type: 'sessions', sessions: [fakeAgent] }
        : defaultResponse(msg),
    );
    renderWithApp(<Selected session={fakeAgent} />, { transport });
    const log = await screen.findByRole('log');
    expect(within(log).getByRole('note')).toHaveTextContent(/no chat transcript/);

    await userEvent.type(screen.getByRole('textbox'), 'say hello');
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(within(log).getByText('say hello')).toBeInTheDocument());
    expect(within(log).getByText('Delivered')).toBeInTheDocument();

    act(() => {
      transport.push({
        type: 'event',
        session_id: 's1',
        event: {
          type: 'output',
          seq: 1,
          at: Date.now(),
          data: enc.encode('say hello\r\nHello!\r\n'),
        },
      });
    });
    expect(await within(log).findByText('Hello!')).toBeInTheDocument();

    await userEvent.click(within(log).getByRole('button', { name: 'Open Terminal' }));
    expect(await screen.findByTestId('terminal')).toBeInTheDocument();
    expect(screen.queryByRole('log')).not.toBeInTheDocument();
  });
});

const observed = localSession({
  id: 'obs1',
  display_name: 'my-terminal',
  control: 'terminal_api',
});
const managed = localSession({ id: 'm1', display_name: 'managed' });

function controlServer(
  sessions: () => SessionInfo[],
  onControl: () => ServerMsg = () => ({ type: 'ok' }),
) {
  return new FakeTransport((msg) => {
    if (msg.type === 'list_sessions') return { type: 'sessions', sessions: sessions() };
    if (msg.type === 'control') return onControl();
    return defaultResponse(msg);
  });
}

/** The cards the store holds (to see stale ones go away) and the toasts. */
function CardIds() {
  const order = useAppState((s) => s.order);
  const toasts = useAppState((s) => s.toasts);
  return (
    <>
      <output data-testid="cards">{order.join(',')}</output>
      <output data-testid="toasts">{toasts.map((t) => `${t.title} ${t.body}`).join('|')}</output>
    </>
  );
}

const statusWith = (text: string) =>
  screen.getAllByRole('status').find((el) => el.textContent?.includes(text));

const CANT_RESTART =
  "Sessions started directly in a terminal on the PC can't be restarted from VCA.";

describe('LiveView terminal tab', () => {
  it('explains the empty screen of a session started outside VCA', async () => {
    renderWithApp(<Selected session={observed} />, { transport: controlServer(() => [observed]) });
    await userEvent.click(await screen.findByRole('button', { name: 'Terminal' }));
    expect(await screen.findByText(/runs in a terminal window on the PC/)).toBeInTheDocument();
  });
});

describe('LiveView session actions', () => {
  it('dims pause, restart and stop for sessions VCA did not start, and explains why', async () => {
    const transport = controlServer(() => [observed]);
    renderWithApp(<Selected session={observed} />, { transport });
    const restart = await screen.findByRole('button', { name: 'Restart' });
    const pause = screen.getByRole('button', { name: 'Pause' });
    const stop = screen.getByRole('button', { name: 'Stop' });
    for (const b of [restart, pause, stop]) expect(b).toHaveAttribute('aria-disabled', 'true');
    expect(restart).toHaveAccessibleDescription(CANT_RESTART);
    expect(restart).toHaveAttribute('title', CANT_RESTART);
    await userEvent.click(restart);
    expect(statusWith(CANT_RESTART)).toBeDefined();
    await userEvent.click(stop);
    expect(statusWith('Use the Interrupt key')).toBeDefined();
    // Nothing reached the server, and no confirmation opened.
    expect(transport.bodies('control')).toEqual([]);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('keeps the actions for managed sessions', async () => {
    const transport = controlServer(() => [managed]);
    renderWithApp(<Selected session={managed} />, { transport });
    const restart = await screen.findByRole('button', { name: 'Restart' });
    expect(restart).not.toHaveAttribute('aria-disabled');
    await userEvent.click(restart);
    await waitFor(() =>
      expect(transport.bodies('control')).toEqual([
        { type: 'control', session_id: 'm1', action: 'restart' },
      ]),
    );
  });

  it('drops a stale session card and says so in plain words', async () => {
    let list = [managed, observed];
    const transport = controlServer(
      () => list,
      () => ({ type: 'error', code: 'not_found', message: 'unknown session m1' }),
    );
    renderWithApp(
      <>
        <Selected session={managed} />
        <CardIds />
      </>,
      { transport },
    );
    await waitFor(() => expect(screen.getByTestId('cards')).toHaveTextContent('m1,obs1'));
    // The PC restarted: the session is gone there.
    list = [observed];
    await userEvent.click(await screen.findByRole('button', { name: 'Pause' }));
    // The list was reloaded, the stale card dropped, and a friendly toast shown.
    await waitFor(() => expect(screen.getByTestId('cards')).toHaveTextContent(/^obs1$/));
    expect(transport.bodies('list_sessions').length).toBeGreaterThanOrEqual(2);
    expect(screen.getByTestId('toasts')).toHaveTextContent(
      'Session not found (it may have ended already).',
    );
    expect(screen.queryByText(/unknown session/)).toBeNull();
  });

  it('shows the raw server text only under Details', async () => {
    const transport = controlServer(
      () => [managed],
      () => ({
        type: 'error',
        code: 'unsupported',
        message: 'only managed sessions can be restarted',
      }),
    );
    renderWithApp(<Selected session={managed} />, { transport });
    await userEvent.click(await screen.findByRole('button', { name: 'Restart' }));
    const banner = await screen.findByRole('alert');
    expect(banner).toHaveTextContent(
      "This session was started in a terminal on the PC, so VCA can't control it.",
    );
    expect(banner).not.toHaveTextContent('only managed');
    await userEvent.click(within(banner).getByRole('button', { name: 'Details' }));
    expect(banner).toHaveTextContent('only managed sessions can be restarted');
  });

  it('closes the error banner with X, or on its own after a few seconds', async () => {
    const transport = controlServer(
      () => [managed],
      () => ({ type: 'error', code: 'unavailable', message: 'broker busy' }),
    );
    renderWithApp(<Selected session={managed} />, { transport });
    await userEvent.click(await screen.findByRole('button', { name: 'Restart' }));
    const banner = await screen.findByRole('alert');
    expect(banner).toHaveTextContent('The PC can');
    await userEvent.click(within(banner).getByRole('button', { name: 'Close' }));
    expect(screen.queryByRole('alert')).toBeNull();

    await userEvent.click(screen.getByRole('button', { name: 'Restart' }));
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull(), { timeout: 8000 });
  }, 12000);

  it('replaces the session list after a reconnect, so no stale card survives', async () => {
    let list = [managed, observed];
    const transport = controlServer(() => list);
    renderWithApp(<CardIds />, { transport });
    await waitFor(() => expect(screen.getByTestId('cards')).toHaveTextContent('m1,obs1'));
    // The PC's server restarted: observed sessions came back under new ids.
    list = [managed, { ...observed, id: 'obs2' }];
    const t = transport as unknown as { setState(s: string): void };
    act(() => t.setState('disconnected'));
    act(() => t.setState('connecting'));
    act(() => t.setState('connected'));
    await waitFor(() => expect(screen.getByTestId('cards')).toHaveTextContent(/^m1,obs2$/));
  });
});
