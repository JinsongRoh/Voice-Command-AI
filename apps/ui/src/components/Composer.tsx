import { useEffect, useId, useRef, useState, type KeyboardEvent, type PointerEvent } from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import { folderName, isCloud, originLabel, providerLabel } from '../protocol/helpers';
import type { ControlKey, SessionInfo } from '../protocol/types';
import { useSpeech } from '../speech/context';
import {
  composeDictation,
  Dictation,
  formatElapsed,
  isTooShort,
  type DictationState,
} from '../speech/dictation';
import type { Recorder } from '../speech/recorder';
import { isSpeechDisabledError, type TranscriptCorrection } from '../speech/transcribe';
import { canSendInput } from '../state/reducer';
import { useActions, useAppState, useSettings } from '../state/store';
import { Icon, type IconName } from './Icon';
import { OriginalTranscript } from './OriginalTranscript';
import { TranscriptEditor } from './TranscriptEditor';
import { VoiceWave } from './VoiceWave';
import styles from './Composer.module.css';

type Phase = 'idle' | 'recording' | 'transcribing';

const ELAPSED_TICK_MS = 250;
const HINT_MS = 4000;

const KEYS: { key: ControlKey; label: MessageKey; icon?: IconName }[] = [
  { key: 'interrupt', label: 'key.interrupt' },
  { key: 'escape', label: 'key.escape' },
  { key: 'enter', label: 'key.enter' },
  { key: 'tab', label: 'key.tab' },
  { key: 'up', label: 'key.up', icon: 'up' },
  { key: 'down', label: 'key.down', icon: 'down' },
  { key: 'left', label: 'key.left', icon: 'back' },
  { key: 'right', label: 'key.right', icon: 'back' },
];

const TEMPLATE_KEYS: MessageKey[] = [
  'template.runTests',
  'template.commitPr',
  'template.summarize',
  'template.review',
  'template.continue',
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
 * not leak between sessions.
 *
 * Voice input writes straight into the text box: while dictating, the live
 * transcript appears after the existing text (interim tail dimmed) and stays
 * editable once dictation stops. There is no separate review step.
 */
export function Composer({ session }: { session: SessionInfo }) {
  const { t, locale } = useI18n();
  const actions = useActions();
  const speech = useSpeech();
  const { settings } = useSettings();
  const delivery = useAppState((s) => s.delivery[session.id]);
  const [text, setText] = useState('');
  const [corrections, setCorrections] = useState<TranscriptCorrection[]>([]);
  const [phase, setPhase] = useState<Phase>('idle');
  const [micError, setMicError] = useState<string | null>(null);
  /** Neutral information about voice input (not an error the user caused). */
  const [micNotice, setMicNotice] = useState<string | null>(null);
  /** Brief, self-dismissing usage hint (e.g. after an accidental tap). */
  const [micHint, setMicHint] = useState<string | null>(null);
  const [elapsedMs, setElapsedMs] = useState(0);
  /** Trailing characters of `text` that are still an interim transcript. */
  const [tailLength, setTailLength] = useState(0);
  const [expanded, setExpanded] = useState(false);
  /** The last completed dictation, for the "Original" view. */
  const [lastRun, setLastRun] = useState<LastRun | null>(null);
  /** Bumped on every successful send to play the converging motion once. */
  const [sentPulse, setSentPulse] = useState(0);
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
  const enabled = canSendInput(session);
  const cloud = isCloud(session.origin);
  const micMode = settings.micMode;
  const dictating = phase !== 'idle';

  const setPhaseBoth = (p: Phase) => {
    phaseRef.current = p;
    setPhase(p);
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

  const startRecording = () => {
    if (phaseRef.current !== 'idle' || !enabled) return;
    setMicError(null);
    setMicNotice(null);
    setMicHint(null);
    setElapsedMs(0);
    startedAtRef.current = Date.now();
    baseRef.current = { text, corrections };
    setLastRun(null);
    setPhaseBoth('recording');

    const transcribeWith = (audio: Blob, context: string | null) =>
      speech.transcribe(audio, {
        engine: settings.sttEngine,
        locale,
        sessionId: session.id,
        context,
      });
    const begin = (recorder: Recorder) => {
      const dictation: Dictation = new Dictation({
        recorder,
        transcribe: transcribeWith,
        base: text,
        onUpdate: (state) => {
          if (dictationRef.current === dictation) showDictation(state);
        },
      });
      dictationRef.current = dictation;
      return dictation;
    };

    const recorder = speech.createRecorder();
    recorderRef.current = recorder;
    const dictation = begin(recorder);
    startingRef.current = recorder.start().then(
      () => {
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

  const stopRecording = async () => {
    if (phaseRef.current !== 'recording') return;
    const elapsed = Date.now() - startedAtRef.current;
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
    if (dictationRef.current !== dictation) return;
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
    if (result.error) {
      if (isSpeechDisabledError(result.error)) setMicNotice(t('composer.speechOff'));
      else setMicError((result.error as Error).message);
    }
    setPhaseBoth('idle');
  };

  /** Cancel (X): discard this dictation and restore the text from before it. */
  const cancelRecording = () => {
    if (phaseRef.current === 'idle') return;
    dropDictation();
    restoreBase();
    setPhaseBoth('idle');
    micRef.current?.focus();
  };

  // Release the microphone when the composer goes away.
  useEffect(() => () => void dictationRef.current?.abandon(), []);

  // VOC-04: release the microphone whenever the app leaves the foreground.
  // Long dictations are kept: the recording is stopped and transcribed.
  const stopRef = useRef(stopRecording);
  useEffect(() => {
    stopRef.current = stopRecording;
  });
  useEffect(() => {
    const onVisibility = () => {
      if (document.visibilityState === 'hidden') void stopRef.current();
    };
    document.addEventListener('visibilitychange', onVisibility);
    return () => document.removeEventListener('visibilitychange', onVisibility);
  }, []);

  // Elapsed-time display while recording.
  useEffect(() => {
    if (phase !== 'recording') return;
    const timer = setInterval(
      () => setElapsedMs(Date.now() - startedAtRef.current),
      ELAPSED_TICK_MS,
    );
    return () => clearInterval(timer);
  }, [phase]);

  useEffect(() => {
    if (!micHint) return;
    const timer = setTimeout(() => setMicHint(null), HINT_MS);
    return () => clearTimeout(timer);
  }, [micHint]);

  /** Typing during dictation ends it gracefully: what is shown becomes plain text. */
  const onTextChange = (value: string) => {
    if (dropDictation()) {
      setTailLength(0);
      setPhaseBoth('idle');
    }
    setText(value);
  };

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

  const send = async () => {
    const value = text.trim();
    if (!value || !enabled || phaseRef.current !== 'idle') return;
    try {
      await actions.sendText(session.id, value);
      setSentPulse((n) => n + 1);
      setText('');
      setCorrections([]);
      setLastRun(null);
    } catch {
      // The failure is shown through the delivery status.
    }
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

  const micHandlers =
    micMode === 'toggle'
      ? { onClick: toggleRecording }
      : {
          onPointerDown: (e: PointerEvent<HTMLButtonElement>) => {
            e.currentTarget.setPointerCapture?.(e.pointerId);
            startRecording();
          },
          onPointerUp: () => void stopRecording(),
          onPointerCancel: () => void stopRecording(),
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

  const location =
    session.origin.type === 'local'
      ? folderName(session.origin.project_dir)
      : providerLabel(session.origin.provider);

  const describedBy = [corrections.length ? correctionsId : '', statusId].filter(Boolean).join(' ');

  return (
    <section
      className={styles.composer}
      aria-label={t('composer.label', { name: session.display_name })}
    >
      {!enabled && <p className={styles.notice}>{t('composer.disabled')}</p>}
      <div className={styles.toolbar}>
        <div className={styles.templates} role="group" aria-label={t('composer.templates')}>
          {TEMPLATE_KEYS.map((key) => (
            <button
              key={key}
              type="button"
              className={styles.template}
              disabled={!enabled || dictating}
              onClick={() => {
                setText(t(key));
                setCorrections([]);
                setLastRun(null);
              }}
            >
              {t(key)}
            </button>
          ))}
        </div>
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
      </div>
      <label htmlFor={inputId} className="visually-hidden">
        {t('composer.label', { name: session.display_name })}
      </label>
      {/* Brand promise: the target session and project are visible before sending. */}
      <p className={styles.target}>
        <span className={styles.targetTo}>{t('composer.to')}</span>{' '}
        <span className={styles.targetName}>{session.display_name}</span>{' '}
        <span className={styles.targetMeta}>
          {originLabel(session.origin)}
          <span aria-hidden="true"> · </span>
          {location}
        </span>
      </p>
      <TranscriptEditor
        id={inputId}
        value={text}
        onChange={onTextChange}
        corrections={corrections}
        placeholder={t('composer.placeholder')}
        describedBy={describedBy}
        disabled={!enabled}
        onSubmit={send}
        live={dictating}
        tailLength={tailLength}
        expanded={expanded}
      />
      {corrections.length > 0 && (
        <p id={correctionsId} className={styles.corrections}>
          {t('composer.corrections', {
            list: corrections.map((c) => `“${c.from}” → ${c.to}`).join(', '),
          })}
        </p>
      )}
      {showOriginal && (
        <OriginalTranscript
          raw={lastRun.raw}
          corrections={lastRun.corrections}
          onUseOriginal={text.includes(lastRun.dictated) ? applyOriginal : undefined}
        />
      )}
      <div className={styles.row}>
        <button
          ref={micRef}
          type="button"
          className={`${styles.mic} ${phase === 'recording' ? styles.recording : ''}`}
          aria-label={micLabel}
          aria-pressed={phase === 'recording'}
          data-mode={micMode}
          disabled={!enabled || phase === 'transcribing'}
          onContextMenu={(e) => e.preventDefault()}
          {...micHandlers}
        >
          <Icon name="mic" size={30} />
        </button>
        <div className={styles.status}>
          <span className={styles.phase}>
            {phase === 'recording' && (
              <span className={styles.live}>
                <VoiceWave mode="live" />
                <span role="timer" aria-label={t('composer.elapsed')} className={styles.timer}>
                  {formatElapsed(elapsedMs)}
                </span>
              </span>
            )}
            {phase === 'idle' && sentPulse > 0 && (
              <VoiceWave key={sentPulse} mode="converge" onDone={() => setSentPulse(0)} />
            )}
            {micLabel}
          </span>
          <span
            id={statusId}
            role="status"
            aria-live="polite"
            className={styles.delivery}
            data-status={delivery?.status}
          >
            {deliveryText}
          </span>
          {micError && (
            <span className={styles.error} role="alert">
              {micError}
            </span>
          )}
          {micNotice && (
            <span className={styles.info} role="status">
              <Icon name="info" size={14} />
              {micNotice}
            </span>
          )}
          {micHint && (
            <span className={styles.info} role="status">
              <Icon name="info" size={14} />
              {micHint}
            </span>
          )}
        </div>
        {phase === 'recording' && micMode === 'toggle' && (
          <button
            type="button"
            className={styles.cancel}
            aria-label={t('composer.micCancel')}
            onClick={cancelRecording}
          >
            <Icon name="close" size={20} />
          </button>
        )}
        <button
          type="button"
          className="btn btn-primary"
          onClick={send}
          disabled={!enabled || !text.trim() || dictating || delivery?.status === 'sending'}
        >
          <Icon name="send" size={18} />
          {t('composer.send')}
        </button>
      </div>
      {!cloud && (
        <div className={styles.keys} role="group" aria-label={t('composer.keys')}>
          {KEYS.map(({ key, label, icon }) => (
            <button
              key={key}
              type="button"
              className={`btn btn-sm ${key === 'interrupt' ? styles.interrupt : ''}`}
              disabled={!enabled}
              aria-label={t(label)}
              onClick={() => actions.sendKey(session.id, key).catch(() => {})}
            >
              {icon ? (
                <Icon name={icon} size={16} className={key === 'right' ? styles.flip : undefined} />
              ) : (
                t(label)
              )}
            </button>
          ))}
        </div>
      )}
    </section>
  );
}
