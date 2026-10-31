import { act, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect } from 'react';
import { describe, expect, it, vi } from 'vitest';

import type { SessionInfo } from '../protocol/types';
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
