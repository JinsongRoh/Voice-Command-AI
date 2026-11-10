import { useEffect, useId, useRef, useState, type KeyboardEvent } from 'react';

import { useI18n } from '../i18n/context';
import { Icon } from './Icon';
import styles from './ChatSearch.module.css';

/** Typing settles for this long before the chat is searched. */
export const SEARCH_DEBOUNCE_MS = 150;

/**
 * Search box for the current session's chat (next to the Chat / Terminal
 * tabs). A search icon opens it (over the tabs row on phones); Enter = next,
 * Shift+Enter = previous, Escape clears and closes.
 */
export function ChatSearch({
  count,
  index,
  onQuery,
  onIndex,
  canSearchOlder = false,
  searchingOlder = false,
  onSearchOlder,
  onStopSearchOlder,
  initialOpen = false,
  onClosed,
}: {
  /** Matches for the current query. */
  count: number;
  /** Current match (0-based). */
  index: number;
  /** Debounced query ('' when closed). */
  onQuery: (query: string) => void;
  onIndex: (index: number) => void;
  /** Earlier messages exist that are not loaded (and so not searched). */
  canSearchOlder?: boolean;
  searchingOlder?: boolean;
  /** Loads earlier pages until the search finds more. */
  onSearchOlder?: () => void;
  /** Stops a running search of earlier pages. */
  onStopSearchOlder?: () => void;
  /** Starts open (the phone top bar opens it from its "more" menu). */
  initialOpen?: boolean;
  /** Called after closing (instead of showing the search icon again). */
  onClosed?: () => void;
}) {
  const { t } = useI18n();
  const id = useId();
  const [open, setOpen] = useState(initialOpen);
  const [text, setText] = useState('');
  const input = useRef<HTMLInputElement>(null);
  const toggle = useRef<HTMLButtonElement>(null);
  const onQueryRef = useRef(onQuery);
  useEffect(() => {
    onQueryRef.current = onQuery;
  });

  useEffect(() => {
    const timer = setTimeout(() => onQueryRef.current(text.trim()), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [text]);

  // Focus the field when it opens, and the search button when it closes.
  const wasOpen = useRef(false);
  useEffect(() => {
    if (open) input.current?.focus();
    else if (wasOpen.current) toggle.current?.focus();
    wasOpen.current = open;
  }, [open]);

  const close = () => {
    setText('');
    onQueryRef.current('');
    if (onClosed) onClosed();
    else setOpen(false);
  };
  const step = (dir: 1 | -1) => {
    if (count === 0) return;
    onIndex((index + dir + count) % count);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    // Enter that confirms an IME composition (Korean, Japanese) is not a search step.
    if (e.nativeEvent.isComposing || e.keyCode === 229) return;
    if (e.key === 'Enter') {
      e.preventDefault();
      step(e.shiftKey ? -1 : 1);
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      close();
    }
  };

  if (!open) {
    return (
      <button
        ref={toggle}
        type="button"
        className={`btn btn-sm ${styles.toggle}`}
        aria-label={t('search.open')}
        title={t('search.open')}
        onClick={() => setOpen(true)}
      >
        <Icon name="search" size={16} />
      </button>
    );
  }

  const status = !text.trim()
    ? ''
    : count === 0
      ? t('search.none')
      : t('search.position', { current: index + 1, total: count });

  return (
    <div className={styles.search} role="search">
      <Icon name="search" size={16} className={styles.icon} />
      <label htmlFor={`${id}-q`} className="visually-hidden">
        {t('search.label')}
      </label>
      <input
        ref={input}
        id={`${id}-q`}
        type="search"
        className={styles.input}
        placeholder={t('search.placeholder')}
        value={text}
        onChange={(e) => {
          setText(e.target.value);
          onIndex(0);
        }}
        onKeyDown={onKeyDown}
        autoComplete="off"
        spellCheck={false}
        enterKeyHint="search"
      />
      <span className={styles.count} aria-live="polite" data-testid="search-count">
        {status}
      </span>
      {canSearchOlder && onSearchOlder && text.trim() && !searchingOlder && (
        <button
          type="button"
          className="btn btn-sm"
          title={t('search.loadedOnly')}
          onClick={onSearchOlder}
        >
          {t('search.older')}
        </button>
      )}
      {searchingOlder && (
        <>
          <span className={styles.count} role="status">
            {t('search.olderBusy')}
          </span>
          {onStopSearchOlder && (
            <button type="button" className="btn btn-sm" onClick={onStopSearchOlder}>
              {t('search.olderStop')}
            </button>
          )}
        </>
      )}
      <button
        type="button"
        className={`icon-btn ${styles.nav}`}
        aria-label={t('search.previous')}
        disabled={count === 0}
        onClick={() => step(-1)}
      >
        <Icon name="up" size={16} />
      </button>
      <button
        type="button"
        className={`icon-btn ${styles.nav}`}
        aria-label={t('search.next')}
        disabled={count === 0}
        onClick={() => step(1)}
      >
        <Icon name="down" size={16} />
      </button>
      <button
        type="button"
        className={`icon-btn ${styles.nav}`}
        aria-label={t('search.close')}
        onClick={close}
      >
        <Icon name="close" size={16} />
      </button>
    </div>
  );
}
