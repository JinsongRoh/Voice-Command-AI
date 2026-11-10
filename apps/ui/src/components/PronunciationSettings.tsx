import {
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type FormEvent,
} from 'react';

import { useI18n } from '../i18n/context';
import type { Pronunciation } from '../protocol/pronunciations';
import { useSpeech } from '../speech/context';
import { previewVoice } from '../speech/handsFree';
import type { Recorder } from '../speech/recorder';
import {
  MAX_SPOKEN_CHARS,
  MAX_TERM_CHARS,
  getPronunciations,
  heardAsSpoken,
  latinPart,
  subscribePronunciations,
  termKey,
  validTerm,
} from '../speech/pronunciation';
import { speakableText } from '../speech/radioSpeech';
import { RequestError } from '../state/core';
import { useActions, useSettings } from '../state/store';
import { Icon } from './Icon';
import { Modal } from './Modal';
import styles from './PronunciationSettings.module.css';

function baseLang(tag: string): string {
  return tag.toLowerCase().split(/[-_]/)[0] ?? '';
}

/** The active entries of the selected PC (kept current by pushes). */
function usePronunciations(): readonly Pronunciation[] {
  return useSyncExternalStore(subscribePronunciations, getPronunciations, getPronunciations);
}

/**
 * Speaks text with the voice chosen for spoken alerts (device or PC engine),
 * through the app's one speaker, holding the voice like any spoken flow.
 */
function usePreview(): (text: string, lang: string) => void {
  const actions = useActions();
  const { settings } = useSettings();
  return (text, lang) => {
    void previewVoice((signal) =>
      actions.speakAlert(
        text,
        lang,
        {
          serverEngine: settings.ttsServerEngine,
          serverVoice: settings.ttsServerVoice,
          voice: settings.ttsVoice,
          rate: settings.ttsRate,
        },
        signal,
      ),
    );
  };
}

type MicState = 'idle' | 'recording' | 'transcribing';

/**
 * The form of one entry: the written term, how to say it (typed, or said
 * aloud with "Teach by voice"), a preview and Save.
 */
export function PronunciationEditor({
  initialTerm = '',
  initialSpoken = '',
  lockTerm = false,
  language,
  onDone,
  onCancel,
}: {
  initialTerm?: string;
  initialSpoken?: string;
  /** Editing an existing entry: the term stays. */
  lockTerm?: boolean;
  /** Voice language of the entry (base tag). */
  language: string;
  onDone: () => void;
  onCancel: () => void;
}) {
  const { t, locale } = useI18n();
  const actions = useActions();
  const speech = useSpeech();
  const { settings } = useSettings();
  const preview = usePreview();
  const id = useId();
  const [term, setTerm] = useState(initialTerm);
  const [spoken, setSpoken] = useState(initialSpoken);
  const [mic, setMic] = useState<MicState>('idle');
  const [heard, setHeard] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const recorderRef = useRef<Recorder | null>(null);
  useEffect(() => () => recorderRef.current?.cancel(), []);

  const termOk = validTerm(term);
  const spokenOk = spoken.trim().length > 0 && spoken.trim().length <= MAX_SPOKEN_CHARS;

  const save = async (e?: FormEvent) => {
    e?.preventDefault();
    if (!termOk || !spokenOk || busy) return;
    setBusy(true);
    setError(null);
    try {
      await actions.pronunciation({
        type: 'set_pronunciation',
        term: term.trim(),
        spoken: spoken.trim(),
        language,
      });
      onDone();
    } catch (err) {
      setError(t('pron.error', { error: (err as Error).message }));
      setBusy(false);
    }
  };

  /** "Teach by voice": the transcript is shown for review, never saved on its own. */
  const toggleMic = async () => {
    setError(null);
    if (mic === 'recording') {
      const recorder = recorderRef.current;
      recorderRef.current = null;
      if (!recorder) return;
      setMic('transcribing');
      try {
        const audio = await recorder.stop();
        const result = await speech.transcribe(audio, {
          locale,
          // The voice language, so the recognizer writes what it hears in Hangul.
          language,
          sessionId: null,
          final: true,
        });
        const said = heardAsSpoken(result.text, result.rawText);
        if (said) {
          setSpoken(said.slice(0, MAX_SPOKEN_CHARS));
          setHeard(said);
        } else {
          setHeard(null);
          setError(t('pron.heardNothing'));
        }
      } catch (err) {
        setError(t('pron.micError', { error: (err as Error).message }));
      } finally {
        setMic('idle');
      }
      return;
    }
    if (mic !== 'idle') return;
    const recorder = speech.createRecorder();
    try {
      await recorder.start();
      recorderRef.current = recorder;
      setHeard(null);
      setMic('recording');
    } catch (err) {
      setError(t('pron.micError', { error: (err as Error).message }));
    }
  };

  return (
    <form className={styles.editor} onSubmit={(e) => void save(e)} noValidate>
      <div className="field">
        <label htmlFor={`${id}-term`}>{t('pron.term')}</label>
        <input
          id={`${id}-term`}
          type="text"
          value={term}
          maxLength={MAX_TERM_CHARS}
          readOnly={lockTerm}
          onChange={(e) => setTerm(e.target.value)}
          placeholder="SkyHarbor"
          autoComplete="off"
          spellCheck={false}
          aria-invalid={term && !termOk ? true : undefined}
          aria-describedby={`${id}-term-hint`}
        />
        <span id={`${id}-term-hint`} className={term && !termOk ? styles.problem : styles.hint}>
          {term && !termOk ? t('pron.invalidTerm') : t('pron.termHint')}
        </span>
      </div>
      <div className="field">
        <label htmlFor={`${id}-spoken`}>{t('pron.spoken')}</label>
        <div className={styles.spokenRow}>
          <input
            id={`${id}-spoken`}
            type="text"
            value={spoken}
            maxLength={MAX_SPOKEN_CHARS}
            onChange={(e) => {
              setSpoken(e.target.value);
              setHeard(null);
            }}
            placeholder={t('pron.spokenPlaceholder')}
            autoComplete="off"
            spellCheck={false}
          />
          <button
            type="button"
            className="icon-btn"
            aria-label={t('pron.preview')}
            title={t('pron.preview')}
            disabled={!spokenOk}
            onClick={() => preview(spoken.trim(), settings.language)}
          >
            <Icon name="play" size={16} />
          </button>
        </div>
      </div>
      <button
        type="button"
        className={`btn btn-sm ${mic === 'recording' ? styles.recording : ''}`}
        aria-pressed={mic === 'recording'}
        disabled={mic === 'transcribing'}
        onClick={() => void toggleMic()}
      >
        <Icon name="mic" size={16} />
        {mic === 'recording'
          ? t('pron.stopTeaching')
          : mic === 'transcribing'
            ? t('pron.transcribing')
            : t('pron.teach')}
      </button>
      <p className={styles.hint} role="status" aria-live="polite">
        {mic === 'recording'
          ? t('pron.listening')
          : heard
            ? t('pron.heard', { text: heard })
            : t('pron.teachHint')}
      </p>
      {error && (
        <p role="alert" className={styles.problem}>
          {error}
        </p>
      )}
      <div className={styles.actions}>
        <button type="button" className="btn btn-sm" onClick={onCancel}>
          {t('common.cancel')}
        </button>
        <button
          type="submit"
          className="btn btn-primary btn-sm"
          disabled={!termOk || !spokenOk || busy}
        >
          {t('pron.save')}
        </button>
      </div>
    </form>
  );
}

/**
 * Settings → spoken alerts → pronunciation dictionary: the PC's entries of
 * the voice language with search, preview, edit and delete, and adding an
 * entry by typing or by voice.
 */
export function PronunciationSettings() {
  const { t } = useI18n();
  const actions = useActions();
  const { settings } = useSettings();
  const preview = usePreview();
  const entries = usePronunciations();
  const language = baseLang(settings.language);
  const [query, setQuery] = useState('');
  const [editing, setEditing] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [unsupported, setUnsupported] = useState(false);

  useEffect(() => {
    let active = true;
    actions.pronunciation({ type: 'list_pronunciations', language: null }).then(
      () => {
        if (active) setUnsupported(false);
      },
      (err: unknown) => {
        if (active && err instanceof RequestError && err.code === 'unsupported')
          setUnsupported(true);
      },
    );
    return () => {
      active = false;
    };
  }, [actions]);

  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    return entries
      .filter((e) => baseLang(e.language) === language)
      .filter(
        (e) =>
          !q ||
          e.term.toLowerCase().includes(q) ||
          e.spoken.toLowerCase().includes(q) ||
          termKey(e.term)
            .replace(/ /g, '')
            .includes(q.replace(/[\s_-]/g, '')),
      );
  }, [entries, language, query]);

  const remove = async (entry: Pronunciation) => {
    setError(null);
    try {
      await actions.pronunciation({
        type: 'delete_pronunciation',
        term: entry.term,
        language: entry.language,
      });
    } catch (err) {
      setError(t('pron.error', { error: (err as Error).message }));
    }
  };

  return (
    <details className={styles.box} data-testid="pronunciations">
      <summary>{t('pron.title')}</summary>
      <p className={styles.hint}>{t('pron.help')}</p>
      {unsupported ? (
        <p className="muted">{t('pron.unsupported')}</p>
      ) : (
        <>
          <div className={styles.toolbar}>
            <input
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={t('pron.search')}
              aria-label={t('pron.search')}
            />
            <button
              type="button"
              className="btn btn-sm"
              onClick={() => {
                setEditing(null);
                setAdding(true);
              }}
              disabled={adding}
            >
              <Icon name="plus" size={16} />
              {t('pron.add')}
            </button>
          </div>
          {adding && (
            <PronunciationEditor
              language={language}
              onDone={() => setAdding(false)}
              onCancel={() => setAdding(false)}
            />
          )}
          {error && (
            <p role="alert" className={styles.problem}>
              {error}
            </p>
          )}
          {shown.length === 0 ? (
            <p className="muted">{query.trim() ? t('pron.noMatch') : t('pron.empty')}</p>
          ) : (
            <ul className={styles.list}>
              {shown.map((e) => {
                const key = `${e.language}:${termKey(e.term)}`;
                return (
                  <li key={key} className={styles.item}>
                    {editing === key ? (
                      <PronunciationEditor
                        initialTerm={e.term}
                        initialSpoken={e.spoken}
                        lockTerm
                        language={e.language}
                        onDone={() => setEditing(null)}
                        onCancel={() => setEditing(null)}
                      />
                    ) : (
                      <div className={styles.row}>
                        <span className={styles.term}>{e.term}</span>
                        <span className={styles.arrow} aria-hidden="true">
                          →
                        </span>
                        <span className={styles.spoken}>{e.spoken}</span>
                        {e.source !== 'user' && (
                          <span className={`badge ${styles.source}`}>
                            {t(`pron.source.${e.source}`)}
                          </span>
                        )}
                        <span className={styles.buttons}>
                          <button
                            type="button"
                            className="icon-btn"
                            aria-label={t('pron.previewOf', { term: e.term })}
                            title={t('pron.preview')}
                            onClick={() => preview(e.spoken, settings.language)}
                          >
                            <Icon name="play" size={16} />
                          </button>
                          <button
                            type="button"
                            className="icon-btn"
                            aria-label={t('pron.editOf', { term: e.term })}
                            title={t('pron.edit')}
                            onClick={() => {
                              setAdding(false);
                              setEditing(key);
                            }}
                          >
                            <Icon name="edit" size={16} />
                          </button>
                          <button
                            type="button"
                            className="icon-btn"
                            aria-label={t('pron.deleteOf', { term: e.term })}
                            title={t('pron.delete')}
                            onClick={() => void remove(e)}
                          >
                            <Icon name="close" size={16} />
                          </button>
                        </span>
                      </div>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
        </>
      )}
    </details>
  );
}

/**
 * "Fix pronunciation" of a session card: the Latin words of its name, how
 * they are said now, and the editor.
 */
export function PronunciationDialog({ name, onClose }: { name: string; onClose: () => void }) {
  const { t } = useI18n();
  const { settings } = useSettings();
  const preview = usePreview();
  usePronunciations();
  const term = latinPart(name) || name;
  const language = baseLang(settings.language);
  const now = speakableText(name, settings.language);
  const known = getPronunciations().find(
    (e) => baseLang(e.language) === language && termKey(e.term) === termKey(term),
  );
  return (
    <Modal title={t('pron.fixTitle')} onClose={onClose}>
      <p className={styles.hint}>
        {t('pron.now', { text: now })}{' '}
        <button
          type="button"
          className="icon-btn"
          aria-label={t('pron.previewOf', { term: name })}
          title={t('pron.preview')}
          onClick={() => preview(name, settings.language)}
        >
          <Icon name="play" size={14} />
        </button>
      </p>
      <PronunciationEditor
        initialTerm={term}
        initialSpoken={known?.spoken ?? ''}
        language={language}
        onDone={onClose}
        onCancel={onClose}
      />
    </Modal>
  );
}
