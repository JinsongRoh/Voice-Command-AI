import { useEffect, useMemo, useRef } from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import type { SessionEvent } from '../protocol/types';
import type { SentPrompt, SentStatus } from '../state/reducer';
import { buildTimeline } from './chatTimeline';
import { Icon } from './Icon';
import { stateKey } from './stateKey';
import styles from './ChatView.module.css';

const NO_PROMPTS: SentPrompt[] = [];

const SENT_STATUS: Record<SentStatus, MessageKey> = {
  sending: 'chat.sending',
  queued: 'chat.queued',
  delivered: 'chat.delivered',
  acknowledged: 'chat.acknowledged',
  failed: 'chat.failed',
};

export interface ChatViewProps {
  events: SessionEvent[];
  gapFrom: number | null;
  /** Prompts sent from this client (local echo). */
  sent?: SentPrompt[];
  /**
   * The agent has no chat transcript: show its terminal output as plain-text
   * bubbles plus a hint pointing at the terminal view.
   */
  outputFallback?: boolean;
  /** Switches the live view to the terminal (shown in the fallback hint). */
  onShowTerminal?: () => void;
}

/** Structured view of a session: messages, tool calls and decisions (MON-02). */
export function ChatView({
  events,
  gapFrom,
  sent = NO_PROMPTS,
  outputFallback = false,
  onShowTerminal,
}: ChatViewProps) {
  const { t } = useI18n();
  const ref = useRef<HTMLOListElement>(null);
  const stick = useRef(true);
  const items = useMemo(
    () => buildTimeline(events, sent, { includeOutput: outputFallback }),
    [events, sent, outputFallback],
  );
  // Output bubbles grow in place, so follow the text length as well.
  const last = items[items.length - 1];
  const tail = `${items.length}:${last?.kind === 'output' ? last.text.length : 0}`;

  useEffect(() => {
    const el = ref.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [tail]);

  return (
    <ol
      ref={ref}
      className={styles.chat}
      role="log"
      aria-label={t('live.log')}
      onScroll={(e) => {
        const el = e.currentTarget;
        stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
      }}
    >
      {outputFallback && (
        <li className={styles.hint} role="note">
          <Icon name="terminal" size={16} />
          <span className={styles.hintText}>{t('chat.noTranscript')}</span>
          {onShowTerminal && (
            <button type="button" className="btn btn-sm" onClick={onShowTerminal}>
              {t('chat.openTerminal')}
            </button>
          )}
        </li>
      )}
      {gapFrom !== null && (
        <li className={styles.gap} role="note">
          {t('live.gap', { seq: gapFrom })}
        </li>
      )}
      {items.length === 0 && <li className="muted">{t('live.empty')}</li>}
      {items.map((item) => {
        if (item.kind === 'sent') {
          const { prompt } = item;
          return (
            <li
              key={item.key}
              className={`${styles.msg} ${styles.user} ${styles.outgoing}`}
              data-status={prompt.status}
            >
              {/* The bubble side already shows who sent it. */}
              <span className="visually-hidden">{t('role.user')}</span>
              <p className={styles.text}>{prompt.text}</p>
              <span
                className={styles.status}
                title={prompt.status === 'failed' ? prompt.error : undefined}
              >
                {t(SENT_STATUS[prompt.status])}
              </span>
            </li>
          );
        }
        if (item.kind === 'output') {
          return (
            <li key={item.key} className={`${styles.msg} ${styles.output}`}>
              <span className={styles.role}>{t('chat.agentOutput')}</span>
              <pre className={styles.outputText}>
                {item.truncated ? `…${item.text}` : item.text}
              </pre>
            </li>
          );
        }
        const e = item.event;
        switch (e.type) {
          case 'message':
            return (
              <li key={item.key} className={`${styles.msg} ${styles[e.role]}`}>
                <span className={styles.role}>{t(`role.${e.role}`)}</span>
                <p className={styles.text}>{e.text}</p>
              </li>
            );
          case 'tool_call':
            return (
              <li key={item.key} className={styles.tool}>
                <span className={styles.toolName}>{e.name}</span>
                <span className={styles.toolSummary}>{e.summary}</span>
              </li>
            );
          case 'state_changed':
            return (
              <li key={item.key} className={styles.state}>
                {t(stateKey(e.state))}
              </li>
            );
          case 'decision_requested':
            return (
              <li key={item.key} className={styles.decision}>
                <span className={styles.role}>{t('decision.title')}</span>
                <p className={styles.text}>{e.decision.prompt}</p>
                <ul className={styles.options}>
                  {e.decision.options.map((o) => (
                    <li key={o.id}>{o.label}</li>
                  ))}
                </ul>
              </li>
            );
          case 'notification':
            return (
              <li key={item.key} className={styles.notification}>
                <Icon name="bell" size={16} />
                <span>
                  <strong>{e.title}</strong> {e.body}
                </span>
              </li>
            );
          default:
            return null;
        }
      })}
    </ol>
  );
}
