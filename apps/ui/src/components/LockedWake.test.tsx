import { act, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { diagEntries, formatDiag, setDiagEnabled } from '../diag/log';
import type { AppLockVerifier, UnlockResult } from '../native/appLock';
import type { VoiceIO } from '../speech/handsFree';
import { VoiceIOProvider } from '../speech/useHandsFree';
import { LOCKED_WAKE_INTERVAL_MS } from '../speech/useLockedWake';
import { VocoTestProvider, type VocoListenerLike } from '../speech/useVoco';
import { VOCO_PHRASES, type VocoTimers } from '../speech/vocoFlow';
import type { HeardUtterance, VocoListenerOptions } from '../speech/vocoListener';
import { isAppLocked } from '../state/appLock';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { AppLockProvider } from './AppLock';
import { Composer } from './Composer';

beforeEach(() => {
  localStorage.clear();
  // The log is opt-in; on here so the test can check what it leaves out.
  setDiagEnabled(true);
});
afterEach(() => {
  vi.restoreAllMocks();
});

function manualTimers() {
  let now = 0;
  const queue: { at: number; fn: () => void; dead: boolean }[] = [];
  const timers: VocoTimers & { skip(ms: number): void } = {
    now: () => now,
    set(fn, ms) {
      const t = { at: now + ms, fn, dead: false };
      queue.push(t);
      return () => {
        t.dead = true;
      };
    },
    // Moves the clock without running the watchdog.
    skip(ms) {
      now += ms;
    },
  };
  return timers;
}

function fakeListener() {
  return {
    start: vi.fn<() => void>(),
    pause: vi.fn<() => Promise<void>>(async () => {}),
    resume: vi.fn<() => void>(),
    stop: vi.fn<() => Promise<void>>(async () => {}),
  } satisfies VocoListenerLike;
}

/** A verifier whose answers the test hands out. */
function fakeVerifier() {
  const answers: ((r: UnlockResult) => void)[] = [];
  const verifier: AppLockVerifier = {
    verify: vi.fn(
      () =>
        new Promise<UnlockResult>((resolve) => {
          answers.push(resolve);
        }),
    ),
  };
  const next = async (answer: UnlockResult = 'verified') => {
    await waitFor(() => expect(answers.length).toBeGreaterThan(0));
    await act(async () => answers.shift()!(answer));
  };
  return { verifier, next };
}

const SECRET = '보코 삼번 세션에 비밀번호 바꿔서 보내 줘';

function renderLocked(settings: object) {
  const spoken: string[] = [];
  const io: VoiceIO = {
    speak: vi.fn(async (text: string) => {
      spoken.push(text);
    }),
    listen: vi.fn(async () => null),
  };
  const timers = manualTimers();
  const created: { options: VocoListenerOptions; listener: ReturnType<typeof fakeListener> }[] = [];
  const transcribe = vi.fn(async () => ({ text: SECRET, corrections: [] }));
  const { verifier, next } = fakeVerifier();
  const result = renderWithApp(
    <VocoTestProvider
      hooks={{
        createListener: (options) => {
          const listener = fakeListener();
          created.push({ options, listener });
          return listener;
        },
        chime: () => {},
        timers,
      }}
    >
      <VoiceIOProvider io={io}>
        <AppLockProvider verifier={verifier}>
          <Composer session={localSession()} />
        </AppLockProvider>
      </VoiceIOProvider>
    </VocoTestProvider>,
    {
      speech: {
        transcribe,
        createRecorder: () => ({
          start: async () => {},
          stop: async () => new Blob(),
          cancel: () => {},
        }),
      },
      settings: { appLock: true, handsFreeNoteSeen: true, language: 'ko', ...settings },
    },
  );
  const say = async () => {
    const blob = new Blob(['x']);
    const u: HeardUtterance = {
      audio: blob,
      head: () => blob,
      speechMs: 2500,
      pausedBeforeLast: false,
      endedAt: timers.now(),
    };
    const { options } = created[0]!;
    await act(async () => {
      options.onSpeechStart();
      options.onSpeechEnd(u);
    });
  };
  return { ...result, spoken, timers, created, transcribe, say, next };
}

describe('wake word while the app is locked', () => {
  it('answers the wake word with the fixed phrase and nothing else', async () => {
    const { transport, spoken, timers, created, transcribe, say, container } = renderLocked({
      handsFree: true,
      vocoWake: true,
    });
    await waitFor(() => expect(isAppLocked()).toBe(true));
    expect(created).toHaveLength(1);
    expect(created[0]!.listener.start).toHaveBeenCalled();
    // No composer on the lock screen.
    expect(container.querySelector('textarea')).toBeNull();

    await say();
    await waitFor(() => expect(spoken).toEqual([VOCO_PHRASES.ko.locked]));
    // The wake check is tied to no session.
    expect(transcribe).toHaveBeenCalledWith(
      expect.anything(),
      expect.objectContaining({ sessionId: null, final: false }),
    );
    // Nothing that followed the wake word was sent, shown or logged.
    expect(transport.bodies('send_input')).toEqual([]);
    expect(container.textContent).not.toContain('비밀번호');
    const log = formatDiag();
    expect(log).toContain('voco.locked_wake');
    expect(log).not.toContain('비밀번호');
    expect(diagEntries().some((e) => JSON.stringify(e).includes('보코'))).toBe(false);

    // At most once per interval.
    await say();
    await waitFor(() => expect(transcribe).toHaveBeenCalledTimes(2));
    expect(spoken).toHaveLength(1);
    timers.skip(LOCKED_WAKE_INTERVAL_MS);
    await say();
    await waitFor(() => expect(spoken).toHaveLength(2));
    expect(transport.bodies('send_input')).toEqual([]);
  });

  it('does not listen while locked when the wake word is off', async () => {
    const { created } = renderLocked({ handsFree: true, vocoWake: false });
    await waitFor(() => expect(isAppLocked()).toBe(true));
    expect(created).toHaveLength(0);
  });

  it('does not listen while locked when hands-free mode is off', async () => {
    const { created } = renderLocked({ handsFree: false, vocoWake: true });
    await waitFor(() => expect(isAppLocked()).toBe(true));
    expect(created).toHaveLength(0);
  });

  it('hands over to the composer listener on unlock', async () => {
    const { created, next } = renderLocked({ handsFree: true, vocoWake: true });
    await waitFor(() => expect(created).toHaveLength(1));
    const locked = created[0]!.listener;
    // The lock screen's own verification.
    await next('verified');
    await waitFor(() => expect(isAppLocked()).toBe(false));
    await waitFor(() => expect(created).toHaveLength(2));
    expect(locked.stop).toHaveBeenCalled();
    expect(created[1]!.listener.start).toHaveBeenCalled();
    // One listener: the locked one stopped before the composer's started.
    expect(locked.stop.mock.invocationCallOrder[0]!).toBeLessThan(
      created[1]!.listener.start.mock.invocationCallOrder[0]!,
    );
  });
});
