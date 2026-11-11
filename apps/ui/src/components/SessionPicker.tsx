import { useId, useMemo, useRef, useState, type KeyboardEvent } from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import { originLabel, sessionLocation } from '../protocol/helpers';
import type { SessionId, SessionInfo } from '../protocol/types';
import { AgentIcon } from './AgentIcon';
import { Icon } from './Icon';
import { groupSessions, type PickerSection } from './sessionSearch';
import { StateChip } from './StateChip';
import styles from './SessionPicker.module.css';

const SECTION_LABEL: Record<PickerSection, MessageKey> = {
  attention: 'picker.section.attention',
  pinned: 'picker.section.pinned',
  recent: 'picker.section.recent',
  all: 'picker.section.all',
  results: 'picker.section.results',
};

/**
 * Searchable session list: the quick switcher and "Send to session…".
 * Sections (needs attention, pinned, recent, all) while the search is empty,
 * matches when it is not. Arrow keys move between the entries.
 */
export function SessionPicker({
  sessions,
  onPick,
  searchLabel,
  pins = [],
  onTogglePin,
  currentId = null,
  unavailable,
  searchSessions,
}: {
  sessions: SessionInfo[];
  onPick: (session: SessionInfo) => void;
  searchLabel: string;
  pins?: readonly SessionId[];
  /** Shows a pin button per entry (pins are per device). */
  onTogglePin?: (id: SessionId) => void;
  /** The session on screen: marked, still pickable. */
  currentId?: SessionId | null;
  /** Why an entry cannot be picked (`null`: it can); shown under it. */
  unavailable?: (session: SessionInfo) => string | null;
  /** Searched instead of `sessions` while there is a query (a filtered list still finds every session). */
  searchSessions?: SessionInfo[];
}) {
  const { t } = useI18n();
  const id = useId();
  const [query, setQuery] = useState('');
  const listRef = useRef<HTMLDivElement>(null);
  const pool = query.trim() && searchSessions ? searchSessions : sessions;
  const groups = useMemo(() => groupSessions(pool, pins, query), [pool, pins, query]);
  const first = groups.flatMap((g) => g.sessions).find((s) => !unavailable?.(s));

  const entries = () =>
    Array.from(listRef.current?.querySelectorAll<HTMLButtonElement>('[data-pick]') ?? []).filter(
      (b) => !b.disabled,
    );
  const moveFocus = (from: HTMLElement | null, step: 1 | -1) => {
    const list = entries();
    if (list.length === 0) return;
    const at = from ? list.indexOf(from as HTMLButtonElement) : -1;
    const next = at < 0 ? (step > 0 ? 0 : list.length - 1) : at + step;
    if (next < 0) document.getElementById(`${id}-search`)?.focus();
    else list[Math.min(next, list.length - 1)]?.focus();
  };

  const onSearchKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      moveFocus(null, 1);
    } else if (e.key === 'Enter' && !e.nativeEvent.isComposing) {
      e.preventDefault();
      if (first) onPick(first);
    }
  };
  const onListKey = (e: KeyboardEvent<HTMLDivElement>) => {
    const target = e.target as HTMLElement;
    if (!target.matches('[data-pick]')) return;
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      moveFocus(target, e.key === 'ArrowDown' ? 1 : -1);
    }
  };

  return (
    <div className={styles.picker}>
      <div className={styles.search}>
        <Icon name="search" size={16} />
        <label htmlFor={`${id}-search`} className="visually-hidden">
          {searchLabel}
        </label>
        <input
          id={`${id}-search`}
          type="search"
          value={query}
          placeholder={searchLabel}
          autoComplete="off"
          enterKeyHint="go"
          aria-controls={`${id}-list`}
          aria-describedby={`${id}-count`}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={onSearchKey}
        />
      </div>
      <span id={`${id}-count`} className="visually-hidden" role="status" aria-live="polite">
        {query.trim()
          ? t('picker.count', { count: groups.reduce((n, g) => n + g.sessions.length, 0) })
          : ''}
      </span>
      <div id={`${id}-list`} ref={listRef} className={styles.list} onKeyDown={onListKey}>
        {groups.length === 0 && <p className={styles.empty}>{t('picker.none')}</p>}
        {groups.map((g) => (
          <section key={g.section} className={styles.group} aria-labelledby={`${id}-${g.section}`}>
            <h3 id={`${id}-${g.section}`} className={styles.heading}>
              {t(SECTION_LABEL[g.section])}
            </h3>
            <ul className={styles.items}>
              {g.sessions.map((s) => {
                const why = unavailable?.(s) ?? null;
                const pinned = pins.includes(s.id);
                return (
                  <li
                    key={s.id}
                    className={styles.item}
                    data-current={s.id === currentId || undefined}
                  >
                    <button
                      type="button"
                      className={styles.pick}
                      data-pick
                      data-testid={`pick-${s.id}`}
                      disabled={why !== null}
                      aria-current={s.id === currentId ? 'true' : undefined}
                      aria-describedby={why ? `${id}-why-${s.id}` : undefined}
                      onClick={() => onPick(s)}
                    >
                      <AgentIcon origin={s.origin} size={28} />
                      <span className={styles.text}>
                        <span className={styles.name}>{s.display_name}</span>
                        <span className={styles.meta}>
                          <StateChip state={s.state} terminalPrompt={s.terminal_prompt} />
                          <span className={styles.where} title={sessionLocation(s)}>
                            {originLabel(s.origin)} · {sessionLocation(s)}
                          </span>
                        </span>
                        {why && (
                          <span id={`${id}-why-${s.id}`} className={styles.why}>
                            {why}
                          </span>
                        )}
                      </span>
                    </button>
                    {onTogglePin && (
                      <button
                        type="button"
                        className={`icon-btn ${styles.pin}`}
                        aria-pressed={pinned}
                        aria-label={t(pinned ? 'picker.unpin' : 'picker.pin', {
                          name: s.display_name,
                        })}
                        title={t(pinned ? 'picker.unpin' : 'picker.pin', { name: s.display_name })}
                        data-pinned={pinned || undefined}
                        onClick={() => onTogglePin(s.id)}
                      >
                        <Icon name="star" size={16} />
                      </button>
                    )}
                  </li>
                );
              })}
            </ul>
          </section>
        ))}
      </div>
    </div>
  );
}
