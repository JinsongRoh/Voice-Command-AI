import { useMemo, useState, type DragEvent, type PointerEvent, type ReactNode } from 'react';

import { useI18n } from '../i18n/context';
import { FEATURE_PRONUNCIATIONS } from '../protocol/pronunciations';
import {
  FEATURE_SCAN_SESSIONS,
  FEATURE_SESSION_ORDER,
  type SessionId,
  type SessionInfo,
} from '../protocol/types';
import { attentionCount, sessionNumber, visibleSessions, type DeckFilter } from '../state/reducer';
import { moveBy, moveNextTo } from '../state/sessionOrder';
import { useActions, useAppState } from '../state/store';
import { waitingOnYou } from '../state/tasks';
import { Icon } from './Icon';
import { ImportSessionsSheet } from './ImportSessionsSheet';
import { PronunciationDialog } from './PronunciationSettings';
import { PullToRefresh } from './PullToRefresh';
import { useRemoveFromVca } from './RemoveSession';
import { useDeviceScope } from './ShellProvider';
import { RenameSessionDialog } from './RenameSessionDialog';
import { SessionCard, type CardReorder } from './SessionCard';
import { SessionCarousel } from './SessionCarousel';
import { useEndedCommands } from './SessionEnded';
import { useBringToFront } from './WindowsPanel';
import styles from './SessionDeck.module.css';

const FILTERS: DeckFilter[] = ['all', 'attention', 'local', 'cloud'];

/** The `dataTransfer` type of a dragged session card. */
const DRAG_TYPE = 'application/x-vca-session';

/** Mouse movement on the handle that starts a drag. */
const POINTER_DRAG_SLOP_PX = 4;

export function SessionDeck({
  onOpen,
  onNewSession,
  variant = 'list',
  trailing,
}: {
  /** Called after a card is selected. */
  onOpen: (id: string) => void;
  onNewSession: () => void;
  /**
   * `list`: full cards in a column (wide screens). `carousel`: a swipeable row
   * of compact cards above the selected session (phones). `strip`: the phone
   * session strip under the top bar (short cards only; filters, search and
   * pins are in the switcher sheet behind `trailing`).
   */
  variant?: 'list' | 'carousel' | 'strip';
  /** The strip's last item (its search button). */
  trailing?: ReactNode;
}) {
  const { t } = useI18n();
  const state = useAppState((s) => s);
  const actions = useActions();
  const sessions = useMemo(() => visibleSessions(state), [state]);
  const attention = attentionCount(state);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const bringToFront = useBringToFront();
  // Ended sessions: Reopen (shells, sessions VCA started).
  const endedCommands = useEndedCommands();
  const failed = (session: SessionInfo, what: string, problem: string) =>
    actions.showToast({
      id: `ended:${session.id}:${what}`,
      category: 'error',
      sessionId: session.id,
      title: session.display_name,
      body: problem,
    });
  const canReopen = (s: SessionInfo) =>
    s.state.type === 'exited' &&
    (endedCommands.kindOf(s) === 'shell' || endedCommands.kindOf(s) === 'restart');
  const onReopen = (id: string) => {
    const session = state.sessions[id];
    if (!session) return;
    void endedCommands.reopen(session).then((problem) => {
      if (problem) failed(session, 'reopen', problem);
    });
  };
  // "Remove from VCA" for any session (asks first for a running one).
  const removal = useRemoveFromVca((session, problem) => failed(session, 'remove', problem));
  const onRemove = (id: string) => {
    const session = state.sessions[id];
    if (session) removal.ask(session);
  };
  const removeProps = (s: SessionInfo) => (removal.offered(s) ? { onRemove } : {});
  const renaming = renamingId ? state.sessions[renamingId] : undefined;
  // "Fix pronunciation" of a session name (servers with the dictionary).
  const [pronouncingId, setPronouncingId] = useState<string | null>(null);
  const pronouncing = pronouncingId ? state.sessions[pronouncingId] : undefined;
  const canPronounce =
    state.connection === 'connected' && state.serverFeatures.includes(FEATURE_PRONUNCIATIONS);
  const pronounceDialog = pronouncing && (
    <PronunciationDialog name={pronouncing.display_name} onClose={() => setPronouncingId(null)} />
  );
  // "Bring in" AI sessions from other terminal windows; older servers lack it.
  const canScan =
    state.connection === 'connected' && state.serverFeatures.includes(FEATURE_SCAN_SESSIONS);
  const [importing, setImporting] = useState(false);
  const importButton = canScan && (
    <button
      type="button"
      className="btn btn-sm"
      onClick={() => setImporting(true)}
      aria-label={t('scan.open')}
      title={t('scan.openHint')}
      data-testid="scan-open"
    >
      <Icon name="restart" size={16} />
      {variant === 'list' && t('scan.button')}
    </button>
  );
  const importSheet = importing && <ImportSessionsSheet onClose={() => setImporting(false)} />;
  /** Pull to refresh on phones: a scan, then a short summary. */
  const pullScan = async () => {
    try {
      const report = await actions.scanSessions();
      const listed = report.found.filter(
        (f) => f.status === 'imported' || f.status === 'no_transcript_yet',
      ).length;
      actions.showToast({
        id: `scan:${Date.now()}`,
        category: 'activity',
        sessionId: null,
        title: t('scan.pulledTitle'),
        body: t('scan.pulledBody', { count: report.found.length, listed }),
      });
    } catch (err) {
      actions.showToast({
        id: `scan:${Date.now()}`,
        category: 'error',
        sessionId: null,
        title: t('scan.failed'),
        body: (err as Error).message,
      });
    }
  };
  const select = (id: string) => {
    actions.select(id);
    onOpen(id);
  };

  // The order the user chose on the PC (servers that keep one; view-only
  // devices cannot change it).
  const scope = useDeviceScope();
  const canReorder =
    state.connection === 'connected' &&
    state.serverFeatures.includes(FEATURE_SESSION_ORDER) &&
    scope !== 'view';
  const [dragId, setDragId] = useState<SessionId | null>(null);
  const [drop, setDrop] = useState<{ id: SessionId; after: boolean } | null>(null);
  const applyOrder = (order: SessionId[]) => {
    if (order === state.order) return;
    actions.setSessionOrder(order).catch((err: Error) =>
      actions.showToast({
        id: 'order-failed',
        category: 'error',
        sessionId: null,
        title: t('order.failed', { error: err.message }),
        body: '',
      }),
    );
  };
  const endDrag = () => {
    setDragId(null);
    setDrop(null);
  };
  const reorderOf = (id: SessionId): CardReorder => ({
    move: (offset) => {
      applyOrder(
        moveBy(
          state.order,
          id,
          offset,
          sessions.map((s) => s.id),
        ),
      );
      // The card moved in the list: keep the keyboard on it.
      requestAnimationFrame(() =>
        document.querySelector<HTMLElement>(`[data-testid="session-card-${id}"]`)?.focus(),
      );
    },
    dragging: dragId === id,
    dropAt: drop?.id === id && dragId !== id ? (drop.after ? 'after' : 'before') : null,
    onHandlePointerDown: (e: PointerEvent<HTMLElement>) => {
      if (e.pointerType !== 'mouse' || e.button !== 0) return;
      // No HTML drag and no text selection: the pointer drives the drag.
      e.preventDefault();
      const handle = e.currentTarget;
      const pointerId = e.pointerId;
      const startY = e.clientY;
      const order = state.order;
      let started = false;
      let target: { id: SessionId; after: boolean } | null = null;
      try {
        handle.setPointerCapture(pointerId);
      } catch {
        // Capture is optional; moves outside the handle still arrive on it in most engines.
      }
      const onMove = (ev: globalThis.PointerEvent) => {
        if (!started) {
          if (Math.abs(ev.clientY - startY) < POINTER_DRAG_SLOP_PX) return;
          started = true;
          setDragId(id);
        }
        const item = document
          .elementFromPoint(ev.clientX, ev.clientY)
          ?.closest<HTMLElement>('[data-session-item]');
        const over = item?.dataset.sessionItem;
        if (!item || !over || over === id) {
          target = null;
          setDrop(null);
          return;
        }
        const box = item.getBoundingClientRect();
        const after = box.height > 0 && ev.clientY > box.top + box.height / 2;
        target = { id: over, after };
        setDrop(target);
      };
      const finish = (apply: boolean) => {
        handle.removeEventListener('pointermove', onMove);
        handle.removeEventListener('pointerup', onUp);
        handle.removeEventListener('pointercancel', onCancel);
        handle.removeEventListener('lostpointercapture', onUp);
        try {
          handle.releasePointerCapture(pointerId);
        } catch {
          // Already released.
        }
        endDrag();
        if (apply && started && target) {
          applyOrder(moveNextTo(order, id, target.id, target.after));
        }
      };
      const onUp = () => finish(true);
      const onCancel = () => finish(false);
      handle.addEventListener('pointermove', onMove);
      handle.addEventListener('pointerup', onUp);
      handle.addEventListener('pointercancel', onCancel);
      handle.addEventListener('lostpointercapture', onUp);
    },
    onDragStart: (e: DragEvent<HTMLElement>) => {
      e.dataTransfer.effectAllowed = 'move';
      e.dataTransfer.setData(DRAG_TYPE, id);
      e.dataTransfer.setData('text/plain', state.sessions[id]?.display_name ?? id);
      // Drag the whole card, not just the grip.
      const item = e.currentTarget.closest('li');
      if (item && typeof e.dataTransfer.setDragImage === 'function') {
        e.dataTransfer.setDragImage(item, 24, 24);
      }
      setDragId(id);
    },
    onDragEnd: endDrag,
    onDragOver: (e: DragEvent<HTMLLIElement>) => {
      if (!dragId) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = 'move';
      const box = e.currentTarget.getBoundingClientRect();
      const after = box.height > 0 && e.clientY > box.top + box.height / 2;
      if (drop?.id !== id || drop.after !== after) setDrop({ id, after });
    },
    onDragLeave: (e: DragEvent<HTMLLIElement>) => {
      if (e.currentTarget.contains(e.relatedTarget as Node | null)) return;
      if (drop?.id === id) setDrop(null);
    },
    onDrop: (e: DragEvent<HTMLLIElement>) => {
      const moved = dragId ?? e.dataTransfer.getData(DRAG_TYPE);
      if (!moved) return;
      e.preventDefault();
      const after = drop?.id === id ? drop.after : false;
      endDrag();
      applyOrder(moveNextTo(state.order, moved, id, after));
    },
  });

  const filters = (
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
  );

  if (variant === 'strip') {
    return (
      <section className={styles.strip} aria-labelledby="deck-title" data-testid="session-strip">
        <h2 id="deck-title" className="visually-hidden">
          {t('deck.title')}
        </h2>
        <SessionCarousel
          sessions={sessions}
          numberOf={(id) => sessionNumber(state, id) ?? 0}
          selectedId={state.selectedId}
          onSelect={select}
          waitingOf={(id) => waitingOnYou(state.tasks, id)}
          compact
          trailing={trailing}
        />
      </section>
    );
  }

  if (variant === 'carousel') {
    return (
      <section className={styles.compact} aria-labelledby="deck-title">
        <h2 id="deck-title" className="visually-hidden">
          {t('deck.title')}
        </h2>
        <PullToRefresh
          onRefresh={pullScan}
          disabled={!canScan}
          labels={{
            pull: t('scan.pull'),
            release: t('scan.release'),
            refreshing: t('scan.scanning'),
          }}
        >
          <SessionCarousel
            sessions={sessions}
            numberOf={(id) => sessionNumber(state, id) ?? 0}
            selectedId={state.selectedId}
            onSelect={select}
            onRename={setRenamingId}
            canReopen={canReopen}
            onReopen={onReopen}
            {...(state.connection === 'connected' && scope !== 'view' ? { onRemove } : {})}
            waitingOf={(id) => waitingOnYou(state.tasks, id)}
          >
            {filters}
            {importButton}
            <button
              type="button"
              className={`btn btn-primary btn-sm ${styles.newIcon}`}
              onClick={onNewSession}
              aria-label={t('deck.new')}
              title={t('deck.new')}
            >
              <Icon name="plus" size={16} />
            </button>
          </SessionCarousel>
        </PullToRefresh>
        {sessions.length === 0 && <p className={`muted ${styles.empty}`}>{t('deck.empty')}</p>}
        {renaming && (
          <RenameSessionDialog
            session={renaming}
            onClose={() => setRenamingId(null)}
            {...(canPronounce
              ? {
                  onFixPronunciation: () => {
                    setRenamingId(null);
                    setPronouncingId(renaming.id);
                  },
                }
              : {})}
          />
        )}
        {pronounceDialog}
        {importSheet}
        {removal.dialog}
      </section>
    );
  }

  return (
    <section className={styles.deck} aria-labelledby="deck-title">
      <div className={styles.head}>
        <h2 id="deck-title" className={styles.title}>
          {t('deck.title')}
        </h2>
        {attention > 0 && (
          <span className={styles.attention}>{t('deck.attentionCount', { count: attention })}</span>
        )}
        {importButton}
        <button type="button" className="btn btn-primary btn-sm" onClick={onNewSession}>
          <Icon name="plus" size={16} />
          {t('deck.new')}
        </button>
      </div>
      {filters}
      {sessions.length === 0 ? (
        <p className="muted">{t('deck.empty')}</p>
      ) : (
        <ul className={styles.list}>
          {sessions.map((s) => (
            <SessionCard
              key={s.id}
              session={s}
              index={sessionNumber(state, s.id) ?? 0}
              preview={s.preview ?? ''}
              selected={s.id === state.selectedId}
              onSelect={select}
              onRename={setRenamingId}
              {...(canReopen(s) ? { onReopen } : {})}
              {...removeProps(s)}
              {...(canPronounce ? { onFixPronunciation: setPronouncingId } : {})}
              waitingOnYou={waitingOnYou(state.tasks, s.id)}
              {...(bringToFront && s.origin.type === 'local'
                ? { onBringToFront: () => void bringToFront(s.id, s.display_name) }
                : {})}
              {...(canReorder && sessions.length > 1 ? { reorder: reorderOf(s.id) } : {})}
            />
          ))}
        </ul>
      )}
      {renaming && (
        <RenameSessionDialog
          session={renaming}
          onClose={() => setRenamingId(null)}
          {...(canPronounce
            ? {
                onFixPronunciation: () => {
                  setRenamingId(null);
                  setPronouncingId(renaming.id);
                },
              }
            : {})}
        />
      )}
      {pronounceDialog}
      {importSheet}
      {removal.dialog}
    </section>
  );
}
