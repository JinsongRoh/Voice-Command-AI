import { useEffect, useState, useSyncExternalStore } from 'react';

import { useMediaQuery } from '../hooks/useMediaQuery';
import { useI18n } from '../i18n/context';
import { voiceQueue } from '../speech/voiceQueue';
import { useActions, useSettings } from '../state/store';
import { Icon } from './Icon';

/** Phones get a one-time note on what will be read aloud. */
const PHONE_QUERY = '(max-width: 640px)';

/**
 * Quick switch for spoken alerts (the same setting as Settings >
 * Notifications). Turning it off silences the voice at once (the store
 * stops the voice queue on that change).
 */
export function VoiceToggle({ compact = false }: { compact?: boolean }) {
  const { t } = useI18n();
  const { settings, updateSettings } = useSettings();
  const actions = useActions();
  const phone = useMediaQuery(PHONE_QUERY);
  const [note, setNote] = useState(false);
  const on = settings.spokenAlerts;
  const toggle = () => {
    const next = !on;
    if (next && phone && !settings.spokenAlertsNoteSeen) {
      // The header has no room for a line: a notice says it there.
      if (compact) {
        actions.showToast({
          id: 'voice-note',
          category: 'activity',
          sessionId: null,
          title: t('settings.spoken'),
          body: t('voice.toggleNote'),
        });
      } else {
        setNote(true);
      }
      updateSettings({ spokenAlerts: true, spokenAlertsNoteSeen: true });
      return;
    }
    if (!next) setNote(false);
    updateSettings({ spokenAlerts: next });
  };
  return (
    <>
      <button
        type="button"
        className={compact ? 'icon-btn' : 'btn btn-sm'}
        aria-pressed={on}
        aria-label={t('voice.toggle')}
        title={t(on ? 'voice.toggleOff' : 'voice.toggleOn')}
        onClick={toggle}
      >
        <Icon name={on ? 'speaker' : 'speakerOff'} size={compact ? 20 : 14} />
        {!compact && ` ${t(on ? 'voice.toggleOff' : 'voice.toggleOn')}`}
      </button>
      {note && (
        <p role="status" className="muted">
          {t('voice.toggleNote')}{' '}
          <button type="button" className="btn btn-sm" onClick={() => setNote(false)}>
            {t('voice.noteOk')}
          </button>
        </p>
      )}
    </>
  );
}

/**
 * Shown while the voice queue speaks: a tap (or Esc) stops everything that
 * is playing or waiting.
 */
export function SpeakingIndicator() {
  const { t } = useI18n();
  const speaking = useSyncExternalStore(
    voiceQueue.subscribe,
    () => voiceQueue.speaking,
    () => false,
  );
  useEffect(() => {
    if (!speaking) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') voiceQueue.stopAll();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [speaking]);
  if (!speaking) return null;
  return (
    <button
      type="button"
      className="btn btn-sm"
      aria-label={t('voice.speaking')}
      title={t('voice.speaking')}
      onClick={() => voiceQueue.stopAll()}
    >
      <Icon name="speaker" size={14} />
      <span aria-hidden="true"> …</span>
    </button>
  );
}
