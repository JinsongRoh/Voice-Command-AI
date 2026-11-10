import { act, fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ClientMsg, ServerMsg, SessionInfo } from '../protocol/types';
import { useActions, useAppState } from '../state/store';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { LiveView } from './LiveView';
import { SessionDeck } from './SessionDeck';

vi.mock('./TerminalView', () => ({ default: () => <div data-testid="terminal" /> }));

function deckWith(session: SessionInfo, respond?: (msg: ClientMsg) => ServerMsg | undefined) {
  const transport = new FakeTransport(
    (msg) =>
      respond?.(msg) ??
      (msg.type === 'list_sessions'
        ? { type: 'sessions', sessions: [session] }
        : defaultResponse(msg)),
  );
  renderWithApp(<SessionDeck onOpen={() => {}} onNewSession={() => {}} />, { transport });
  return transport;
}

async function openRenameFromMenu(name: string) {
  await userEvent.click(await screen.findByRole('button', { name: `More actions for ${name}` }));
  const menu = screen.getByRole('menu');
  await userEvent.click(within(menu).getByRole('menuitem', { name: 'Rename' }));
  return screen.getByRole('dialog', { name: 'Rename session' });
}

const renames = (t: FakeTransport) =>
  t.bodies('rename_session') as Extract<ClientMsg, { type: 'rename_session' }>[];

afterEach(() => vi.useRealTimers());

describe('Rename session', () => {
  it('renames from the card menu with Enter and shows the pushed name', async () => {
    const session = localSession({ auto_name: 'payments-api', custom_name: false });
    const transport = deckWith(session);
    const dialog = await openRenameFromMenu('payments-api');
    const input = within(dialog).getByRole('textbox', { name: 'Session name' });
    expect(input).toHaveValue('payments-api');
    expect(input).toHaveFocus();
    expect(within(dialog).getByText('Automatic name: payments-api')).toBeInTheDocument();
    // No reset while the automatic name is in use.
    expect(within(dialog).queryByRole('button', { name: 'Reset to automatic name' })).toBeNull();

    await userEvent.clear(input);
    await userEvent.type(input, '  API work {Enter}');
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(renames(transport)).toEqual([
      { type: 'rename_session', session_id: 's1', name: 'API work' },
    ]);
    // The card follows the server's `session_updated`.
    act(() =>
      transport.push({
        type: 'session_updated',
        session: { ...session, display_name: 'API work', custom_name: true },
      }),
    );
    await waitFor(() =>
      expect(screen.getByTestId('session-card-s1')).toHaveAccessibleName(/API work/),
    );
    expect(screen.getByRole('button', { name: 'More actions for API work' })).toBeInTheDocument();
  });

  it('cancels with Escape without sending anything', async () => {
    const transport = deckWith(localSession());
    const dialog = await openRenameFromMenu('payments-api');
    await userEvent.type(within(dialog).getByRole('textbox'), 'other');
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(renames(transport)).toHaveLength(0);
    // Focus goes back to the card menu button.
    expect(screen.getByRole('button', { name: 'More actions for payments-api' })).toHaveFocus();
  });

  it('resets a custom name to the automatic one', async () => {
    const transport = deckWith(
      localSession({ display_name: 'API work', auto_name: 'payments-api', custom_name: true }),
    );
    const dialog = await openRenameFromMenu('API work');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Reset to automatic name' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(renames(transport)).toEqual([{ type: 'rename_session', session_id: 's1', name: null }]);
  });

  it('validates length and shows server errors', async () => {
    const transport = deckWith(localSession(), (msg) =>
      msg.type === 'rename_session'
        ? { type: 'error', code: 'forbidden', message: 'not allowed' }
        : undefined,
    );
    const dialog = await openRenameFromMenu('payments-api');
    const input = within(dialog).getByRole('textbox');
    const save = within(dialog).getByRole('button', { name: 'Save' });
    fireEvent.change(input, { target: { value: 'x'.repeat(81) } });
    expect(within(dialog).getByText('Use at most 80 characters.')).toBeInTheDocument();
    expect(input).toHaveAttribute('aria-invalid', 'true');
    expect(save).toBeDisabled();

    fireEvent.change(input, { target: { value: 'x'.repeat(80) } });
    expect(save).toBeEnabled();
    await userEvent.click(save);
    expect(await within(dialog).findByRole('alert')).toHaveTextContent(
      'Could not rename the session: not allowed',
    );
    expect(renames(transport)).toHaveLength(1);
  });

  it('opens the card menu with a long press without selecting the card', async () => {
    vi.useFakeTimers();
    const onOpen = vi.fn();
    const transport = new FakeTransport((msg) =>
      msg.type === 'list_sessions'
        ? { type: 'sessions', sessions: [localSession()] }
        : defaultResponse(msg),
    );
    renderWithApp(<SessionDeck onOpen={onOpen} onNewSession={() => {}} />, { transport });
    await act(() => vi.runOnlyPendingTimersAsync());
    const card = screen.getByTestId('session-card-s1');
    fireEvent.pointerDown(card, { pointerType: 'touch', clientX: 10, clientY: 10 });
    act(() => vi.advanceTimersByTime(600));
    fireEvent.pointerUp(card);
    fireEvent.click(card);
    expect(screen.getByRole('menu')).toBeInTheDocument();
    expect(onOpen).not.toHaveBeenCalled();
    expect(screen.getByRole('menuitem', { name: 'Rename' })).toHaveFocus();

    // A right click opens it too; Escape closes it.
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
    fireEvent.contextMenu(card);
    expect(screen.getByRole('menu')).toBeInTheDocument();
  });

  it('renames from the session detail header', async () => {
    const session = localSession({ auto_name: 'payments-api' });
    const transport = new FakeTransport((msg) =>
      msg.type === 'list_sessions'
        ? { type: 'sessions', sessions: [session] }
        : defaultResponse(msg),
    );
    function Selected() {
      const actions = useActions();
      const current = useAppState((s) => s.sessions[session.id]);
      useEffect(() => {
        if (current) actions.select(session.id);
      }, [actions, current]);
      return current ? <LiveView session={current} /> : null;
    }
    renderWithApp(<Selected />, { transport });
    await userEvent.click(await screen.findByRole('button', { name: 'Rename session' }));
    const dialog = screen.getByRole('dialog', { name: 'Rename session' });
    const input = within(dialog).getByRole('textbox');
    await userEvent.clear(input);
    await userEvent.type(input, 'Billing{Enter}');
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(renames(transport)).toEqual([
      { type: 'rename_session', session_id: 's1', name: 'Billing' },
    ]);
    act(() =>
      transport.push({
        type: 'session_updated',
        session: { ...session, display_name: 'Billing', custom_name: true },
      }),
    );
    expect(await screen.findByRole('heading', { name: 'Billing' })).toBeInTheDocument();
  });
});
