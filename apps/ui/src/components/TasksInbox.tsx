import { useState } from 'react';

import { useI18n } from '../i18n/context';
import { useAppState } from '../state/store';
import { blockingOpen, isActive } from '../state/tasks';
import { AlertsView } from './AlertsView';
import { TasksView } from './TasksView';
import styles from './TasksView.module.css';

export type InboxTab = 'alerts' | 'tasks';

/**
 * The "알림 · 할 일" menu: approvals and questions waiting in sessions on
 * one tab, the to-do list from agents on the other.
 */
export function AlertsAndTasksView({
  tab: controlled,
  onTab,
}: {
  tab?: InboxTab;
  onTab?: (tab: InboxTab) => void;
}) {
  const { t } = useI18n();
  const alerts = useAppState((s) => s.alerts.active.length);
  const blocking = useAppState((s) => blockingOpen(s.tasks).length);
  const open = useAppState((s) => (s.tasks?.list ?? []).filter((x) => isActive(x.status)).length);
  // With nothing to decide but an agent waiting on a task, start on the list.
  const [own, setOwn] = useState<InboxTab>(() =>
    alerts === 0 && blocking > 0 ? 'tasks' : 'alerts',
  );
  const tab = controlled ?? own;
  const select = (next: InboxTab) => {
    setOwn(next);
    onTab?.(next);
  };
  const tabs: { id: InboxTab; label: string; count: number }[] = [
    { id: 'alerts', label: t('inbox.alerts'), count: alerts },
    { id: 'tasks', label: t('inbox.tasks'), count: open },
  ];

  return (
    <div className={styles.inbox}>
      <div className={styles.tabs} role="tablist" aria-label={t('inbox.tabs')}>
        {tabs.map((x) => (
          <button
            key={x.id}
            type="button"
            role="tab"
            id={`inbox-tab-${x.id}`}
            aria-selected={tab === x.id}
            aria-controls={`inbox-panel-${x.id}`}
            tabIndex={tab === x.id ? 0 : -1}
            className={styles.tab}
            onClick={() => select(x.id)}
            onKeyDown={(e) => {
              if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
                e.preventDefault();
                select(x.id === 'alerts' ? 'tasks' : 'alerts');
              }
            }}
          >
            {x.label}
            {x.count > 0 && (
              <span className="badge" aria-hidden="true">
                {x.count}
              </span>
            )}
            {x.count > 0 && <span className="visually-hidden">({x.count})</span>}
          </button>
        ))}
      </div>
      <div role="tabpanel" id={`inbox-panel-${tab}`} aria-labelledby={`inbox-tab-${tab}`}>
        {tab === 'alerts' ? <AlertsView /> : <TasksView />}
      </div>
    </div>
  );
}
