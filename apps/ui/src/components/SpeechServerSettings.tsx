import { useEffect, useState } from 'react';

import { useI18n } from '../i18n/context';
import { SPEECH_LLM_NONE, type SpeechProfileInfo } from '../protocol/types';
import { useActions, useAppState, type SpeechOptions } from '../state/store';
import styles from './SettingsView.module.css';

/**
 * Server-side speech choices of this device (Settings → Speech recognition):
 * the speech engine and the transcript-correction LLM, picked from the
 * profiles configured on the PC. Profiles whose key is missing are shown
 * disabled with the command that adds it. Keys are never entered here. A
 * choice is saved as soon as it is picked, like the other settings.
 */
export function SpeechServerSettings() {
  const { t } = useI18n();
  const actions = useActions();
  const connected = useAppState((s) => s.connection === 'connected');
  const [options, setOptions] = useState<SpeechOptions | null>(null);
  const [engine, setEngine] = useState('');
  const [llm, setLlm] = useState(SPEECH_LLM_NONE);
  const [status, setStatus] = useState<'idle' | 'saving' | 'saved'>('idle');
  const [error, setError] = useState<string | null>(null);

  const apply = (o: SpeechOptions) => {
    setOptions(o);
    setEngine(o.current_engine ?? '');
    setLlm(o.current_llm ?? SPEECH_LLM_NONE);
  };

  useEffect(() => {
    if (!connected) return;
    let active = true;
    actions.listSpeechOptions().then(
      (o) => {
        if (active) apply(o);
      },
      () => {
        // Older servers do not know the request; the section stays hidden.
      },
    );
    return () => {
      active = false;
    };
  }, [actions, connected]);

  if (!options || options.engines.length === 0) return null;

  const save = (nextEngine: string, nextLlm: string) => {
    setEngine(nextEngine);
    setLlm(nextLlm);
    setStatus('saving');
    setError(null);
    actions.setSpeechPreferences(nextEngine || null, nextLlm).then(
      (o) => {
        apply(o);
        setStatus('saved');
      },
      (err: Error) => {
        // Back to what the PC still uses.
        apply(options);
        setStatus('idle');
        setError(t('error.request', { error: err.message }));
      },
    );
  };

  const optionLabel = (p: SpeechProfileInfo) => {
    const notes = [
      p.default ? t('settings.speechDefault') : null,
      p.configured ? null : t('settings.speechNotConfigured'),
    ].filter(Boolean);
    return notes.length ? `${p.label} (${notes.join(', ')})` : p.label;
  };

  const hints = (list: SpeechProfileInfo[]) =>
    list
      .filter((p) => !p.configured)
      .map((p) => (
        <p key={p.id} className="muted" data-testid={`speech-hint-${p.id}`}>
          {t('settings.speechLoginHint', { label: p.label, id: p.id })}
        </p>
      ));

  return (
    <div className={styles.speechServer} data-testid="speech-server-settings">
      <div className="field">
        <label htmlFor="set-speech-engine">{t('settings.speechEngine')}</label>
        <select
          id="set-speech-engine"
          value={engine}
          disabled={status === 'saving'}
          onChange={(e) => save(e.target.value, llm)}
        >
          {options.engines.map((p) => (
            <option key={p.id} value={p.id} disabled={!p.configured}>
              {optionLabel(p)}
            </option>
          ))}
        </select>
        {hints(options.engines)}
      </div>
      <div className="field">
        <label htmlFor="set-speech-llm">{t('settings.speechLlm')}</label>
        <select
          id="set-speech-llm"
          value={llm}
          disabled={status === 'saving'}
          onChange={(e) => save(engine, e.target.value)}
        >
          <option value={SPEECH_LLM_NONE}>{t('settings.speechLlmOff')}</option>
          {options.llms.map((p) => (
            <option key={p.id} value={p.id} disabled={!p.configured}>
              {optionLabel(p)}
            </option>
          ))}
        </select>
        <p className="muted">{t('settings.speechLlmHelp')}</p>
        {hints(options.llms)}
      </div>
      <div className={styles.speechActions}>
        <span role="status" aria-live="polite" className="muted">
          {status === 'saved' ? t('settings.speechSaved') : ''}
        </span>
      </div>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
    </div>
  );
}
