/**
 * React side of hands-free mode: the real `VoiceIO` (or a test double from
 * `VoiceIOProvider`), the read-back of the composer and spoken decisions.
 */
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react';

import { diag } from '../diag/log';
import { useI18n } from '../i18n/context';
import type { Decision, SessionInfo } from '../protocol/types';
import { speechLanguageParam } from '../state/settings';
import { useActions, useSettings } from '../state/store';
import { useSpeech } from './context';
import {
  ANSWER_PROMPTS,
  confirmSend,
  interruptOnTap,
  isDictationActive,
  PHRASES,
  sameDecision,
  spokenDecision,
  voiceChoices,
  voiceLang,
  withVoice,
  type ConfirmResult,
  type DecisionVoiceStatus,
  type SendStatus,
  type VoiceIO,
} from './handsFree';
import { captureAnswer, speakAndWait } from './handsFreeIO';
import { recordVoiceAnswer } from './voiceAudit';

/** How long "보냄" / "취소됨" stay on screen. */
export const RESULT_MS = 3000;
/** Poll interval while the composer is dictating (decisions wait for it). */
const MIC_BUSY_POLL_MS = 300;

const VoiceIOContext = createContext<VoiceIO | null>(null);

/** Replaces the microphone and voice of hands-free mode (tests, demos). */
export function VoiceIOProvider({ io, children }: { io: VoiceIO; children: ReactNode }) {
  return <VoiceIOContext.Provider value={io}>{children}</VoiceIOContext.Provider>;
}

/** Speech I/O of hands-free mode: the spoken-alert voice and the dictation recorder. */
// eslint-disable-next-line react-refresh/only-export-components
export function useVoiceIO(sessionId: string | null): VoiceIO {
  const override = useContext(VoiceIOContext);
  const actions = useActions();
  const speech = useSpeech();
  const { settings } = useSettings();
  const { locale } = useI18n();
  const latest = useRef({ settings, speech, locale, sessionId });
  useEffect(() => {
    latest.current = { settings, speech, locale, sessionId };
  });
  return useMemo<VoiceIO>(
    () =>
      override ?? {
        speak: (text, signal) => {
          const s = latest.current.settings;
          return speakAndWait(text, signal, {
            speak: (t) =>
              actions.speakAlert(t, s.language, {
                serverEngine: s.ttsServerEngine,
                serverVoice: s.ttsServerVoice,
                voice: s.ttsVoice,
                rate: s.ttsRate,
              }),
          });
        },
        listen: (signal) => {
          const cur = latest.current;
          return captureAnswer({
            recorder: cur.speech.createRecorder(),
            // Not `final`: a one-word answer needs no contextual LLM correction.
            transcribe: (audio) =>
              cur.speech.transcribe(audio, {
                locale: cur.locale,
                // A short answer: the listener's language and a bias toward
                // the answer words ("보내 취소 응 네 아니").
                language:
                  speechLanguageParam(cur.settings.speechLanguage) ??
                  voiceLang(cur.settings.language),
                sessionId: cur.sessionId,
                context: ANSWER_PROMPTS[voiceLang(cur.settings.language)],
                final: false,
              }),
            signal,
          });
        },
      },
    [override, actions],
  );
}

/** How a read-back ended on screen. */
export type ReadBackResult = 'sent' | 'cancelled' | 'kept';

/** Why a kept text was not sent (spoken instead of the generic phrase). */
export type KeptReason = 'attachments' | 'changed' | 'failed';

export interface ReadBack {
  /** Visual state; `null` when no read-back is going on or shown. */
  status: SendStatus | null;
  /** Reads `text` back and waits for the spoken answer. */
  run(text: string): Promise<ConfirmResult>;
  /** Shows (and, when `speak`, says) the result of the last read-back. */
  finish(result: ReadBackResult | null, speak?: boolean, why?: KeptReason): void;
}

/** The composer's read-back ("삼번 세션에 … 보낼까요?"). */
// eslint-disable-next-line react-refresh/only-export-components
export function useReadBack(sessionId: string | null, sessionName?: string | null): ReadBack {
  const io = useVoiceIO(sessionId);
  const { settings } = useSettings();
  const language = useRef(settings.language);
  const target = useRef({ sessionName, full: settings.handsFreeFullReadBack });
  useEffect(() => {
    language.current = settings.language;
    target.current = { sessionName, full: settings.handsFreeFullReadBack };
  });
  const [status, setStatus] = useState<SendStatus | null>(null);
  const controllerRef = useRef<AbortController | null>(null);

  useEffect(() => () => controllerRef.current?.abort(), []);
  useEffect(() => {
    if (status !== 'sent' && status !== 'cancelled' && status !== 'kept') return;
    const timer = setTimeout(() => setStatus(null), RESULT_MS);
    return () => clearTimeout(timer);
  }, [status]);

  const run = useCallback(
    async (text: string): Promise<ConfirmResult> => {
      controllerRef.current?.abort();
      const controller = new AbortController();
      controllerRef.current = controller;
      const stopOnTap = interruptOnTap(controller);
      diag('handsfree.readback.start', {
        chars: text.length,
        full: !!target.current.full,
        named: !!target.current.sessionName,
      });
      try {
        return await withVoice(() =>
          confirmSend({
            text,
            lang: voiceLang(language.current),
            sessionName: target.current.sessionName,
            full: target.current.full,
            io,
            signal: controller.signal,
            onStatus: (s) => {
              if (controllerRef.current === controller) setStatus(s);
            },
          }),
        );
      } finally {
        diag('handsfree.readback.end', { aborted: controller.signal.aborted });
        stopOnTap();
        if (controllerRef.current === controller) controllerRef.current = null;
      }
    },
    [io],
  );

  const finish = useCallback(
    (result: ReadBackResult | null, speak = false, why?: KeptReason) => {
      setStatus(result);
      diag('handsfree.finish', { result: result ?? 'none', speak, why: why ?? 'none' });
      if (result && speak) {
        const phrases = PHRASES[voiceLang(language.current)];
        const phrase =
          result === 'kept' && why
            ? phrases[
                why === 'attachments'
                  ? 'keptAttachments'
                  : why === 'changed'
                    ? 'keptChanged'
                    : 'keptFailed'
              ]
            : phrases[result];
        const controller = new AbortController();
        const stopOnTap = interruptOnTap(controller);
        void io.speak(phrase, controller.signal).finally(stopOnTap);
      }
    },
    [io],
  );

  return { status, run, finish };
}

async function waitForMicFree(signal: AbortSignal): Promise<void> {
  while (isDictationActive() && !signal.aborted) {
    await new Promise((resolve) => setTimeout(resolve, MIC_BUSY_POLL_MS));
  }
}

/**
 * Reads the pending decision out in hands-free mode and takes a spoken
 * answer for `safe` commands (see `spokenDecision`). Each decision is read
 * once; a tap anywhere stops the voice and leaves the screen in charge.
 * Returns the visual state of the decision currently shown.
 */
// eslint-disable-next-line react-refresh/only-export-components
export function useSpokenDecision(
  session: SessionInfo,
  decision: Decision | undefined,
): DecisionVoiceStatus | null {
  const { settings } = useSettings();
  const actions = useActions();
  const io = useVoiceIO(session.id);
  const [state, setState] = useState<{ id: string; status: DecisionVoiceStatus } | null>(null);
  const latest = useRef({ decision, name: session.display_name, language: settings.language });
  useEffect(() => {
    latest.current = { decision, name: session.display_name, language: settings.language };
  });
  const decisionId = decision?.id ?? null;
  const sessionId = session.id;
  const enabled = settings.handsFree;

  useEffect(() => {
    const read = latest.current.decision;
    if (!enabled || !read || read.id !== decisionId) return;
    const controller = new AbortController();
    const { signal } = controller;
    const stopOnTap = interruptOnTap(controller);
    const choices = voiceChoices(read);
    // A Voco message being dictated is not interrupted: wait before queueing.
    void waitForMicFree(signal)
      .then(() =>
        withVoice(async () => {
          await waitForMicFree(signal);
          if (signal.aborted || !sameDecision(latest.current.decision, read)) return;
          const result = await spokenDecision({
            decision: read,
            sessionName: latest.current.name,
            lang: voiceLang(latest.current.language),
            io,
            signal,
            // Only what the server lists as approvable by voice counts as safe;
            // a level of `safe` just means it knows nothing against the command.
            classify: async (command) => {
              const danger = await actions.classifyCommand(command);
              if (danger.voice_ok) return 'safe';
              return danger.level === 'safe' ? 'unclassified' : danger.level;
            },
            current: () => latest.current.decision,
            choose: async (optionId) => {
              // Exactly what a tap sends.
              await actions.choose(sessionId, read.id, optionId);
              recordVoiceAnswer({
                at: Date.now(),
                sessionId,
                decisionId: read.id,
                optionId,
                heard: optionId === choices?.allow ? 'allow' : 'deny',
              });
            },
            onStatus: (status) => {
              if (!signal.aborted) setState({ id: read.id, status });
            },
          });
          if (result.result === 'stopped' && result.reason === 'interrupted') {
            setState((s) => (s?.id === read.id ? null : s));
          }
        }),
      )
      .catch(() => {})
      .finally(stopOnTap);
    return () => {
      controller.abort();
      stopOnTap();
    };
  }, [enabled, decisionId, sessionId, io, actions]);

  if (!enabled || !state) return null;
  if (state.id === decisionId) return state.status;
  // The answered decision is gone: its result stays until the next one.
  const answered = state.status === 'approved' || state.status === 'denied';
  return decisionId === null && answered ? state.status : null;
}
