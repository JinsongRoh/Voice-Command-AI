import { useEffect, useState } from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import { FEATURE_HUMAN_TASKS } from '../protocol/tasks';
import type { SessionInfo } from '../protocol/types';
import { newestFirst, type ApprovalKind } from '../state/approvals';
import {
  loadOsNotificationState,
  osNotificationState,
  requestOsNotifications,
  type OsNotificationState,
} from '../state/osNotify';
import { APPROVAL_MODES, type ApprovalMode } from '../state/settings';
import { useActions, useAppState, useSettings } from '../state/store';
import { BriefingButton } from './BriefingButton';
import { ConductorRequests } from './ConductorRequests';
import { Icon } from './Icon';
import { QuestionForm } from './QuestionForm';
import { TaskEditorDialog } from './TasksEditor';
import { VoiceToggle } from './VoiceControls';
import { useBringToFront } from './WindowsPanel';
import styles from './AlertsView.module.css';

const MODE_LABELS: Record<ApprovalMode, MessageKey> = {
  ask: 'alerts.mode.ask',
  auto: 'alerts.mode.auto',
  off: 'alerts.mode.off',
};

/** "When approval is needed": ask, switch automatically, or only notify (never switch). */
export function ApprovalModeControl() {
  const { t } = useI18n();
  const { settings, updateSettings } = useSettings();
  return (
    <div className={styles.mode}>
      <span className="label" id="approval-mode-label">
        {t('alerts.mode.title')}
      </span>
      <div className="segmented" role="group" aria-labelledby="approval-mode-label">
        {APPROVAL_MODES.map((mode) => (
          <button
            key={mode}
            type="button"
            aria-pressed={settings.approvalMode === mode}
            onClick={() => updateSettings({ approvalMode: mode })}
          >
            {t(MODE_LABELS[mode])}
          </button>
        ))}
      </div>
      {settings.approvalMode === 'auto' && (
        <p className={styles.hint}>{t('alerts.mode.autoHint')}</p>
      )}
      {settings.approvalMode === 'off' && <p className={styles.hint}>{t('alerts.mode.offHint')}</p>}
    </div>
  );
}

/** Everything waiting for the person, newest first (the Alerts menu). */
/** What an alert without request text says. */
function waitingKey(kind: ApprovalKind): MessageKey {
  switch (kind) {
    case 'decision':
      return 'alerts.needsDecision';
    case 'question':
      return 'alerts.hasQuestion';
    case 'input':
      return 'alerts.needsInput';
  }
}

export function AlertsView() {
  const { t, relative } = useI18n();
  const actions = useActions();
  const book = useAppState((s) => s.alerts);
  const alerts = newestFirst(book);
  const sessions = useAppState((s) => s.sessions);
  const bringToFront = useBringToFront();
  // "할 일로 만들기": an alert becomes a task only when the person converts it.
  const canConvert = useAppState((s) => s.serverFeatures.includes(FEATURE_HUMAN_TASKS));
  const [converting, setConverting] = useState<(typeof alerts)[number] | null>(null);
  const [osState, setOsState] = useState<OsNotificationState>(osNotificationState);
  useEffect(() => {
    void loadOsNotificationState().then(setOsState);
  }, []);

  return (
    <section className={styles.view} aria-labelledby="alerts-title">
      <h2 id="alerts-title" className={styles.title}>
        {t('alerts.title')}
      </h2>
      {/* Sends the conductor wants to make wait here too (Allow / Deny). */}
      <ConductorRequests heading />
      <div className={`panel ${styles.settings}`}>
        <ApprovalModeControl />
        {osState === 'default' && (
          <button
            type="button"
            className="btn btn-sm"
            onClick={() => void requestOsNotifications().then(setOsState)}
          >
            <Icon name="bell" size={14} /> {t('alerts.allowOs')}
          </button>
        )}
        <BriefingButton />
        <VoiceToggle />
      </div>
      {alerts.length === 0 ? (
        <p className={`muted ${styles.empty}`}>{t('alerts.empty')}</p>
      ) : (
        <ul className={styles.list}>
          {alerts.map((alert) => (
            <li key={alert.key} className={`panel ${styles.item}`} data-kind={alert.kind}>
              <button
                type="button"
                className={styles.open}
                aria-label={t('alerts.openItem', { name: alert.name })}
                onClick={() => actions.openAlert(alert)}
              >
                <span className={styles.name}>
                  {alert.name}
                  {alert.conductor && (
                    <span className={styles.conductor} data-testid="alert-conductor">
                      {t('alerts.conductorSession')}
                    </span>
                  )}
                </span>
                <span className={styles.request}>
                  {alert.inferred
                    ? [t('alerts.inferred'), alert.request].filter(Boolean).join(' · ')
                    : alert.request || t(waitingKey(alert.kind))}
                </span>
                <span className={styles.time}>{relative(alert.at)}</span>
              </button>
              {bringToFront && (
                <button
                  type="button"
                  className="icon-btn"
                  aria-label={t('windows.frontLabel', { name: alert.name })}
                  title={t('windows.front')}
                  onClick={() => void bringToFront(alert.sessionId, alert.name)}
                >
                  <Icon name="windows" size={18} />
                </button>
              )}
              {canConvert && (
                <button
                  type="button"
                  className="icon-btn"
                  aria-label={t('tasks.fromAlert', { name: alert.name })}
                  title={t('tasks.addFromMessage')}
                  onClick={() => setConverting(alert)}
                >
                  <Icon name="bookmark" size={18} />
                </button>
              )}
              <button
                type="button"
                className="icon-btn"
                aria-label={t('alerts.markHandled', { name: alert.name })}
                title={t('alerts.handled')}
                onClick={() => {
                  actions.markAlertHandled(alert.key);
                  // One stray tap must not hide a waiting agent for good.
                  actions.showToast({
                    id: `handled:${alert.key}`,
                    category: 'activity',
                    sessionId: null,
                    title: t('alerts.markedHandled', { name: alert.name }),
                    body: '',
                    action: { label: t('alerts.undo'), run: () => actions.reopenAlert(alert) },
                  });
                }}
              >
                <Icon name="check" size={18} />
              </button>
              {alert.kind === 'decision' && <AlertQuestion session={sessions[alert.sessionId]} />}
            </li>
          ))}
        </ul>
      )}
      {converting && (
        <TaskEditorDialog
          prefill={{
            sessionId: converting.sessionId,
            title: converting.request || t(waitingKey(converting.kind)),
            steps: '',
            command: null,
            messageSeq: null,
            anchorLatest: true,
          }}
          onClose={() => setConverting(null)}
        />
      )}
    </section>
  );
}

/** A question form answered right in the Alerts list. */
function AlertQuestion({ session }: { session: SessionInfo | undefined }) {
  const decision = session?.pending_decision?.decision;
  if (!session || !decision?.questions?.length) return null;
  return (
    <div className={styles.question}>
      <QuestionForm session={session} decision={decision} compact />
    </div>
  );
}
