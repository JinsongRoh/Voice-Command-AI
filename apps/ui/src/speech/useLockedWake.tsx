/**
 * The wake word while the app is locked.
 *
 * The lock screen replaces the whole app, so the composer's Voco listener
 * (`useVoco`) is gone. With hands-free mode and the wake word on, this one
 * keeps listening on the lock screen, and its only answer to "보코" is one
 * fixed phrase ("잠겨 있습니다. 잠금을 풀어 주세요."), at most once per
 * `LOCKED_WAKE_INTERVAL_MS`.
 *
 * Rules (the lock exists so a found device cannot drive the sessions):
 * - Nothing said with or after the wake word is executed, sent, shown or
 *   stored: the transcript only answers "was it the wake word" and is then
 *   dropped. The log gets counts only (`voco.locked_wake`).
 * - No composer, no session names, no content in speech: the phrase is fixed.
 * - Spoken alerts stay muted (`isAppLocked`); the phrase goes through the
 *   voice queue like the flow's own prompts, which the lock does not gate.
 * - One listener at a time: this one only exists while locked, the composer's
 *   only while unlocked; both register as the microphone holder, and the
 *   native start/stop calls run strictly in order (`serialMicOp`).
 */
import { useEffect, useRef } from 'react';

import { diag, errorInfo } from '../diag/log';
import { useI18n } from '../i18n/context';
import { isAppLocked } from '../state/appLock';
import { phraseList, speechLanguageParam } from '../state/settings';
import { useSettings } from '../state/store';
import { useSpeech } from './context';
import { micBorrowed, setMicHolder, voiceLang, withVoice } from './handsFree';
import { voiceBusy } from './handsFreeIO';
import { useVoiceIO } from './useHandsFree';
import {
  NOT_LISTENING_MS,
  useVisible,
  useVocoSupported,
  useVocoTestHooks,
  WAKE_PROMPTS,
  WATCHDOG_MS,
} from './useVoco';
import { VOCO_PHRASES, WAKE_HEAD_MS } from './vocoFlow';
import {
  vocoEndSilenceMs,
  VocoListener,
  type HeardUtterance,
  type VocoListenerOptions,
} from './vocoListener';
import { matchWakeWord } from './wakeWord';

/** The locked phrase is said at most once in this long. */
export const LOCKED_WAKE_INTERVAL_MS = 20_000;

/** Why the wake word is not listened for on the lock screen (`null`: it is). */
// eslint-disable-next-line react-refresh/only-export-components
export function lockedWakeBlockReason(o: {
  handsFree: boolean;
  wake: boolean;
  supported: boolean;
  visible: boolean;
}): string | null {
  if (!o.handsFree) return 'handsfree_off';
  if (!o.wake) return 'wake_off';
  if (!o.supported) return 'unsupported';
  if (!o.visible) return 'hidden';
  return null;
}

/** Rendered by the lock screen only. */
export function LockedWakeListener(): null {
  useLockedWake();
  return null;
}

function useLockedWake(): void {
  const { settings } = useSettings();
  const { locale } = useI18n();
  const speech = useSpeech();
  const io = useVoiceIO(null);
  const hooks = useVocoTestHooks();
  const supported = useVocoSupported();
  const visible = useVisible();
  const latest = useRef({ settings, locale, speech, io });
  useEffect(() => {
    latest.current = { settings, locale, speech, io };
  });
  const blockReason = lockedWakeBlockReason({
    handsFree: settings.handsFree,
    wake: settings.vocoWake,
    supported,
    visible,
  });
  const active = blockReason === null;

  useEffect(() => {
    if (blockReason) diag('voco.locked.blocked', { reason: blockReason });
  }, [blockReason]);

  useEffect(() => {
    if (!active) return;
    const cur = () => latest.current;
    const timers = hooks?.timers;
    const now = () => timers?.now() ?? Date.now();
    const lang = () => voiceLang(cur().settings.language);
    let disposed = false;
    let checking = false;
    let lastSpoken = -Infinity;

    /** True when `u` starts with the wake word. The text never leaves here. */
    const heardWake = async (u: HeardUtterance): Promise<boolean> => {
      const c = cur();
      const audio = u.speechMs > WAKE_HEAD_MS ? u.head(WAKE_HEAD_MS) : u.audio;
      const t = await c.speech.transcribe(audio, {
        locale: c.locale,
        language: speechLanguageParam(c.settings.speechLanguage) ?? lang(),
        // No session: nothing is tied to (or stored with) a conversation.
        sessionId: null,
        context: WAKE_PROMPTS[lang()],
        final: false,
      });
      const opts = { extra: phraseList(c.settings.vocoWakeWords), speechMs: u.speechMs };
      return !!(matchWakeWord(t.text, opts) ?? (t.rawText ? matchWakeWord(t.rawText, opts) : null));
    };

    const answer = async (u: HeardUtterance) => {
      const wake = await heardWake(u).catch((err: unknown) => {
        diag('voco.locked.error', errorInfo(err));
        return false;
      });
      if (!wake || disposed || !isAppLocked()) return;
      const limited = now() - lastSpoken < LOCKED_WAKE_INTERVAL_MS;
      diag('voco.locked_wake', { lang: lang(), speechMs: u.speechMs, spoken: !limited });
      if (limited) return;
      lastSpoken = now();
      const phrase = VOCO_PHRASES[lang()].locked;
      await withVoice(() => cur().io.speak(phrase, new AbortController().signal));
    };

    // One utterance at a time; what is said meanwhile is ignored.
    const onSpeechEnd = (u: HeardUtterance | null) => {
      if (!u || checking || disposed) return;
      checking = true;
      void answer(u).finally(() => {
        checking = false;
      });
    };
    const options: VocoListenerOptions = {
      createRecorder: () => cur().speech.createRecorder(),
      onSpeechStart: () => {},
      endSilenceFor: (speechMs) => vocoEndSilenceMs(true, speechMs),
      onSpeechEnd,
      busy: voiceBusy,
    };
    const listener = hooks?.createListener?.(options) ?? new VocoListener(options);
    const unregister = setMicHolder({
      release: () => listener.pause(),
      resume: () => listener.resume(),
    });
    diag('voco.locked.on', { borrowed: micBorrowed() });
    if (!micBorrowed()) listener.start();

    // Watchdog: a listener closed while nobody holds the microphone is
    // started again (as on the composer).
    let closedSince: number | null = null;
    const check = () => {
      if (micBorrowed() || listener.listening !== false) {
        closedSince = null;
        return;
      }
      closedSince ??= now();
      if (now() - closedSince >= NOT_LISTENING_MS) {
        diag('voco.watchdog.restart', { ms: now() - closedSince, locked: true });
        closedSince = null;
        listener.start();
      }
    };
    let cancelTick: () => void = () => {};
    const schedule = (fn: () => void, ms: number): (() => void) => {
      if (timers) return timers.set(fn, ms);
      const id = setTimeout(fn, ms);
      return () => clearTimeout(id);
    };
    const tick = () => {
      check();
      cancelTick = schedule(tick, WATCHDOG_MS);
    };
    cancelTick = schedule(tick, WATCHDOG_MS);

    return () => {
      disposed = true;
      diag('voco.locked.off');
      cancelTick();
      unregister();
      void listener.stop();
    };
  }, [active, hooks]);
}
