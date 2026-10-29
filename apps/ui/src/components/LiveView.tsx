import { lazy, Suspense, useState } from 'react';

import { useI18n } from '../i18n/context';
import { hasChatTranscript, isCloud, providerLabel, sessionLocation } from '../protocol/helpers';
import type { SessionAction, SessionInfo } from '../protocol/types';
import { useActions, useAppState } from '../state/store';
import { AgentIcon } from './AgentIcon';
import { ChatView } from './ChatView';
import { Composer } from './Composer';
import { DecisionSheet } from './DecisionSheet';
import { Icon } from './Icon';
import { ConfirmDialog } from './Modal';
import { StateChip } from './StateChip';
import styles from './LiveView.module.css';

const TerminalView = lazy(() => import('./TerminalView'));

type Mode = 'chat' | 'terminal';

export function LiveView({ session, onBack }: { session: SessionInfo; onBack?: () => void }) {
  const { t } = useI18n();
  const actions = useActions();
  const view = useAppState((s) => s.views[session.id]);
  const decision = useAppState((s) => s.decisions[session.id]);
  const sent = useAppState((s) => s.sent[session.id]);
  const [mode, setMode] = useState<Mode>('chat');
  const [confirmStop, setConfirmStop] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const cloud = isCloud(session.origin);
  const effectiveMode: Mode = cloud ? 'chat' : mode;
  const st = session.state.type;
  const events = view?.events ?? [];

  const control = (action: SessionAction) => {
    setError(null);
    actions
      .control(session.id, action)
      .catch((err: Error) => setError(t('error.request', { error: err.message })));
  };

  return (
    <section className={styles.live} aria-labelledby="live-title">
      <header className={styles.header}>
        {onBack && (
          <button type="button" className="icon-btn" onClick={onBack} aria-label={t('live.back')}>
            <Icon name="back" />
          </button>
        )}
        <AgentIcon origin={session.origin} size={36} />
        <div className={styles.titles}>
          <h2 id="live-title" className={styles.title}>
            {session.display_name}
          </h2>
          <span className={styles.sub}>
            <StateChip state={session.state} />
            <span className={styles.location} title={sessionLocation(session)}>
              {sessionLocation(session)}
            </span>
            <span className={styles.tier}>{t(`tier.${session.control}`)}</span>
          </span>
        </div>
      </header>

      <div className={styles.toolbar}>
        <div className="segmented" role="group" aria-label={t('live.viewMode')}>
          <button
            type="button"
            aria-pressed={effectiveMode === 'chat'}
            onClick={() => setMode('chat')}
          >
            <Icon name="chat" size={14} /> {t('live.chat')}
          </button>
          <button
            type="button"
            aria-pressed={effectiveMode === 'terminal'}
            onClick={() => setMode('terminal')}
            disabled={cloud}
            title={cloud ? t('live.noTerminal') : undefined}
          >
            <Icon name="terminal" size={14} /> {t('live.terminal')}
          </button>
        </div>
        <div className={styles.actions} role="group" aria-label={t('live.actions')}>
          {session.origin.type === 'cloud' && session.origin.url && (
            <a
              className="btn btn-sm"
              href={session.origin.url}
              target="_blank"
              rel="noreferrer"
              aria-label={t('live.openProvider', {
                provider: providerLabel(session.origin.provider),
              })}
            >
              <Icon name="external" size={14} />
              <span className={styles.btnLabel}>
                {t('live.openProvider', { provider: providerLabel(session.origin.provider) })}
              </span>
            </a>
          )}
          {!cloud && st !== 'exited' && st !== 'paused' && (
            <button
              type="button"
              className="btn btn-sm"
              aria-label={t('action.pause')}
              title={t('action.pause')}
              onClick={() => control('pause')}
            >
              <Icon name="pause" size={14} />
              <span className={styles.btnLabel}>{t('action.pause')}</span>
            </button>
          )}
          {!cloud && st === 'paused' && (
            <button
              type="button"
              className="btn btn-sm"
              aria-label={t('action.resume')}
              title={t('action.resume')}
              onClick={() => control('resume')}
            >
              <Icon name="play" size={14} />
              <span className={styles.btnLabel}>{t('action.resume')}</span>
            </button>
          )}
          {!cloud && (
            <button
              type="button"
              className="btn btn-sm"
              aria-label={t('action.restart')}
              title={t('action.restart')}
              onClick={() => control('restart')}
            >
              <Icon name="restart" size={14} />
              <span className={styles.btnLabel}>{t('action.restart')}</span>
            </button>
          )}
          {st !== 'exited' && (
            <button
              type="button"
              className="btn btn-sm"
              aria-label={t('action.stop')}
              title={t('action.stop')}
              onClick={() => setConfirmStop(true)}
            >
              <Icon name="stop" size={14} />
              <span className={styles.btnLabel}>{t('action.stop')}</span>
            </button>
          )}
        </div>
      </div>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}

      <div className={styles.content}>
        {effectiveMode === 'chat' ? (
          <ChatView
            events={events}
            gapFrom={view?.gapFrom ?? null}
            sent={sent}
            outputFallback={!hasChatTranscript(session.origin)}
            onShowTerminal={() => setMode('terminal')}
          />
        ) : (
          <Suspense fallback={<p className="muted">…</p>}>
            <TerminalView sessionId={session.id} events={events} />
          </Suspense>
        )}
      </div>

      {(st === 'awaiting_decision' || decision) && (
        <DecisionSheet session={session} decision={decision} />
      )}
      <Composer key={session.id} session={session} />

      {confirmStop && (
        <ConfirmDialog
          title={t('action.stop')}
          message={t('live.confirmStop', { name: session.display_name })}
          confirmLabel={t('action.stop')}
          danger
          onCancel={() => setConfirmStop(false)}
          onConfirm={() => {
            setConfirmStop(false);
            control('stop');
          }}
        />
      )}
    </section>
  );
}

export function LiveEmpty() {
  const { t } = useI18n();
  return (
    <div className={styles.empty}>
      <Icon name="list" size={32} />
      <p>{t('live.noSelection')}</p>
    </div>
  );
}
