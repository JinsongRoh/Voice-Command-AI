import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useSyncExternalStore } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { DangerLevel, Decision } from '../protocol/types';
import type { VoiceIO } from '../speech/handsFree';
import { VoiceIOProvider } from '../speech/useHandsFree';
import { loadVoiceAnswers } from '../speech/voiceAudit';
import { localSession } from '../test/fixtures';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { renderWithApp } from '../test/render';
import { Composer } from './Composer';
import { DecisionSheet } from './DecisionSheet';
import { HandsFreeSettings } from './HandsFree';

/** Controlled clock for recording durations (Date.now). */
let clock = 1_000_000;
beforeEach(() => {
  clock = 1_000_000;
  vi.spyOn(Date, 'now').mockImplementation(() => clock);
  localStorage.clear();
});
afterEach(() => {
  vi.restoreAllMocks();
});

/**
 * Scripted voice: `speak` records the text (and, with `holdSpeech`, waits
 * until the flow is interrupted); `listen` returns the scripted answers.
 */
function fakeVoice(answers: (string | null)[], { holdSpeech = false } = {}) {
  const queue = [...answers];
  const spoken: string[] = [];
  const io = {
    spoken,
    speak: vi.fn(
      (text: string, signal: AbortSignal) =>
        new Promise<void>((resolve) => {
          spoken.push(text);
          if (!holdSpeech) return resolve();
          signal.addEventListener('abort', () => resolve(), { once: true });
        }),
    ),
    listen: vi.fn<(signal: AbortSignal) => Promise<string | null>>(async () =>
      queue.length ? queue.shift()! : null,
    ),
  } satisfies VoiceIO & { spoken: string[] };
  return io;
}

const HANDS_FREE = { handsFree: true, handsFreeNoteSeen: true, language: 'en' as const };

function recorder() {
  return {
    start: vi.fn(async () => {}),
    stop: vi.fn(async () => new Blob(['a'])),
    cancel: vi.fn(),
  };
}

/** Taps the mic, waits, taps again: one dictation of `heard`. */
async function dictate() {
  const mic = screen.getByRole('button', { name: 'Tap to dictate' });
  await act(async () => {
    fireEvent.click(mic);
  });
  clock += 2000;
  await act(async () => {
    fireEvent.click(screen.getByRole('button', { name: /tap to stop/ }));
  });
}

function renderComposer(
  io: VoiceIO,
  settings: object = HANDS_FREE,
  { text = 'commit the changes', name }: { text?: string; name?: string } = {},
) {
  const rec = recorder();
  const transcribe = vi.fn(async () => ({ text, corrections: [] }));
  const session = name ? localSession({ display_name: name }) : localSession();
  const result = renderWithApp(
    <VoiceIOProvider io={io}>
      <Composer session={session} />
    </VoiceIOProvider>,
    { speech: { createRecorder: () => rec, transcribe }, settings },
  );
  return { ...result, rec, transcribe };
}

const sentTexts = (transport: FakeTransport) =>
  transport
    .bodies('send_input')
    .map((b) => (b.type === 'send_input' && b.input.type === 'text' ? b.input.text : null));

describe('hands-free composer', () => {
  it('is off by default: no read-back', async () => {
    const io = fakeVoice(['yes']);
    const { transport } = renderComposer(io, { language: 'en' });
    await dictate();
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue('commit the changes'));
    expect(io.speak).not.toHaveBeenCalled();
    expect(sentTexts(transport)).toEqual([]);
    expect(screen.getByRole('button', { name: 'Hands-free mode' })).toHaveAttribute(
      'aria-pressed',
      'false',
    );
  });

  it('reads the text back and sends it on "yes"', async () => {
    const io = fakeVoice(['Yes.']);
    const { transport } = renderComposer(io);
    await dictate();
    await waitFor(() => expect(sentTexts(transport)).toEqual(['commit the changes']));
    expect(io.spoken[0]).toBe('To payments-api: commit the changes. Send it?');
    expect(io.listen).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId('hands-free-status')).toHaveTextContent('Sent');
    expect(screen.getByRole('textbox')).toHaveValue('');
    await waitFor(() => expect(io.spoken).toContain('Sent.'));
  });

  it('cancels on "no" and keeps the text', async () => {
    const io = fakeVoice(['아니']);
    const { transport } = renderComposer(io);
    await dictate();
    await waitFor(() =>
      expect(screen.getByTestId('hands-free-status')).toHaveTextContent('Cancelled'),
    );
    expect(sentTexts(transport)).toEqual([]);
    expect(screen.getByRole('textbox')).toHaveValue('commit the changes');
  });

  it('dictates again on "다시"', async () => {
    const io = fakeVoice(['다시']);
    const { rec, transport } = renderComposer(io);
    await dictate();
    await waitFor(() => expect(rec.start).toHaveBeenCalledTimes(2));
    expect(screen.getByRole('textbox')).toHaveValue('');
    expect(screen.getByRole('button', { name: /tap to stop/ })).toBeInTheDocument();
    expect(sentTexts(transport)).toEqual([]);
  });

  it('sends on a yes word with fillers and courtesy words', async () => {
    const io = fakeVoice(['Um, send it now please.']);
    const { transport } = renderComposer(io);
    await dictate();
    await waitFor(() => expect(sentTexts(transport)).toEqual(['commit the changes']));
  });

  it('asks again on unclear answers, then keeps the text unsent', async () => {
    const io = fakeVoice(['maybe later', 'hmm', 'what was that']);
    const { transport } = renderComposer(io);
    await dictate();
    await waitFor(() =>
      expect(screen.getByTestId('hands-free-status')).toHaveTextContent(
        'Not sent: the text is kept',
      ),
    );
    expect(io.listen).toHaveBeenCalledTimes(3);
    expect(io.spoken.slice(1, 3)).toEqual([
      'Send it? Say yes or cancel.',
      'Send it? Say yes or cancel.',
    ]);
    expect(sentTexts(transport)).toEqual([]);
    expect(screen.getByRole('textbox')).toHaveValue('commit the changes');
    await waitFor(() => expect(io.spoken).toContain('Not sent. The text is still in the box.'));
    expect(io.spoken).not.toContain('Cancelled.');
  });

  it('keeps the text on silence: never a cancel', async () => {
    const io = fakeVoice([null, null, null]);
    const { transport } = renderComposer(io);
    await dictate();
    await waitFor(() =>
      expect(screen.getByTestId('hands-free-status')).toHaveTextContent(
        'Not sent: the text is kept',
      ),
    );
    expect(io.listen).toHaveBeenCalledTimes(3);
    expect(sentTexts(transport)).toEqual([]);
    expect(screen.getByRole('textbox')).toHaveValue('commit the changes');
  });

  // Regression: "보내" after the read-back was cancelled instead of sent.
  it('sends on "보내" in Korean, also after a silent first answer', async () => {
    const io = fakeVoice([null, '어, 보내.']);
    const { transport } = renderComposer(
      io,
      { ...HANDS_FREE, language: 'ko' },
      {
        text: '로그인 화면의 버튼 색을 파란색으로 바꾸고 테스트를 다시 돌린 다음 결과를 알려 줘',
        name: '3번 세션',
      },
    );
    await dictate();
    await waitFor(() => expect(sentTexts(transport)).toHaveLength(1));
    expect(io.spoken[0]).toBe(
      '삼번 세션에 로그인 화면의 버튼 색을 파란색으로 바꾸고 테스트를 다시… 보낼까요?',
    );
    expect(io.spoken[1]).toBe('보낼까요? 응 또는 취소로 답해 주세요.');
    // The UI is English here; the voice follows the language setting.
    expect(screen.getByTestId('hands-free-status')).toHaveTextContent('Sent');
    await waitFor(() => expect(io.spoken).toContain('보냈습니다.'));
    expect(io.spoken).not.toContain('취소했습니다.');
  });

  // Intermittent field failure: the answer was classified "send" but nothing
  // was sent. The text approved by the read-back is what is sent, even when
  // the box was rewritten meanwhile (a late dictation update).
  it('sends the read-back text even if the box changes while listening', async () => {
    const io = fakeVoice([]);
    io.listen = vi.fn(async () => {
      fireEvent.change(screen.getByRole('textbox'), { target: { value: 'something else' } });
      return '보내';
    });
    const { transport } = renderComposer(io, { ...HANDS_FREE, language: 'ko' });
    await dictate();
    await waitFor(() => expect(sentTexts(transport)).toEqual(['commit the changes']));
    // The newer text in the box is not discarded.
    expect(screen.getByRole('textbox')).toHaveValue('something else');
    await waitFor(() => expect(io.spoken).toContain('보냈습니다.'));
    expect(io.spoken).not.toContain('취소했습니다.');
  });

  it.each(['어 그래 보내', '어그래보내', '그래 보네요', '어, 그래. 보내.', '보내'])(
    'sends a long dictation on "%s" and never says 취소했습니다',
    async (heard) => {
      const text = '로그인 화면의 버튼 색을 파란색으로 바꾸고 '.repeat(5).trim();
      const io = fakeVoice([heard]);
      const { transport } = renderComposer(io, { ...HANDS_FREE, language: 'ko' }, { text });
      await dictate();
      await waitFor(() => expect(sentTexts(transport)).toEqual([text]));
      await waitFor(() => expect(io.spoken).toContain('보냈습니다.'));
      expect(io.spoken).not.toContain('취소했습니다.');
      // The short excerpt is read, not the whole text.
      expect(io.spoken[0]!.length).toBeLessThan(text.length);
    },
  );

  it('reads the whole text when the full read-back setting is on', async () => {
    const text = 'commit the changes and then push them to the remote branch for review';
    const io = fakeVoice(['yes']);
    const { transport } = renderComposer(
      io,
      { ...HANDS_FREE, handsFreeFullReadBack: true },
      { text },
    );
    await dictate();
    await waitFor(() => expect(sentTexts(transport)).toEqual([text]));
    expect(io.spoken[0]).toBe(`To payments-api: ${text}. Send it?`);
  });

  it('a tap stops the read-back; nothing is sent', async () => {
    const io = fakeVoice(['yes'], { holdSpeech: true });
    const { transport } = renderComposer(io);
    await dictate();
    await waitFor(() =>
      expect(screen.getByTestId('hands-free-status')).toHaveTextContent('Waiting for confirmation'),
    );
    await act(async () => {
      fireEvent.pointerDown(document.body);
    });
    await waitFor(() =>
      expect(screen.getByTestId('hands-free-status')).toHaveTextContent(
        'Not sent: the text is kept',
      ),
    );
    expect(io.listen).not.toHaveBeenCalled();
    expect(sentTexts(transport)).toEqual([]);
    // Interrupted by the screen: no spoken "Cancelled" and the text is kept, not cancelled.
    expect(io.spoken).toEqual(['To payments-api: commit the changes. Send it?']);
  });

  it('shows the listening state while waiting for the answer', async () => {
    let answer!: (v: string | null) => void;
    const io = fakeVoice([]);
    io.listen = vi.fn(() => new Promise<string | null>((resolve) => (answer = resolve)));
    renderComposer(io);
    await dictate();
    await waitFor(() =>
      expect(screen.getByTestId('hands-free-status')).toHaveTextContent('Listening'),
    );
    await act(async () => answer('no'));
    await waitFor(() =>
      expect(screen.getByTestId('hands-free-status')).toHaveTextContent('Cancelled'),
    );
  });

  it('offers the full read-back option in Settings (off by default)', async () => {
    renderWithApp(<HandsFreeSettings />, { settings: { language: 'en', handsFree: true } });
    const full = screen.getByRole('checkbox', { name: 'Read the whole text when confirming' });
    expect(full).not.toBeChecked();
    await userEvent.click(full);
    expect(full).toBeChecked();
  });

  it('shows the read-aloud note once when turned on', async () => {
    renderComposer(fakeVoice([]), { language: 'en' });
    expect(screen.queryByTestId('hands-free-note')).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Hands-free mode' }));
    expect(screen.getByTestId('hands-free-note')).toHaveTextContent(
      'Command text will be read aloud.',
    );
    await userEvent.click(screen.getByRole('button', { name: 'OK' }));
    expect(screen.queryByTestId('hands-free-note')).not.toBeInTheDocument();
    // Off and on again: not shown a second time.
    await userEvent.click(screen.getByRole('button', { name: 'Hands-free mode' }));
    await userEvent.click(screen.getByRole('button', { name: 'Hands-free mode' }));
    expect(screen.queryByTestId('hands-free-note')).not.toBeInTheDocument();
  });
});

// ---------------------------------------------------------------- decisions

const decision = (command = 'pnpm build', id = 'dec-1'): Decision => ({
  id,
  prompt: `Allow Bash command?\n${command}`,
  command,
  options: [
    { id: 'yes', label: 'Yes' },
    { id: 'always', label: "Yes, and don't ask again" },
    { id: 'no', label: 'No' },
  ],
});

/** The decision the harness shows; `setShown` replaces it (as the server would). */
const shownStore = {
  value: undefined as Decision | undefined,
  listeners: new Set<() => void>(),
};
function setShown(d: Decision | undefined) {
  shownStore.value = d;
  shownStore.listeners.forEach((l) => l());
}
const subscribeShown = (l: () => void) => {
  shownStore.listeners.add(l);
  return () => shownStore.listeners.delete(l);
};

function Harness() {
  const shown = useSyncExternalStore(subscribeShown, () => shownStore.value);
  return (
    <DecisionSheet
      session={localSession({ display_name: '5번 세션', state: { type: 'awaiting_decision' } })}
      decision={shown}
    />
  );
}

function renderDecision(
  io: VoiceIO,
  level: DangerLevel,
  initial: Decision | undefined = decision(),
  settings: object = HANDS_FREE,
) {
  const transport = new FakeTransport((msg) =>
    msg.type === 'classify_command'
      ? { type: 'command_danger', level, reasons: [], spans: [], voice_ok: level === 'safe' }
      : defaultResponse(msg),
  );
  shownStore.value = initial;
  renderWithApp(
    <VoiceIOProvider io={io}>
      <Harness />
    </VoiceIOProvider>,
    { transport, settings },
  );
  return transport;
}

const choices = (transport: FakeTransport) =>
  transport
    .bodies('send_input')
    .map((b) => (b.type === 'send_input' && b.input.type === 'choose' ? b.input.option_id : null));

describe('spoken decisions', () => {
  it('approves a safe request by voice exactly like a tap', async () => {
    const io = fakeVoice(['allow']);
    const transport = renderDecision(io, 'safe');
    await waitFor(() =>
      expect(transport.bodies('send_input')).toEqual([
        {
          type: 'send_input',
          session_id: 's1',
          input: { type: 'choose', decision_id: 'dec-1', option_id: 'yes' },
        },
      ]),
    );
    expect(transport.bodies('classify_command')).toEqual([
      { type: 'classify_command', command: 'pnpm build' },
    ]);
    expect(io.spoken[0]).toBe('5번 세션 asks to run pnpm build. Allow it?');
    expect(screen.getByTestId('decision-voice-status')).toHaveTextContent('Allowed by voice');
    // Noted on this device as a voice answer (the protocol has no origin field).
    expect(loadVoiceAnswers()).toEqual([
      expect.objectContaining({ decisionId: 'dec-1', optionId: 'yes', heard: 'allow' }),
    ]);
  });

  it('reads Korean requests with Korean number reading', async () => {
    const io = fakeVoice(['허용']);
    const transport = renderDecision(io, 'safe', decision(), { ...HANDS_FREE, language: 'ko' });
    await waitFor(() => expect(choices(transport)).toEqual(['yes']));
    expect(io.spoken[0]).toBe('오번 세션이 pnpm build 실행을 허용해 달라고 합니다. 허용할까요?');
  });

  it.each(['caution', 'dangerous'] as const)(
    'refuses a voice answer for %s commands',
    async (level) => {
      const io = fakeVoice(['allow']);
      const transport = renderDecision(io, level, decision('rm -rf dist'));
      await waitFor(() =>
        expect(screen.getByTestId('decision-voice-status')).toHaveTextContent('Confirm on screen'),
      );
      expect(io.listen).not.toHaveBeenCalled();
      expect(io.spoken).toEqual(['5번 세션 needs a decision. Please check the screen.']);
      expect(choices(transport)).toEqual([]);
      // The on-screen buttons still work.
      await userEvent.click(screen.getByRole('button', { name: 'No' }));
      await waitFor(() => expect(choices(transport)).toEqual(['no']));
    },
  );

  it('refuses multi-option decisions', async () => {
    const io = fakeVoice(['allow']);
    const transport = renderDecision(io, 'safe', {
      id: 'q1',
      prompt: 'Run command?\nls',
      command: 'ls',
      options: [
        { id: 'a', label: 'Once' },
        { id: 'b', label: 'Always' },
        { id: 'c', label: 'Never' },
      ],
    });
    await waitFor(() =>
      expect(screen.getByTestId('decision-voice-status')).toHaveTextContent('Confirm on screen'),
    );
    expect(io.listen).not.toHaveBeenCalled();
    expect(transport.bodies('classify_command')).toEqual([]);
    expect(choices(transport)).toEqual([]);
  });

  it('does not act on a decision that changed while listening', async () => {
    let answer!: (v: string | null) => void;
    const io = fakeVoice([]);
    io.listen = vi.fn(() => new Promise<string | null>((resolve) => (answer = resolve)));
    const transport = renderDecision(io, 'safe');
    await waitFor(() => expect(io.listen).toHaveBeenCalled());
    // Same id, another command: never approved with the old reading.
    act(() => setShown(decision('git push --force')));
    await act(async () => answer('allow'));
    await waitFor(() =>
      expect(screen.getByTestId('decision-voice-status')).toHaveTextContent('The request changed'),
    );
    expect(choices(transport)).toEqual([]);
  });

  it('does not act on a decision that was replaced while listening', async () => {
    let answer!: (v: string | null) => void;
    const io = fakeVoice([]);
    io.listen = vi.fn(
      (signal: AbortSignal) =>
        new Promise<string | null>((resolve) => {
          answer = resolve;
          signal.addEventListener('abort', () => resolve(null), { once: true });
        }),
    );
    const transport = renderDecision(io, 'safe');
    await waitFor(() => expect(io.listen).toHaveBeenCalledTimes(1));
    const first = answer;
    act(() => setShown(decision('git push --force', 'dec-2')));
    await act(async () => first('allow'));
    expect(choices(transport)).toEqual([]);
  });

  it('a tap stops the reading and the screen answers', async () => {
    const io = fakeVoice(['allow'], { holdSpeech: true });
    const transport = renderDecision(io, 'safe');
    await waitFor(() => expect(io.speak).toHaveBeenCalled());
    await userEvent.click(screen.getByRole('button', { name: 'No' }));
    await waitFor(() => expect(choices(transport)).toEqual(['no']));
    expect(io.listen).not.toHaveBeenCalled();
  });

  it('stays silent when hands-free mode is off', async () => {
    const io = fakeVoice(['allow']);
    const transport = renderDecision(io, 'safe', decision(), { language: 'en' });
    await new Promise((r) => setTimeout(r, 20));
    expect(io.speak).not.toHaveBeenCalled();
    expect(transport.bodies('classify_command')).toEqual([]);
    expect(screen.queryByTestId('decision-voice-status')).not.toBeInTheDocument();
  });
});
