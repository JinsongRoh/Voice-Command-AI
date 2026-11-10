/**
 * React side of the Voco wake flow (hands-free v2, `vocoFlow.ts`): runs the
 * background listener for the composer while hands-free mode and the wake
 * word are on, the composer is idle and the app is in the foreground, and
 * wires the flow to the composer, the voice and the session list.
 */
import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react';

import { diag } from '../diag/log';
import { useI18n } from '../i18n/context';
import type { SessionInfo } from '../protocol/types';
import { isAppLocked } from '../state/appLock';
import { workOrder } from '../state/reducer';
import { phraseList, speechLanguageParam } from '../state/settings';
import { useActions, useAppState, useSettings } from '../state/store';
import { playChime } from './chime';
import { useSpeech } from './context';
import { contextOf } from './dictation';
import {
  interruptOnTap,
  isDictationActive,
  micBorrowed,
  micBorrowerKeys,
  releaseStaleDictation,
  setMicHolder,
  setVocoActive,
  voiceLang,
  withVoice,
} from './handsFree';
import { voiceBusy } from './handsFreeIO';
import { useBriefing } from './useBriefing';
import { RESULT_MS, useVoiceIO } from './useHandsFree';
import {
  handOffAwake,
  takeHandOff,
  VocoFlow,
  type VocoChime,
  type VocoDeps,
  type VocoTimers,
  type VocoView,
} from './vocoFlow';
import { vocoEndSilenceMs, VocoListener, type VocoListenerOptions } from './vocoListener';
import {
  matchSessionName,
  sessionLabel,
  shortSessionName,
  spokenSessionName,
} from './sessionNames';

/** One-word STT prompt of the wake check (biases the recognizer toward the call). */
// eslint-disable-next-line react-refresh/only-export-components
export const WAKE_PROMPTS = { ko: '보코', en: 'Voco', ja: 'ボコ', es: 'Voco' } as const;

/** What the hook needs of a listener (`VocoListener`, or a test double). */
export interface VocoListenerLike {
  /** The microphone is open (unknown when absent: the watchdog leaves it alone). */
  readonly listening?: boolean;
  start(): void;
  pause(): Promise<void>;
  resume(): void;
  stop(): Promise<void>;
}

/** Test doubles for the listener, the earcons and the clock. */
export interface VocoTestHooks {
  createListener?(options: VocoListenerOptions): VocoListenerLike;
  chime?(kind: VocoChime): void;
  timers?: VocoTimers;
}

const VocoTestContext = createContext<VocoTestHooks | null>(null);

export function VocoTestProvider({
  hooks,
  children,
}: {
  hooks: VocoTestHooks;
  children: ReactNode;
}) {
  return <VocoTestContext.Provider value={hooks}>{children}</VocoTestContext.Provider>;
}

/** The test doubles in effect (`null` in the app). */
// eslint-disable-next-line react-refresh/only-export-components
export function useVocoTestHooks(): VocoTestHooks | null {
  return useContext(VocoTestContext);
}

/** Watchdog period, and how long the listener may be closed before it is restarted. */
export const WATCHDOG_MS = 1000;
export const NOT_LISTENING_MS = 2000;

/** Why the wake word is not listened for now (`null`: it is). */
// eslint-disable-next-line react-refresh/only-export-components
export function vocoBlockReason(o: {
  handsFree: boolean;
  wake: boolean;
  supported: boolean;
  visible: boolean;
  composer: VocoComposer;
}): string | null {
  if (!o.handsFree) return 'handsfree_off';
  if (!o.wake) return 'wake_off';
  if (!o.supported) return 'unsupported';
  if (!o.visible) return 'hidden';
  if (!o.composer.ready) return o.composer.blocked?.() ?? 'composer_busy';
  return null;
}

/** What the composer lends the flow. */
export interface VocoComposer {
  session: SessionInfo;
  /** The composer can take a hands-free message now (idle and able to send). */
  ready: boolean;
  /** Why it is not ready (diagnostics), e.g. `dictating`, `cannot_send`. */
  blocked?(): string;
  /** A send now waits for attachments that are still uploading (queued send). */
  uploading?(): boolean;
  /** Drops the send that waits for uploads. */
  cancelQueuedSend?(): void;
  text(): string;
  setText(text: string): void;
  /** Sends exactly `text` (the composer's hands-free send). */
  send(text: string): Promise<boolean>;
  /** The read-back confirmation of a finished dictation. */
  readBack(text: string): Promise<void>;
}

/** Whether this device can listen for the wake word (a streaming recorder). */
// eslint-disable-next-line react-refresh/only-export-components
export function useVocoSupported(): boolean {
  const speech = useSpeech();
  const hooks = useContext(VocoTestContext);
  return useMemo(() => {
    if (hooks?.createListener) return true;
    const r = speech.createRecorder();
    return !!r.peek && !!r.split;
  }, [speech, hooks]);
}

/** Whether the page is visible (the wake word is not listened for while hidden). */
// eslint-disable-next-line react-refresh/only-export-components
export function useVisible(): boolean {
  const [visible, setVisible] = useState(
    () => typeof document === 'undefined' || document.visibilityState !== 'hidden',
  );
  useEffect(() => {
    const on = () => setVisible(document.visibilityState !== 'hidden');
    document.addEventListener('visibilitychange', on);
    return () => document.removeEventListener('visibilitychange', on);
  }, []);
  return visible;
}

/** The Voco flow of `composer`; returns its visual state (`null` when off). */
// eslint-disable-next-line react-refresh/only-export-components
export function useVoco(composer: VocoComposer): VocoView | null {
  const { settings } = useSettings();
  const { locale } = useI18n();
  const speech = useSpeech();
  const actions = useActions();
  const briefing = useBriefing();
  const order = useAppState(workOrder);
  const sessions = useAppState((s) => s.sessions);
  const io = useVoiceIO(composer.session.id);
  const hooks = useContext(VocoTestContext);
  const supported = useVocoSupported();
  const visible = useVisible();
  const [view, setView] = useState<VocoView | null>(null);
  /** The microphone could not be opened for listening. */
  const [failed, setFailed] = useState(false);
  const latest = useRef({ settings, locale, speech, briefing, order, sessions, io, composer });
  useEffect(() => {
    latest.current = { settings, locale, speech, briefing, order, sessions, io, composer };
  });

  const blockReason = vocoBlockReason({
    handsFree: settings.handsFree,
    wake: settings.vocoWake,
    supported,
    visible,
    composer,
  });
  const active = blockReason === null;
  const sessionId = composer.session.id;

  // Device logs say why the wake word is not answered.
  useEffect(() => {
    if (blockReason) diag('voco.blocked', { reason: blockReason });
  }, [blockReason]);

  useEffect(() => {
    if (!active) return;
    const cur = () => latest.current;
    const lang = () => voiceLang(cur().settings.language);
    let flow: VocoFlow | null = null;
    const labelOf = (id: string): string | null => {
      const c = cur();
      const session = c.sessions[id];
      if (!session) return null;
      return sessionLabel(spokenSessionName(session, c.sessions, c.order, lang()), lang());
    };
    const found = (id: string) => ({
      label: labelOf(id) ?? id,
      select: (carry: string) => {
        if (id === sessionId) {
          flow?.wakeNow(carry);
          return;
        }
        // The next session's composer starts listening with the text.
        handOffAwake(carry, Date.now());
        actions.select(id);
      },
    });
    const deps: VocoDeps = {
      lang,
      settings: () => {
        const s = cur().settings;
        return {
          confirm: s.vocoConfirm,
          autoSend: s.vocoAutoSend,
          countdownMs: s.vocoCountdownSec * 1000,
          wakeWords: phraseList(s.vocoWakeWords),
          sendPhrases: phraseList(s.vocoSendPhrases),
        };
      },
      transcribe: (audio, purpose) => {
        const c = cur();
        return c.speech.transcribe(audio, {
          locale: c.locale,
          // A wake check names the language and a one-word prompt ("보코")
          // that biases the recognizer toward the call; no correction pass.
          language:
            purpose === 'wake'
              ? (speechLanguageParam(c.settings.speechLanguage) ?? lang())
              : speechLanguageParam(c.settings.speechLanguage),
          sessionId,
          context: purpose === 'dictation' ? contextOf(c.composer.text()) : WAKE_PROMPTS[lang()],
          final: purpose === 'dictation',
        });
      },
      speak: (text) =>
        withVoice(async () => {
          const controller = new AbortController();
          const stopOnTap = interruptOnTap(controller);
          try {
            await cur().io.speak(text, controller.signal);
          } finally {
            stopOnTap();
          }
        }),
      chime: (kind) => (hooks?.chime ?? playChime)(kind),
      text: () => cur().composer.text(),
      setText: (text) => cur().composer.setText(text),
      // Never while the app lock covers the app (the composer refuses too).
      send: (text) => {
        if (isAppLocked()) {
          diag('voco.send.blocked', { reason: 'locked' });
          return Promise.resolve(false);
        }
        return cur().composer.send(text);
      },
      queued: () => !!cur().composer.uploading?.(),
      cancelQueued: () => cur().composer.cancelQueuedSend?.(),
      readBack: (text) => cur().composer.readBack(text),
      // Sessions are named by the name the user gave them, not their number.
      sessionLabel: () =>
        labelOf(sessionId) ??
        sessionLabel(shortSessionName(cur().composer.session.display_name), lang()),
      findSession: (ref) => {
        const id = cur().order[ref.number - 1];
        return id ? found(id) : null;
      },
      findSessionByName: (name) => {
        const c = cur();
        const list = c.order.map((id) => c.sessions[id]).filter((x): x is SessionInfo => !!x);
        const match = matchSessionName(name, list);
        if (!match) return null;
        if (match.kind === 'ambiguous') return 'ambiguous';
        return found(match.id);
      },
      // "네" goes through `speak` like every prompt, in the chosen voice; the
      // PC clip is synthesized ahead (`fixedPhrases.ts`), so it plays at once.
      briefing: () => void cur().briefing.run(),
      onView: (v) => {
        setVocoActive(v.phase !== 'waiting');
        setView(v);
      },
      timers: hooks?.timers,
    };
    flow = new VocoFlow(deps);
    const f = flow;
    const options: VocoListenerOptions = {
      createRecorder: () => cur().speech.createRecorder(),
      onSpeechStart: () => f.speechStart(),
      // A short call ends after a short silence; dictation keeps the longer one.
      endSilenceFor: (speechMs) => vocoEndSilenceMs(f.state === 'waiting', speechMs),
      onSpeechEnd: (u) => f.speechEnd(u),
      onError: () => {
        // The listener retries by itself; the chip hides until it listens again.
        diag('voco.listen.failed');
        setFailed(true);
      },
      onListening: () => setFailed(false),
      busy: voiceBusy,
    };
    const listener = hooks?.createListener?.(options) ?? new VocoListener(options);
    const unregister = setMicHolder({
      release: () => listener.pause(),
      resume: () => {
        diag('voco.resume');
        listener.resume();
      },
    });
    diag('voco.on', { borrowed: micBorrowed() });
    if (!micBorrowed()) listener.start();
    else diag('voco.blocked', { reason: 'mic_borrowed', by: micBorrowerKeys().join(',') });
    const handed = takeHandOff(Date.now());
    if (handed) f.wakeNow(handed.carry);
    // A tap stops a countdown (the text stays in the box).
    const onTap = () => f.tap();
    document.addEventListener('pointerdown', onTap, true);

    // Watchdog: the composer is idle and the wake word is on, so the
    // microphone should be open. A dictation borrow left behind is returned,
    // and a listener that stays closed while nobody borrows the microphone
    // is started again.
    const timers = hooks?.timers;
    const now = () => timers?.now() ?? Date.now();
    let closedSince: number | null = null;
    let lastBorrowLog = -Infinity;
    const check = (force = false) => {
      if (releaseStaleDictation()) diag('voco.blocked', { reason: 'stale_dictation_borrow' });
      if (micBorrowed()) {
        closedSince = null;
        if (now() - lastBorrowLog >= 10_000) {
          lastBorrowLog = now();
          diag('voco.blocked', {
            reason: 'mic_borrowed',
            by: micBorrowerKeys().join(','),
            dictating: isDictationActive(),
          });
        }
        return;
      }
      if (listener.listening !== false) {
        closedSince = null;
        return;
      }
      closedSince ??= now();
      if (force || now() - closedSince >= NOT_LISTENING_MS) {
        diag('voco.blocked', { reason: 'not_listening', ms: now() - closedSince });
        diag('voco.watchdog.restart', { ms: now() - closedSince, force });
        closedSince = null;
        listener.start();
      }
    };
    let cancelTick: () => void = () => {};
    const tick = () => {
      check();
      cancelTick = schedule(tick, WATCHDOG_MS);
    };
    const schedule = (fn: () => void, ms: number): (() => void) => {
      if (timers) return timers.set(fn, ms);
      const id = setTimeout(fn, ms);
      return () => clearTimeout(id);
    };
    cancelTick = schedule(tick, WATCHDOG_MS);
    // Back in the foreground (the photo picker, another app): check at once.
    const onForeground = () => check(true);
    window.addEventListener('focus', onForeground);
    window.addEventListener('pageshow', onForeground);
    return () => {
      diag('voco.off');
      cancelTick();
      window.removeEventListener('focus', onForeground);
      window.removeEventListener('pageshow', onForeground);
      document.removeEventListener('pointerdown', onTap, true);
      f.dispose();
      unregister();
      void listener.stop();
      setVocoActive(false);
      setView(null);
      setFailed(false);
    };
  }, [active, sessionId, hooks, actions]);

  // The result ("보냄", "취소됨") stays for a moment; "queued" until the send goes.
  const result = view?.result;
  useEffect(() => {
    if (!result || result === 'queued') return;
    const timer = setTimeout(() => setView((v) => (v ? { phase: v.phase } : v)), RESULT_MS);
    return () => clearTimeout(timer);
  }, [result]);

  // Until the flow reports otherwise it is waiting for the wake word.
  return active && !failed ? (view ?? { phase: 'waiting' }) : null;
}
