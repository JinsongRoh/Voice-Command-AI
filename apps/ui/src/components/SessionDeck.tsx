import { useMemo } from 'react';

import { useI18n } from '../i18n/context';
import { attentionCount, visibleSessions, type DeckFilter } from '../state/reducer';
import { useActions, useAppState } from '../state/store';
import { Icon } from './Icon';
import { SessionCard } from './SessionCard';
import styles from './SessionDeck.module.css';

const FILTERS: DeckFilter[] = ['all', 'attention', 'local', 'cloud'];

export function SessionDeck({
  onOpen,
  onNewSession,
}: {
  /** Called after a card is selected (mobile navigates to the live view). */
  onOpen: (id: string) => void;
  onNewSession: () => void;
}) {
  const { t } = useI18n();
  const state = useAppState((s) => s);
  const actions = useActions();
  const sessions = useMemo(() => visibleSessions(state), [state]);
  const attention = attentionCount(state);

  return (
    <section className={styles.deck} aria-labelledby="deck-title">
      <div className={styles.head}>
        <h2 id="deck-title" className={styles.title}>
          {t('deck.title')}
        </h2>
        {attention > 0 && (
          <span className={styles.attention}>{t('deck.attentionCount', { count: attention })}</span>
        )}
        <button type="button" className="btn btn-primary btn-sm" onClick={onNewSession}>
          <Icon name="plus" size={16} />
          {t('deck.new')}
        </button>
      </div>
      <div className="segmented" role="group" aria-label={t('deck.filterLabel')}>
        {FILTERS.map((f) => (
          <button
            key={f}
            type="button"
            aria-pressed={state.filter === f}
            onClick={() => actions.setFilter(f)}
          >
            {t(`filter.${f}`)}
          </button>
        ))}
      </div>
      {sessions.length === 0 ? (
        <p className="muted">{t('deck.empty')}</p>
      ) : (
        <ul className={styles.list}>
          {sessions.map((s) => (
            <SessionCard
              key={s.id}
              session={s}
              index={state.order.indexOf(s.id) + 1}
              preview={state.views[s.id]?.preview ?? ''}
              selected={s.id === state.selectedId}
              onSelect={(id) => {
                actions.select(id);
                onOpen(id);
              }}
            />
          ))}
        </ul>
      )}
    </section>
  );
}
