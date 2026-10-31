import { useState } from 'react';

import { useI18n } from '../i18n/context';
import type { Decision, SessionInfo } from '../protocol/types';
import { useActions } from '../state/store';
import styles from './DecisionSheet.module.css';

/** One-tap answer to a pending agent decision (NTF-05). */
export function DecisionSheet({
  session,
  decision,
}: {
  session: SessionInfo;
  decision: Decision | undefined;
}) {
  const { t } = useI18n();
  const actions = useActions();
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const answer = async (optionId: string) => {
    if (!decision) return;
    setBusy(optionId);
    setError(null);
    try {
      await actions.choose(session.id, decision.id, optionId);
    } catch (err) {
      setError(t('error.request', { error: (err as Error).message }));
    } finally {
      setBusy(null);
    }
  };

  const [headline, ...details] = (decision?.prompt ?? '').split('\n');

  return (
    <section
      className={styles.sheet}
      role="region"
      aria-label={t('decision.region', { name: session.display_name })}
      data-testid="decision-sheet"
    >
      <h3 className={styles.title}>{t('decision.title')}</h3>
      {decision ? (
        <>
          <p className={styles.prompt}>{headline}</p>
          {details.length > 0 && <pre className={styles.detail}>{details.join('\n')}</pre>}
          <div className={styles.options}>
            {decision.options.map((o, i) => (
              <button
                key={o.id}
                type="button"
                className={`btn ${i === 0 ? 'btn-primary' : ''}`}
                disabled={busy !== null}
                onClick={() => void answer(o.id)}
              >
                {o.label}
              </button>
            ))}
          </div>
          {error && (
            <p role="alert" className={styles.error}>
              {error}
            </p>
          )}
        </>
      ) : (
        <p className="muted">{t('decision.waiting')}</p>
      )}
    </section>
  );
}
