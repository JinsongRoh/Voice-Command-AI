import { useState } from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import { conductorSessionWait, requestTerminal } from '../state/conductor';
import { useActions, useAppState, useConductor } from '../state/store';
import styles from './Conductor.module.css';
import { isStepUpCanceled } from '../native/security';
import { useStepUp } from './StepUpPrompt';

/** Why a send waits for the person (`ConductorRequest.reason`). */
const NEEDS: Record<string, MessageKey> = {
  no_instruction: 'conductor.needs.no_instruction',
  not_named: 'conductor.needs.not_named',
  expired: 'conductor.needs.expired',
  too_long: 'conductor.needs.too_long',
  secret: 'conductor.needs.secret',
  risky_command: 'conductor.needs.risky_command',
  link: 'conductor.needs.link',
  copied_output: 'conductor.needs.copied_output',
  awaiting_decision: 'conductor.needs.awaiting_decision',
};

/**
 * Sends the conductor wants to make, each waiting for the person: "The
 * conductor wants to send this to X: …" with Allow / Deny. Shown at the top
 * of the Command Center and in Alerts; nothing when none waits.
 *
 * With `sessionWait` (Command Center), what the conductor's own session waits on
 * comes first: its first-run "trust this folder" question or a permission
 * decision. Alerts lists such a decision itself, as "Conductor session".
 */
export function ConductorRequests({
  heading = false,
  sessionWait = false,
  onOpenSession,
}: {
  heading?: boolean;
  sessionWait?: boolean;
  /** Opens a session (on its terminal with `terminal`). */
  onOpenSession?: (sessionId: string, terminal: boolean) => void;
}) {
  const { t, relative } = useI18n();
  const actions = useActions();
  const stepUp = useStepUp();
  const state = useConductor();
  const sessions = useAppState((s) => s.sessions);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const pending = state?.pending ?? [];
  const wait = sessionWait ? conductorSessionWait(state, sessions) : null;
  if (pending.length === 0 && !wait) return null;

  const decide = async (id: string, allow: boolean) => {
    setBusy(id);
    setError(null);
    try {
      // Only allowing needs the person; refusing never does.
      await stepUp.run('conductor_decide', () =>
        actions.conductor({ type: 'conductor_decide', request_id: id, allow }),
      );
    } catch (err) {
      if (!isStepUpCanceled(err)) setError(t('error.request', { error: (err as Error).message }));
    } finally {
      setBusy(null);
    }
  };

  const trust = async () => {
    setBusy('trust');
    setError(null);
    try {
      await actions.conductor({ type: 'conductor_trust_folder' });
    } catch (err) {
      setError(t('error.request', { error: (err as Error).message }));
    } finally {
      setBusy(null);
    }
  };

  const open = (sessionId: string, terminal: boolean) => {
    if (terminal) requestTerminal(sessionId);
    onOpenSession?.(sessionId, terminal);
  };

  return (
    <section className={`panel ${styles.section}`} aria-labelledby="conductor-requests">
      <h3 id="conductor-requests" className={heading ? undefined : 'visually-hidden'}>
        {t('conductor.alerts')}
      </h3>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
      <ul className={styles.requests}>
        {wait?.kind === 'trust' && (
          <li className={styles.request} data-testid="conductor-trust">
            <p className={styles.requestTitle}>{t('conductor.trustTitle')}</p>
            <p className={styles.help}>{t('conductor.trustHelp')}</p>
            <div className={styles.buttons}>
              <button
                type="button"
                className="btn btn-sm btn-primary"
                disabled={busy === 'trust'}
                onClick={() => void trust()}
              >
                {t('conductor.trustStart')}
              </button>
              {onOpenSession && (
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() => open(wait.session.id, true)}
                >
                  {t('live.showTerminal')}
                </button>
              )}
            </div>
          </li>
        )}
        {wait?.kind === 'decision' && (
          <li className={styles.request} data-testid="conductor-decision">
            <p className={styles.requestTitle}>{t('conductor.decisionTitle')}</p>
            {wait.session.pending_decision && (
              <pre className={styles.requestText}>
                {wait.session.pending_decision.decision.prompt}
              </pre>
            )}
            <p className={styles.help}>{t('conductor.decisionHelp')}</p>
            {onOpenSession && (
              <div className={styles.buttons}>
                <button
                  type="button"
                  className="btn btn-sm btn-primary"
                  onClick={() => open(wait.session.id, false)}
                >
                  {t('conductor.decisionOpen')}
                </button>
              </div>
            )}
          </li>
        )}
        {pending.map((r) => (
          <li key={r.id} className={styles.request} data-testid={`conductor-request-${r.id}`}>
            <p className={styles.requestTitle}>
              {t('conductor.requestTitle', { session: r.session_name })}
            </p>
            <pre className={styles.requestText}>{r.text}</pre>
            {r.reason && NEEDS[r.reason] && (
              <p className={styles.help}>{t(NEEDS[r.reason]!, { minutes: 10 })}</p>
            )}
            <p className={styles.help}>
              {t('conductor.expires', { time: relative(r.expires_at) })}
            </p>
            <div className={styles.buttons}>
              <button
                type="button"
                className="btn btn-sm btn-primary"
                disabled={busy === r.id}
                onClick={() => void decide(r.id, true)}
              >
                {t('conductor.allow')}
              </button>
              <button
                type="button"
                className="btn btn-sm"
                disabled={busy === r.id}
                onClick={() => void decide(r.id, false)}
              >
                {t('conductor.deny')}
              </button>
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}
