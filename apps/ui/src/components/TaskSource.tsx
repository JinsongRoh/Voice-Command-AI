/**
 * Where a to-do came from: "원본 대화 보기" (the session's chat, scrolled to
 * the source message), "앞뒤 메시지 보기" (a sheet with the message and a few
 * around it, from the timeline API), the stored excerpt when the session or
 * the message is gone, and the copy buttons of a task.
 */
import { useCallback, useEffect, useRef, useState } from 'react';

import { useI18n } from '../i18n/context';
import type { HumanTask } from '../protocol/tasks';
import type { SessionId } from '../protocol/types';
import {
  clearChatFocus,
  requestChatFocus,
  resolveChatFocus,
  useChatFocus,
  type ChatFocus,
} from '../state/chatFocus';
import type { SessionView } from '../state/reducer';
import { useActions, useAppState } from '../state/store';
import { hasSource, messagesAround, taskText, type ContextMessage } from '../state/taskSource';
import { CopyButton } from './ChatView';
import { copyText } from './clipboard';
import { Icon } from './Icon';
import { Modal } from './Modal';
import styles from './TaskSource.module.css';

/** Events fetched for the sheet (tool calls and state changes sit between messages). */
const CONTEXT_EVENTS = 60;
/** Pages "원본 대화 보기" may load while looking for the message. */
const MAX_ANCHOR_PAGES = 80;

/** Copies text and confirms with a toast ("복사했습니다"). */
// eslint-disable-next-line react-refresh/only-export-components
export function useCopyWithToast(): (text: string) => Promise<boolean> {
  const { t } = useI18n();
  const actions = useActions();
  return useCallback(
    async (text: string) => {
      const ok = await copyText(text);
      actions.showToast({
        id: `task-copy:${Date.now()}`,
        category: ok ? 'completed' : 'error',
        sessionId: null,
        title: t(ok ? 'tasks.source.copied' : 'tasks.source.copyFailed'),
        body: '',
        security: true,
      });
      return ok;
    },
    [actions, t],
  );
}

type Loaded =
  { state: 'loading' } | { state: 'found'; messages: ContextMessage[] } | { state: 'missing' };

/** Fetches the messages around a task's source message (none: `missing`). */
function useSourceContext(sessionId: SessionId | null, seq: number | null, skip: boolean): Loaded {
  const actions = useActions();
  const known = useAppState((s) => (sessionId ? !!s.sessions[sessionId] : false));
  const fetchable = !skip && !!sessionId && seq !== null && known;
  const key = `${sessionId}:${seq}`;
  const [result, setResult] = useState<{ key: string; loaded: Loaded } | null>(null);
  useEffect(() => {
    if (!fetchable || !sessionId || seq === null) return;
    let live = true;
    actions
      .fetchHistory(sessionId, seq + CONTEXT_EVENTS / 2, CONTEXT_EVENTS)
      .then((events) => {
        if (!live) return;
        const messages = messagesAround(events, seq);
        setResult({ key, loaded: messages ? { state: 'found', messages } : { state: 'missing' } });
      })
      .catch(() => live && setResult({ key, loaded: { state: 'missing' } }));
    return () => {
      live = false;
    };
  }, [actions, fetchable, key, sessionId, seq]);
  if (!fetchable) return { state: 'missing' };
  return result?.key === key ? result.loaded : { state: 'loading' };
}

/** The stored excerpt with the note that the message is gone. */
function Excerpt({ excerpt }: { excerpt: string | null | undefined }) {
  const { t } = useI18n();
  return (
    <div className={styles.missing}>
      <p role="note" className={styles.note}>
        <Icon name="info" size={14} />
        <span>{t(excerpt ? 'tasks.source.missing' : 'tasks.source.noExcerpt')}</span>
      </p>
      {excerpt && (
        <figure className={styles.excerpt}>
          <blockquote className={styles.text}>{excerpt}</blockquote>
          <CopyButton text={excerpt} label={t('tasks.source.copyMessage')} />
        </figure>
      )}
    </div>
  );
}

/**
 * "앞뒤 메시지 보기": the source message and a few around it. With `missing`
 * (the chat could not find it) only the stored excerpt is shown.
 */
export function TaskSourceSheet({
  sessionId,
  seq,
  excerpt,
  missing = false,
  onOpenChat,
  onClose,
}: {
  sessionId: SessionId | null;
  seq: number | null;
  excerpt: string | null | undefined;
  missing?: boolean;
  /** "채팅에서 열기" (absent: no button). */
  onOpenChat?: () => void;
  onClose: () => void;
}) {
  const { t, relative } = useI18n();
  const loaded = useSourceContext(sessionId, seq, missing);
  const anchorRef = useRef<HTMLLIElement>(null);
  useEffect(() => {
    if (loaded.state !== 'found') return;
    anchorRef.current?.scrollIntoView?.({ block: 'center' });
  }, [loaded.state]);
  return (
    <Modal
      title={t('tasks.source.title')}
      onClose={onClose}
      fullScreen
      wide
      footer={
        <>
          {onOpenChat && loaded.state === 'found' && (
            <button type="button" className="btn" onClick={onOpenChat}>
              <Icon name="chat" size={14} /> {t('tasks.source.openChat')}
            </button>
          )}
          <button type="button" className="btn btn-primary" onClick={onClose}>
            {t('common.close')}
          </button>
        </>
      }
    >
      {loaded.state === 'loading' && (
        <p role="status" className="muted">
          {t('tasks.source.loading')}
        </p>
      )}
      {loaded.state === 'missing' && <Excerpt excerpt={excerpt} />}
      {loaded.state === 'found' && (
        <ol className={styles.context} aria-label={t('tasks.source.title')}>
          {loaded.messages.map((m) => {
            const anchor = m.seq === seq;
            return (
              <li
                key={m.seq}
                ref={anchor ? anchorRef : undefined}
                className={styles.message}
                data-role={m.role}
                data-anchor={anchor ? 'true' : undefined}
                aria-current={anchor ? 'true' : undefined}
              >
                <div className={styles.head}>
                  <span className={styles.role}>{t(`role.${m.role}`)}</span>
                  {anchor && <span className={styles.badge}>{t('tasks.source.anchor')}</span>}
                  <time className="muted" dateTime={new Date(m.at).toISOString()}>
                    {relative(m.at)}
                  </time>
                  <CopyButton text={m.text} label={t('chat.copy')} />
                </div>
                <p className={styles.text}>{m.text}</p>
              </li>
            );
          })}
        </ol>
      )}
    </Modal>
  );
}

/** The source message's full text when it is still there (else the excerpt). */
function useSourceText(task: HumanTask): string | null {
  const actions = useActions();
  const sessionId = task.session_id;
  const seq = task.message_seq;
  const known = useAppState((s) => (sessionId ? !!s.sessions[sessionId] : false));
  const [full, setFull] = useState<string | null>(null);
  useEffect(() => {
    if (!sessionId || seq === null || !known) return;
    let live = true;
    actions
      .fetchHistory(sessionId, seq + 1, 1)
      .then((events) => {
        const e = events.find((x) => x.seq === seq);
        if (live && e?.type === 'message') setFull(e.text);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [actions, sessionId, seq, known]);
  return full ?? task.excerpt ?? null;
}

/**
 * The source and copy buttons of an open task: "원본 대화 보기", "앞뒤
 * 메시지 보기", "할 일 복사" and "원본 메시지 복사".
 */
export function TaskSourceActions({ task }: { task: HumanTask }) {
  const { t } = useI18n();
  const actions = useActions();
  const copy = useCopyWithToast();
  const [sheet, setSheet] = useState(false);
  const sessionId = task.session_id;
  const known = useAppState((s) => (sessionId ? !!s.sessions[sessionId] : false));
  const source = hasSource(task);
  const sourceText = useSourceText(task);
  const canOpenChat = known && sessionId !== null && task.message_seq !== null;

  const openChat = () => {
    if (!sessionId || task.message_seq === null) return;
    requestChatFocus(sessionId, task.message_seq, task.excerpt ?? null);
    actions.openAlert({ sessionId });
  };

  return (
    <div className={styles.actions}>
      {source && (
        <button
          type="button"
          className="btn btn-sm"
          // Without the session the sheet shows the stored excerpt.
          onClick={() => (canOpenChat ? openChat() : setSheet(true))}
        >
          <Icon name="chat" size={14} /> {t('tasks.source.view')}
        </button>
      )}
      {canOpenChat && (
        <button type="button" className="btn btn-sm" onClick={() => setSheet(true)}>
          <Icon name="list" size={14} /> {t('tasks.source.context')}
        </button>
      )}
      <button type="button" className="btn btn-sm" onClick={() => void copy(taskText(task))}>
        <Icon name="copy" size={14} /> {t('tasks.source.copyTask')}
      </button>
      {source && sourceText && (
        <button type="button" className="btn btn-sm" onClick={() => void copy(sourceText)}>
          <Icon name="copy" size={14} /> {t('tasks.source.copyMessage')}
        </button>
      )}
      {sheet && (
        <TaskSourceSheet
          sessionId={known ? sessionId : null}
          seq={task.message_seq}
          excerpt={task.excerpt}
          onOpenChat={
            canOpenChat
              ? () => {
                  setSheet(false);
                  openChat();
                }
              : undefined
          }
          onClose={() => setSheet(false)}
        />
      )}
    </div>
  );
}

/**
 * The session view's side of "원본 대화 보기": pages in earlier history until
 * the source message is loaded, then returns its seq to highlight; when it
 * cannot be found, `missing` holds the request (its excerpt is shown).
 */
// eslint-disable-next-line react-refresh/only-export-components
export function useChatAnchor(
  sessionId: SessionId,
  view: SessionView | undefined,
): {
  /** A request still looking for its message. */
  pending: ChatFocus | null;
  highlight: number | null;
  missing: ChatFocus | null;
  dismissMissing: () => void;
} {
  const actions = useActions();
  const focus = useChatFocus(sessionId);
  const pages = useRef<{ token: number; count: number }>({ token: 0, count: 0 });
  const pending = focus?.status === 'pending' ? focus : null;
  useEffect(() => {
    if (!pending || !view || view.loading) return;
    if (pages.current.token !== pending.token) pages.current = { token: pending.token, count: 0 };
    if (view.events.some((e) => e.seq === pending.seq)) {
      resolveChatFocus(pending.token, 'found');
      return;
    }
    const first = view.events[0]?.seq ?? null;
    const older =
      view.gapFrom !== null &&
      view.more !== false &&
      (first === null || first > pending.seq) &&
      pages.current.count < MAX_ANCHOR_PAGES;
    if (!older) {
      resolveChatFocus(pending.token, 'missing');
      return;
    }
    if (view.loadingOlder) return;
    pages.current.count += 1;
    actions.loadOlder(sessionId).catch(() => resolveChatFocus(pending.token, 'missing'));
  }, [actions, pending, sessionId, view]);
  // Leaving the session ends the request.
  const token = focus?.token;
  useEffect(
    () => () => {
      if (token !== undefined) clearChatFocus(token);
    },
    [token],
  );
  return {
    pending,
    highlight: focus?.status === 'found' ? focus.seq : null,
    missing: focus?.status === 'missing' ? focus : null,
    dismissMissing: () => {
      if (focus) clearChatFocus(focus.token);
    },
  };
}
