import { useEffect, useId, useState, type FormEvent } from 'react';

import type { MessageKey } from '../i18n/i18n';
import { useI18n } from '../i18n/context';
import {
  MAX_PERSONAL_TERM_CHARS,
  MAX_PERSONAL_TERMS,
  MAX_PERSONAL_VARIANTS,
  splitForms,
  wordProblem,
  type PersonalTerm,
  type WordProblem,
} from '../protocol/personalTerms';
import { useActions } from '../state/store';
import { Icon } from './Icon';
import styles from './PronunciationSettings.module.css';
import { useDeviceScope } from './ShellProvider';

const PROBLEM_TEXT: Record<WordProblem['rule'], MessageKey> = {
  empty: 'misheard.problem.empty',
  tooLong: 'misheard.problem.tooLong',
  tooManyForms: 'misheard.problem.tooManyForms',
  duplicateWord: 'misheard.problem.duplicateWord',
  formTooShort: 'misheard.problem.formTooShort',
  formIsWord: 'misheard.problem.formIsWord',
  duplicateForm: 'misheard.problem.duplicateForm',
};

/** The form of one word. `onSave` rejects with the reason it was not stored. */
function WordEditor({
  initial,
  others,
  onSave,
  onCancel,
}: {
  initial: PersonalTerm | null;
  /** The rest of the list. */
  others: PersonalTerm[];
  onSave: (word: PersonalTerm) => Promise<void>;
  onCancel: () => void;
}) {
  const { t } = useI18n();
  const id = useId();
  const [term, setTerm] = useState(initial?.term ?? '');
  const [forms, setForms] = useState(initial?.variants.join(', ') ?? '');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const save = async (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    const word = { term: term.trim(), variants: splitForms(forms) };
    const problem = wordProblem(word, others);
    if (problem) {
      setError(
        t(PROBLEM_TEXT[problem.rule], {
          text: 'text' in problem ? problem.text : '',
          chars: MAX_PERSONAL_TERM_CHARS,
          max: MAX_PERSONAL_VARIANTS,
        }),
      );
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await onSave(word);
    } catch (err) {
      setError(t('misheard.error', { error: (err as Error).message }));
      setBusy(false);
    }
  };

  return (
    <form className={styles.editor} onSubmit={(e) => void save(e)}>
      <label htmlFor={`${id}-term`}>{t('misheard.word')}</label>
      <input
        id={`${id}-term`}
        value={term}
        onChange={(e) => setTerm(e.target.value)}
        autoComplete="off"
        spellCheck={false}
      />
      <label htmlFor={`${id}-forms`}>{t('misheard.forms')}</label>
      <input
        id={`${id}-forms`}
        value={forms}
        onChange={(e) => setForms(e.target.value)}
        aria-describedby={`${id}-forms-help`}
        autoComplete="off"
        spellCheck={false}
      />
      <p id={`${id}-forms-help`} className={styles.hint}>
        {t('misheard.formsHelp')}
      </p>
      {error && (
        <p role="alert" className={styles.problem}>
          {error}
        </p>
      )}
      <div className={styles.actions}>
        <button type="button" className="btn btn-sm" onClick={onCancel}>
          {t('misheard.cancel')}
        </button>
        <button type="submit" className="btn btn-sm btn-primary" disabled={busy}>
          {t('misheard.save')}
        </button>
      </div>
    </form>
  );
}

/**
 * Settings -> Voice -> "Commonly misheard words": each correct word with what
 * the recognizer writes instead. The list lives on the PC and applies to the
 * next dictation. Only shown when the server has the feature; saving needs
 * the admin scope, so other devices get a read-only list. Keyed by machine in
 * Settings: another PC has another list.
 */
export function PersonalTermsSettings() {
  const { t } = useI18n();
  const actions = useActions();
  const scope = useDeviceScope();
  const canEdit = scope === null || scope === 'admin';
  const [terms, setTerms] = useState<PersonalTerm[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** The word being changed, or `new` for the add form. */
  const [editing, setEditing] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    actions
      .personalTerms({ type: 'list_personal_terms' })
      .then((list) => {
        if (live) setTerms(list);
      })
      .catch((err: Error) => {
        if (live) setError(err.message);
      });
    return () => {
      live = false;
    };
  }, [actions]);

  const store = async (next: PersonalTerm[]) => {
    setTerms(await actions.personalTerms({ type: 'save_personal_terms', terms: next }));
    setEditing(null);
  };

  const remove = async (term: string) => {
    if (!terms) return;
    setError(null);
    try {
      await store(terms.filter((w) => w.term !== term));
    } catch (err) {
      setError((err as Error).message);
    }
  };

  const others = (term: string | null) => (terms ?? []).filter((w) => w.term !== term);
  const full = (terms?.length ?? 0) >= MAX_PERSONAL_TERMS;

  return (
    <div data-testid="misheard-words">
      <p className={styles.hint}>{t('misheard.help')}</p>
      {!canEdit && <p className={styles.hint}>{t('misheard.needsAdmin')}</p>}
      {canEdit && (
        <div className={styles.toolbar}>
          <button
            type="button"
            className="btn btn-sm"
            disabled={terms === null || full || editing === 'new'}
            onClick={() => setEditing('new')}
          >
            <Icon name="plus" size={16} />
            {t('misheard.add')}
          </button>
        </div>
      )}
      {canEdit && full && (
        <p className={styles.hint}>{t('misheard.full', { max: MAX_PERSONAL_TERMS })}</p>
      )}
      {editing === 'new' && (
        <WordEditor
          initial={null}
          others={others(null)}
          onSave={(word) => store([...(terms ?? []), word])}
          onCancel={() => setEditing(null)}
        />
      )}
      {error && (
        <p role="alert" className={styles.problem}>
          {t('misheard.error', { error })}
        </p>
      )}
      {terms === null ? (
        !error && <p className="muted">{t('misheard.loading')}</p>
      ) : terms.length === 0 ? (
        <p className="muted">{t('misheard.empty')}</p>
      ) : (
        <ul className={styles.list}>
          {terms.map((word) => (
            <li key={word.term} className={styles.item}>
              {editing === word.term ? (
                <WordEditor
                  initial={word}
                  others={others(word.term)}
                  onSave={(next) => store(terms.map((w) => (w === word ? next : w)))}
                  onCancel={() => setEditing(null)}
                />
              ) : (
                <div className={styles.row}>
                  <span className={styles.term}>{word.term}</span>
                  {word.variants.length > 0 && (
                    <span className={styles.spoken}>{word.variants.join(', ')}</span>
                  )}
                  {canEdit && (
                    <span className={styles.buttons}>
                      <button
                        type="button"
                        className="icon-btn"
                        aria-label={t('misheard.editOf', { term: word.term })}
                        title={t('misheard.edit')}
                        onClick={() => setEditing(word.term)}
                      >
                        <Icon name="edit" size={16} />
                      </button>
                      <button
                        type="button"
                        className="icon-btn"
                        aria-label={t('misheard.deleteOf', { term: word.term })}
                        title={t('misheard.delete')}
                        onClick={() => void remove(word.term)}
                      >
                        <Icon name="close" size={16} />
                      </button>
                    </span>
                  )}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
