import {
  createContext,
  type ReactNode,
  useContext,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react';

import { useStickToBottom } from '../hooks/useStickToBottom';
import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import { middleEllipsis } from '../protocol/helpers';
import type { SessionEvent } from '../protocol/types';
import type { SentAttachment, SentPrompt, SentStatus } from '../state/reducer';
import type { ActivityKind, ActivityStatus } from './chatActivity';
import {
  countMatches,
  firstLine,
  indexMatches,
  messageParts,
  splitMatches,
} from './chatSearchIndex';
import { ChatProgressLine } from './ChatProgressLine';
import type { ChatProgressInfo } from './chatProgress';
import { buildTimeline } from './chatTimeline';
import { copyText, splitFences } from './clipboard';
import { Icon, type IconName } from './Icon';
import { hasMarkdown, parseMarkdown, type Block, type Inline } from './markdown';
import { SendToButton } from './SendToSession';
import { useShell } from './ShellProvider';
import { stateKey } from './stateKey';
import styles from './ChatView.module.css';

const NO_PROMPTS: SentPrompt[] = [];

/** How long the copy button shows its result. */
export const COPY_FEEDBACK_MS = 1500;

const SENT_STATUS: Record<SentStatus, MessageKey> = {
  sending: 'chat.sending',
  queued: 'chat.queued',
  delivered: 'chat.delivered',
  acknowledged: 'chat.acknowledged',
  failed: 'chat.failed',
};

const ACTIVITY_LABEL: Record<ActivityKind, MessageKey> = {
  subagent: 'chat.activity.subagent',
  task: 'chat.activity.task',
  handback: 'chat.activity.handback',
  peer: 'chat.activity.peer',
  summary: 'chat.activity.summary',
  command: 'chat.activity.command',
  marker: 'chat.activity.system',
  system: 'chat.activity.system',
};

const ACTIVITY_ICON: Record<ActivityKind, IconName> = {
  subagent: 'agent',
  task: 'terminal',
  handback: 'agent',
  peer: 'chat',
  summary: 'list',
  command: 'terminal',
  marker: 'info',
  system: 'info',
};

const STATUS_LABEL: Record<ActivityStatus, MessageKey> = {
  started: 'chat.status.started',
  running: 'chat.status.running',
  completed: 'chat.status.completed',
  failed: 'chat.status.failed',
  stopped: 'chat.status.stopped',
};

/** Closer than this to the top (px) loads the previous page. */
export const LOAD_OLDER_DISTANCE = 160;

export interface ChatViewProps {
  events: SessionEvent[];
  gapFrom: number | null;
  /** The first page is on its way: show a placeholder instead of "no activity". */
  loading?: boolean;
  /** Earlier events can still be fetched (`false`: the beginning was reached). */
  more?: boolean;
  /** A page of earlier events is being fetched. */
  loadingOlder?: boolean;
  /** Fetches the previous page; it is put in front without moving the view. */
  onLoadOlder?: () => void;
  /** Prompts sent from this client (local echo). */
  sent?: SentPrompt[];
  /**
   * The agent has no chat transcript: show its terminal output as plain-text
   * bubbles plus a hint pointing at the terminal view.
   */
  outputFallback?: boolean;
  /** Switches the live view to the terminal (shown in the fallback hint). */
  onShowTerminal?: () => void;
  /** Chat search: the (debounced) query; empty for none. */
  searchQuery?: string;
  /** Index of the current match (0-based). */
  searchIndex?: number;
  /** Reports the number of matches whenever it changes. */
  onSearchCount?: (count: number) => void;
  /** "Add to to-do" on agent messages (absent: no button). */
  onAddTask?: (text: string, seq: number) => void;
  /** A to-do's source message: scrolled to, focused and highlighted. */
  highlightSeq?: number | null;
  /** The agent works on a prompt: the "Working · 42s" line under the chat. */
  progress?: ChatProgressInfo | null;
}

interface SearchState {
  query: string;
  current: number;
}

const SearchContext = createContext<SearchState>({ query: '', current: -1 });

/** `text` with search matches marked; `base` is the global index of its first match. */
function Hl({ text, base = 0 }: { text: string; base?: number }) {
  const { query, current } = useContext(SearchContext);
  if (!query) return <>{text}</>;
  const parts = splitMatches(text, query);
  if (parts.length === 1 && parts[0]!.match === null) return <>{text}</>;
  return (
    <>
      {parts.map((p, i) =>
        p.match === null ? (
          <span key={i}>{p.text}</span>
        ) : (
          <mark
            key={i}
            className={styles.hit}
            data-search-index={base + p.match}
            data-current={base + p.match === current || undefined}
          >
            {p.text}
          </mark>
        ),
      )}
    </>
  );
}

/** Global match index of each text part (in render order). */
function partBases(parts: string[], base: number, query: string): number[] {
  const out: number[] = [];
  let n = base;
  for (const part of parts) {
    out.push(n);
    if (query) n += countMatches(part, query);
  }
  return out;
}

/** Copies `text` and briefly shows a checkmark (or a failure mark). */
export function CopyButton({
  text,
  label,
  className,
}: {
  text: string;
  label: string;
  className?: string;
}) {
  const { t } = useI18n();
  const [state, setState] = useState<'idle' | 'copied' | 'failed'>('idle');
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const shown =
    state === 'copied' ? t('chat.copied') : state === 'failed' ? t('chat.copyFailed') : label;
  return (
    <button
      type="button"
      className={`${styles.copy} ${className ?? ''}`}
      data-state={state}
      aria-label={shown}
      title={shown}
      onClick={() => {
        void copyText(text).then((ok) => {
          setState(ok ? 'copied' : 'failed');
          clearTimeout(timer.current);
          timer.current = setTimeout(() => setState('idle'), COPY_FEEDBACK_MS);
        });
      }}
    >
      <Icon name={state === 'copied' ? 'check' : state === 'failed' ? 'close' : 'copy'} size={14} />
      <span className="visually-hidden" aria-live="polite">
        {state === 'idle' ? '' : shown}
      </span>
    </button>
  );
}

/** Code block languages that hold shell commands (empty: unspecified). */
const SHELL_LANGS = new Set([
  '',
  'sh',
  'bash',
  'zsh',
  'shell',
  'console',
  'terminal',
  'powershell',
  'ps',
  'ps1',
  'pwsh',
  'cmd',
  'bat',
  'batch',
]);

/** Strips a leading `$ ` / `> ` / `PS> ` prompt from a one-line command. */
function commandOf(code: string): string | null {
  const line = code.trim();
  if (!line || line.includes('\n') || line.includes('\r')) return null;
  return line.replace(/^(\$|>|PS[^>]*>)\s+/, '');
}

/** A fenced code block: language, "send to terminal" for one-line commands, copy. */
function CodeBlock({ lang, code, base }: { lang: string; code: string; base: number }) {
  const { t } = useI18n();
  const shell = useShell();
  const command = commandOf(code);
  return (
    <div className={styles.code}>
      <div className={styles.codeBar}>
        <span className={styles.codeLang}>{lang}</span>
        <span className={styles.codeActions}>
          {shell?.allowed && SHELL_LANGS.has(lang.toLowerCase()) && command !== null && (
            <button
              type="button"
              className={styles.copy}
              aria-label={t('chat.sendToTerminal')}
              title={t('chat.sendToTerminal')}
              onClick={() => shell.runInShell({ command })}
            >
              <Icon name="terminal" size={14} />
            </button>
          )}
          <SendToButton text={code} className={styles.copy} />
          <CopyButton text={code} label={t('chat.copyCode')} />
        </span>
      </div>
      <pre className={styles.codeText}>
        <code>
          <Hl text={code} base={base} />
        </code>
      </pre>
    </div>
  );
}

/** Message text as typed: prose and fenced code blocks, no other formatting. */
function MessageText({ text, base = 0 }: { text: string; base?: number }) {
  const { query } = useContext(SearchContext);
  const parts = useMemo(() => splitFences(text), [text]);
  const bases = useMemo(
    () => (query ? partBases(messageParts(text), base, query) : []),
    [text, base, query],
  );
  return (
    <>
      {parts.map((part, i) =>
        part.type === 'text' ? (
          <p key={i} className={styles.text}>
            <Hl text={part.text} base={bases[i]} />
          </p>
        ) : (
          <CodeBlock key={i} lang={part.lang} code={part.code} base={bases[i] ?? 0} />
        ),
      )}
    </>
  );
}

/**
 * Agent text rendered as markdown (see `markdown.ts`). Text runs are
 * highlighted in the order `markdownRuns` lists them, so search numbering
 * matches. Links open outside the app (a new browser window), never in the
 * app's own view.
 */
function MarkdownText({ text, base = 0 }: { text: string; base?: number }) {
  const { query } = useContext(SearchContext);
  const blocks = useMemo(() => parseMarkdown(text), [text]);
  // Render-order counter: each run takes the next match indexes.
  const counter = { n: base };
  const run = (value: string, key: number | string) => {
    const at = counter.n;
    if (query) counter.n += countMatches(value, query);
    return <Hl key={key} text={value} base={at} />;
  };
  const inline = (nodes: Inline[]): ReactNode[] =>
    nodes.map((node, i) => {
      switch (node.type) {
        case 'text':
          return run(node.text, i);
        case 'code':
          return (
            <code key={i} className={styles.inlineCode}>
              {run(node.text, 'c')}
            </code>
          );
        case 'strong':
          return <strong key={i}>{inline(node.children)}</strong>;
        case 'em':
          return <em key={i}>{inline(node.children)}</em>;
        case 'del':
          return <del key={i}>{inline(node.children)}</del>;
        case 'link':
          return (
            <a key={i} href={node.href} target="_blank" rel="noopener noreferrer">
              {inline(node.children)}
            </a>
          );
      }
    });
  const block = (b: Block, i: number): ReactNode => {
    switch (b.type) {
      case 'p':
        return (
          <p key={i} className={styles.text}>
            {inline(b.inline)}
          </p>
        );
      case 'h': {
        const Tag = `h${Math.min(6, b.level + 2)}` as 'h3';
        return (
          <Tag key={i} className={styles.mdHeading} data-level={b.level}>
            {inline(b.inline)}
          </Tag>
        );
      }
      case 'code': {
        const at = counter.n;
        if (query) counter.n += countMatches(b.code, query);
        return <CodeBlock key={i} lang={b.lang} code={b.code} base={at} />;
      }
      case 'hr':
        return <hr key={i} className={styles.mdRule} />;
      case 'quote':
        return (
          <blockquote key={i} className={styles.mdQuote}>
            {b.blocks.map(block)}
          </blockquote>
        );
      case 'list': {
        const items = b.items.map((item, j) => <li key={j}>{item.map(block)}</li>);
        return b.ordered ? (
          <ol key={i} className={styles.mdList} start={b.start === 1 ? undefined : b.start}>
            {items}
          </ol>
        ) : (
          <ul key={i} className={styles.mdList}>
            {items}
          </ul>
        );
      }
      case 'table':
        return (
          // Wide tables scroll inside the bubble; the page never widens.
          <div key={i} className={styles.mdTableWrap} tabIndex={0} role="region">
            <table className={styles.mdTable}>
              <thead>
                <tr>
                  {b.head.map((cell, c) => (
                    <th key={c} style={{ textAlign: b.align[c] ?? undefined }}>
                      {inline(cell)}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {b.rows.map((row, r) => (
                  <tr key={r}>
                    {row.map((cell, c) => (
                      <td key={c} style={{ textAlign: b.align[c] ?? undefined }}>
                        {inline(cell)}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        );
    }
  };
  return <div className={styles.markdown}>{blocks.map(block)}</div>;
}

/** Thumbnails and file chips of the attachments a prompt went out with. */
function SentFiles({ files }: { files?: SentAttachment[] }) {
  const { t } = useI18n();
  if (!files || files.length === 0) return null;
  return (
    <ul className={styles.sentFiles} aria-label={t('chat.sentAttachments')}>
      {files.map((a, i) => (
        <li key={i} className={styles.sentFile} title={a.name}>
          {a.thumbUrl ? (
            <img className={styles.sentThumb} src={a.thumbUrl} alt={a.name} />
          ) : (
            <>
              <span className={styles.sentFileIcon} aria-hidden="true">
                {a.kind === 'folder' ? '▤' : '▢'}
              </span>
              <span className={styles.sentFileName}>{middleEllipsis(a.name, 22)}</span>
            </>
          )}
        </li>
      ))}
    </ul>
  );
}

/** Switches a message between rendered markdown and its source. */
function SourceToggle({ source, onToggle }: { source: boolean; onToggle: () => void }) {
  const { t } = useI18n();
  const label = source ? t('chat.showFormatted') : t('chat.showSource');
  return (
    <button
      type="button"
      className={styles.copy}
      aria-pressed={source}
      aria-label={label}
      title={label}
      onClick={onToggle}
    >
      <Icon name={source ? 'chat' : 'terminal'} size={14} />
    </button>
  );
}

function StatusChip({ status }: { status: ActivityStatus | null }) {
  const { t } = useI18n();
  if (!status) return null;
  return (
    <span className={styles.status} data-status={status}>
      {t(STATUS_LABEL[status])}
    </span>
  );
}

/**
 * De-emphasised row for activity the person did not type (sub-agents,
 * notifications, tool output). Collapsed to one line; expands on click.
 */
function ActivityRow({
  icon,
  label,
  title,
  status = null,
  body,
  markdown = false,
  tone,
  onExpand,
  base = 0,
  forceOpen = false,
}: {
  icon: IconName;
  label: string;
  title: string;
  status?: ActivityStatus | null;
  body: string;
  /** Render the body as markdown (agent prose); tool output stays plain. */
  markdown?: boolean;
  tone?: 'agent' | 'error';
  onExpand: () => void;
  /** Global index of the row's first search match. */
  base?: number;
  /** The current search match is in the body: show it. */
  forceOpen?: boolean;
}) {
  const { t } = useI18n();
  const id = useId();
  const { query } = useContext(SearchContext);
  const [openState, setOpen] = useState(false);
  const open = openState || forceOpen;
  const bodyBase = base + (query ? countMatches(title, query) : 0);
  const summary: ReactNode = (
    <>
      <Icon name={icon} size={14} className={styles.activityIcon} />
      <span className={styles.activityLabel}>{label}</span>
      <span className={styles.activityTitle}>
        <Hl text={title} base={base} />
      </span>
      <StatusChip status={status} />
    </>
  );
  return (
    <li className={styles.activity} data-tone={tone} data-open={open || undefined}>
      {body ? (
        <button
          type="button"
          className={styles.activityHead}
          aria-expanded={open}
          aria-controls={id}
          onClick={() => {
            if (!open) onExpand();
            setOpen(!open);
          }}
        >
          {summary}
          <Icon name="chevron" size={14} className={styles.chevron} />
        </button>
      ) : (
        <div className={styles.activityHead}>{summary}</div>
      )}
      {body && open && (
        <div id={id} className={styles.activityBody}>
          <CopyButton text={body} label={t('chat.copy')} className={styles.bodyCopy} />
          {markdown ? (
            <MarkdownText text={body} base={bodyBase} />
          ) : (
            <MessageText text={body} base={bodyBase} />
          )}
        </div>
      )}
    </li>
  );
}

/** Items put in front of the one that used to be first. */
function prependedBefore(items: { key: string }[], first: string | undefined): number {
  if (first === undefined || items[0]?.key === first) return 0;
  const at = items.findIndex((i) => i.key === first);
  return at > 0 ? at : 0;
}

/** Seq of the oldest loaded event. */
function firstSeq(events: SessionEvent[]): number {
  return events[0]?.seq ?? 0;
}

/** Structured view of a session: messages, tool calls and decisions (MON-02). */
export function ChatView({
  events,
  gapFrom,
  loading = false,
  more,
  loadingOlder = false,
  onLoadOlder,
  sent = NO_PROMPTS,
  outputFallback = false,
  onShowTerminal,
  searchQuery = '',
  searchIndex = 0,
  onSearchCount,
  onAddTask,
  highlightSeq = null,
  progress = null,
}: ChatViewProps) {
  const { t } = useI18n();
  const items = useMemo(
    () => buildTimeline(events, sent, { includeOutput: outputFallback }),
    [events, sent, outputFallback],
  );
  const query = searchQuery.trim();
  /** Messages shown as their markdown source instead of formatted. */
  const [sourceKeys, setSourceKeys] = useState<ReadonlySet<string>>(() => new Set());
  const toggleSource = (key: string) =>
    setSourceKeys((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  const found = useMemo(() => indexMatches(items, query, sourceKeys), [items, query, sourceKeys]);
  const current = found.total > 0 ? ((searchIndex % found.total) + found.total) % found.total : -1;
  const search = useMemo(() => ({ query, current }), [query, current]);
  /** Key of the item whose collapsible body holds the current match. */
  const openKey = useMemo(() => {
    if (current < 0) return null;
    for (const [key, m] of found.byKey) {
      if (current >= m.base + m.head && current < m.base + m.head + m.body) return key;
    }
    return null;
  }, [found, current]);
  const baseOf = (key: string) => found.byKey.get(key)?.base ?? 0;
  const onCountRef = useRef(onSearchCount);
  useEffect(() => {
    onCountRef.current = onSearchCount;
  });
  useEffect(() => {
    onCountRef.current?.(found.total);
  }, [found.total]);
  // Output bubbles grow in place, so follow the text length as well.
  const last = items[items.length - 1];
  // A page of earlier history is not news: it must neither count as unseen
  // messages nor move the view (see the anchoring below).
  const firstKey = items[0]?.key;
  const [seen, setSeen] = useState({ first: firstKey, prepended: 0 });
  const prependedNow = prependedBefore(items, seen.first);
  if (seen.first !== firstKey) {
    // Derived during render, so the hook below never sees the jump in count.
    setSeen({ first: firstKey, prepended: seen.prepended + prependedNow });
  }
  const newest = items.length - seen.prepended - prependedNow;
  // The progress line under the chat (and its step) is followed too.
  const progressKey = progress
    ? `${progress.phase}:${progress.activity?.type ?? ''}:${
        progress.activity?.type === 'tool' ? progress.activity.summary : ''
      }`
    : '';
  const tail = `${newest}:${last?.kind === 'output' ? last.text.length : 0}:${progressKey}`;
  const { scrollRef, contentRef, onScroll, stuck, unseen, jumpToLatest, release } =
    useStickToBottom<HTMLDivElement, HTMLOListElement>(newest, tail);
  // Keep what the person reads where it is when earlier items are put above.
  const layout = useRef<{ first: string | undefined; height: number }>({
    first: firstKey,
    height: 0,
  });
  useLayoutEffect(() => {
    const el = scrollRef.current;
    const prev = layout.current;
    const prepended = prependedBefore(items, prev.first) > 0;
    if (el && prepended) {
      el.scrollTop += el.scrollHeight - prev.height;
    }
    layout.current = { first: firstKey, height: el?.scrollHeight ?? 0 };
  });
  const canLoadOlder = !!onLoadOlder && more !== false && gapFrom !== null && !loading;
  const loadOlderRef = useRef(onLoadOlder);
  useEffect(() => {
    loadOlderRef.current = onLoadOlder;
  });
  // A short page that does not fill the screen cannot be scrolled up: fetch more.
  useEffect(() => {
    const el = scrollRef.current;
    if (!canLoadOlder || loadingOlder || !el) return;
    if (el.scrollHeight <= el.clientHeight + 1) loadOlderRef.current?.();
  }, [canLoadOlder, loadingOlder, items.length, scrollRef]);
  const handleScroll = () => {
    onScroll();
    const el = scrollRef.current;
    if (canLoadOlder && !loadingOlder && el && el.scrollTop < LOAD_OLDER_DISTANCE) {
      onLoadOlder?.();
    }
  };
  // Show the current match: pause following new output and center it.
  useEffect(() => {
    if (current < 0) return;
    release();
    const el = scrollRef.current?.querySelector<HTMLElement>(
      `mark[data-search-index="${current}"]`,
    );
    el?.scrollIntoView?.({ block: 'center' });
  }, [current, query, openKey, release, scrollRef]);
  // A to-do's source message: stop following new output, center and focus it.
  const highlightShown = highlightSeq !== null && events.some((e) => e.seq === highlightSeq);
  useEffect(() => {
    if (!highlightShown) return;
    release();
    const el = scrollRef.current?.querySelector<HTMLElement>(`[data-seq="${highlightSeq}"]`);
    el?.scrollIntoView?.({ block: 'center' });
    el?.focus({ preventScroll: true });
  }, [highlightShown, highlightSeq, release, scrollRef]);
  const jumpLabel =
    unseen > 0
      ? `${t('chat.jumpToLatest')} (${t('chat.newMessages', { count: unseen })})`
      : t('chat.jumpToLatest');

  return (
    <SearchContext.Provider value={search}>
      <div className={styles.wrap}>
        <div
          ref={scrollRef}
          className={styles.scroller}
          onScroll={handleScroll}
          data-testid="chat-scroller"
        >
          <ol ref={contentRef} className={styles.chat} role="log" aria-label={t('live.log')}>
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
            {loading && (
              <li className={styles.skeleton} role="status" aria-label={t('chat.loading')}>
                <span />
                <span />
                <span />
              </li>
            )}
            {!loading && gapFrom !== null && !onLoadOlder && (
              <li className={styles.gap} role="note">
                {t('live.gap', { seq: gapFrom })}
              </li>
            )}
            {!loading && gapFrom !== null && onLoadOlder && more !== false && (
              <li className={styles.older}>
                {loadingOlder ? (
                  <span role="status">{t('chat.loadingOlder')}</span>
                ) : (
                  <button type="button" className="btn btn-sm" onClick={onLoadOlder}>
                    {t('chat.loadOlder')}
                  </button>
                )}
              </li>
            )}
            {!loading && gapFrom !== null && onLoadOlder && more === false && (
              <li className={styles.gap} role="note">
                {firstSeq(events) > 1
                  ? t('live.gap', { seq: firstSeq(events) })
                  : t('chat.beginning')}
              </li>
            )}
            {!loading && items.length === 0 && <li className="muted">{t('live.empty')}</li>}
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
                    <span className={styles.floatCopy}>
                      <SendToButton text={prompt.text} className={styles.copy} />
                      <CopyButton text={prompt.text} label={t('chat.copy')} />
                    </span>
                    <SentFiles files={prompt.attachments} />
                    <MessageText text={prompt.text} base={baseOf(item.key)} />
                    <span
                      className={styles.sentStatus}
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
                    <div className={styles.msgHead}>
                      <span className={styles.role}>{t('chat.agentOutput')}</span>
                      <span className={styles.msgActions}>
                        <SendToButton text={item.text} className={styles.copy} />
                        <CopyButton text={item.text} label={t('chat.copy')} />
                      </span>
                    </div>
                    <pre className={styles.outputText}>
                      {item.truncated && '…'}
                      <Hl text={item.text} base={baseOf(item.key)} />
                    </pre>
                  </li>
                );
              }
              if (item.kind === 'activity') {
                const { activity } = item;
                if (activity.kind === 'marker') {
                  return (
                    <li key={item.key} className={styles.marker}>
                      <Hl text={activity.title} base={baseOf(item.key)} />
                    </li>
                  );
                }
                return (
                  <ActivityRow
                    key={item.key}
                    icon={ACTIVITY_ICON[activity.kind]}
                    label={t(ACTIVITY_LABEL[activity.kind])}
                    title={activity.title}
                    status={activity.status}
                    body={activity.body}
                    markdown
                    base={baseOf(item.key)}
                    forceOpen={openKey === item.key}
                    tone={
                      activity.status === 'failed'
                        ? 'error'
                        : activity.kind === 'subagent' || activity.kind === 'handback'
                          ? 'agent'
                          : undefined
                    }
                    onExpand={release}
                  />
                );
              }
              if (item.kind === 'subagent') {
                return (
                  <ActivityRow
                    key={item.key}
                    icon="agent"
                    label={t('chat.activity.subagent')}
                    title={item.description || item.tool}
                    status={item.status}
                    body={item.report}
                    markdown
                    base={baseOf(item.key)}
                    forceOpen={openKey === item.key}
                    tone={item.status === 'failed' ? 'error' : 'agent'}
                    onExpand={release}
                  />
                );
              }
              if (item.kind === 'tool_result') {
                const text = item.isError ? item.text.replace(/^error:\s*/, '') : item.text;
                return (
                  <ActivityRow
                    key={item.key}
                    icon="terminal"
                    label={
                      item.isError
                        ? t('chat.activity.error')
                        : item.tool
                          ? `${item.tool} · ${t('chat.activity.result')}`
                          : t('chat.activity.result')
                    }
                    title={firstLine(text)}
                    body={text}
                    base={baseOf(item.key)}
                    forceOpen={openKey === item.key}
                    tone={item.isError ? 'error' : undefined}
                    onExpand={release}
                  />
                );
              }
              const e = item.event;
              switch (e.type) {
                case 'message': {
                  // People's own messages stay plain; agent and system text is markdown.
                  const formatted = e.role !== 'user';
                  const source = sourceKeys.has(item.key);
                  const anchor = e.seq === highlightSeq;
                  return (
                    <li
                      key={item.key}
                      className={`${styles.msg} ${styles[e.role]}`}
                      data-seq={e.seq}
                      data-anchor={anchor ? 'true' : undefined}
                      aria-current={anchor ? 'true' : undefined}
                      tabIndex={anchor ? -1 : undefined}
                    >
                      <div className={styles.msgHead}>
                        <span className={styles.role}>{t(`role.${e.role}`)}</span>
                        <span className={styles.msgActions}>
                          {formatted && hasMarkdown(e.text) && (
                            <SourceToggle source={source} onToggle={() => toggleSource(item.key)} />
                          )}
                          {onAddTask && e.role === 'assistant' && (
                            <button
                              type="button"
                              className={styles.copy}
                              aria-label={t('tasks.addFromMessage')}
                              title={t('tasks.addFromMessage')}
                              onClick={() => onAddTask(e.text, e.seq)}
                            >
                              <Icon name="bookmark" size={14} />
                            </button>
                          )}
                          <SendToButton text={e.text} className={styles.copy} />
                          <CopyButton text={e.text} label={t('chat.copy')} />
                        </span>
                      </div>
                      <SentFiles files={item.attachments} />
                      {formatted && !source ? (
                        <MarkdownText text={e.text} base={baseOf(item.key)} />
                      ) : (
                        <MessageText text={e.text} base={baseOf(item.key)} />
                      )}
                    </li>
                  );
                }
                case 'tool_call':
                  return (
                    <li key={item.key} className={styles.tool}>
                      <span className={styles.toolName}>
                        <Hl text={e.name} base={baseOf(item.key)} />
                      </span>
                      <span className={styles.toolSummary}>
                        <Hl
                          text={e.summary}
                          base={baseOf(item.key) + (query ? countMatches(e.name, query) : 0)}
                        />
                      </span>
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
                      <p className={styles.text}>
                        <Hl text={e.decision.prompt} base={baseOf(item.key)} />
                      </p>
                      <ul className={styles.options}>
                        {e.decision.options.map((o, i, all) => (
                          <li key={o.id}>
                            <Hl
                              text={o.label}
                              base={
                                baseOf(item.key) +
                                (query
                                  ? countMatches(e.decision.prompt, query) +
                                    all
                                      .slice(0, i)
                                      .reduce((n, x) => n + countMatches(x.label, query), 0)
                                  : 0)
                              }
                            />
                          </li>
                        ))}
                      </ul>
                    </li>
                  );
                case 'notification':
                  return (
                    <li key={item.key} className={styles.notification}>
                      <Icon name="bell" size={16} />
                      <span>
                        <strong>
                          <Hl text={e.title} base={baseOf(item.key)} />
                        </strong>{' '}
                        <Hl
                          text={e.body}
                          base={baseOf(item.key) + (query ? countMatches(e.title, query) : 0)}
                        />
                      </span>
                    </li>
                  );
                default:
                  return null;
              }
            })}
          </ol>
          {progress && !loading && <ChatProgressLine progress={progress} />}
        </div>
        {!stuck && (
          <button
            type="button"
            className={styles.jump}
            onClick={jumpToLatest}
            aria-label={jumpLabel}
          >
            <Icon name="down" size={16} />
            {unseen > 0 && (
              <span className={styles.jumpCount}>{t('chat.newMessages', { count: unseen })}</span>
            )}
          </button>
        )}
      </div>
    </SearchContext.Provider>
  );
}
