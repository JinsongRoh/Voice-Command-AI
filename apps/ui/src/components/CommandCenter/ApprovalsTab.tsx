import { useState } from 'react';

import { useI18n } from '../../i18n/context';
import type { MessageKey } from '../../i18n/i18n';
import { isStepUpCanceled } from '../../native/security';
import { requestTerminal } from '../../state/conductor';
import { deferItem, type InboxItem, type InboxKind } from '../../state/inbox';
import { useActions, useAppState } from '../../state/store';
import { DecisionSheet } from '../DecisionSheet';
import { Icon, type IconName } from '../Icon';
import { useStepUp } from '../StepUpPrompt';
import { useDeferred, useInbox } from './useInbox';
import styles from './CommandCenter.module.css';

const KIND_LABELS: Record<InboxKind, MessageKey> = {
  escalation: 'cc.inbox.kind.escalation',
  decision: 'cc.inbox.kind.decision',
  question: 'cc.inbox.kind.question',
  input: 'cc.inbox.kind.input',
  conductor_send: 'cc.inbox.kind.conductorSend',
  conductor_trust: 'cc.inbox.kind.conductorTrust',
  task: 'cc.inbox.kind.task',
};

const KIND_ICONS: Record<InboxKind, IconName> = {
  escalation: 'shield',
  decision: 'shield',
  question: 'chat',
  input: 'edit',
  conductor_send: 'send',
  conductor_trust: 'conductor',
  task: 'check',
};

/** Why VCA raised a decision itself (`ConductorEscalation.reason`). */
const FALLBACK_TEXT: Record<string, MessageKey> = {
  timeout: 'cc.inbox.escalation.fallbackTimeout',
  stopped: 'cc.inbox.escalation.fallbackStopped',
  paused: 'cc.inbox.escalation.fallbackPaused',
  revoked: 'cc.inbox.escalation.fallbackRevoked',
  not_routed: 'cc.inbox.escalation.fallbackNotRouted',
};

/** What an item without text asks. */
const EMPTY_TEXT: Partial<Record<InboxKind, MessageKey>> = {
  escalation: 'alerts.needsDecision',
  decision: 'alerts.needsDecision',
  question: 'alerts.hasQuestion',
  input: 'alerts.needsInput',
  conductor_trust: 'conductor.trustTitle',
};

/**
 * The Approvals inbox: every decision, question and input wait of the
 * sessions, the conductor's requests and the to-dos agents are blocked on,
 * in one list. Answers use the existing paths: a decision's own options
 * (the same card as on the session screen), `conductor_decide` behind the
 * step-up, or opening the session. "Later" moves an item down for now.
 */
export function ApprovalsTab({
  onOpenSession,
  onOpenTasks,
}: {
  onOpenSession?: (sessionId: string, terminal: boolean) => void;
  onOpenTasks?: () => void;
}) {
  const { t } = useI18n();
  const items = useInbox();
  const deferred = useDeferred();
  const now = items.filter((i) => !deferred.has(i.key));
  const later = items.filter((i) => deferred.has(i.key));

  return (
    <section className={styles.inbox} aria-labelledby="cc-inbox-title">
      <h3 id="cc-inbox-title" className="visually-hidden">
        {t('cc.tab.approvals')}
      </h3>
      <p className={styles.help}>{t('cc.inbox.help')}</p>
      {now.length === 0 ? (
        <p className={`muted ${styles.empty}`} data-testid="cc-inbox-empty">
          {t('cc.inbox.empty')}
        </p>
      ) : (
        <ul className={styles.items} data-testid="cc-inbox">
          {now.map((item) => (
            <InboxRow
              key={item.key}
              item={item}
              onOpenSession={onOpenSession}
              onOpenTasks={onOpenTasks}
            />
          ))}
        </ul>
      )}
      {later.length > 0 && (
        <>
          <h4 className={styles.subhead}>{t('cc.inbox.later', { count: later.length })}</h4>
          <ul className={styles.items} data-testid="cc-inbox-later">
            {later.map((item) => (
              <InboxRow
                key={item.key}
                item={item}
                deferred
                onOpenSession={onOpenSession}
                onOpenTasks={onOpenTasks}
              />
            ))}
          </ul>
        </>
      )}
    </section>
  );
}

function InboxRow({
  item,
  deferred = false,
  onOpenSession,
  onOpenTasks,
}: {
  item: InboxItem;
  deferred?: boolean;
  onOpenSession?: (sessionId: string, terminal: boolean) => void;
  onOpenTasks?: () => void;
}) {
  const { t, relative } = useI18n();
  const actions = useActions();
  const stepUp = useStepUp();
  const session = useAppState((s) => (item.sessionId ? s.sessions[item.sessionId] : undefined));
  const decision = useAppState((s) => (item.sessionId ? s.decisions[item.sessionId] : undefined));
  const [answering, setAnswering] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const name = item.name || t('cc.inbox.noSession');
  const text =
    item.text ||
    (EMPTY_TEXT[item.kind] ? t(EMPTY_TEXT[item.kind]!) : '') ||
    t(KIND_LABELS[item.kind]);
  const escalation = item.escalation;
  // The decision card answers only what the agent can take from VCA:
  // not an inferred prompt (terminal only).
  const canAnswer =
    (item.kind === 'decision' || item.kind === 'escalation') &&
    !item.inferred &&
    !session?.pending_decision?.decision.inferred &&
    !!session &&
    !!(decision ?? session.pending_decision);
  const fallback =
    escalation?.source === 'fallback'
      ? t(FALLBACK_TEXT[escalation.reason ?? ''] ?? 'cc.inbox.escalation.fallbackProblem')
      : null;
  const titleId = `cc-item-${item.key.replace(/[^\w-]/g, '_')}`;

  const run = async (work: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await work();
    } catch (err) {
      if (!isStepUpCanceled(err)) setError(t('error.request', { error: (err as Error).message }));
    } finally {
      setBusy(false);
    }
  };
  const decide = (allow: boolean) =>
    run(() =>
      // The same confirmation as everywhere a conductor request is answered.
      stepUp.run('conductor_decide', () =>
        actions.conductor({
          type: 'conductor_decide',
          request_id: item.request!.id,
          allow,
        }),
      ),
    );
  const open = () => {
    if (item.alert) actions.openAlert(item.alert);
    else if (item.sessionId) onOpenSession?.(item.sessionId, false);
  };
  const laterButton = (
    <button
      type="button"
      className="btn btn-sm"
      onClick={() => deferItem(item.key, !deferred)}
      aria-describedby={titleId}
    >
      {t(deferred ? 'cc.inbox.backToList' : 'cc.inbox.laterAction')}
    </button>
  );

  return (
    <li
      className={`panel ${styles.item}`}
      data-kind={item.kind}
      data-testid={`inbox-item-${item.key}`}
      aria-labelledby={titleId}
    >
      <div className={styles.itemHead}>
        <span className={styles.kindIcon} aria-hidden="true">
          <Icon name={KIND_ICONS[item.kind]} size={18} />
        </span>
        <div className={styles.itemBody}>
          <p className={styles.itemMeta}>
            <span className={styles.itemName} id={titleId}>
              {name}
            </span>
            {item.conductor && <span className={styles.chip}>{t('alerts.conductorSession')}</span>}
            {escalation && (
              <span className={styles.chip}>
                {t(
                  escalation.source === 'fallback'
                    ? 'cc.inbox.escalation.fallbackChip'
                    : 'cc.inbox.escalation.byConductor',
                )}
              </span>
            )}
            <span className={styles.kind}>{t(KIND_LABELS[item.kind])}</span>
            <time className={styles.time} dateTime={new Date(item.at).toISOString()}>
              {relative(item.at)}
            </time>
          </p>
          {item.kind === 'conductor_send' ? (
            <>
              <p className={styles.itemText}>
                {t('conductor.requestTitle', { session: item.name })}
              </p>
              <pre className={styles.requestText}>{item.text}</pre>
              {item.request && (
                <p className={styles.help}>
                  {t('conductor.expires', { time: relative(item.request.expires_at) })}
                </p>
              )}
            </>
          ) : escalation ? (
            <>
              <p className={styles.itemText} data-testid="escalation-summary">
                {fallback ?? escalation.summary}
              </p>
              {escalation.recommendation && (
                <p className={styles.help} data-testid="escalation-recommendation">
                  {t('cc.inbox.escalation.recommendation', { text: escalation.recommendation })}
                </p>
              )}
              <p className={styles.help}>{t('cc.inbox.escalation.request')}</p>
              <pre className={styles.requestText} data-testid="escalation-request">
                {escalation.command && !escalation.request.includes(escalation.command)
                  ? `${escalation.request}\n${escalation.command}`
                  : escalation.request || text}
              </pre>
            </>
          ) : (
            <p className={styles.itemText}>
              {item.inferred ? [t('alerts.inferred'), text].filter(Boolean).join(' · ') : text}
            </p>
          )}
          {item.kind === 'conductor_trust' && (
            <p className={styles.help}>{t('conductor.trustHelp')}</p>
          )}
          {item.kind === 'decision' && item.conductor && (
            <p className={styles.help}>{t('conductor.decisionHelp')}</p>
          )}
        </div>
      </div>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
      {answering && canAnswer && session && (
        <div className={styles.answer}>
          <DecisionSheet session={session} decision={decision} quiet />
        </div>
      )}
      <div className={styles.buttons}>
        {(item.kind === 'decision' || item.kind === 'escalation') && canAnswer && (
          <button
            type="button"
            className={`btn btn-sm ${answering ? '' : 'btn-primary'}`}
            aria-expanded={answering}
            aria-describedby={titleId}
            onClick={() => setAnswering((v) => !v)}
          >
            {t(answering ? 'cc.inbox.hideAnswer' : 'cc.inbox.answer')}
          </button>
        )}
        {item.kind === 'conductor_send' && item.request && (
          <>
            <button
              type="button"
              className="btn btn-sm btn-primary"
              disabled={busy}
              aria-describedby={titleId}
              onClick={() => void decide(true)}
            >
              {t('conductor.allow')}
            </button>
            <button
              type="button"
              className="btn btn-sm"
              disabled={busy}
              aria-describedby={titleId}
              onClick={() => void decide(false)}
            >
              {t('conductor.deny')}
            </button>
          </>
        )}
        {item.kind === 'conductor_trust' && (
          <>
            <button
              type="button"
              className="btn btn-sm btn-primary"
              disabled={busy}
              onClick={() => void run(() => actions.conductor({ type: 'conductor_trust_folder' }))}
            >
              {t('conductor.trustStart')}
            </button>
            {item.sessionId && onOpenSession && (
              <button
                type="button"
                className="btn btn-sm"
                onClick={() => {
                  requestTerminal(item.sessionId!);
                  onOpenSession(item.sessionId!, true);
                }}
              >
                {t('live.showTerminal')}
              </button>
            )}
          </>
        )}
        {item.kind === 'task' ? (
          onOpenTasks && (
            <button
              type="button"
              className="btn btn-sm"
              aria-describedby={titleId}
              onClick={onOpenTasks}
            >
              {t('cc.inbox.openTask')}
            </button>
          )
        ) : item.kind !== 'conductor_trust' && item.sessionId ? (
          <button type="button" className="btn btn-sm" aria-describedby={titleId} onClick={open}>
            {t('cc.inbox.open')}
          </button>
        ) : null}
        {item.kind === 'escalation' && item.sessionId && onOpenSession && (
          <button
            type="button"
            className="btn btn-sm"
            aria-describedby={titleId}
            onClick={() => {
              requestTerminal(item.sessionId!);
              onOpenSession(item.sessionId!, true);
            }}
          >
            {t('live.showTerminal')}
          </button>
        )}
        {item.kind !== 'conductor_trust' && laterButton}
      </div>
    </li>
  );
}
