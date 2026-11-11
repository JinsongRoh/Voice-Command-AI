import { act, fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { VoiceIO } from '../speech/handsFree';
import { VoiceIOProvider } from '../speech/useHandsFree';
import { VocoTestProvider, type VocoListenerLike } from '../speech/useVoco';
import { UNDO_MS, type VocoTimers } from '../speech/vocoFlow';
import type { HeardUtterance, VocoListenerOptions } from '../speech/vocoListener';
import { localSession } from '../test/fixtures';
import type { FakeTransport } from '../test/fakeTransport';
import { renderWithApp } from '../test/render';
import { Composer } from './Composer';
import { HandsFreeSettings } from './HandsFree';

let clock = 1_000_000;
beforeEach(() => {
  clock = 1_000_000;
  vi.spyOn(Date, 'now').mockImplementation(() => clock);
  localStorage.clear();
});
afterEach(() => {
  vi.restoreAllMocks();
});

function fakeVoice(answers: (string | null)[] = []) {
  const queue = [...answers];
  const spoken: string[] = [];
  const io = {
    spoken,
    speak: vi.fn(async (text: string) => {
      spoken.push(text);
    }),
    listen: vi.fn<(signal: AbortSignal) => Promise<string | null>>(async () =>
      queue.length ? queue.shift()! : null,
    ),
  } satisfies VoiceIO & { spoken: string[] };
  return io;
}

/** Timers of the flow, advanced by hand. */
function manualTimers() {
  let now = 0;
  const queue: { at: number; fn: () => void; dead: boolean }[] = [];
  const timers: VocoTimers & { advance(ms: number): Promise<void> } = {
    now: () => now,
    set(fn, ms) {
      const t = { at: now + ms, fn, dead: false };
      queue.push(t);
      return () => {
        t.dead = true;
      };
    },
    async advance(ms) {
      const end = now + ms;
      for (;;) {
        const next = queue.filter((t) => !t.dead && t.at <= end).sort((a, b) => a.at - b.at)[0];
        if (!next) break;
        now = next.at;
        next.dead = true;
        await act(async () => next.fn());
      }
      now = end;
    },
  };
  return timers;
}

const SETTINGS = { handsFree: true, handsFreeNoteSeen: true, language: 'ko' as const };

/**
 * The composer with a scripted listener and STT: `say(key)` plays one
 * utterance whose transcript is `script[key]`.
 */
function renderVoco(
  script: Record<string, string>,
  settings: object = SETTINGS,
  session = localSession(),
  layout?: 'phone',
) {
  const io = fakeVoice(['네']);
  const timers = manualTimers();
  const names = new Map<Blob, string>();
  let options: VocoListenerOptions | null = null;
  const listener = {
    start: vi.fn<() => void>(),
    pause: vi.fn<() => Promise<void>>(async () => {}),
    resume: vi.fn<() => void>(),
    stop: vi.fn<() => Promise<void>>(async () => {}),
  } satisfies VocoListenerLike;
  const chimes: string[] = [];
  const transcribe = vi.fn(async (audio: Blob) => ({
    text: names.has(audio) ? (script[names.get(audio)!] ?? '') : 'commit the changes',
    corrections: [],
  }));
  const rec = {
    start: vi.fn(async () => {}),
    stop: vi.fn(async () => new Blob(['a'])),
    cancel: vi.fn(),
  };
  const result = renderWithApp(
    <VocoTestProvider
      hooks={{
        createListener: (o) => {
          options = o;
          return listener;
        },
        chime: (k) => chimes.push(k),
        timers,
      }}
    >
      <VoiceIOProvider io={io}>
        <Composer session={session} layout={layout} />
      </VoiceIOProvider>
    </VocoTestProvider>,
    { speech: { createRecorder: () => rec, transcribe }, settings },
  );
  const say = async (key: string) => {
    const blob = new Blob([key]);
    names.set(blob, key);
    const u: HeardUtterance = {
      audio: blob,
      head: () => blob,
      speechMs: 1000,
      pausedBeforeLast: false,
      endedAt: timers.now(),
    };
    await act(async () => {
      options!.onSpeechStart();
      options!.onSpeechEnd(u);
    });
  };
  return { ...result, io, timers, listener, chimes, transcribe, rec, say };
}

const sentTexts = (transport: FakeTransport) =>
  transport
    .bodies('send_input')
    .map((b) => (b.type === 'send_input' && b.input.type === 'text' ? b.input.text : null));

describe('Voco in the composer', () => {
  it('"보컬" wakes, the message is dictated and sent after "보내줘 오버"', async () => {
    const { transport, io, timers, say, chimes, listener } = renderVoco({
      a: '보컬',
      b: '어 로그인 버그 고쳐줘 보내줘 오버',
    });
    expect(listener.start).toHaveBeenCalled();
    expect(screen.getByTestId('voco-status')).toHaveTextContent('Waiting · say "Voco"');
    await say('a');
    await waitFor(() => expect(screen.getByTestId('voco-status')).toHaveTextContent('Listening…'));
    expect(io.spoken).toEqual(['네']);
    expect(chimes).toEqual(['wake']);
    // The voice was spoken with the microphone released.
    expect(listener.pause).toHaveBeenCalled();
    await say('b');
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue('어 로그인 버그 고쳐줘'));
    expect(screen.getByTestId('voco-status')).toHaveTextContent('Sending in 3 s');
    expect(io.spoken[1]).toBe('로저, payments-api 세션에 전송합니다');
    expect(sentTexts(transport)).toEqual([]);
    await timers.advance(UNDO_MS);
    await waitFor(() => expect(sentTexts(transport)).toEqual(['어 로그인 버그 고쳐줘']));
    expect(screen.getByRole('textbox')).toHaveValue('');
    expect(screen.getByTestId('voco-status')).toHaveTextContent('Sent');
  });

  it('works in the compact phone row: idle chip in the tray, live states above the row', async () => {
    const { transport, timers, say } = renderVoco(
      { a: '보컬', b: '어 로그인 버그 고쳐줘 보내줘 오버' },
      SETTINGS,
      localSession(),
      'phone',
    );
    // At rest the row stays one line: the idle chip is in the tools tray.
    expect(screen.queryByTestId('composer-voice-line')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'More input tools' }));
    const tray = screen.getByTestId('composer-tray');
    expect(within(tray).getByTestId('voco-status')).toHaveTextContent('Waiting · say "Voco"');
    expect(within(tray).getByRole('button', { name: 'Hands-free mode' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    await userEvent.keyboard('{Escape}');
    // Called by name: the live state shows above the row.
    await say('a');
    await waitFor(() =>
      expect(
        within(screen.getByTestId('composer-voice-line')).getByTestId('voco-status'),
      ).toHaveTextContent('Listening…'),
    );
    await say('b');
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue('어 로그인 버그 고쳐줘'));
    await timers.advance(UNDO_MS);
    await waitFor(() => expect(sentTexts(transport)).toEqual(['어 로그인 버그 고쳐줘']));
  });

  it('never sends while the agent waits for a decision: the text stays', async () => {
    const waiting = localSession({
      state: { type: 'awaiting_decision' },
      pending_decision: {
        at: 100,
        decision: { id: 'd1', prompt: 'Allow?', options: [{ id: 'yes', label: 'Yes' }] },
      },
    });
    const { transport, timers, say } = renderVoco(
      { a: '보코', b: '테스트 돌려 줘 오버' },
      SETTINGS,
      waiting,
    );
    await say('a');
    await say('b');
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue('테스트 돌려 줘'));
    await timers.advance(UNDO_MS + 10_000);
    expect(sentTexts(transport)).toEqual([]);
    expect(screen.getByRole('textbox')).toHaveValue('테스트 돌려 줘');
  });

  it('"취소" in the undo window keeps the message unsent', async () => {
    const { transport, io, timers, say } = renderVoco({
      a: '보코',
      b: '테스트 돌려 줘 오버',
      c: '취소',
    });
    await say('a');
    await say('b');
    await timers.advance(1000);
    await say('c');
    await timers.advance(10_000);
    expect(sentTexts(transport)).toEqual([]);
    expect(io.spoken).toContain('취소했어요. 보내지 않았어요.');
    expect(screen.getByRole('textbox')).toHaveValue('테스트 돌려 줘');
  });

  it('ordinary speech is never shown or sent', async () => {
    const { transport, io, say, transcribe } = renderVoco({ a: '보컬 트레이닝 앱 만들어' });
    await say('a');
    await timers0();
    expect(screen.getByRole('textbox')).toHaveValue('');
    expect(io.spoken).toEqual([]);
    expect(sentTexts(transport)).toEqual([]);
    // A wake check: the one-word prompt and the language, no correction pass.
    expect(transcribe).toHaveBeenCalledWith(
      expect.any(Blob),
      expect.objectContaining({ context: '보코', language: 'ko', final: false }),
    );
  });

  it('is off without the wake word setting', () => {
    const { listener } = renderVoco({}, { ...SETTINGS, vocoWake: false });
    expect(listener.start).not.toHaveBeenCalled();
    expect(screen.queryByTestId('voco-status')).not.toBeInTheDocument();
  });

  it('the tap path still works: the listener lets go of the microphone first', async () => {
    const { transport, listener, rec, io } = renderVoco({}, { ...SETTINGS, language: 'en' });
    io.listen.mockResolvedValueOnce('yes');
    const order: string[] = [];
    listener.pause.mockImplementation(async () => {
      order.push('pause');
    });
    rec.start.mockImplementation(async () => {
      order.push('start');
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Tap to dictate' }));
    });
    clock += 2000;
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /tap to stop/ }));
    });
    await waitFor(() => expect(sentTexts(transport)).toEqual(['commit the changes']));
    expect(order.slice(0, 2)).toEqual(['pause', 'start']);
    // Listening resumes once the composer is idle again.
    await waitFor(() => expect(listener.start.mock.calls.length).toBeGreaterThanOrEqual(2));
  });
});

/** Lets pending promises run. */
const timers0 = () => act(async () => new Promise<void>((r) => setTimeout(r, 0)));

describe('Voco settings', () => {
  it('shows the wake word options (on by default)', async () => {
    renderWithApp(<HandsFreeSettings />, { settings: { language: 'en', handsFree: true } });
    const wake = screen.getByRole('checkbox', { name: 'Wake word "Voco"' });
    expect(wake).toBeChecked();
    expect(
      screen.getByRole('checkbox', {
        name: 'Schedule the send when a message ends like a command',
      }),
    ).toBeChecked();
    const confirm = screen.getByRole('combobox', { name: 'When a message ends' });
    expect(confirm).toHaveValue('undo');
    await userEvent.selectOptions(confirm, 'readback');
    expect(confirm).toHaveValue('readback');
    const words = screen.getByRole('textbox', { name: 'Extra wake words' });
    await userEvent.type(words, '자비스');
    expect(words).toHaveValue('자비스');
    await userEvent.click(wake);
    expect(wake).not.toBeChecked();
    expect(words).toBeDisabled();
  });
});
