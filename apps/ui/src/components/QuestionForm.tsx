import { useEffect, useId, useState } from 'react';

import { useI18n } from '../i18n/context';
import type { Decision, Question, SessionInfo } from '../protocol/types';
import { RequestError } from '../state/core';
import { useActions } from '../state/store';
import {
  EMPTY_DRAFT as EMPTY,
  answered,
  splitRecommended,
  toAnswer,
  type Draft,
} from './questionDraft';
import styles from './QuestionForm.module.css';

/**
 * How long to wait for the form to clear after the answers were sent:
 * the server types them key by key, then waits for the agent to take them
 * (up to 15 s where it cannot read the terminal's screen).
 */
function confirmWaitMs(questionCount: number): number {
  return 25_000 + questionCount * 2_000;
}

/**
 * Where an answer stands: being written, sent but not confirmed (the
 * server says why), or not known to have reached the PC at all (the
 * connection dropped). A retry is always safe: the PC types into a form
 * once and refuses an answer for a form that is gone.
 */
type Phase = 'edit' | 'sending' | 'unconfirmed' | 'unsent';

/**
 * A question form the agent asks (Claude Code's `AskUserQuestion`): one
 * card per question with its options (check boxes for multiple choice), a
 * "type your own answer" field where the form has one, and Submit once
 * every question is answered. The server types the answers into the
 * agent's own form; if the form does not clear, the person is sent to the
 * terminal.
 */
export function QuestionForm({
  session,
  decision,
  compact = false,
}: {
  session: SessionInfo;
  decision: Decision;
  /** Smaller type for the Alerts list. */
  compact?: boolean;
}) {
  const { t } = useI18n();
  const actions = useActions();
  const baseId = useId();
  const questions = decision.questions ?? [];
  const [drafts, setDrafts] = useState<Draft[]>(() => questions.map(() => EMPTY));
  const [shownId, setShownId] = useState(decision.id);
  const [phase, setPhase] = useState<Phase>('edit');
  const [error, setError] = useState<string | null>(null);
  // A new form replaces the drafts.
  if (shownId !== decision.id) {
    setShownId(decision.id);
    setDrafts(questions.map(() => EMPTY));
    setPhase('edit');
    setError(null);
  }

  // Sent and still pending after a while: the agent did not take it.
  useEffect(() => {
    if (phase !== 'sending') return;
    const timer = setTimeout(() => setPhase('unconfirmed'), confirmWaitMs(questions.length));
    return () => clearTimeout(timer);
  }, [phase, questions.length]);

  const update = (i: number, change: (d: Draft) => Draft) => {
    setDrafts((all) => all.map((d, j) => (j === i ? change(d) : d)));
    if (phase === 'unconfirmed' || phase === 'unsent') setPhase('edit');
  };
  const pick = (i: number, q: Question, option: number) =>
    update(i, (d) =>
      q.multi_select
        ? {
            ...d,
            options: d.options.includes(option)
              ? d.options.filter((o) => o !== option)
              : [...d.options, option],
          }
        : // Single choice: an option replaces typed text.
          { options: [option], text: '' },
    );
  const type = (i: number, q: Question, text: string) =>
    update(i, (d) => (q.multi_select ? { ...d, text } : { options: [], text }));

  const done = questions.filter((q, i) => answered(q, drafts[i] ?? EMPTY)).length;
  const complete = done === questions.length && questions.length > 0;
  const busy = phase === 'sending';

  const submit = async () => {
    if (!complete || busy) return;
    setError(null);
    setPhase('sending');
    try {
      await actions.answer(
        session.id,
        decision.id,
        questions.map((q, i) => toAnswer(q, drafts[i] ?? EMPTY)),
      );
    } catch (err) {
      // No reply: the connection dropped before or while sending. The
      // drafts stay for "Send again".
      setPhase(err instanceof RequestError && err.code === 'transport' ? 'unsent' : 'unconfirmed');
      setError(t('error.request', { error: (err as Error).message }));
    }
  };

  return (
    <form
      className={styles.form}
      data-compact={compact}
      data-testid="question-form"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      {questions.map((q, i) => {
        const draft = drafts[i] ?? EMPTY;
        const groupId = `${baseId}-q${i}`;
        return (
          <fieldset
            key={`${i}-${q.question}`}
            className={styles.card}
            data-testid="question-card"
            aria-labelledby={`${groupId}-text`}
            disabled={busy}
          >
            <div className={styles.cardHead}>
              <span className={styles.header}>
                {q.header || t('question.number', { n: i + 1 })}
              </span>
              {answered(q, draft) && (
                <span className={styles.check} aria-hidden="true">
                  ✓
                </span>
              )}
            </div>
            <p id={`${groupId}-text`} className={styles.question}>
              {q.question}
            </p>
            {q.multi_select && <p className={styles.hint}>{t('question.multiHint')}</p>}
            <div className={styles.options} role={q.multi_select ? 'group' : 'radiogroup'}>
              {q.options.map((o, j) => {
                const { label, recommended } = splitRecommended(o.label);
                const on = draft.options.includes(j);
                return (
                  <button
                    key={j}
                    type="button"
                    role={q.multi_select ? 'checkbox' : 'radio'}
                    aria-checked={on}
                    className={styles.option}
                    data-selected={on}
                    onClick={() => pick(i, q, j)}
                  >
                    <span className={styles.mark} aria-hidden="true">
                      {q.multi_select ? (on ? '☑' : '☐') : on ? '◉' : '○'}
                    </span>
                    <span className={styles.optionText}>
                      <span className={styles.optionLabel}>
                        {label}
                        {recommended && (
                          <span className={styles.recommended} data-testid="question-recommended">
                            {t('question.recommended')}
                          </span>
                        )}
                      </span>
                      {o.description && <span className={styles.description}>{o.description}</span>}
                    </span>
                  </button>
                );
              })}
            </div>
            {q.free_text && (
              <label className={styles.other}>
                <span className={styles.otherLabel}>{t('question.other')}</span>
                <input
                  type="text"
                  value={draft.text}
                  maxLength={2000}
                  placeholder={t('question.otherPlaceholder')}
                  onChange={(e) => type(i, q, e.target.value)}
                />
              </label>
            )}
          </fieldset>
        );
      })}
      <div className={styles.footer}>
        <span className={styles.progress} aria-live="polite">
          {t('question.progress', { answered: done, total: questions.length })}
        </span>
        {complete && (
          <button type="submit" className="btn btn-primary" disabled={busy}>
            {busy
              ? t('question.sending')
              : phase === 'unsent'
                ? t('question.retry')
                : t('question.submit')}
          </button>
        )}
      </div>
      {phase === 'unconfirmed' && (
        <p role="alert" className={styles.error} data-testid="question-check-terminal">
          {t('question.checkTerminal')}
          {error && <span className={styles.errorDetail}> {error}</span>}
        </p>
      )}
      {phase === 'unsent' && (
        <p role="alert" className={styles.error} data-testid="question-unsent">
          {t('question.unsent')}
          {error && <span className={styles.errorDetail}> {error}</span>}
        </p>
      )}
    </form>
  );
}
