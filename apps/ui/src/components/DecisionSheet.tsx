import { useEffect, useId, useRef, useState } from 'react';

import { useMediaQuery } from '../hooks/useMediaQuery';
import { useI18n } from '../i18n/context';
import { commandFromDecision } from '../protocol/shell';
import type { Decision, SessionInfo } from '../protocol/types';
import { useSpokenDecision } from '../speech/useHandsFree';
import { useActions, useAppState } from '../state/store';
import { DecisionVoiceStatusChip } from './HandsFree';
import { Icon } from './Icon';
import { QuestionForm } from './QuestionForm';
import { requestSettingsView } from './settingsNav';
import { useShell } from './ShellProvider';
import styles from './DecisionSheet.module.css';

/**
 * Phones start with the card collapsed so the conversation stays visible,
 * unless the request carries a command: that is read in full before answering.
 */
const PHONE_QUERY = '(max-width: 899px)';

/**
 * One-tap answer to a pending agent decision (NTF-05). Collapsible: the
 * collapsed card is a short summary (the question on one line, or up to
 * three lines of the command) with the primary answers, so the chat above keeps its room on phones. A command in the
 * decision can be run in a remote shell instead ("Run in shell").
 */
export function DecisionSheet({
  session,
  decision,
  quiet = false,
}: {
  session: SessionInfo;
  decision: Decision | undefined;
  /**
   * Shown in a list (지휘소 › Approvals): never read aloud or answered by
   * voice there, and alerts do not move the focus to it. The answers are the
   * same as on the session screen.
   */
  quiet?: boolean;
}) {
  const { t } = useI18n();
  const actions = useActions();
  const shell = useShell();
  const phone = useMediaQuery(PHONE_QUERY);
  const bodyId = useId();
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const command = commandFromDecision(decision);
  /** `null` until the user toggles: follows the screen size and the request. */
  const [expandedChoice, setExpanded] = useState<boolean | null>(null);
  const expanded = expandedChoice ?? (!phone || !!command);
  const sheetRef = useRef<HTMLElement>(null);
  // Opened from an alert: show the whole question and take the focus here.
  const jump = useAppState((s) =>
    !quiet && s.jump?.sessionId === session.id ? s.jump.token : null,
  );
  const [shownJump, setShownJump] = useState<number | null>(null);
  if (jump !== shownJump) {
    setShownJump(jump);
    if (jump !== null) setExpanded(true);
  }
  useEffect(() => {
    if (jump === null) return;
    sheetRef.current?.focus({ preventScroll: false });
    actions.finishJump();
  }, [jump, actions]);

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

  // Hands-free: read out; a spoken answer only for `safe` plain yes/no requests.
  const voiceStatus = useSpokenDecision(session, quiet ? undefined : decision);
  const [headline = '', ...details] = (decision?.prompt ?? '').split('\n');
  // Inferred without hooks: answered only in the agent's own terminal.
  const inferred = !!decision?.inferred;
  // A question form: one card per question instead of answer buttons.
  const form = decision?.questions?.length ? decision : null;
  const canRunInShell =
    !inferred && !form && !!command && !!shell?.allowed && session.origin.type === 'local';
  const runInShell = () => {
    if (!command || !shell) return;
    shell.runInShell({
      command,
      projectDir: session.origin.type === 'local' ? session.origin.project_dir : null,
    });
  };
  // Collapsed: the command itself says the most; otherwise the question.
  const summary = command ?? headline;
  // Collapsed: the first answer (usually "Yes") and the last (usually "No").
  const options = inferred ? [] : (decision?.options ?? []);
  const primary = expanded
    ? options
    : options.length > 1
      ? [options[0]!, options[options.length - 1]!]
      : options;
  /** Answers left out of the collapsed card; a "+N more" button shows them. */
  const hidden = options.length - primary.length;

  return (
    <section
      ref={sheetRef}
      tabIndex={-1}
      className={styles.sheet}
      data-expanded={expanded}
      role="region"
      aria-label={t('decision.region', { name: session.display_name })}
      data-testid="decision-sheet"
    >
      <div className={styles.head}>
        <h3 className={styles.title}>
          {t(form ? 'question.title' : inferred ? 'decision.inferredTitle' : 'decision.title')}
        </h3>
        {!expanded && decision && (
          <p
            className={`${styles.summary} ${command ? styles.summaryCommand : ''}`}
            title={decision.prompt}
          >
            {summary}
          </p>
        )}
        {decision && (
          <button
            type="button"
            className={`icon-btn ${styles.toggle}`}
            aria-expanded={expanded}
            aria-controls={bodyId}
            aria-label={t(expanded ? 'decision.collapse' : 'decision.expand')}
            title={t(expanded ? 'decision.collapse' : 'decision.expand')}
            onClick={() => setExpanded(!expanded)}
          >
            <Icon name={expanded ? 'down' : 'up'} size={18} />
          </button>
        )}
      </div>
      {decision ? (
        <div id={bodyId}>
          {expanded && !form && (
            <>
              <p className={styles.prompt}>{headline}</p>
              {details.length > 0 && <pre className={styles.detail}>{details.join('\n')}</pre>}
            </>
          )}
          {/* Kept mounted while collapsed, so the answers typed so far stay. */}
          {form && (
            <div hidden={!expanded}>
              <QuestionForm session={session} decision={form} />
            </div>
          )}
          {inferred && (
            <div className={styles.inferred}>
              <p className="muted" data-testid="decision-inferred-hint">
                {t('decision.inferredHint')}
              </p>
              {/* Agent hooks report the exact choices, answerable here. */}
              <button
                type="button"
                className="btn btn-sm"
                onClick={() => requestSettingsView('set-agent-hooks')}
              >
                <Icon name="settings" size={16} />
                {t('decision.setupHooks')}
              </button>
            </div>
          )}
          <div className={styles.options}>
            {primary.map((o, i) => (
              <button
                key={o.id}
                type="button"
                className={`btn ${i === 0 ? 'btn-primary' : ''}`}
                disabled={busy !== null}
                title={o.label}
                onClick={() => void answer(o.id)}
              >
                <span className={styles.optionLabel}>{o.label}</span>
              </button>
            ))}
            {hidden > 0 && (
              <button
                type="button"
                className={`btn ${styles.more}`}
                aria-controls={bodyId}
                aria-expanded={false}
                onClick={() => setExpanded(true)}
              >
                {t('decision.moreOptions', { count: hidden })}
              </button>
            )}
            {canRunInShell && (
              <button
                type="button"
                className={`btn ${styles.runInShell} ${expanded ? '' : styles.iconOnly}`}
                disabled={busy !== null}
                onClick={runInShell}
                aria-label={t('decision.runInShell')}
                title={t('decision.runInShellHint')}
              >
                <Icon name="terminal" size={16} />
                {expanded && <span className={styles.optionLabel}>{t('decision.runInShell')}</span>}
              </button>
            )}
          </div>
          <DecisionVoiceStatusChip status={voiceStatus} />
          {error && (
            <p role="alert" className={styles.error}>
              {error}
            </p>
          )}
        </div>
      ) : (
        <>
          <p className="muted">{t('decision.waiting')}</p>
          <DecisionVoiceStatusChip status={voiceStatus} />
        </>
      )}
    </section>
  );
}
