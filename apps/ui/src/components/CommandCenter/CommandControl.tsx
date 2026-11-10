import {
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent,
  type MouseEvent,
  type PointerEvent,
} from 'react';

import { useMediaQuery } from '../../hooks/useMediaQuery';
import { useI18n } from '../../i18n/context';
import { isLocalMachine } from '../../native/host';
import { isStepUpCanceled } from '../../native/security';
import { FEATURE_COMMAND_CENTER, FEATURE_CONDUCTOR } from '../../protocol/types';
import { useSpeech } from '../../speech/context';
import { borrowMic, DICTATION_MIC, setDictationActive } from '../../speech/handsFree';
import type { Recorder } from '../../speech/recorder';
import {
  CONDUCTOR_CHAT_DRAFT,
  STATUS_KEYS,
  conductorPaused,
  conductorStopped,
  conductorUsable,
} from '../../state/conductor';
import { speechLanguageParam } from '../../state/settings';
import { useActions, useAppState, useConductor, useMachines, useSettings } from '../../state/store';
import { appendToDraft } from '../composerDrafts';
import { Icon } from '../Icon';
import { ConfirmDialog } from '../Modal';
import { isErrorCode } from '../shellErrors';
import { useStepUp } from '../StepUpPrompt';
import { TotpPrompt } from '../TotpPrompt';
import type { CommandTab } from './commandTabs';
import { killErrorText } from './killErrors';
import { useInboxCount } from './useInbox';
import styles from './CommandControl.module.css';

/** "Talk to the conductor": starting the microphone, recording, transcribing. */
type TalkPhase = 'idle' | 'starting' | 'recording' | 'transcribing';

/** The hands-free switch shows on its own from this width; below, in the menu. */
const WIDE_QUERY = '(min-width: 640px)';

/**
 * The header's Command Center control, on every page while the PC has a
 * conductor that is turned on: its status and what waits on the person (opens
 * the Command Center), "talk to the conductor" (the transcript lands in its
 * box for review), hands-free ("보코 … 오버" (Voco … over)) and the stop menu:
 * pause and resume, stop (start again on the PC) and the kill switch. Servers with
 * `command_center` pause with `cc_pause` (nothing wakes the conductor, any
 * device may resume); older ones pause by stopping. The kill switch asks
 * for confirmation and a step-up: the PC's local verification
 * (`conductor_kill`), or on a phone the authenticator code.
 */
export function CommandControl({
  onOpen,
  showHandsFree = true,
}: {
  onOpen?: (tab?: CommandTab) => void;
  /**
   * The hands-free switch here (off inside a phone session: the composer's
   * tools tray has the same switch).
   */
  showHandsFree?: boolean;
}) {
  const { t, locale } = useI18n();
  const actions = useActions();
  const stepUp = useStepUp();
  const speech = useSpeech();
  const state = useConductor();
  const { settings, updateSettings } = useSettings();
  const { machine } = useMachines();
  const local = isLocalMachine(machine.id);
  const supported = useAppState(
    (s) => s.connection === 'connected' && s.serverFeatures.includes(FEATURE_CONDUCTOR),
  );
  const ccPause = useAppState((s) => s.serverFeatures.includes(FEATURE_COMMAND_CENTER));
  const conductorSession = useAppState((s) =>
    state?.session_id ? s.sessions[state.session_id] : undefined,
  );
  const count = useInboxCount();
  const wide = useMediaQuery(WIDE_QUERY);
  const [open, setOpen] = useState(false);
  const [confirmKill, setConfirmKill] = useState(false);
  /** A phone's kill switch asks for the authenticator code. */
  const [killCode, setKillCode] = useState(false);
  const [mic, setMic] = useState<TalkPhase>('idle');
  /** The phase for handlers that outlive a render (pointer up, visibility). */
  const micPhase = useRef<TalkPhase>('idle');
  const recorderRef = useRef<Recorder | null>(null);
  /** Bumped by a cancel: a recording or transcript still on its way is dropped. */
  const talkRun = useRef(0);
  /** This control holds the dictation microphone (Voco paused, decisions wait). */
  const holdingMic = useRef(false);
  const wrapRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const menuId = useId();
  const hold = settings.micMode === 'hold';

  const setMicBoth = (phase: TalkPhase) => {
    micPhase.current = phase;
    setMic(phase);
  };
  const endTalk = () => {
    recorderRef.current = null;
    if (holdingMic.current) {
      holdingMic.current = false;
      setDictationActive(false);
    }
    setMicBoth('idle');
  };
  const micFailed = (err: unknown) =>
    actions.showToast({
      id: 'cc-mic',
      category: 'error',
      sessionId: null,
      title: t('conductor.micError', { error: (err as Error).message }),
      body: '',
    });
  /**
   * Talk to the conductor: like the composer's dictation, it takes the
   * microphone from the Voco listener (given back after) and holds spoken
   * decisions while it records.
   */
  const startTalk = async () => {
    if (micPhase.current !== 'idle') return;
    const run = ++talkRun.current;
    const recorder = speech.createRecorder();
    recorderRef.current = recorder;
    holdingMic.current = true;
    setDictationActive(true);
    setMicBoth('starting');
    try {
      await borrowMic(DICTATION_MIC);
      await recorder.start();
    } catch (err) {
      if (talkRun.current !== run) return;
      endTalk();
      micFailed(err);
      return;
    }
    // Cancelled (or a hold released) while the microphone was starting.
    if (talkRun.current !== run) {
      recorder.cancel();
      return;
    }
    setMicBoth('recording');
  };
  /** Discards the recording (or the transcript on its way); nothing lands in the box. */
  const cancelTalk = () => {
    if (micPhase.current === 'idle') return;
    talkRun.current++;
    recorderRef.current?.cancel();
    endTalk();
  };
  /** Push to talk: the transcript goes to the conductor's box, never sent by itself. */
  const stopTalk = async () => {
    // Released before the microphone was ready: too short to say anything.
    if (micPhase.current === 'starting') {
      cancelTalk();
      return;
    }
    if (micPhase.current !== 'recording') return;
    const recorder = recorderRef.current;
    recorderRef.current = null;
    if (!recorder) return;
    const run = talkRun.current;
    setMicBoth('transcribing');
    try {
      const audio = await recorder.stop();
      const result = await speech.transcribe(audio, {
        locale,
        language: speechLanguageParam(settings.speechLanguage),
        sessionId: conductorSession?.id ?? null,
        final: true,
      });
      if (talkRun.current !== run) return;
      // Nothing heard (an accidental tap): stay where the person is.
      if (!result.text.trim()) return;
      appendToDraft(conductorSession?.id ?? CONDUCTOR_CHAT_DRAFT, result.text);
      onOpen?.('conversation');
    } catch (err) {
      if (talkRun.current === run) micFailed(err);
    } finally {
      if (talkRun.current === run) endTalk();
    }
  };
  const talkRef = useRef({ stop: stopTalk, cancel: cancelTalk, hold });
  useEffect(() => {
    talkRef.current = { stop: stopTalk, cancel: cancelTalk, hold };
  });

  useEffect(
    () => () => {
      talkRun.current++;
      recorderRef.current?.cancel();
      if (holdingMic.current) setDictationActive(false);
    },
    [],
  );
  // The app leaves the foreground: the microphone is released. A tap-to-talk
  // recording is kept (stopped and transcribed); a held one is discarded
  // (the finger left the button without releasing it on purpose).
  useEffect(() => {
    const onVisibility = () => {
      if (document.visibilityState !== 'hidden') return;
      if (micPhase.current === 'idle' || micPhase.current === 'transcribing') return;
      if (talkRef.current.hold) talkRef.current.cancel();
      else void talkRef.current.stop();
    };
    document.addEventListener('visibilitychange', onVisibility);
    return () => document.removeEventListener('visibilitychange', onVisibility);
  }, []);
  // Escape discards a recording, wherever the focus is.
  const talking = mic !== 'idle';
  useEffect(() => {
    if (!talking) return;
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key !== 'Escape' || e.defaultPrevented) return;
      e.preventDefault();
      talkRef.current.cancel();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [talking]);
  useEffect(() => {
    if (!open) return;
    // The first item takes the focus when the menu opens.
    menuRef.current?.querySelector<HTMLElement>('[role^="menuitem"]')?.focus();
    const onPointer = (e: globalThis.PointerEvent) => {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('pointerdown', onPointer);
    return () => document.removeEventListener('pointerdown', onPointer);
  }, [open]);

  if (!supported || !state?.settings.enabled) return null;

  const stopped = conductorStopped(state);
  const paused = conductorPaused(state);
  const usable = conductorUsable(state);
  const status = t(STATUS_KEYS[state.status]);
  const fail = (err: unknown) => {
    if (isStepUpCanceled(err)) return;
    actions.showToast({
      id: `cc-control:${Date.now()}`,
      category: 'error',
      sessionId: null,
      title: t('error.request', { error: (err as Error).message }),
      body: '',
    });
  };
  const close = (focusTrigger = true) => {
    setOpen(false);
    if (focusTrigger) triggerRef.current?.focus();
  };
  const stop = () => {
    close();
    actions.conductor({ type: 'conductor_stop', kill: false }).catch(fail);
  };
  const pause = () => {
    if (!ccPause) {
      stop();
      return;
    }
    close();
    actions.conductor({ type: 'cc_pause', paused: true }).catch(fail);
  };
  const unpause = () => {
    close();
    actions.conductor({ type: 'cc_pause', paused: false }).catch(fail);
  };
  const askKill = () => {
    close();
    // A phone on a current server: the code is the confirmation.
    if (!local && ccPause) setKillCode(true);
    else setConfirmKill(true);
  };
  const kill = () => {
    setConfirmKill(false);
    if (local) {
      stepUp
        .run('conductor_kill', () => actions.conductor({ type: 'conductor_stop', kill: true }))
        .catch(fail);
      return;
    }
    actions.conductor({ type: 'conductor_stop', kill: true }).catch((err: unknown) => {
      // A server that wants the code without announcing it: ask for it.
      if (isErrorCode(err, 'totp_required')) setKillCode(true);
      else fail(err);
    });
  };
  const resume = () => {
    close();
    stepUp.run('conductor_setup', () => actions.conductor({ type: 'conductor_start' })).catch(fail);
  };

  const toggleTalk = () => {
    if (micPhase.current === 'idle') void startTalk();
    else if (micPhase.current === 'recording') void stopTalk();
  };
  // Hold to talk: press, speak, release. The keyboard (a click without a
  // pointer) still toggles.
  const talkHandlers = hold
    ? {
        onPointerDown: (e: PointerEvent<HTMLButtonElement>) => {
          if ((e.button ?? 0) > 0) return;
          e.currentTarget.setPointerCapture?.(e.pointerId);
          void startTalk();
        },
        onPointerUp: () => void stopTalk(),
        onPointerCancel: cancelTalk,
        onClick: (e: MouseEvent<HTMLButtonElement>) => {
          if (e.detail === 0) toggleTalk();
        },
      }
    : { onClick: toggleTalk };

  const onMenuKey = (e: KeyboardEvent<HTMLDivElement>) => {
    const items = [...(menuRef.current?.querySelectorAll<HTMLElement>('[role^="menuitem"]') ?? [])];
    const at = items.indexOf(document.activeElement as HTMLElement);
    let next = -1;
    if (e.key === 'ArrowDown') next = (at + 1) % items.length;
    else if (e.key === 'ArrowUp') next = (at - 1 + items.length) % items.length;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = items.length - 1;
    else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      close();
      return;
    } else if (e.key === 'Tab') {
      close(false);
      return;
    }
    if (next < 0 || items.length === 0) return;
    e.preventDefault();
    items[next]!.focus();
  };

  const handsFree = (
    <button
      type="button"
      className={`icon-btn ${styles.btn}`}
      aria-pressed={settings.handsFree}
      aria-label={t('handsFree.label')}
      title={t('cc.control.handsFreeHint')}
      onClick={() => updateSettings({ handsFree: !settings.handsFree })}
      data-testid="cc-hands-free"
    >
      <Icon name="headset" size={20} />
    </button>
  );

  return (
    <div className={styles.control} ref={wrapRef} role="group" aria-label={t('cc.title')}>
      <button
        type="button"
        className={styles.open}
        onClick={() => onOpen?.(count > 0 ? 'approvals' : 'conversation')}
        aria-label={t('cc.control.open', { status, count })}
        title={t('cc.control.open', { status, count })}
        data-testid="cc-control-open"
      >
        <span className={styles.dot} data-status={state.status} aria-hidden="true" />
        <Icon name="conductor" size={20} />
        {count > 0 && (
          <span className={`badge ${styles.badge}`} aria-hidden="true">
            {count}
          </span>
        )}
      </button>
      <button
        type="button"
        className={`icon-btn ${styles.btn}`}
        disabled={(!usable && mic === 'idle') || mic === 'transcribing'}
        aria-pressed={mic === 'recording' || mic === 'starting'}
        aria-label={
          mic === 'recording' || mic === 'starting'
            ? t('cc.control.talkStop')
            : mic === 'transcribing'
              ? t('conductor.transcribing')
              : t('cc.control.talk')
        }
        title={t('cc.control.talkHint')}
        data-mode={settings.micMode}
        {...talkHandlers}
        data-testid="cc-talk"
      >
        <Icon name={mic === 'idle' || mic === 'transcribing' ? 'mic' : 'stop'} size={20} />
      </button>
      {talking && (
        <button
          type="button"
          className={`icon-btn ${styles.btn}`}
          aria-label={t('composer.micCancel')}
          title={t('composer.micCancel')}
          onClick={cancelTalk}
          data-testid="cc-talk-cancel"
        >
          <Icon name="close" size={20} />
        </button>
      )}
      {wide && showHandsFree && handsFree}
      <button
        ref={triggerRef}
        type="button"
        className={`icon-btn ${styles.btn}`}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        aria-label={t('cc.control.stopMenu')}
        title={t('cc.control.stopMenu')}
        onClick={() => setOpen((v) => !v)}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown' && !open) {
            e.preventDefault();
            setOpen(true);
          }
        }}
        data-testid="cc-stop-menu"
      >
        <Icon name="power" size={20} />
      </button>
      {open && (
        <div
          ref={menuRef}
          id={menuId}
          className={styles.menu}
          role="menu"
          aria-label={t('cc.control.stopMenu')}
          onKeyDown={onMenuKey}
        >
          <p className={styles.menuStatus} role="presentation">
            {t('cc.control.status', { status })}
          </p>
          {!wide && showHandsFree && (
            <button
              type="button"
              role="menuitemcheckbox"
              aria-checked={settings.handsFree}
              className={styles.item}
              tabIndex={-1}
              onClick={() => updateSettings({ handsFree: !settings.handsFree })}
            >
              <Icon name="headset" size={18} />
              <span className={styles.itemText}>
                <span>{t('handsFree.label')}</span>
                <span className={styles.itemHelp}>{t('cc.control.handsFreeHint')}</span>
              </span>
            </button>
          )}
          {!stopped && !paused && (
            <button
              type="button"
              role="menuitem"
              className={styles.item}
              tabIndex={-1}
              onClick={pause}
            >
              <Icon name="pause" size={18} />
              <span className={styles.itemText}>
                <span>{t('cc.control.pause')}</span>
                <span className={styles.itemHelp}>
                  {t(ccPause ? 'cc.control.pauseCcHelp' : 'cc.control.pauseHelp')}
                </span>
              </span>
            </button>
          )}
          {paused && (
            <button
              type="button"
              role="menuitem"
              className={styles.item}
              tabIndex={-1}
              onClick={unpause}
            >
              <Icon name="play" size={18} />
              <span className={styles.itemText}>
                <span>{t('cc.control.resumeCc')}</span>
                <span className={styles.itemHelp}>{t('cc.control.resumeCcHelp')}</span>
              </span>
            </button>
          )}
          {ccPause && !stopped && (
            <button
              type="button"
              role="menuitem"
              className={styles.item}
              tabIndex={-1}
              onClick={stop}
            >
              <Icon name="stop" size={18} />
              <span className={styles.itemText}>
                <span>{t('cc.control.stop')}</span>
                <span className={styles.itemHelp}>{t('cc.control.stopHelp')}</span>
              </span>
            </button>
          )}
          {stopped && local && (
            <button
              type="button"
              role="menuitem"
              className={styles.item}
              tabIndex={-1}
              onClick={resume}
            >
              <Icon name="play" size={18} />
              <span className={styles.itemText}>
                <span>{t('cc.control.resume')}</span>
                <span className={styles.itemHelp}>{t('cc.control.resumeHelp')}</span>
              </span>
            </button>
          )}
          {stopped && !local && (
            <p className={styles.menuStatus} role="presentation">
              {t('cc.stopped.remote')}
            </p>
          )}
          {state.status !== 'revoked' && (
            <button
              type="button"
              role="menuitem"
              className={`${styles.item} ${styles.danger}`}
              tabIndex={-1}
              onClick={askKill}
            >
              <Icon name="power" size={18} />
              <span className={styles.itemText}>
                <span>{t('conductor.kill')}</span>
                <span className={styles.itemHelp}>{t('conductor.killHelp')}</span>
              </span>
            </button>
          )}
        </div>
      )}
      {confirmKill && (
        <ConfirmDialog
          title={t('conductor.kill')}
          message={t('conductor.killConfirm')}
          confirmLabel={t('conductor.kill')}
          danger
          onCancel={() => setConfirmKill(false)}
          onConfirm={kill}
        />
      )}
      {killCode && (
        <TotpPrompt
          title={t('conductor.kill')}
          description={t('cc.kill.codeDescription')}
          submitLabel={t('conductor.kill')}
          onSubmit={async (code) => {
            await actions.conductor({ type: 'conductor_stop', kill: true, code });
            setKillCode(false);
          }}
          onCancel={() => setKillCode(false)}
          onFail={(err) => {
            setKillCode(false);
            fail(err);
          }}
          describeError={(err) => killErrorText(t, err)}
        />
      )}
    </div>
  );
}
