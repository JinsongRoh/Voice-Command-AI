import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ClientMsg, ServerMsg } from '../protocol/types';
import type { Transcript } from '../speech/transcribe';
import { voiceQueue } from '../speech/voiceQueue';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { AlertsView } from './AlertsView';
import { Composer } from './Composer';
import { ResultSummarySettings } from './ResultSummarySettings';
import { Toasts } from './Toasts';

// Spoken output goes through the alert voice (queued `play`); nothing real
// is played.
const speak = vi.hoisted(() => vi.fn(async () => 'device' as const));
vi.mock('../speech/alertVoice', () => ({
  createAlertVoice: () => ({ speak, play: speak, stop: vi.fn(), clear: vi.fn() }),
}));

const BRIEFING = '세션 2개: 실행 중 2개. 기다리는 세션은 없습니다.';

function transport(extra: (msg: ClientMsg) => ServerMsg | null = () => null) {
  return new FakeTransport((msg) => {
    const own = extra(msg);
    if (own) return own;
    switch (msg.type) {
      case 'list_sessions':
        return {
          type: 'sessions',
          sessions: [localSession(), localSession({ id: 's2', display_name: 'web' })],
        };
      case 'briefing':
        return { type: 'briefing', text: BRIEFING };
      case 'summarize':
        return { type: 'summary', session_id: msg.session_id, text: 'Session 2: Fixed the build.' };
      default:
        return defaultResponse(msg);
    }
  });
}

let clock = 1_000_000;
beforeEach(() => {
  clock = 1_000_000;
  voiceQueue.stopAll();
  speak.mockClear();
  vi.spyOn(Date, 'now').mockImplementation(() => clock);
});
afterEach(() => {
  vi.restoreAllMocks();
});

describe('status briefing', () => {
  it('the Alerts button asks the PC in card order, shows and speaks the briefing', async () => {
    const t = transport();
    renderWithApp(
      <>
        <AlertsView />
        <Toasts onOpen={() => {}} />
      </>,
      { transport: t },
    );
    const button = await screen.findByRole('button', { name: /Listen to status/ });
    await waitFor(() => expect(button).toBeEnabled());
    await userEvent.click(button);
    await waitFor(() => expect(t.bodies('briefing')).toHaveLength(1));
    expect(t.bodies('briefing')[0]).toEqual({
      type: 'briefing',
      language: 'en',
      order: ['s1', 's2'],
    });
    expect(await screen.findByText(BRIEFING)).toBeInTheDocument();
    expect(speak).toHaveBeenCalledWith(
      BRIEFING,
      expect.any(String),
      expect.anything(),
      expect.any(Function),
    );
  });

  describe('spoken trigger in the composer', () => {
    const tr = (text: string): Transcript => ({ text, corrections: [] });
    function recorder() {
      return {
        start: vi.fn(async () => {}),
        stop: vi.fn(async () => new Blob(['a'])),
        cancel: vi.fn(),
      };
    }

    async function dictate(text: string, t: FakeTransport) {
      const transcribe = vi.fn(async () => tr(text));
      renderWithApp(
        <>
          <Composer session={localSession()} />
          <Toasts onOpen={() => {}} />
        </>,
        { transport: t, speech: { createRecorder: recorder, transcribe } },
      );
      const mic = await screen.findByRole('button', { name: 'Tap to dictate' });
      await act(async () => {
        fireEvent.click(mic);
      });
      clock += 2_000;
      await act(async () => {
        fireEvent.click(mic);
      });
      await waitFor(() => expect(transcribe).toHaveBeenCalled());
    }

    it('runs the briefing for an exact phrase and can still send it', async () => {
      const t = transport();
      await dictate('현황 알려줘', t);
      await waitFor(() => expect(t.bodies('briefing')).toHaveLength(1));
      // Nothing went to the session and the box is empty again.
      expect(t.bodies('send_input')).toHaveLength(0);
      expect(screen.getByRole('textbox')).toHaveValue('');
      expect(await screen.findByText('Taken as a status request')).toBeInTheDocument();
      expect(speak).toHaveBeenCalled();
      // "Send anyway" sends the phrase to the session after all.
      await userEvent.click(screen.getByRole('button', { name: 'Send anyway' }));
      await waitFor(() =>
        expect(t.bodies('send_input')).toEqual([
          {
            type: 'send_input',
            session_id: 's1',
            input: { type: 'text', text: '현황 알려줘', submit: true },
          },
        ]),
      );
    });

    it('leaves any other dictation in the text box', async () => {
      const t = transport();
      await dictate('현황 알려줘 그리고 테스트 돌려줘', t);
      await waitFor(() =>
        expect(screen.getByRole('textbox')).toHaveValue('현황 알려줘 그리고 테스트 돌려줘'),
      );
      expect(t.bodies('briefing')).toHaveLength(0);
      expect(screen.queryByText('Taken as a status request')).not.toBeInTheDocument();
    });
  });
});

describe('result summaries', () => {
  it('are off by default; turning them on tells the PC', async () => {
    const t = transport();
    renderWithApp(<ResultSummarySettings />, { transport: t });
    const box = screen.getByRole('checkbox', { name: 'Read work results aloud' });
    expect(box).not.toBeChecked();
    expect(box).toHaveAccessibleDescription(/reads conversation content/);
    expect(screen.queryByRole('checkbox', { name: /Include commands/ })).toBeNull();
    await waitFor(() => expect(t.state).toBe('connected'));
    await userEvent.click(box);
    await waitFor(() =>
      expect(t.bodies('set_voice_summary')).toEqual([
        { type: 'set_voice_summary', enabled: true, details: false },
      ]),
    );
    await userEvent.click(screen.getByRole('checkbox', { name: /Include commands/ }));
    await waitFor(() => expect(t.bodies('set_voice_summary')).toHaveLength(2));
    expect(t.bodies('set_voice_summary')[1]).toEqual({
      type: 'set_voice_summary',
      enabled: true,
      details: true,
    });
  });

  it('without summaries a finished turn says what finished (by the session name)', async () => {
    const off = transport();
    renderWithApp(<Toasts onOpen={() => {}} />, {
      transport: off,
      settings: { spokenAlerts: true },
    });
    await waitFor(() => expect(off.bodies('list_sessions')).toHaveLength(1));
    act(() =>
      off.push({
        type: 'session_updated',
        session: localSession({ id: 's2', display_name: 'web', state: { type: 'idle' } }),
      }),
    );
    await waitFor(() =>
      expect(speak).toHaveBeenCalledWith(
        'web finished its work.',
        expect.any(String),
        expect.anything(),
        expect.any(Function),
      ),
    );
    expect(off.bodies('summarize')).toHaveLength(0);
  });

  it('a finished turn is summarized and spoken only with the setting on', async () => {
    const on = transport();
    renderWithApp(<Toasts onOpen={() => {}} />, {
      transport: on,
      settings: { resultSummary: true, spokenAlerts: true },
    });
    await waitFor(() => expect(on.bodies('list_sessions')).toHaveLength(1));
    // The opt-in is sent when connecting.
    expect(on.bodies('set_voice_summary')).toEqual([
      { type: 'set_voice_summary', enabled: true, details: false },
    ]);
    act(() =>
      on.push({
        type: 'session_updated',
        session: localSession({ id: 's2', display_name: 'web', state: { type: 'idle' } }),
      }),
    );
    await waitFor(() =>
      expect(on.bodies('summarize')).toEqual([
        { type: 'summarize', session_id: 's2', language: 'en', number: 2 },
      ]),
    );
    await waitFor(() =>
      expect(speak).toHaveBeenCalledWith(
        'Session 2: Fixed the build.',
        expect.any(String),
        expect.anything(),
        expect.any(Function),
      ),
    );
    // The summary replaces the plain completion alert.
    expect(speak).not.toHaveBeenCalledWith(
      'web finished its work.',
      expect.anything(),
      expect.anything(),
      expect.anything(),
    );
  });
});
