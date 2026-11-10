import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { Toasts } from '../components/Toasts';
import type { SessionInfo } from '../protocol/types';
import { voiceQueue } from '../speech/voiceQueue';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { resetInputGuard } from './inputGuard';
import type { ApprovalMode, Settings } from './settings';
import { useActions, useAppState } from './store';

// Spoken alerts go through the alert voice; nothing real is played.
const speak = vi.hoisted(() => vi.fn(async () => 'device' as const));
vi.mock('../speech/alertVoice', () => ({
  createAlertVoice: () => ({ speak, play: speak, stop: vi.fn(), clear: vi.fn() }),
}));

const shown: string[] = [];
class FakeNotification {
  static permission = 'granted';
  onclick: (() => void) | null = null;
  constructor(readonly title: string) {
    shown.push(title);
  }
  close() {}
}

function server() {
  return new FakeTransport((msg) =>
    msg.type === 'list_sessions'
      ? {
          type: 'sessions',
          sessions: [localSession(), localSession({ id: 's2', display_name: 'web' })],
        }
      : defaultResponse(msg),
  );
}

function Probe({ select }: { select?: string }) {
  const actions = useActions();
  const selected = useAppState((s) => s.selectedId);
  return (
    <>
      {select && (
        <button type="button" onClick={() => actions.select(select)}>
          select
        </button>
      )}
      <output data-testid="selected">{selected ?? ''}</output>
    </>
  );
}

async function setup(
  mode: ApprovalMode,
  {
    focused,
    select,
    settings,
  }: { focused: boolean; select?: string; settings?: Partial<Settings> },
) {
  vi.spyOn(document, 'hasFocus').mockReturnValue(focused);
  const transport = server();
  renderWithApp(
    <>
      <Probe select={select} />
      <Toasts onOpen={() => {}} />
    </>,
    { transport, settings: { approvalMode: mode, ...settings } },
  );
  await waitFor(() => expect(transport.bodies('list_sessions')).toHaveLength(1));
  if (select) fireEvent.click(screen.getByRole('button', { name: 'select' }));
  return transport;
}

function wait(transport: FakeTransport, state: SessionInfo['state']) {
  act(() =>
    transport.push({
      type: 'session_updated',
      session: localSession({ id: 's2', display_name: 'web', state }),
    }),
  );
}

beforeEach(() => {
  shown.length = 0;
  speak.mockClear();
  voiceQueue.stopAll();
  vi.stubGlobal('Notification', FakeNotification);
});
afterEach(() => {
  resetInputGuard();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('Switch automatically', () => {
  it('opens the card and still speaks and notifies while the window is in the background', async () => {
    const transport = await setup('auto', { focused: false, settings: { spokenAlerts: true } });
    wait(transport, { type: 'awaiting_decision' });
    await waitFor(() => expect(screen.getByTestId('selected')).toHaveTextContent('s2'));
    expect(shown).toEqual(['web needs a decision']);
    await waitFor(() =>
      expect(speak).toHaveBeenCalledWith(
        'web needs a decision',
        expect.any(String),
        expect.anything(),
        expect.any(Function),
      ),
    );
    // The card is open: no Go / Later on top of it.
    expect(screen.queryByRole('button', { name: 'Go' })).toBeNull();
  });

  it('speaks a decision under hands-free even with spoken alerts off', async () => {
    const transport = await setup('auto', {
      focused: false,
      settings: { spokenAlerts: false, handsFree: true },
    });
    // Hidden: the decision card cannot read itself out on screen.
    vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden');
    wait(transport, { type: 'awaiting_decision' });
    await waitFor(() => expect(speak).toHaveBeenCalled());
    expect(shown).toEqual(['web needs a decision']);
  });

  it('sends no OS notification while the app is in front', async () => {
    const transport = await setup('auto', { focused: true });
    wait(transport, { type: 'awaiting_decision' });
    await waitFor(() => expect(screen.getByTestId('selected')).toHaveTextContent('s2'));
    expect(shown).toEqual([]);
  });
});

describe('in-app alert toasts', () => {
  it('are skipped for the session being watched, for every kind', async () => {
    const transport = await setup('ask', { focused: true, select: 's2' });
    wait(transport, { type: 'awaiting_input' });
    wait(transport, { type: 'running' });
    wait(transport, { type: 'awaiting_decision' });
    await act(async () => {});
    expect(screen.queryByRole('button', { name: 'Go' })).toBeNull();
    expect(screen.queryByText(/web needs a decision|web is waiting for input/)).toBeNull();
  });

  it('are shown for the selected session while the window is not focused', async () => {
    const transport = await setup('ask', { focused: false, select: 's2' });
    wait(transport, { type: 'awaiting_decision' });
    expect(await screen.findByRole('button', { name: 'Go' })).toBeInTheDocument();
    expect(screen.getByText('web needs a decision')).toBeInTheDocument();
  });

  it('are shown for another session', async () => {
    const transport = await setup('ask', { focused: true, select: 's1' });
    wait(transport, { type: 'awaiting_input' });
    expect(await screen.findByText('web is waiting for input')).toBeInTheDocument();
  });
});
