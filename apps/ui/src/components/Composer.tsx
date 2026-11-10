import {
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type FocusEvent,
  type KeyboardEvent,
  type MouseEvent,
  type PointerEvent,
} from 'react';

import { useKeyboardOpen } from '../hooks/visualViewport';
import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import { useNative } from '../native/context';
import { isLocalMachine } from '../native/host';
import {
  folderName,
  isCloud,
  isOpenShell,
  isShellSession,
  originLabel,
  providerLabel,
} from '../protocol/helpers';
import { commandProblem } from '../protocol/shell';
import type { SessionInfo } from '../protocol/types';
import { useSpeech } from '../speech/context';
import {
  composeDictation,
  Dictation,
  formatElapsed,
  isTooShort,
  joinText,
  retryFailedChunks,
  type DictationState,
  type FailedChunk,
} from '../speech/dictation';
import { diag } from '../diag/log';
import { borrowMic, DICTATION_MIC, setDictationActive, voiceLang } from '../speech/handsFree';
import { sessionLabel, spokenSessionName } from '../speech/sessionNames';
import { firstFrameAt, type Recorder } from '../speech/recorder';
import {
  isNetworkError,
  isSpeechDisabledError,
  isSpeechUnreachableError,
  type TranscriptCorrection,
} from '../speech/transcribe';
import { useBriefing } from '../speech/useBriefing';
import { useReadBack } from '../speech/useHandsFree';
import { useVoco } from '../speech/useVoco';
import { noteComposer } from '../state/inputGuard';
import { awaitsDecision, canSendInput, inputBlockReason, workOrder } from '../state/reducer';
import { isAppLocked } from '../state/appLock';
import { speechLanguageParam } from '../state/settings';
import { addSnippet, loadSnippets, removeSnippet, saveSnippets } from '../state/snippets';
import { requestConductorView } from '../state/conductor';
import { useActions, useAppState, useConductor, useMachines, useSettings } from '../state/store';
import attachStyles from './Attachments.module.css';
import { useComposerAttachments } from './ComposerAttachments';
import { ControlKeys } from './ControlKeys';
import { describeCrossSendBlock, useCrossSend } from './crossSend';
import { reopenKind } from './SessionEnded';
import { leadingMention, mentionMatches, mentionQuery } from './sessionSearch';
import { MicSymbol } from './MicSymbol';
import { clearDraft, loadDraft, onDraftAppended, saveDraft } from './composerDrafts';
import { requestSettingsView } from './settingsNav';
import { HandsFreeNote, HandsFreeStatus, HandsFreeToggle, VocoStatus } from './HandsFree';
import { Icon } from './Icon';
import { OriginalTranscript } from './OriginalTranscript';
import { useShell } from './ShellProvider';
import { describeShellError, isErrorCode, isShellJoinRequired } from './shellErrors';
import { ShellConfirmNeeded } from '../state/core';
import { TranscriptEditor } from './TranscriptEditor';
import { VoiceWave } from './VoiceWave';
import styles from './Composer.module.css';

type Phase = 'idle' | 'recording' | 'transcribing';

const ELAPSED_TICK_MS = 250;
const HINT_MS = 4000;
/** How long a word ("Delivered") shows next to the delivery dot after a change. */
const DELIVERY_FLASH_MS = 3000;
/** How long "Draft replaced · Undo" stays after a quick prompt replaced a draft. */
const UNDO_MS = 10_000;
/** How long a key bar error (empty clipboard, failed key) stays on screen. */
const KEY_ERROR_MS = 3000;

/** Length of the dictate button's send motion (converge + flare). */
const SEND_FX_MS = 950;

const TEMPLATE_KEYS: MessageKey[] = [
  'template.runTests',
  'template.commitPr',
  'template.summarize',
  'template.review',
  'template.continue',
];

/** Quick prompts of the Command Center: asks about the whole factory. */
const CONDUCTOR_TEMPLATE_KEYS: MessageKey[] = [
  'template.cc.status',
  'template.cc.waiting',
  'template.cc.problems',
  'template.cc.today',
  'template.cc.next',
];

/** What the last completed dictation inserted, and what was actually heard. */
interface LastRun {
  /** Corrected text inserted by the run. */
  dictated: string;
  /** Raw recognizer output for the same audio. */
  raw: string;
  corrections: TranscriptCorrection[];
}

/** A recorder for demo mode when the microphone cannot be opened. */
const silentRecorder = (): Recorder => ({
  start: async () => {},
  stop: async () => new Blob(),
  cancel: () => {},
});

/**
 * Command input for one session. Render with `key={session.id}` so drafts do
 * not leak between sessions; an unsent draft is kept per session in
 * `composerDrafts` and restored when the session is shown again.
 *
 * Voice input writes straight into the text box: while dictating, the live
 * transcript appears after the existing text (interim tail dimmed) and stays
 * editable once dictation stops. There is no separate review step.
 */
export function Composer({
  session,
  layout,
  reading = false,
  endedBarShown = false,
}: {
  session: SessionInfo;
  /**
   * `phone`: two rows. The text box on top at full width; below it attach,
   * the tools tray, dictate (centred) and send. The quick prompts, key bar,
   * snippets, hands-free, conductor mode and expand open in a drawer above
   * both rows. Desktop: the full composer.
   */
  layout?: 'phone';
  /** Phones, reading mode: only a small dictate button floats over the chat. */
  reading?: boolean;
  /** The session's ended bar is already on screen: do not repeat it here. */
  endedBarShown?: boolean;
}) {
  const { t, locale } = useI18n();
  const actions = useActions();
  const shellServices = useShell();
  const speech = useSpeech();
  const briefing = useBriefing();
  const { settings } = useSettings();
  // "지휘 모드": a shortcut to 지휘소 while the PC has a conductor.
  const conductorOn = !!useConductor()?.settings.enabled;
  const delivery = useAppState((s) => s.delivery[session.id]);
  // An unsent draft survives switching to another session and back.
  const [text, setText] = useState(() => loadDraft(session.id)?.text ?? '');
  const [corrections, setCorrections] = useState<TranscriptCorrection[]>(
    () => loadDraft(session.id)?.corrections ?? [],
  );
  const [phase, setPhase] = useState<Phase>('idle');
  const [micError, setMicError] = useState<string | null>(null);
  /** Neutral information about voice input (not an error the user caused). */
  const [micNotice, setMicNotice] = useState<string | null>(null);
  /**
   * Chunks of the last dictation that never got a transcript (the link was
   * weak or lost), with their audio: "Retry" sends them again, nothing has
   * to be said twice. `shown` is the box text they belong to.
   */
  const [retry, setRetry] = useState<{
    state: DictationState;
    failed: FailedChunk[];
    shown: string;
  } | null>(null);
  /** The notice is "speech recognition is off on the PC". */
  const [speechOff, setSpeechOff] = useState(false);
  // The speech engine screen exists only in the desktop app, for its own PC.
  const native = useNative();
  const { machine } = useMachines();
  const canOpenEngines = !!native?.host?.engines && isLocalMachine(machine.id);
  /** Brief, self-dismissing usage hint (e.g. after an accidental tap). */
  const [micHint, setMicHint] = useState<string | null>(null);
  const [elapsedMs, setElapsedMs] = useState(0);
  /** Trailing characters of `text` that are still an interim transcript. */
  const [tailLength, setTailLength] = useState(0);
  const [expanded, setExpanded] = useState(false);
  /** The last completed dictation, for the "Original" view. */
  const [lastRun, setLastRun] = useState<LastRun | null>(null);
  /**
   * The draft a quick prompt or snippet just replaced, for Undo. Offered
   * only while the box still holds exactly what was inserted.
   */
  const [replaced, setReplaced] = useState<{
    sessionId: string;
    inserted: string;
    text: string;
    corrections: TranscriptCorrection[];
    lastRun: LastRun | null;
  } | null>(null);
  /** Bumped on every successful send to play the converging motion once. */
  const [sentPulse, setSentPulse] = useState(0);
  /** The dictate button's one-time "converge" motion after a send (landing page motion). */
  const [sendFx, setSendFx] = useState(0);
  /** Dictation topic detected by the server's optional LLM pass. */
  const [topic, setTopic] = useState<string | null>(null);
  const phaseRef = useRef<Phase>('idle');
  const recorderRef = useRef<Recorder | null>(null);
  const startingRef = useRef<Promise<void> | null>(null);
  const dictationRef = useRef<Dictation | null>(null);
  /** Text and corrections from before the current dictation (restored on cancel). */
  const baseRef = useRef<{ text: string; corrections: TranscriptCorrection[] }>({
    text: '',
    corrections: [],
  });
  const startedAtRef = useRef(0);
  const micRef = useRef<HTMLButtonElement>(null);
  const inputId = useId();
  const statusId = useId();
  const correctionsId = useId();
  const targetId = useId();
  const enabled = canSendInput(session);
  const cloud = isCloud(session.origin);
  const isShell = isShellSession(session);
  const [snippetsOpen, setSnippetsOpen] = useState(false);
  const [snippets, setSnippets] = useState<string[]>(() => loadSnippets());
  const [keyError, setKeyError] = useState<string | null>(null);
  // Key bar problems ("the clipboard is empty") are momentary: they fade out.
  useEffect(() => {
    if (!keyError) return;
    const timer = setTimeout(() => setKeyError(null), KEY_ERROR_MS);
    return () => clearTimeout(timer);
  }, [keyError]);
  /**
   * Why a send did not go ("@name" blocked, a decision is open): shown in
   * every session (cloud too) and kept until the text changes or it is closed.
   */
  const [sendError, setSendError] = useState<string | null>(null);
  const snippetsId = useId();
  const mentionListId = useId();
  /**
   * `@name` at the start of the box sends to that session instead (a "To:"
   * chip shows it; removing the chip cancels). Not in remote shells, where a
   * leading `@` is shell syntax.
   */
  const crossSend = useCrossSend();
  const order = useAppState(workOrder);
  const sessionMap = useAppState((s) => s.sessions);
  const mentionTargets = useMemo(
    () =>
      isShellSession(session)
        ? []
        : order
            .filter((id) => id !== session.id)
            .map((id) => sessionMap[id])
            .filter((s): s is SessionInfo => !!s),
    [order, sessionMap, session],
  );
  const mention = leadingMention(text, mentionTargets);
  const [mentionActive, setMentionActive] = useState(0);
  /** The text at which Escape closed the suggestions (they stay closed until it changes). */
  const [mentionDismissed, setMentionDismissed] = useState<string | null>(null);
  const mentionQ =
    mentionTargets.length > 0 && !mention && mentionDismissed !== text
      ? mentionQuery(text, text.length)
      : null;
  const mentionList = mentionQ !== null ? mentionMatches(mentionTargets, mentionQ).slice(0, 6) : [];
  const mentionOpen = mentionList.length > 0;
  const activeMention = Math.min(mentionActive, Math.max(0, mentionList.length - 1));
  const pickMention = (target: SessionInfo) => {
    setText(`@${target.display_name} `);
    setMentionActive(0);
  };
  const removeMention = () => {
    if (mention) setText(mention.rest);
  };
  const onMentionKey = (e: KeyboardEvent<HTMLTextAreaElement>): boolean => {
    if (!mentionOpen || e.nativeEvent.isComposing) return false;
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const step = e.key === 'ArrowDown' ? 1 : -1;
      setMentionActive((activeMention + step + mentionList.length) % mentionList.length);
      return true;
    }
    if (e.key === 'Enter' || e.key === 'Tab') {
      e.preventDefault();
      pickMention(mentionList[activeMention]!);
      return true;
    }
    if (e.key === 'Escape') {
      e.preventDefault();
      setMentionDismissed(text);
      return true;
    }
    return false;
  };
  const micMode = settings.micMode;
  const dictating = phase !== 'idle';
  /** Focus is somewhere in the composer (normally its text box). */
  const [focusWithin, setFocusWithin] = useState(false);
  const keyboardOpen = useKeyboardOpen();
  /**
   * Typing with the on-screen keyboard open: only the text box, a one-line
   * attachment summary and the send row stay, so the text being edited is
   * right above the keyboard. The quick prompts, the key bar and the large
   * dictate button come back when the keyboard closes.
   */
  const compact = keyboardOpen && focusWithin;
  const phoneRow = layout === 'phone';
  /** Phones: the tools drawer above the row. */
  const [trayOpen, setTrayOpen] = useState(false);
  const trayId = useId();
  const trayRef = useRef<HTMLDivElement>(null);
  const trayButtonRef = useRef<HTMLButtonElement>(null);
  const trayShown = phoneRow && trayOpen && !compact && !reading;
  useEffect(() => {
    if (!trayShown) return;
    const onPointer = (e: globalThis.PointerEvent) => {
      const target = e.target as Node;
      if (trayRef.current?.contains(target) || trayButtonRef.current?.contains(target)) return;
      setTrayOpen(false);
    };
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key !== 'Escape' || e.defaultPrevented) return;
      setTrayOpen(false);
      trayButtonRef.current?.focus();
    };
    document.addEventListener('pointerdown', onPointer);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('pointerdown', onPointer);
      document.removeEventListener('keydown', onKey);
    };
  }, [trayShown]);
  /**
   * An action in the drawer closes it, except the key bar (keys are pressed
   * again and again) and buttons that open something inside it (snippets).
   */
  const onTrayClick = (e: MouseEvent<HTMLDivElement>) => {
    const button = (e.target as Element).closest('button');
    if (!button || button.disabled) return;
    if (button.closest('[data-keep-tray]') || button.hasAttribute('aria-expanded')) return;
    setTrayOpen(false);
    trayButtonRef.current?.focus();
  };
  // Files and photos for the agent; mentions the server did not type into
  // the session are added to the text box.
  // `send` is defined below; the attachments prompt calls it through this.
  const sendRef = useRef<
    (force?: boolean, expected?: string, openAtPress?: boolean) => Promise<boolean>
  >(async () => false);
  /** Hands-free read-back of a finished dictation. */
  // Read back to the session by the name the user gave it ("Orbit Platform 세션에 …").
  const spokenLang = voiceLang(settings.language);
  const spokenName = useAppState((s) =>
    sessionLabel(spokenSessionName(session, s.sessions, workOrder(s), spokenLang), spokenLang),
  );
  const readBack = useReadBack(session.id, spokenName);
  const attach = useComposerAttachments(session, {
    canPrefill: enabled,
    onMention: (mention) => {
      const add = (prev: string) => (prev.trim() ? `${prev.trimEnd()} ${mention} ` : `${mention} `);
      // A dictation rewrites the box from its own base on every update: the
      // mention must join that base, or the next update would drop it.
      const dictation = dictationRef.current;
      if (dictation) {
        baseRef.current = { ...baseRef.current, text: add(baseRef.current.text) };
        dictation.setBase(baseRef.current.text);
        return;
      }
      setText(add);
    },
    onSendAnyway: () => void sendRef.current(true),
    compact,
  });

  // Text put into this session's draft from elsewhere lands in the box for
  // review (never sent by itself): the conductor's "talk to the conductor"
  // transcript, or "Send to session…" from another session.
  useEffect(
    () =>
      onDraftAppended(session.id, () => {
        if (phaseRef.current !== 'idle') return;
        const draft = loadDraft(session.id);
        if (!draft) return;
        setText(draft.text);
        setCorrections(draft.corrections);
      }),
    [session.id],
  );

  const setPhaseBoth = (p: Phase) => {
    phaseRef.current = p;
    setPhase(p);
    // Every end of a dictation gives the microphone back at once (the Voco
    // listener resumes), not only once the next render commits.
    if (p === 'idle') setDictationActive(false);
    // Likewise queued voice stops before the microphone opens.
    if (p === 'recording') setDictationActive(true);
  };

  const showDictation = (state: DictationState) => {
    const composed = composeDictation(state);
    setText(composed.text);
    setTailLength(composed.tailLength);
    setCorrections([...baseRef.current.corrections, ...composed.corrections]);
  };

  const restoreBase = () => {
    setText(baseRef.current.text);
    setCorrections(baseRef.current.corrections);
    setTailLength(0);
  };

  /** Ends the current dictation without applying any further results. */
  const dropDictation = () => {
    const dictation = dictationRef.current;
    dictationRef.current = null;
    recorderRef.current = null;
    dictation?.abandon();
    return dictation !== null;
  };

  const showTooShort = () =>
    setMicHint(t(micMode === 'toggle' ? 'composer.tooShortToggle' : 'composer.tooShortHold'));

  const transcribeWith = async (audio: Blob, context: string | null, final: boolean) => {
    const result = await speech.transcribe(audio, {
      locale,
      language: speechLanguageParam(settings.speechLanguage),
      sessionId: session.id,
      context,
      final,
    });
    if (result.topic) setTopic(result.topic);
    return result;
  };

  /** `from`: the text to dictate after (hands-free "다시" passes the restored one). */
  const startRecording = (from?: { text: string; corrections: TranscriptCorrection[] }) => {
    if (phaseRef.current !== 'idle' || !enabled) return;
    diag('composer.dictate.start', {
      attachments: attach.count,
      attachBusy: attach.busy,
      visibility: document.visibilityState,
      textChars: text.length,
    });
    setMicError(null);
    setMicNotice(null);
    setSpeechOff(false);
    setMicHint(null);
    setRetry(null);
    setElapsedMs(0);
    startedAtRef.current = Date.now();
    baseRef.current = from ?? { text, corrections };
    setLastRun(null);
    setPhaseBoth('recording');

    const begin = (recorder: Recorder) => {
      const dictation: Dictation = new Dictation({
        recorder,
        transcribe: transcribeWith,
        base: baseRef.current.text,
        onUpdate: (state) => {
          if (dictationRef.current === dictation) showDictation(state);
        },
        // A microphone that delivers no audio never errors on some phones:
        // give up, say so and hand the composer back.
        onStall: () => {
          if (dictationRef.current !== dictation) return;
          dropDictation();
          restoreBase();
          setPhaseBoth('idle');
          setMicError(t('composer.dictationStalled'));
        },
        // The audio so far is kept and recording goes on, but the gap is lost.
        onMicStalled: () => {
          if (dictationRef.current === dictation) setMicNotice(t('composer.micInterrupted'));
        },
      });
      dictationRef.current = dictation;
      return dictation;
    };

    const recorder = speech.createRecorder();
    recorderRef.current = recorder;
    const dictation = begin(recorder);
    const pressedAt = startedAtRef.current;
    let releasedAt = pressedAt;
    // The Voco listener may hold the microphone: it is released first.
    startingRef.current = borrowMic(DICTATION_MIC)
      .then(() => {
        releasedAt = Date.now();
        return recorder.start();
      })
      .then(
        () => {
          const openedAt = Date.now();
          void firstFrameAt(recorder, () => dictationRef.current === dictation).then((at) =>
            diag('composer.dictate.timing', {
              releaseMs: releasedAt - pressedAt,
              openMs: openedAt - releasedAt,
              firstFrameMs: at === null ? null : at - pressedAt,
            }),
          );
          if (dictationRef.current === dictation && phaseRef.current === 'recording') {
            void dictation.run();
          }
        },
        (err: Error) => {
          if (dictationRef.current !== dictation) return;
          recorderRef.current = null;
          if (speech.fallbackWithoutMic) {
            setMicError(t('composer.micDemo', { error: err.message }));
            begin(silentRecorder());
          } else {
            dictationRef.current = null;
            setMicError(t('composer.micUnavailable', { error: err.message }));
            setPhaseBoth('idle');
          }
        },
      );
  };

  /** The latest `startRecording` (hands-free "다시" starts a new dictation). */
  const startRef = useRef(startRecording);
  /** The text box as last rendered (hands-free sends only what is on screen). */
  const shownRef = useRef(text);
  /** Why the last `send` did not happen (hands-free speaks it). */
  type SendBlock =
    'empty' | 'changed' | 'busy' | 'attachments' | 'failed' | 'cancelled' | 'decision' | 'locked';
  const sendBlockRef = useRef<SendBlock | null>(null);
  const sendingRef = useRef(false);
  /**
   * Hands-free: "삼번 세션에 … 보낼까요?", then send, cancel, dictate again or
   * (no clear answer) leave the text in the box unsent. The text is never
   * cleared here: only a successful send empties the box.
   */
  const confirmHandsFree = async (value: string) => {
    diag('handsfree.flow.start', { chars: value.length });
    const result = await readBack.run(value);
    diag('handsfree.flow.answer', {
      outcome: result.outcome,
      reason: 'reason' in result ? result.reason : 'none',
    });
    if (result.outcome === 'send') {
      // The text to send is fixed now: what was read back and approved.
      const sent = await sendRef.current(false, value);
      const block = sendBlockRef.current;
      // A send that did not happen (the box changed, attachments, delivery
      // failed) is never a cancel: the text is still there, and the reason
      // is spoken.
      readBack.finish(
        sent ? 'sent' : 'kept',
        true,
        sent
          ? undefined
          : block === 'attachments' || block === 'cancelled'
            ? 'attachments'
            : block === 'changed'
              ? block
              : 'failed',
      );
    } else if (result.outcome === 'kept') {
      readBack.finish('kept', true);
    } else if (result.outcome === 'again') {
      readBack.finish(null);
      restoreBase();
      if (micMode === 'toggle') startRef.current(baseRef.current);
      else micRef.current?.focus();
    } else if (result.reason === 'interrupted') {
      // A tap stopped it: the screen is in charge, the voice stays silent and
      // the text stays in the box (nothing was cancelled by a word).
      readBack.finish('kept', false);
    } else {
      // Only an explicit cancel word gets here.
      readBack.finish('cancelled', true);
    }
  };

  /** Attachments still uploading: a send now is queued until they are done. */
  const uploadingRef = useRef(attach.busy);
  useEffect(() => {
    uploadingRef.current = attach.busy;
  });
  /** Hands-free v2: "보코 … 오버" (see speech/vocoFlow.ts). */
  const voco = useVoco({
    session,
    // Attachments (present or uploading) never hold the wake word back: a
    // spoken send waits for the uploads like a tapped one.
    ready: phase === 'idle' && enabled,
    blocked: () => (!enabled ? 'cannot_send' : `dictation_${phaseRef.current}`),
    uploading: () => uploadingRef.current,
    text: () => shownRef.current,
    setText: (value) => {
      // The flow reads the text right after changing it.
      shownRef.current = value;
      setText(value);
    },
    send: (value) => sendRef.current(false, value),
    cancelQueuedSend: () => attach.cancelQueuedSend(),
    readBack: (value) => confirmHandsFree(value),
  });

  /**
   * Says why a dictation (or its retry) failed. Chunks lost on the way keep
   * their audio and get a "Retry" button.
   */
  const reportDictationError = (error: unknown, state: DictationState, failed: FailedChunk[]) => {
    const resendable =
      failed.length > 0 && (isNetworkError(error) || isSpeechUnreachableError(error));
    setRetry(resendable ? { state, failed, shown: composeDictation(state).text } : null);
    if (isSpeechDisabledError(error)) {
      setMicNotice(t('composer.speechOff'));
      setSpeechOff(true);
    } else if (isNetworkError(error)) {
      setMicError(t(resendable ? 'composer.speechNetwork' : 'composer.speechNetworkLost'));
    } else if (isSpeechUnreachableError(error)) {
      setMicError(t('composer.speechUnreachable'));
    } else setMicError((error as Error).message);
  };

  /** "Retry": sends the kept audio of the failed chunks again. */
  const retryDictation = async () => {
    const pending = retry;
    if (!pending || phaseRef.current !== 'idle') return;
    diag('composer.dictate.retry', { chunks: pending.failed.length });
    setRetry(null);
    setMicError(null);
    setPhaseBoth('transcribing');
    const out = await retryFailedChunks(pending.state, pending.failed, transcribeWith);
    // Cancelled meanwhile (the box is the user's again).
    if ((phaseRef.current as Phase) !== 'transcribing') return;
    if (shownRef.current === pending.shown) {
      // The box is as the dictation left it: the recognized text takes the
      // place of the stand-in.
      showDictation(out.state);
      setTailLength(0);
    } else {
      // The box changed meanwhile: what was recognized now is added at the end.
      const recovered = pending.failed
        .filter((f) => !out.failed.some((o) => o.seq === f.seq))
        .map((f) => out.state.segments.find((seg) => seg.seq === f.seq)?.text ?? '')
        .reduce((acc, part) => joinText(acc, part), '');
      if (recovered) setText(joinText(shownRef.current, recovered));
    }
    if (out.error) reportDictationError(out.error, out.state, out.failed);
    setPhaseBoth('idle');
  };

  const stopRecording = async () => {
    if (phaseRef.current !== 'recording') return;
    const elapsed = Date.now() - startedAtRef.current;
    diag('composer.dictate.stop', {
      elapsedMs: elapsed,
      attachments: attach.count,
      attachBusy: attach.busy,
      visibility: document.visibilityState,
    });
    setPhaseBoth('transcribing');
    await startingRef.current;
    const dictation = dictationRef.current;
    if (!dictation) {
      // Microphone failed (already reported) or the dictation was dropped.
      setPhaseBoth('idle');
      return;
    }
    recorderRef.current = null;
    if (isTooShort(elapsed)) {
      // An accidental tap: release the microphone and never call the server.
      dropDictation();
      restoreBase();
      setPhaseBoth('idle');
      showTooShort();
      return;
    }
    const result = await dictation.finish();
    // The user may have typed (or cancelled) meanwhile; the box is theirs then.
    if (dictationRef.current !== dictation) {
      diag('composer.dictate.discarded', { reason: 'superseded' });
      return;
    }
    dictationRef.current = null;
    if (result.tooShort) {
      restoreBase();
      showTooShort();
    } else {
      showDictation(result.state);
      setTailLength(0);
      const composed = composeDictation(result.state);
      setLastRun(
        composed.dictated
          ? {
              dictated: composed.dictated,
              raw: composed.raw,
              corrections: composed.corrections,
            }
          : null,
      );
    }
    if (result.error) reportDictationError(result.error, result.state, result.failed);
    setPhaseBoth('idle');
    // A dictation into an empty box that is exactly a briefing phrase ("현황
    // 알려줘", "status") runs the briefing instead; the toast can still send it.
    if (
      !result.tooShort &&
      !result.error &&
      !baseRef.current.text.trim() &&
      briefing.takeDictation(composeDictation(result.state).text, session.id)
    ) {
      restoreBase();
      setLastRun(null);
      return;
    }
    // Hands-free: read the text back and send it only on a spoken "yes".
    // Spoken commands of a dictation (e.g. a briefing trigger) go before this.
    if (
      settings.handsFree &&
      !result.tooShort &&
      !result.error &&
      composeDictation(result.state).dictated.trim() &&
      document.visibilityState !== 'hidden'
    ) {
      void confirmHandsFree(composeDictation(result.state).text.trim());
    }
  };

  /** Cancel (X): discard this dictation and restore the text from before it. */
  const cancelRecording = () => {
    if (phaseRef.current === 'idle') return;
    diag('composer.dictate.cancel');
    dropDictation();
    restoreBase();
    setPhaseBoth('idle');
    micRef.current?.focus();
  };

  // Release the microphone when the composer goes away.
  useEffect(() => () => void dictationRef.current?.abandon(), []);
  // Automatic session switching must not take the screen from this text.
  useEffect(() => {
    noteComposer({ hasText: text.trim() !== '', dictating });
    return () => noteComposer({ hasText: false, dictating: false });
  }, [text, dictating]);
  // Hands-free decisions are not read out while the microphone is dictating.
  useEffect(() => {
    setDictationActive(dictating);
    return () => setDictationActive(false);
  }, [dictating]);

  // VOC-04: release the microphone whenever the app leaves the foreground.
  // A tap-to-toggle dictation is kept: stopped and transcribed. Hold to talk
  // means the finger left the button without releasing it on purpose (a call,
  // a system gesture, the screen locking): that recording is discarded.
  const stopRef = useRef(stopRecording);
  const cancelRef = useRef(cancelRecording);
  const micModeRef = useRef(micMode);
  useEffect(() => {
    stopRef.current = stopRecording;
    cancelRef.current = cancelRecording;
    micModeRef.current = micMode;
  });
  useEffect(() => {
    const onVisibility = () => {
      if (document.visibilityState === 'hidden') {
        const hold = micModeRef.current === 'hold' && phaseRef.current === 'recording';
        diag('composer.hidden.stop', { phase: phaseRef.current, cancel: hold });
        if (hold) cancelRef.current();
        else void stopRef.current();
      }
    };
    document.addEventListener('visibilitychange', onVisibility);
    return () => document.removeEventListener('visibilitychange', onVisibility);
  }, []);
  // Hold to talk has no visible cancel button while recording: Escape
  // discards the recording, wherever the focus is.
  const holdRecording = micMode === 'hold' && phase === 'recording';
  useEffect(() => {
    if (!holdRecording) return;
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      e.preventDefault();
      cancelRef.current();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [holdRecording]);

  // Elapsed-time display while recording.
  useEffect(() => {
    if (phase !== 'recording') return;
    const timer = setInterval(
      () => setElapsedMs(Date.now() - startedAtRef.current),
      ELAPSED_TICK_MS,
    );
    return () => clearInterval(timer);
  }, [phase]);

  // The send motion plays once (0.9 s), then the button rests.
  useEffect(() => {
    if (!sendFx) return;
    const timer = setTimeout(() => setSendFx(0), SEND_FX_MS);
    return () => clearTimeout(timer);
  }, [sendFx]);

  useEffect(() => {
    if (!micHint) return;
    const timer = setTimeout(() => setMicHint(null), HINT_MS);
    return () => clearTimeout(timer);
  }, [micHint]);

  useEffect(() => {
    if (!replaced) return;
    const timer = setTimeout(() => setReplaced(null), UNDO_MS);
    return () => clearTimeout(timer);
  }, [replaced]);

  /**
   * A quick prompt or saved snippet fills the box. A draft already there is
   * never lost silently: it can be brought back with Undo.
   */
  const insertPrompt = (value: string) => {
    setReplaced(
      text.trim() && text !== value
        ? { sessionId: session.id, inserted: value, text, corrections, lastRun }
        : null,
    );
    setText(value);
    setCorrections([]);
    setLastRun(null);
  };
  const undoReplace = () => {
    if (!replaced) return;
    setText(replaced.text);
    setCorrections(replaced.corrections);
    setLastRun(replaced.lastRun);
    setReplaced(null);
  };
  const canUndoReplace =
    !!replaced && replaced.sessionId === session.id && replaced.inserted === text;

  /** Typing during dictation ends it gracefully: what is shown becomes plain text. */
  const onTextChange = (value: string) => {
    if (dropDictation()) {
      setTailLength(0);
      setPhaseBoth('idle');
    }
    setText(value);
    setSendError(null);
  };

  /** The current press started the recording (toggle mode). */
  const pressStartedRef = useRef(false);
  const toggleRecording = () => {
    if (phaseRef.current === 'idle') startRecording();
    else if (phaseRef.current === 'recording') void stopRecording();
  };

  // Hold mode: Space/Enter act like pressing and releasing the button. In
  // toggle mode the native button click (also fired by Space/Enter) toggles.
  const onMicKeyDown = (e: KeyboardEvent<HTMLButtonElement>) => {
    if ((e.key === ' ' || e.key === 'Enter') && !e.repeat) {
      e.preventDefault();
      startRecording();
    }
  };
  const onMicKeyUp = (e: KeyboardEvent<HTMLButtonElement>) => {
    if (e.key === ' ' || e.key === 'Enter') {
      e.preventDefault();
      void stopRecording();
    }
  };

  /**
   * A shell refused text that looks dangerous outside the step-up window:
   * offer the confirm sheet (classification, biometrics, code) for one line.
   */
  const confirmInShell = (value: string): boolean => {
    if (!isShell || !shellServices || !isOpenShell(session)) return false;
    if (commandProblem(value.trim())) return false;
    shellServices.runInShell({ command: value.trim(), sessionId: session.id });
    return true;
  };

  /**
   * The shell held input back: several lines need "Run these N lines?"
   * (`onRan` after they ran), and a shell another device opened needs a
   * code to join it. `false` when the error is neither.
   */
  const handleShellHold = (err: unknown, onRan?: () => void): boolean => {
    if (!isShell || !shellServices) return false;
    if (err instanceof ShellConfirmNeeded) {
      void shellServices
        .confirmLines({ sessionId: err.sessionId, command: err.command, lines: err.lines })
        .then((ran) => {
          if (ran) onRan?.();
        });
      return true;
    }
    if (isShellJoinRequired(err)) {
      void shellServices.joinShell(session.id);
      return true;
    }
    return false;
  };

  /**
   * `force`: the user chose to send without the attachments that failed.
   * `expected`: send only if the box still holds exactly this text (hands-free
   * sends what was read back, never anything else). `openAtPress`: a typed send
   * that waited for uploads: was a decision open when the person pressed Send?
   * Their text was meant for that prompt, not for one that opened since.
   * Resolves `true` once sent.
   */
  const send = async (
    force = false,
    expected?: string,
    openAtPress?: boolean,
  ): Promise<boolean> => {
    // Hands-free (`expected`) sends exactly the text that was read back and
    // approved, even if the box was rewritten meanwhile (a late dictation
    // update); the box is only emptied when it still holds that text.
    const value = expected !== undefined ? expected.trim() : text.trim();
    const block = (reason: SendBlock): false => {
      sendBlockRef.current = reason;
      diag('composer.send.blocked', { reason, handsFree: expected !== undefined });
      return false;
    };
    sendBlockRef.current = null;
    setSendError(null);
    // The app lock covers the app: nothing is sent (a late hands-free or
    // Voco send included), whatever is still running.
    if (isAppLocked()) return block('locked');
    // "@name …": to that session, through its own guards (crossSend.ts).
    const target = leadingMention(value, mentionTargets);
    if (target) {
      if (expected !== undefined) {
        // Voice never redirects a message: the person sends it by hand.
        setSendError(t('mention.voiceBlocked', { name: target.session.display_name }));
        return block('changed');
      }
      if (phaseRef.current !== 'idle') return block('changed');
      if (attach.count > 0 || attach.busy) {
        setSendError(t('mention.noAttachments', { name: target.session.display_name }));
        return block('attachments');
      }
      if (sendingRef.current) return block('busy');
      sendingRef.current = true;
      try {
        const result = await crossSend(target.session.id, target.rest);
        if (result.status === 'blocked') {
          setSendError(describeCrossSendBlock(t, result.reason, target.session.display_name));
          return block('failed');
        }
        if (result.status === 'failed') {
          setSendError(
            t('sendTo.failed', {
              name: target.session.display_name,
              error: result.error.message,
            }),
          );
          return block('failed');
        }
        clearDraft(session.id);
        setText('');
        setCorrections([]);
        setLastRun(null);
        if (result.status === 'sent') setSentPulse((n) => n + 1);
        diag('composer.send.mention', { chars: target.rest.length });
        return true;
      } finally {
        sendingRef.current = false;
      }
    }
    if (!value || !enabled) return block('empty');
    if (phaseRef.current !== 'idle') return block('changed');
    // Hands-free never answers an agent's prompt: the text stays in the box.
    const openedSince = openAtPress === false;
    if ((expected !== undefined || openedSince) && awaitsDecision(session)) {
      setSendError(t('composer.decisionOpen'));
      return block('decision');
    }
    if (sendingRef.current) return block('busy');
    if (attach.busy) {
      // Attachments still uploading: the text waits for them and goes as ONE
      // prompt with their (single) mention. Sending now would leave the
      // later mentions to start the next prompt.
      if (attach.sendQueued) return block('busy');
      diag('composer.send.queued', {
        attachments: attach.count,
        handsFree: expected !== undefined,
      });
      const openNow = awaitsDecision(session);
      const wait = await attach.waitForUploads();
      if (!wait.go) return block('cancelled');
      // Mentions put into the box meanwhile belong to the read-back text too.
      const approved =
        expected !== undefined && wait.mentions.length > 0
          ? [expected.trim(), ...wait.mentions].join(' ')
          : expected;
      // Failed attachments do not hold the text back: it goes without them,
      // and the tray says which ones.
      const sent = await sendRef.current(true, approved, openNow);
      if (sent && wait.failed.length > 0) {
        attach.showNotice(
          t('attach.sentWithoutFailed', {
            count: wait.failed.length,
            names: wait.failed.join(', '),
          }),
        );
      }
      return sent;
    }
    if (!force && !attach.allowSend()) return block('attachments');
    sendingRef.current = true;
    const outgoing = attach.delivered();
    try {
      // A tap or Enter: the person typed this, also at an open prompt.
      await actions.sendText(session.id, value, outgoing.attachments, expected === undefined);
      // Delivered with the prompt: the chips move into its chat bubble.
      attach.clearDelivered(outgoing.ids);
      setSentPulse((n) => n + 1);
      setSendFx((n) => n + 1);
      if (shownRef.current.trim() === value || expected === undefined) {
        clearDraft(session.id);
        setText('');
        setCorrections([]);
        setLastRun(null);
      }
      diag('composer.send.ok', { chars: value.length, handsFree: expected !== undefined });
      return true;
    } catch (err) {
      // Other failures are shown through the delivery status.
      const handled = handleShellHold(err, () => {
        clearDraft(session.id);
        setText('');
        setCorrections([]);
      });
      if (!handled && isErrorCode(err, 'totp_required') && confirmInShell(value)) setText('');
      diag('composer.send.error', { code: (err as { code?: string }).code ?? 'unknown' });
      return block('failed');
    } finally {
      sendingRef.current = false;
    }
  };

  // Remember the draft while no dictation is running (a dictation's interim
  // text is not a draft; its base text already is).
  useEffect(() => {
    if (phase === 'idle') saveDraft(session.id, { text, corrections });
  }, [session.id, text, corrections, phase]);

  useEffect(() => {
    sendRef.current = send;
    startRef.current = startRecording;
    shownRef.current = text;
  });

  const paste = async () => {
    setKeyError(null);
    let clip: string;
    try {
      clip = (await navigator.clipboard?.readText?.()) ?? '';
    } catch {
      setKeyError(t('keys.pasteFailed'));
      return;
    }
    if (!clip) {
      setKeyError(t('keys.pasteEmpty'));
      return;
    }
    try {
      await actions.sendRaw(session.id, clip);
    } catch (err) {
      if (handleShellHold(err)) return;
      if (isErrorCode(err, 'totp_required')) {
        if (!confirmInShell(clip)) setKeyError(t('keys.pasteNeedsCode'));
      } else {
        setKeyError(describeShellError(t, err));
      }
    }
  };

  const updateSnippets = (next: string[]) => {
    setSnippets(next);
    saveSnippets(next);
  };

  const deliveryText = (() => {
    if (!delivery) return '';
    switch (delivery.status) {
      case 'sending':
        return t('delivery.sending');
      case 'queued':
        return t('delivery.queued');
      case 'delivered':
        return t('delivery.delivered');
      case 'acknowledged':
        return t('delivery.acknowledged');
      case 'failed':
        return t('delivery.failed', { error: delivery.error ?? '' });
    }
  })();

  const micLabel =
    phase === 'transcribing'
      ? t('composer.transcribing')
      : micMode === 'toggle'
        ? t(phase === 'recording' ? 'composer.micTapRecording' : 'composer.micTap')
        : t(phase === 'recording' ? 'composer.micRecording' : 'composer.micHold');

  /** The live input level for the mic button's motion (`null`: none measurable). */
  const readMicLevel = () => recorderRef.current?.level?.() ?? null;

  const micHandlers =
    micMode === 'toggle'
      ? {
          // A tap starts on the press, not when the finger comes up: what is
          // said during the tap is not lost. Its click then is not a second tap.
          onPointerDown: (e: PointerEvent<HTMLButtonElement>) => {
            pressStartedRef.current = e.button === 0 && phaseRef.current === 'idle';
            if (!pressStartedRef.current) return;
            e.currentTarget.setPointerCapture?.(e.pointerId);
            startRecording();
          },
          onClick: () => {
            if (!pressStartedRef.current) toggleRecording();
            pressStartedRef.current = false;
          },
          // The press became a scroll or a gesture: it was no tap after all.
          onPointerCancel: () => {
            if (pressStartedRef.current) cancelRecording();
            pressStartedRef.current = false;
          },
        }
      : {
          onPointerDown: (e: PointerEvent<HTMLButtonElement>) => {
            e.currentTarget.setPointerCapture?.(e.pointerId);
            startRecording();
          },
          onPointerUp: () => void stopRecording(),
          // The system took the pointer (a call, a gesture): not a release.
          onPointerCancel: () => {
            if (phaseRef.current === 'recording') cancelRecording();
          },
          onKeyDown: onMicKeyDown,
          onKeyUp: onMicKeyUp,
        };

  const showOriginal =
    !dictating && lastRun !== null && lastRun.raw.trim() !== lastRun.dictated.trim();

  /** Replaces the last dictation's corrected text in the composer with its raw text. */
  const applyOriginal = () => {
    if (!lastRun) return;
    const at = text.lastIndexOf(lastRun.dictated);
    if (at < 0) return;
    setText(text.slice(0, at) + lastRun.raw + text.slice(at + lastRun.dictated.length));
    setCorrections((prev) => prev.filter((c) => !lastRun.corrections.includes(c)));
    setLastRun(null);
  };

  /** Delivery problems are spelled out; progress is a dot next to the label. */
  const deliveryLoud = delivery?.status === 'failed' || delivery?.status === 'queued';
  /**
   * The dot alone is easy to miss (and colour only): each change of the
   * delivery state also shows a short word next to it for a few seconds.
   */
  const deliveryQuiet = delivery && !deliveryLoud ? delivery.status : null;
  const [shownDelivery, setShownDelivery] = useState(deliveryQuiet);
  const [deliveryFlash, setDeliveryFlash] = useState(false);
  if (deliveryQuiet !== shownDelivery) {
    setShownDelivery(deliveryQuiet);
    setDeliveryFlash(deliveryQuiet !== null);
  }
  useEffect(() => {
    if (!deliveryFlash) return;
    const timer = setTimeout(() => setDeliveryFlash(false), DELIVERY_FLASH_MS);
    return () => clearTimeout(timer);
  }, [deliveryFlash, shownDelivery]);
  const deliveryShort =
    deliveryQuiet === 'acknowledged' ? t('delivery.acknowledgedShort') : deliveryText;
  const sendState: 'idle' | 'uploading' | 'queued' = attach.sendQueued
    ? 'queued'
    : attach.busy
      ? 'uploading'
      : 'idle';
  const uploadPercent =
    attach.progress.total > 0
      ? Math.round((attach.progress.done / attach.progress.total) * 100)
      : 0;
  /** Voco is doing something (not only waiting for its name). */
  const vocoActive = !!voco && (voco.phase !== 'waiting' || !!voco.result || !!voco.hearing);
  // Phones keep the at-rest screen to one composer row: the idle "대기" chip
  // moves into the tools tray, next to the hands-free switch; a live state
  // (awake, countdown, hearing, a result, the read-back) still shows here.
  const showVoiceLine = !!readBack.status || (!!voco && (vocoActive || (!compact && !phoneRow)));

  const location =
    session.origin.type === 'local'
      ? folderName(session.origin.project_dir)
      : providerLabel(session.origin.provider);

  const describedBy = [targetId, corrections.length ? correctionsId : '', statusId]
    .filter(Boolean)
    .join(' ');

  const templatesEl = (
    <>
      <div className={styles.templates} role="group" aria-label={t('composer.templates')}>
        {isShell
          ? // Shells: saved snippets take the place of the agent prompts.
            snippets.map((s) => (
              <button
                key={s}
                type="button"
                className={`${styles.template} ${styles.snippetChip}`}
                disabled={!enabled || dictating}
                title={s}
                onClick={() => insertPrompt(s)}
              >
                {s}
              </button>
            ))
          : (session.conductor ? CONDUCTOR_TEMPLATE_KEYS : TEMPLATE_KEYS).map((key) => (
              <button
                key={key}
                type="button"
                className={styles.template}
                disabled={!enabled || dictating}
                onClick={() => insertPrompt(t(key))}
              >
                {t(key)}
              </button>
            ))}
      </div>
    </>
  );
  const toolbarExtrasEl = (
    <>
      <HandsFreeToggle />
      {phoneRow && !showVoiceLine && voco && <VocoStatus view={voco} />}
      {/* Phones reach 지휘소 from the top bar's "more" menu (its control). */}
      {conductorOn && !session.conductor && !phoneRow && (
        <button
          type="button"
          className={styles.expand}
          aria-label={t('conductor.mode')}
          title={t('conductor.modeHelp')}
          data-testid="composer-conductor"
          onClick={requestConductorView}
        >
          <Icon name="conductor" size={18} />
        </button>
      )}
      <button
        type="button"
        className={styles.expand}
        aria-label={t(expanded ? 'composer.collapse' : 'composer.expand')}
        aria-pressed={expanded}
        aria-controls={inputId}
        onClick={() => setExpanded((v) => !v)}
      >
        <Icon name={expanded ? 'collapse' : 'expand'} size={18} />
      </button>
    </>
  );
  const editorEl = (
    <>
      <TranscriptEditor
        id={inputId}
        value={text}
        onChange={onTextChange}
        corrections={corrections}
        // Names the target on phones too: with the top bar scrolled away it
        // is the one place the session shows before a dictation is sent.
        placeholder={t('composer.placeholderFor', { name: session.display_name })}
        describedBy={describedBy}
        disabled={!enabled}
        onSubmit={() => void send()}
        onKeyCapture={onMentionKey}
        combobox={
          mentionTargets.length > 0
            ? {
                listId: mentionListId,
                expanded: mentionOpen,
                activeId: mentionOpen ? `${mentionListId}-${activeMention}` : undefined,
              }
            : undefined
        }
        live={dictating}
        tailLength={tailLength}
        expanded={expanded}
        compact={compact}
      />
    </>
  );
  const mentionListEl = (
    <>
      {mentionOpen && (
        <ul
          id={mentionListId}
          role="listbox"
          className={styles.mentionList}
          aria-label={t('mention.listLabel')}
        >
          {mentionList.map((m, i) => (
            <li
              key={m.id}
              id={`${mentionListId}-${i}`}
              role="option"
              aria-selected={i === activeMention}
              className={styles.mentionOption}
              // Keeps the focus in the text box (no blur before the click).
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => pickMention(m)}
            >
              <span className={styles.mentionName}>{m.display_name}</span>
              <span className={styles.mentionWhere}>
                {m.origin.type === 'local'
                  ? folderName(m.origin.project_dir)
                  : originLabel(m.origin)}
              </span>
            </li>
          ))}
        </ul>
      )}
    </>
  );
  const micEl = (
    <>
      <button
        ref={micRef}
        type="button"
        className={`${styles.mic} ${phase === 'recording' ? styles.recording : ''}`}
        aria-label={micLabel}
        aria-pressed={phase === 'recording'}
        data-mode={micMode}
        data-phase={phase}
        data-sent={sendFx > 0 || undefined}
        data-animation={settings.micAnimation}
        disabled={!enabled || phase === 'transcribing'}
        onContextMenu={(e) => e.preventDefault()}
        {...micHandlers}
      >
        {/* The brand symbol is the voice mark, moving in the style chosen in
              Settings (MicSymbol; static with reduced motion). */}
        <MicSymbol
          animation={settings.micAnimation}
          phase={phase}
          sent={sendFx > 0}
          replay={sendFx}
          level={readMicLevel}
          logoClassName={styles.micLogo}
        />
      </button>
    </>
  );
  // Recording level and time. On phones it sits in the button row next to
  // the attach button, so recording adds no extra line.
  const liveEl = phase === 'recording' && (
    <span className={styles.live} data-testid="composer-live">
      <VoiceWave mode="live" />
      <span role="timer" aria-label={t('composer.elapsed')} className={styles.timer}>
        {formatElapsed(elapsedMs)}
      </span>
    </span>
  );
  const statusEl = (
    <>
      <div className={styles.status}>
        {!phoneRow && liveEl}
        <span className={styles.phase}>
          {phase === 'idle' && sentPulse > 0 && (
            <VoiceWave key={sentPulse} mode="converge" onDone={() => setSentPulse(0)} />
          )}
          <span className={styles.micText} title={micLabel}>
            {micLabel}
          </span>
          {/* Delivery as a small dot; problems are spelled out below. */}
          {delivery && !deliveryLoud && (
            <span
              className={styles.deliveryDot}
              data-status={delivery.status}
              title={deliveryText}
              aria-hidden="true"
            />
          )}
          {/* The status line below already tells screen readers. */}
          {deliveryFlash && deliveryQuiet && (
            <span
              className={styles.deliveryWord}
              data-status={deliveryQuiet}
              data-testid="delivery-word"
              aria-hidden="true"
            >
              {deliveryShort}
            </span>
          )}
        </span>
        <span
          id={statusId}
          role="status"
          aria-live="polite"
          className={deliveryLoud ? styles.delivery : 'visually-hidden'}
          data-status={delivery?.status}
          title={deliveryLoud ? deliveryText : undefined}
        >
          {deliveryText}
        </span>
      </div>
    </>
  );
  const cancelEl = (
    <>
      {(phase === 'transcribing' || (phase === 'recording' && micMode === 'toggle')) && (
        <button
          type="button"
          className={styles.cancel}
          aria-label={t('composer.micCancel')}
          onClick={cancelRecording}
        >
          <Icon name="close" size={20} />
        </button>
      )}
    </>
  );
  const sendEl = (
    <>
      <button
        type="button"
        className={`btn btn-primary ${styles.send}`}
        data-state={sendState}
        // Phones slim it while dictating even during an upload: the cancel
        // button joins it in a column too narrow for both.
        data-slim={(dictating && (sendState === 'idle' || phoneRow)) || undefined}
        onClick={() => (attach.sendQueued ? attach.cancelQueuedSend() : void send())}
        disabled={
          mention
            ? !mention.rest.trim() || dictating
            : !enabled ||
              (!attach.sendQueued && (!text.trim() || dictating || delivery?.status === 'sending'))
        }
        aria-label={
          attach.sendQueued
            ? t('composer.sendQueuedLabel')
            : attach.busy
              ? t('composer.sendAfterUpload', attach.progress)
              : mention
                ? t('mention.sendTo', { name: mention.session.display_name })
                : undefined
        }
        style={
          sendState === 'idle'
            ? undefined
            : ({ '--upload-progress': `${uploadPercent}%` } as CSSProperties)
        }
      >
        <span className={styles.sendMain}>
          <Icon name={attach.sendQueued ? 'close' : 'send'} size={18} />
          <span className={styles.sendLabel}>
            {attach.sendQueued
              ? t('composer.sendQueuedMain')
              : attach.busy
                ? t('composer.sendAfterUploadMain')
                : mention
                  ? t('mention.sendShort')
                  : t('composer.send')}
          </span>
        </span>
        {sendState !== 'idle' && (
          <>
            <span className={styles.sendSub} aria-hidden="true">
              {t(attach.sendQueued ? 'composer.sendQueuedSub' : 'composer.uploadProgress', {
                done: attach.progress.done,
                total: attach.progress.total,
              })}
            </span>
            <span className={styles.sendBar} aria-hidden="true" />
          </>
        )}
      </button>
    </>
  );
  const snippetsEl = (
    <>
      {!cloud && snippetsOpen && (
        <div
          id={snippetsId}
          className={styles.snippets}
          role="group"
          aria-label={t('keys.snippets')}
        >
          <div className={styles.snippetsHead}>
            <span className={styles.snippetsTitle}>{t('keys.snippets')}</span>
            <button
              type="button"
              className="btn btn-sm"
              data-keep-tray
              disabled={!text.trim()}
              onClick={() => updateSnippets(addSnippet(snippets, text))}
            >
              <Icon name="plus" size={14} />
              {t('keys.saveSnippet')}
            </button>
          </div>
          {snippets.length === 0 ? (
            <p className={styles.snippetsEmpty}>{t('keys.noSnippets')}</p>
          ) : (
            <ul className={styles.snippetList}>
              {snippets.map((s) => (
                <li key={s} className={styles.snippet}>
                  <button
                    type="button"
                    className={styles.snippetText}
                    title={s}
                    aria-label={t('keys.insertSnippet', { text: s })}
                    onClick={() => {
                      insertPrompt(s);
                      setSnippetsOpen(false);
                    }}
                  >
                    {s}
                  </button>
                  <button
                    type="button"
                    className={`icon-btn ${styles.snippetRemove}`}
                    data-keep-tray
                    aria-label={t('keys.removeSnippet', { text: s })}
                    onClick={() => updateSnippets(removeSnippet(snippets, s))}
                  >
                    <Icon name="close" size={14} />
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </>
  );
  const keyErrorEl = (
    <>
      {!cloud && keyError && (
        <p className={styles.error} role="alert">
          {keyError}
        </p>
      )}
    </>
  );
  const keysEl = (
    <>
      {!cloud && (
        <ControlKeys sessionId={session.id} disabled={!enabled} onError={setKeyError}>
          <button
            type="button"
            className="btn btn-sm"
            disabled={!enabled}
            aria-label={t('keys.paste')}
            title={t('keys.paste')}
            onClick={() => void paste()}
          >
            <Icon name="paste" size={16} />
            <span>{t('keys.pasteShort')}</span>
          </button>
          <button
            type="button"
            className="btn btn-sm"
            aria-label={t('keys.snippets')}
            title={t('keys.snippets')}
            aria-expanded={snippetsOpen}
            aria-controls={snippetsOpen ? snippetsId : undefined}
            onClick={() => setSnippetsOpen((v) => !v)}
          >
            <Icon name="bookmark" size={16} />
            <span>{t('keys.snippetsShort')}</span>
          </button>
        </ControlKeys>
      )}
    </>
  );
  /** Phones, reading mode, nothing typed or dictated: only the dictate button floats. */
  // A message to read (a mic error or hint, why a send did not go) or a live
  // voice state brings the full composer back: the floating button alone
  // would fail silently.
  const readingMini =
    phoneRow &&
    reading &&
    !text.trim() &&
    !dictating &&
    attach.count === 0 &&
    !micError &&
    !micNotice &&
    !micHint &&
    !sendError &&
    !readBack.status &&
    // Speech merely heard while waiting for the call never reflows the view.
    !(voco && (voco.phase !== 'waiting' || voco.result));

  const onBlur = (e: FocusEvent<HTMLElement>) => {
    const next = e.relatedTarget;
    if (!(next instanceof Node) || !e.currentTarget.contains(next)) setFocusWithin(false);
  };

  return (
    <section
      className={[
        styles.composer,
        compact ? styles.compact : '',
        attach.dropping ? attachStyles.dropping : '',
      ]
        .filter(Boolean)
        .join(' ')}
      data-has-text={text.trim() ? true : undefined}
      data-shell={isShell || undefined}
      data-keyboard-compact={compact || undefined}
      data-layout={layout}
      data-reading={readingMini || undefined}
      aria-label={t('composer.label', { name: session.display_name })}
      {...attach.dropProps}
      onPaste={attach.onPaste}
      onFocus={() => setFocusWithin(true)}
      onBlur={onBlur}
    >
      {!enabled && !(endedBarShown && inputBlockReason(session) === 'exited') && (
        <p className={styles.notice}>
          {t(
            inputBlockReason(session) !== 'exited'
              ? 'composer.disabledObserve'
              : ['shell', 'restart'].includes(reopenKind(session, !!shellServices?.allowed))
                ? 'composer.disabledExited'
                : 'composer.disabledExitedNoReopen',
          )}
        </p>
      )}
      {!phoneRow && (
        <div className={styles.toolbar}>
          {templatesEl}
          {toolbarExtrasEl}
        </div>
      )}
      {trayShown && (
        <div
          id={trayId}
          ref={trayRef}
          className={styles.tray}
          role="group"
          aria-label={t('composer.trayTitle')}
          onClick={onTrayClick}
          data-testid="composer-tray"
        >
          {templatesEl}
          <div className={styles.trayRow}>{toolbarExtrasEl}</div>
          {snippetsEl}
          <div data-keep-tray>{keysEl}</div>
        </div>
      )}
      {!compact && <HandsFreeNote />}
      {canUndoReplace && (
        <p className={styles.undo} role="status">
          <span>{t('composer.draftReplaced')}</span>
          <button type="button" className="btn btn-sm" onClick={undoReplace}>
            {t('composer.undo')}
          </button>
        </p>
      )}
      <label htmlFor={inputId} className="visually-hidden">
        {t('composer.label', { name: session.display_name })}
      </label>
      {/* The selected session card shows the target; screen readers hear it
          with the text box (and the placeholder names it). */}
      <span id={targetId} className="visually-hidden">
        {mention
          ? `${t('composer.to')} ${mention.session.display_name} · ${originLabel(mention.session.origin)}`
          : `${t('composer.to')} ${session.display_name} · ${originLabel(session.origin)} · ${location}`}
      </span>
      {mention && (
        <p className={styles.mentionChip} data-testid="mention-chip" role="status">
          <span className={styles.targetLabel}>{t('composer.to')}</span>
          <span className={styles.mentionName}>{mention.session.display_name}</span>
          <button
            type="button"
            className={`icon-btn ${styles.mentionRemove}`}
            aria-label={t('mention.remove', {
              name: mention.session.display_name,
              current: session.display_name,
            })}
            title={t('mention.remove', {
              name: mention.session.display_name,
              current: session.display_name,
            })}
            onClick={removeMention}
          >
            <Icon name="close" size={14} />
          </button>
        </p>
      )}
      {/* With the keyboard open the session cards and header step aside: a
          one-line strip keeps the target in sight (screen readers already
          hear it with the text box). */}
      {compact && !mention && (
        <p className={styles.target} aria-hidden="true" data-testid="composer-target">
          <span className={styles.targetLabel}>{t('composer.to')}</span>
          <span className={styles.targetName}>{session.display_name}</span>
        </p>
      )}
      {/* The voice state (wake word, read-back) on its own quiet line, never
          squeezed into the action row. Typing with the keyboard open hides
          the idle "대기" chip; a live state still shows. */}
      {showVoiceLine && (
        <div className={styles.voiceLine} data-testid="composer-voice-line">
          <HandsFreeStatus status={readBack.status} />
          {!readBack.status && <VocoStatus view={voco} />}
        </div>
      )}
      {topic && !compact && (
        <p className={styles.topic} data-testid="composer-topic">
          {t('composer.topic', { topic })}
        </p>
      )}
      {phoneRow ? (
        <div className={styles.textRow} data-testid="composer-text-row">
          {editorEl}
        </div>
      ) : (
        editorEl
      )}
      {mentionListEl}
      {corrections.length > 0 && (
        <p id={correctionsId} className={styles.corrections}>
          {t('composer.corrections', {
            list: corrections.map((c) => `“${c.from}” → ${c.to}`).join(', '),
          })}
        </p>
      )}
      {showOriginal && !compact && (
        <OriginalTranscript
          raw={lastRun.raw}
          corrections={lastRun.corrections}
          onUseOriginal={text.includes(lastRun.dictated) ? applyOriginal : undefined}
        />
      )}
      {attach.tray}
      {phoneRow ? (
        <>
          <div
            className={`${styles.row} ${styles.phoneRow}`}
            data-testid="composer-bottom-row"
            data-dictating={dictating || undefined}
          >
            <div className={styles.rowStart}>
              {attach.button}
              {liveEl}
              {!compact && !reading && !dictating && (
                <button
                  ref={trayButtonRef}
                  type="button"
                  className={styles.trayButton}
                  aria-label={t('composer.tray')}
                  title={t('composer.tray')}
                  aria-expanded={trayShown}
                  aria-controls={trayShown ? trayId : undefined}
                  data-testid="composer-tray-button"
                  onClick={() => setTrayOpen((v) => !v)}
                >
                  <Icon name="more" size={20} />
                </button>
              )}
            </div>
            {micEl}
            <div className={styles.rowEnd}>
              {cancelEl}
              {sendEl}
            </div>
          </div>
          <div className={styles.phoneStatus}>{statusEl}</div>
        </>
      ) : (
        <div className={styles.row} data-dictating={dictating || undefined}>
          {attach.button}
          {micEl}
          {statusEl}
          {cancelEl}
          {sendEl}
        </div>
      )}
      {(micError || micNotice || micHint || sendError) && (
        <div className={styles.messages}>
          {sendError && (
            <p className={`${styles.error} ${styles.sendError}`} role="alert">
              <span>{sendError}</span>
              <button
                type="button"
                className="icon-btn"
                aria-label={t('common.close')}
                title={t('common.close')}
                onClick={() => setSendError(null)}
              >
                <Icon name="close" size={14} />
              </button>
            </p>
          )}
          {micError && (
            <p className={styles.error} role="alert">
              {micError}
              {retry && phase === 'idle' && (
                <button type="button" className="btn btn-sm" onClick={() => void retryDictation()}>
                  {t('composer.speechRetry')}
                </button>
              )}
            </p>
          )}
          {micNotice && (
            <p className={styles.info} role="status">
              <Icon name="info" size={14} />
              {micNotice}
              {speechOff && canOpenEngines && (
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() => requestSettingsView('set-engines')}
                >
                  {t('composer.openEngines')}
                </button>
              )}
            </p>
          )}
          {micHint && (
            <p className={styles.info} role="status">
              <Icon name="info" size={14} />
              {micHint}
            </p>
          )}
        </div>
      )}
      {!phoneRow && snippetsEl}
      {keyErrorEl}
      {!phoneRow && keysEl}
    </section>
  );
}
