import { useEffect, useMemo, useRef, useState } from 'react';

import { useI18n } from '../i18n/context';
import type { SessionInfo } from '../protocol/types';
import { matchesFilter, workOrder, type DeckFilter } from '../state/reducer';
import { togglePin, usePins } from '../state/sessionPins';
import { useActions, useAppState } from '../state/store';
import { Modal } from './Modal';
import { SessionPicker } from './SessionPicker';
import { isSwitcherShortcut, onSessionSwitcherRequest } from './switcherNav';
import styles from './SessionSwitcher.module.css';

const FILTERS: DeckFilter[] = ['all', 'attention', 'local', 'cloud'];

/**
 * The quick session switcher: Ctrl+K on desktops; on phones the search
 * button at the end of the session strip (a bottom sheet for long lists).
 * Searchable; sections for sessions that need attention, pinned ones (per
 * device), recent ones and all the rest.
 */
export function SessionSwitcher({ onOpen }: { onOpen: (id: string) => void }) {
  const [open, setOpen] = useState(false);

  useEffect(() => onSessionSwitcherRequest(() => setOpen(true)), []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!isSwitcherShortcut(e)) return;
      e.preventDefault();
      setOpen((v) => !v);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, []);

  if (!open) return null;
  return (
    <SwitcherSheet
      onClose={() => setOpen(false)}
      onPick={(s) => {
        setOpen(false);
        onOpen(s.id);
      }}
    />
  );
}

function SwitcherSheet({
  onClose,
  onPick,
}: {
  onClose: () => void;
  onPick: (session: SessionInfo) => void;
}) {
  const { t } = useI18n();
  const order = useAppState(workOrder);
  const map = useAppState((s) => s.sessions);
  const selectedId = useAppState((s) => s.selectedId);
  const filter = useAppState((s) => s.filter);
  const actions = useActions();
  const pins = usePins();
  // The search box takes the focus (the dialog would give it to the filter).
  const filtersRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    filtersRef.current
      ?.closest('[role="dialog"]')
      ?.querySelector<HTMLInputElement>('input[type="search"]')
      ?.focus();
  }, []);
  // The deck's category filter (all / attention / local / cloud) lives here
  // on phones, where the session view has no card row above it.
  const sessions = useMemo(
    () =>
      order.map((id) => map[id]).filter((s): s is SessionInfo => !!s && matchesFilter(s, filter)),
    [order, map, filter],
  );
  // A search finds every session, whatever the category filter.
  const all = useMemo(
    () => order.map((id) => map[id]).filter((s): s is SessionInfo => !!s),
    [order, map],
  );
  return (
    <Modal title={t('switcher.title')} onClose={onClose}>
      <p className="visually-hidden">{t('switcher.hint')}</p>
      <div
        ref={filtersRef}
        className={`segmented ${styles.filters}`}
        role="group"
        aria-label={t('deck.filterLabel')}
        data-testid="switcher-filters"
      >
        {FILTERS.map((f) => (
          <button
            key={f}
            type="button"
            aria-pressed={filter === f}
            onClick={() => actions.setFilter(f)}
          >
            {t(`filter.${f}`)}
          </button>
        ))}
      </div>
      <SessionPicker
        sessions={sessions}
        searchSessions={all}
        onPick={onPick}
        searchLabel={t('switcher.search')}
        pins={pins}
        onTogglePin={(id) => togglePin(id)}
        currentId={selectedId}
      />
    </Modal>
  );
}
