import { useEffect, useId, useRef, useState, type KeyboardEvent } from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import { isCloud } from '../protocol/helpers';
import type { ControlKey, SessionInfo } from '../protocol/types';
import { useSpeech } from '../speech/context';
import type { Recorder } from '../speech/recorder';
import type { TranscriptCorrection } from '../speech/transcribe';
import { canSendInput } from '../state/reducer';
import { useActions, useAppState, useSettings } from '../state/store';
import { Icon, type IconName } from './Icon';
import { TranscriptEditor } from './TranscriptEditor';
import styles from './Composer.module.css';

type Phase = 'idle' | 'recording' | 'transcribing';

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

/** Command input for one session. Render with `key={session.id}` so drafts do not leak between sessions. */
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
  const phaseRef = useRef<Phase>('idle');
  const recorderRef = useRef<Recorder | null>(null);
  const startingRef = useRef<Promise<void> | null>(null);
  const inputId = useId();
  const statusId = useId();
  const correctionsId = useId();
  const enabled = canSendInput(session);
  const cloud = isCloud(session.origin);

  const setPhaseBoth = (p: Phase) => {
    phaseRef.current = p;
    setPhase(p);
  };

  // VOC-04: release the microphone whenever the app leaves the foreground.
  useEffect(() => {
    const onVisibility = () => {
      if (document.visibilityState === 'hidden' && phaseRef.current === 'recording') {
        recorderRef.current?.cancel();
        recorderRef.current = null;
        setPhaseBoth('idle');
      }
    };
    document.addEventListener('visibilitychange', onVisibility);
    return () => {
      document.removeEventListener('visibilitychange', onVisibility);
      recorderRef.current?.cancel();
    };
  }, []);

  const startRecording = () => {
    if (phaseRef.current !== 'idle' || !enabled) return;
    setMicError(null);
    setPhaseBoth('recording');
    const recorder = speech.createRecorder();
    recorderRef.current = recorder;
    startingRef.current = recorder.start().catch((err: Error) => {
      recorderRef.current = null;
      if (speech.fallbackWithoutMic) {
        setMicError(t('composer.micDemo', { error: err.message }));
      } else {
        setMicError(t('composer.micUnavailable', { error: err.message }));
        setPhaseBoth('idle');
      }
    });
  };

  const stopRecording = async () => {
    if (phaseRef.current !== 'recording') return;
    setPhaseBoth('transcribing');
    await startingRef.current;
    const recorder = recorderRef.current;
    recorderRef.current = null;
    if (!recorder && !speech.fallbackWithoutMic) {
      setPhaseBoth('idle');
      return;
    }
    try {
      const audio = recorder ? await recorder.stop() : new Blob();
      const transcript = await speech.transcribe(audio, {
        engine: settings.sttEngine,
        locale,
        sessionId: session.id,
      });
      setText((prev) => (prev.trim() ? `${prev.trimEnd()} ${transcript.text}` : transcript.text));
      setCorrections((prev) => [...prev, ...transcript.corrections]);
    } catch (err) {
      setMicError((err as Error).message);
    } finally {
      setPhaseBoth('idle');
    }
  };

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
    if (!value || !enabled) return;
    try {
      await actions.sendText(session.id, value);
      setText('');
      setCorrections([]);
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
    phase === 'recording'
      ? t('composer.micRecording')
      : phase === 'transcribing'
        ? t('composer.transcribing')
        : t('composer.micHold');

  const describedBy = [corrections.length ? correctionsId : '', statusId].filter(Boolean).join(' ');

  return (
    <section
      className={styles.composer}
      aria-label={t('composer.label', { name: session.display_name })}
    >
      {!enabled && <p className={styles.notice}>{t('composer.disabled')}</p>}
      <div className={styles.templates} role="group" aria-label={t('composer.templates')}>
        {TEMPLATE_KEYS.map((key) => (
          <button
            key={key}
            type="button"
            className={styles.template}
            disabled={!enabled}
            onClick={() => {
              setText(t(key));
              setCorrections([]);
            }}
          >
            {t(key)}
          </button>
        ))}
      </div>
      <label htmlFor={inputId} className="visually-hidden">
        {t('composer.label', { name: session.display_name })}
      </label>
      <TranscriptEditor
        id={inputId}
        value={text}
        onChange={setText}
        corrections={corrections}
        placeholder={t('composer.placeholder')}
        describedBy={describedBy}
        disabled={!enabled}
        onSubmit={send}
      />
      {corrections.length > 0 && (
        <p id={correctionsId} className={styles.corrections}>
          {t('composer.corrections', {
            list: corrections.map((c) => `“${c.from}” → ${c.to}`).join(', '),
          })}
        </p>
      )}
      <div className={styles.row}>
        <button
          type="button"
          className={`${styles.mic} ${phase === 'recording' ? styles.recording : ''}`}
          aria-label={micLabel}
          aria-pressed={phase === 'recording'}
          disabled={!enabled || phase === 'transcribing'}
          onPointerDown={(e) => {
            e.currentTarget.setPointerCapture?.(e.pointerId);
            startRecording();
          }}
          onPointerUp={() => void stopRecording()}
          onPointerCancel={() => void stopRecording()}
          onKeyDown={onMicKeyDown}
          onKeyUp={onMicKeyUp}
          onContextMenu={(e) => e.preventDefault()}
        >
          <Icon name="mic" size={30} />
        </button>
        <div className={styles.status}>
          <span className={styles.phase}>{micLabel}</span>
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
        </div>
        <button
          type="button"
          className="btn btn-primary"
          onClick={send}
          disabled={!enabled || !text.trim() || delivery?.status === 'sending'}
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
