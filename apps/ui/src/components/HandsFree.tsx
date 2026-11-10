/**
 * Hands-free mode controls: the composer toggle, the Settings switch, the
 * one-time "command text is read aloud" note and the visual state chips
 * (듣는 중 / 확인 대기 / 보냄 / 취소됨 / 보내지 않음, and the spoken-decision
 * states).
 */
import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import type { DecisionVoiceStatus, SendStatus } from '../speech/handsFree';
import { useVocoSupported } from '../speech/useVoco';
import type { VocoView } from '../speech/vocoFlow';
import type { VocoConfirm } from '../state/settings';
import { useSettings } from '../state/store';
import { Icon } from './Icon';
import styles from './HandsFree.module.css';

/**
 * The one-time note "명령 내용이 소리로 읽힙니다": shown while hands-free
 * mode is on until it is acknowledged once (on this device).
 */
export function HandsFreeNote() {
  const { t } = useI18n();
  const { settings, updateSettings } = useSettings();
  if (!settings.handsFree || settings.handsFreeNoteSeen) return null;
  return (
    <div className={styles.note} role="note" data-testid="hands-free-note">
      <Icon name="info" size={14} />
      <span>{t('handsFree.note')}</span>
      <button
        type="button"
        className="btn btn-sm"
        onClick={() => updateSettings({ handsFreeNoteSeen: true })}
      >
        {t('handsFree.noteOk')}
      </button>
    </div>
  );
}

/** The composer's hands-free toggle. */
export function HandsFreeToggle({ disabled }: { disabled?: boolean }) {
  const { t } = useI18n();
  const { settings, updateSettings } = useSettings();
  return (
    <button
      type="button"
      className={styles.toggle}
      aria-pressed={settings.handsFree}
      aria-label={t('handsFree.label')}
      title={t('handsFree.label')}
      disabled={disabled}
      onClick={() => updateSettings({ handsFree: !settings.handsFree })}
    >
      <Icon name="mic" size={16} />
      <span>{t('handsFree.toggle')}</span>
    </button>
  );
}

/** Settings: the hands-free switch with its safety rules. */
export function HandsFreeSettings() {
  const { t } = useI18n();
  const { settings, updateSettings } = useSettings();
  return (
    <div className="field">
      <label className="check">
        <input
          type="checkbox"
          checked={settings.handsFree}
          aria-describedby="set-hands-free-help"
          onChange={(e) => updateSettings({ handsFree: e.target.checked })}
        />
        {t('handsFree.label')}
      </label>
      <p id="set-hands-free-help" className={`muted ${styles.help}`}>
        {t('handsFree.help')}
      </p>
      <label className="check">
        <input
          type="checkbox"
          checked={settings.handsFreeFullReadBack}
          disabled={!settings.handsFree}
          onChange={(e) => updateSettings({ handsFreeFullReadBack: e.target.checked })}
        />
        {t('handsFree.fullReadBack')}
      </label>
      <VocoSettings />
      <HandsFreeNote />
    </div>
  );
}

/** Settings of the wake word flow ("보코 … 오버"). */
function VocoSettings() {
  const { t } = useI18n();
  const { settings, updateSettings } = useSettings();
  const supported = useVocoSupported();
  const off = !settings.handsFree;
  const wakeOff = off || !settings.vocoWake;
  return (
    <fieldset className={styles.voco} disabled={off}>
      <label className="check">
        <input
          type="checkbox"
          checked={settings.vocoWake}
          aria-describedby="set-voco-help"
          onChange={(e) => updateSettings({ vocoWake: e.target.checked })}
        />
        {t('handsFree.voco.label')}
      </label>
      <p id="set-voco-help" className={`muted ${styles.help}`}>
        {t('handsFree.voco.help')}
      </p>
      {!supported && (
        <p className={`muted ${styles.help}`} data-testid="voco-unsupported">
          {t('handsFree.voco.unsupported')}
        </p>
      )}
      <div className="field">
        <label htmlFor="set-voco-wake">{t('handsFree.voco.wakeWords')}</label>
        <input
          id="set-voco-wake"
          type="text"
          value={settings.vocoWakeWords}
          placeholder={t('handsFree.voco.wakeWordsPlaceholder')}
          disabled={wakeOff}
          onChange={(e) => updateSettings({ vocoWakeWords: e.target.value })}
        />
      </div>
      <div className="field">
        <label htmlFor="set-voco-send">{t('handsFree.voco.sendPhrases')}</label>
        <input
          id="set-voco-send"
          type="text"
          value={settings.vocoSendPhrases}
          placeholder={t('handsFree.voco.sendPhrasesPlaceholder')}
          disabled={wakeOff}
          onChange={(e) => updateSettings({ vocoSendPhrases: e.target.value })}
        />
      </div>
      <div className="field">
        <label htmlFor="set-voco-confirm">{t('handsFree.voco.confirm')}</label>
        <select
          id="set-voco-confirm"
          value={settings.vocoConfirm}
          disabled={wakeOff}
          onChange={(e) => updateSettings({ vocoConfirm: e.target.value as VocoConfirm })}
        >
          <option value="undo">{t('handsFree.voco.confirm.undo')}</option>
          <option value="readback">{t('handsFree.voco.confirm.readback')}</option>
        </select>
      </div>
      <label className="check">
        <input
          type="checkbox"
          checked={settings.vocoAutoSend}
          disabled={wakeOff}
          onChange={(e) => updateSettings({ vocoAutoSend: e.target.checked })}
        />
        {t('handsFree.voco.autoSend')}
      </label>
      <div className="field">
        <label htmlFor="set-voco-countdown">
          {t('handsFree.voco.countdown', { seconds: settings.vocoCountdownSec })}
        </label>
        <input
          id="set-voco-countdown"
          type="range"
          min={2}
          max={5}
          step={1}
          value={settings.vocoCountdownSec}
          disabled={wakeOff || !settings.vocoAutoSend || settings.vocoConfirm !== 'undo'}
          onChange={(e) => updateSettings({ vocoCountdownSec: Number(e.target.value) })}
        />
      </div>
    </fieldset>
  );
}

const SEND_LABELS: Record<SendStatus, MessageKey> = {
  confirming: 'handsFree.status.confirming',
  listening: 'handsFree.status.listening',
  sent: 'handsFree.status.sent',
  cancelled: 'handsFree.status.cancelled',
  kept: 'handsFree.status.kept',
};

const DECISION_LABELS: Record<DecisionVoiceStatus, MessageKey> = {
  checking: 'handsFree.decision.checking',
  reading: 'handsFree.status.confirming',
  listening: 'handsFree.decision.listening',
  approved: 'handsFree.decision.approved',
  denied: 'handsFree.decision.denied',
  screen: 'handsFree.decision.screen',
  changed: 'handsFree.decision.changed',
  stopped: 'handsFree.decision.stopped',
  failed: 'handsFree.decision.failed',
};

/** Tone of a state: listening (live mic), done, or "look at the screen". */
function toneOf(status: SendStatus | DecisionVoiceStatus): string {
  if (status === 'listening') return 'live';
  if (status === 'sent' || status === 'approved' || status === 'denied') return 'done';
  if (status === 'confirming' || status === 'reading' || status === 'checking') return 'wait';
  return 'stop';
}

/** The read-back state in the composer. */
export function HandsFreeStatus({ status }: { status: SendStatus | null }) {
  const { t } = useI18n();
  if (!status) return null;
  return (
    <span
      className={`${styles.chip} ${styles.line}`}
      role="status"
      data-testid="hands-free-status"
      data-state={status}
      data-tone={toneOf(status)}
      title={t(SEND_LABELS[status])}
    >
      {status === 'listening' && <span className={styles.dot} aria-hidden="true" />}
      <span className={styles.chipText}>{t(SEND_LABELS[status])}</span>
    </span>
  );
}

/** The wake word flow in the composer: 대기, 듣는 중…, the send countdown, the result. */
export function VocoStatus({ view }: { view: VocoView | null }) {
  const { t } = useI18n();
  if (!view) return null;
  let label: string;
  let tone: string;
  if (view.result === 'queued') {
    label = t('handsFree.voco.queued');
    tone = 'wait';
  } else if (view.result === 'missed') {
    label = t('handsFree.voco.missed');
    tone = 'stop';
  } else if (view.result) {
    label = t(SEND_LABELS[view.result]);
    tone = view.result === 'sent' ? 'done' : 'stop';
  } else if (view.phase === 'awake') {
    label = t('handsFree.voco.awake');
    tone = 'live';
  } else if (view.phase === 'countdown') {
    label = t('handsFree.voco.countdown.status', { seconds: view.seconds ?? 0 });
    tone = 'wait';
  } else if (view.phase === 'undo') {
    label = t('handsFree.voco.undo.status', { seconds: view.seconds ?? 0 });
    tone = 'wait';
  } else if (view.hearing) {
    // Speech is coming in: it may be the call (instant feedback).
    label = t('handsFree.voco.hearing');
    tone = 'hear';
  } else {
    label = t('handsFree.voco.waiting');
    tone = 'idle';
  }
  const icon =
    view.result === 'queued'
      ? 'upload'
      : tone === 'wait'
        ? 'clock'
        : tone === 'done'
          ? 'check'
          : 'mic';
  return (
    <span
      className={`${styles.chip} ${styles.line}`}
      role="status"
      data-testid="voco-status"
      data-state={view.result ?? view.phase}
      data-tone={tone}
      data-hearing={!!view.hearing && view.phase === 'waiting'}
      title={label}
    >
      {tone === 'live' || tone === 'hear' ? (
        <span className={styles.dot} aria-hidden="true" />
      ) : (
        <Icon name={icon} size={14} className={styles.chipIcon} />
      )}
      <span className={styles.chipText}>{label}</span>
    </span>
  );
}

/** The spoken-decision state on the decision card. */
export function DecisionVoiceStatusChip({ status }: { status: DecisionVoiceStatus | null }) {
  const { t } = useI18n();
  if (!status) return null;
  return (
    <p
      className={styles.chip}
      role="status"
      data-testid="decision-voice-status"
      data-state={status}
      data-tone={toneOf(status)}
    >
      {status === 'listening' && <span className={styles.dot} aria-hidden="true" />}
      {t(DECISION_LABELS[status])}
    </p>
  );
}
