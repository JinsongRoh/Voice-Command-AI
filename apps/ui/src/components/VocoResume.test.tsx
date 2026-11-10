import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { MockAttachments, type AttachMsg } from '../protocol/mockAttachments';
import type { ClientMsg, ServerMsg, SessionInfo } from '../protocol/types';
import { setMicHolder, withVoice, type VoiceIO } from '../speech/handsFree';
import { VoiceIOProvider } from '../speech/useHandsFree';
import {
  NOT_LISTENING_MS,
  VocoTestProvider,
  WATCHDOG_MS,
  type VocoListenerLike,
} from '../speech/useVoco';
import {
  takeHandOff,
  TRAILING_SEND_SILENCE_MS,
  UNDO_MS,
  type VocoTimers,
} from '../speech/vocoFlow';
import type { HeardUtterance, VocoListenerOptions } from '../speech/vocoListener';
import { localSession } from '../test/fixtures';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { renderWithApp } from '../test/render';
import { Composer } from './Composer';

let clock = 1_000_000;
beforeEach(() => {
  clock = 1_000_000;
  vi.spyOn(Date, 'now').mockImplementation(() => clock);
  localStorage.clear();
  // A spoken session switch hands the call to the next composer: never across tests.
  takeHandOff(0);
});
afterEach(() => {
  vi.restoreAllMocks();
});

function fakeVoice() {
  const spoken: string[] = [];
  const io = {
    spoken,
    speak: vi.fn(async (text: string) => {
      spoken.push(text);
    }),
    listen: vi.fn<(signal: AbortSignal) => Promise<string | null>>(async () => null),
  } satisfies VoiceIO & { spoken: string[] };
  return io;
}

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

/** A listener double that knows whether its microphone is open. */
function fakeListener() {
  const l = {
    listening: false as boolean,
    start: vi.fn<() => void>(() => {
      l.listening = true;
    }),
    pause: vi.fn<() => Promise<void>>(async () => {
      l.listening = false;
    }),
    resume: vi.fn<() => void>(() => {
      l.listening = true;
    }),
    stop: vi.fn<() => Promise<void>>(async () => {
      l.listening = false;
    }),
  } satisfies VocoListenerLike & { listening: boolean };
  return l;
}

const ATTACH_TYPES = new Set<ClientMsg['type']>([
  'attach',
  'attach_begin',
  'attach_chunk',
  'attach_end',
  'attach_cancel',
  'attach_folder_begin',
  'attach_folder_end',
  'attach_batch_end',
]);

/** Handles attachments like vca-server; `attach_chunk` waits for `release()` when gated. */
function attachTransport(session: SessionInfo, gated: boolean, sessions: SessionInfo[] = []) {
  const mock = new MockAttachments(
    (id) => (id === session.id ? session : undefined),
    () => Date.UTC(2026, 9, 30, 8, 34, 56),
    () => {},
  );
  let release: () => void = () => {};
  const gate = gated
    ? new Promise<void>((r) => {
        release = r;
      })
    : Promise.resolve();
  const transport = new FakeTransport((msg) => {
    if (ATTACH_TYPES.has(msg.type)) return mock.handle(msg as AttachMsg);
    if (msg.type === 'list_sessions') return { type: 'sessions', sessions } as ServerMsg;
    return defaultResponse(msg);
  });
  const send = transport.send.bind(transport);
  transport.send = async (env) => {
    if (env.body.type === 'attach_chunk') await gate;
    return send(env);
  };
  return { transport, release: () => release() };
}

const SETTINGS = { handsFree: true, handsFreeNoteSeen: true, language: 'ko' as const };

function renderVoco(
  script: Record<string, string>,
  opts: { transport?: FakeTransport; session?: SessionInfo; recStart?: () => Promise<void> } = {},
) {
  const io = fakeVoice();
  const timers = manualTimers();
  const names = new Map<Blob, string>();
  let options: VocoListenerOptions | null = null;
  const listener = fakeListener();
  const transcribe = vi.fn(async (audio: Blob) => ({
    text: names.has(audio) ? (script[names.get(audio)!] ?? '') : 'commit the changes',
    corrections: [],
  }));
  const rec = {
    start: vi.fn(opts.recStart ?? (async () => {})),
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
        chime: () => {},
        timers,
      }}
    >
      <VoiceIOProvider io={io}>
        <Composer session={opts.session ?? localSession()} />
      </VoiceIOProvider>
    </VocoTestProvider>,
    {
      speech: { createRecorder: () => rec, transcribe },
      settings: SETTINGS,
      ...(opts.transport ? { transport: opts.transport } : {}),
    },
  );
  const say = async (key: string) => {
    const blob = new Blob([key]);
    names.set(blob, key);
    const u: HeardUtterance = {
      audio: blob,
      head: () => blob,
      speechMs: 600,
      pausedBeforeLast: false,
      endedAt: timers.now(),
    };
    await act(async () => {
      options!.onSpeechStart();
      options!.onSpeechEnd(u);
    });
  };
  const hear = async () => act(async () => options!.onSpeechStart());
  const noise = async () => act(async () => options!.onSpeechEnd(null));
  return { ...result, io, timers, listener, transcribe, rec, say, hear, noise };
}

const sentTexts = (transport: FakeTransport) =>
  transport
    .bodies('send_input')
    .map((b) => (b.type === 'send_input' && b.input.type === 'text' ? b.input.text : null));

async function pick(files: File[]) {
  const input = await screen.findByTestId('attach-files');
  await act(async () => {
    fireEvent.change(input, { target: { files } });
  });
}

const files = () => [
  new File(['1'], 'a.png', { type: 'text/plain' }),
  new File(['2'], 'b.png', { type: 'text/plain' }),
];

describe('Voco with attachments', () => {
  it('wakes while attachments are in the tray', async () => {
    const session = localSession({ control: 'terminal_api' });
    const { transport } = attachTransport(session, false);
    const { say, io } = renderVoco({ a: '보코' }, { transport, session });
    await waitFor(() => expect(screen.getByRole('button', { name: 'Attach files' })).toBeEnabled());
    await pick(files());
    expect(await screen.findByText('Attached: a.png')).toBeInTheDocument();
    await say('a');
    await waitFor(() => expect(screen.getByTestId('voco-status')).toHaveTextContent('Listening…'));
    expect(io.spoken).toEqual(['네']);
  });

  it('wakes while uploading; "…, 전송해" waits for the uploads and goes as one prompt', async () => {
    const session = localSession({ control: 'terminal_api' });
    const { transport, release } = attachTransport(session, true);
    const { say, io, timers } = renderVoco(
      { a: '보코', b: '이 사진들 비교해, 전송해' },
      { transport, session },
    );
    await waitFor(() => expect(screen.getByRole('button', { name: 'Attach files' })).toBeEnabled());
    await pick(files());
    await screen.findByRole('button', { name: /^Send after upload · 0\/2/ });
    await say('a');
    await waitFor(() => expect(screen.getByTestId('voco-status')).toHaveTextContent('Listening…'));
    await say('b');
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue('이 사진들 비교해'));
    // A plain "전송해" after other words counts once the user stays silent.
    await timers.advance(TRAILING_SEND_SILENCE_MS);
    await timers.advance(UNDO_MS);
    // Queued: nothing went out, the chip says so and Voco listens again.
    await waitFor(() =>
      expect(screen.getByTestId('voco-status')).toHaveTextContent('Sends after the upload'),
    );
    expect(sentTexts(transport)).toEqual([]);
    expect(
      screen.getByRole('button', {
        name: 'Sends when the upload finishes. Cancel the queued send',
      }),
    ).toBeInTheDocument();
    release();
    await waitFor(() => expect(sentTexts(transport)).toHaveLength(1));
    const dir = '.vca/attachments/20261030-083456-batch';
    expect(sentTexts(transport)[0]).toBe(
      `이 사진들 비교해 Attached files (2): @${dir}/a.png @${dir}/b.png`,
    );
    await waitFor(() => expect(screen.getByTestId('voco-status')).toHaveTextContent('Sent'));
    expect(io.spoken.at(-1)).toBe('로저, payments-api 세션에 전송했습니다');
  });
});

describe('Voco resumes after every end of a tap dictation', () => {
  const tap = async (name: RegExp | string) => {
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name }));
    });
  };

  it('after a cancel (X)', async () => {
    const { listener } = renderVoco({});
    expect(listener.start).toHaveBeenCalledTimes(1);
    await tap('Tap to dictate');
    expect(listener.listening).toBe(false);
    await tap('Cancel recording');
    await waitFor(() => expect(listener.listening).toBe(true));
  });

  it('after an accidental tap (too short)', async () => {
    const { listener } = renderVoco({});
    await tap('Tap to dictate');
    await tap(/tap to stop/);
    await waitFor(() => expect(listener.listening).toBe(true));
  });

  it('after the microphone failed to open', async () => {
    const { listener } = renderVoco(
      {},
      { recStart: async () => Promise.reject(new Error('busy')) },
    );
    await tap('Tap to dictate');
    await waitFor(() => expect(screen.getByRole('alert')).toBeInTheDocument());
    await waitFor(() => expect(listener.listening).toBe(true));
  });

  it('after the app went to the background and came back', async () => {
    const { listener } = renderVoco({});
    const visibility = vi.spyOn(document, 'visibilityState', 'get');
    visibility.mockReturnValue('hidden');
    await act(async () => {
      document.dispatchEvent(new Event('visibilitychange'));
    });
    expect(listener.listening).toBe(false);
    visibility.mockReturnValue('visible');
    await act(async () => {
      document.dispatchEvent(new Event('visibilitychange'));
    });
    await waitFor(() => expect(listener.listening).toBe(true));
  });

  it('after the app spoke (the wake reply)', async () => {
    const { listener, say } = renderVoco({ a: '보코' });
    await say('a');
    await waitFor(() => expect(listener.pause).toHaveBeenCalled());
    await waitFor(() => expect(listener.resume).toHaveBeenCalled());
    expect(listener.listening).toBe(true);
  });

  it('after any voice flow (a decision read out) gives the microphone back', async () => {
    const holder = { release: vi.fn(async () => {}), resume: vi.fn() };
    const unregister = setMicHolder(holder);
    await withVoice(async () => {});
    expect(holder.release).toHaveBeenCalledTimes(1);
    expect(holder.resume).toHaveBeenCalledTimes(1);
    unregister();
  });

  it('the watchdog restarts a listener that stays closed', async () => {
    const { listener, timers } = renderVoco({});
    expect(listener.start).toHaveBeenCalledTimes(1);
    // The microphone closed by itself (nobody borrowed it).
    listener.listening = false;
    await timers.advance(WATCHDOG_MS);
    expect(listener.start).toHaveBeenCalledTimes(1);
    await timers.advance(NOT_LISTENING_MS + WATCHDOG_MS);
    expect(listener.start).toHaveBeenCalledTimes(2);
    expect(listener.listening).toBe(true);
  });

  it('coming back to the foreground checks at once', async () => {
    const { listener } = renderVoco({});
    listener.listening = false;
    await act(async () => {
      window.dispatchEvent(new Event('focus'));
    });
    expect(listener.start).toHaveBeenCalledTimes(2);
  });
});

describe('Voco names sessions', () => {
  const sessions = [
    localSession({ id: 's1', display_name: 'payments-api' }),
    localSession({ id: 's2', display_name: 'Orbit Platform' }),
    localSession({ id: 's3', display_name: '결제 서버' }),
    localSession({ id: 's4', display_name: '결제 서버 2' }),
  ];

  it('switches by name and replies with the name', async () => {
    const { transport } = attachTransport(sessions[0]!, false, sessions);
    const { say, io } = renderVoco(
      { a: '보코, Orbit Platform' },
      { transport, session: sessions[0] },
    );
    await waitFor(() => expect(transport.bodies('list_sessions')).toHaveLength(1));
    await say('a');
    await waitFor(() => expect(io.spoken).toEqual(['로저, Orbit Platform 세션']));
  });

  it('asks which session when a name fits two, then takes the answer', async () => {
    const { transport } = attachTransport(sessions[0]!, false, sessions);
    const { say, io } = renderVoco(
      { a: '보코, 결제', b: '결제 서버 2' },
      { transport, session: sessions[0] },
    );
    await waitFor(() => expect(transport.bodies('list_sessions')).toHaveLength(1));
    await say('a');
    await waitFor(() => expect(io.spoken).toEqual(['어느 세션인가요?']));
    await say('b');
    await waitFor(() => expect(io.spoken).toEqual(['어느 세션인가요?', '로저, 결제 서버 2 세션']));
  });

  it('still switches by number, replying with the name', async () => {
    const { transport } = attachTransport(sessions[0]!, false, sessions);
    const { say, io } = renderVoco({ a: '보코, 2번 세션' }, { transport, session: sessions[0] });
    await waitFor(() => expect(transport.bodies('list_sessions')).toHaveLength(1));
    await say('a');
    await waitFor(() => expect(io.spoken).toEqual(['로저, Orbit Platform 세션']));
  });

  it('shows that it hears the moment speech starts', async () => {
    const { hear, noise } = renderVoco({});
    const chip = () => screen.getByTestId('voco-status');
    expect(chip()).toHaveAttribute('data-hearing', 'false');
    await hear();
    expect(chip()).toHaveAttribute('data-hearing', 'true');
    expect(chip()).toHaveTextContent('Hearing you…');
    await noise();
    expect(chip()).toHaveAttribute('data-hearing', 'false');
  });
});
