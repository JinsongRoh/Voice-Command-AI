import { useEffect, useMemo, useRef, useState, type ClipboardEvent, type ReactNode } from 'react';

import {
  pastedFiles,
  useFileDrag,
  useFileDropTarget,
  type DropProps,
} from '../attachments/fileDrop';
import { fromDirectoryInput, MAX_FILE_BYTES, readDrop, selectFolder } from '../attachments/folder';
import type { PickerPlatform } from '../attachments/platform';
import { isResizable, type ImageCodec } from '../attachments/resize';
import {
  folderProblemText,
  formatBytes,
  MAX_FILES_PER_MESSAGE,
  MAX_IMAGES_PER_MESSAGE,
  useAttachments,
} from '../attachments/useAttachments';
import { useI18n } from '../i18n/context';
import { hasDraft } from '../sketch/drafts';
import { isCloud } from '../protocol/helpers';
import { Client } from '../protocol/transport';
import type { SessionInfo } from '../protocol/types';
import type { SentAttachment } from '../state/reducer';
import { useActions, useAppState, useMachines } from '../state/store';
import { AttachMenu } from './AttachMenu';
import { AttachmentTray } from './AttachmentTray';
import { Icon } from './Icon';
import { SketchSheet } from './SketchSheet';
import styles from './Attachments.module.css';

const AUTO_SEND_KEY = 'vca.attach.autoSend';

function loadAutoSend(): boolean {
  try {
    return globalThis.localStorage?.getItem(AUTO_SEND_KEY) === '1';
  } catch {
    return false;
  }
}

function saveAutoSend(value: boolean) {
  try {
    globalThis.localStorage?.setItem(AUTO_SEND_KEY, value ? '1' : '0');
  } catch {
    // Storage unavailable (private mode): keep the choice for this session only.
  }
}

export interface ComposerAttachmentsOptions {
  /** The composer accepts text: mentions the server did not type go there. */
  canPrefill: boolean;
  /** Adds a mention to the composer text. */
  onMention(mention: string): void;
  platform?: PickerPlatform;
  codec?: ImageCodec;
  /** "Send without it": the failed attachments were removed; send the text now. */
  onSendAnyway?(): void;
  /** Typing with the on-screen keyboard open: the tray is one summary line. */
  compact?: boolean;
}

let toastSeq = 0;

/**
 * A toast raised right after a pick would sit under the closing photo
 * picker (iOS) or appear while the page is hidden: wait until the page is
 * visible again, then a moment for the sheet to go away.
 */
export const PICKER_TOAST_DELAY_MS = 800;

function whenVisible(show: () => void) {
  const later = () => void setTimeout(show, PICKER_TOAST_DELAY_MS);
  if (typeof document === 'undefined' || document.visibilityState !== 'hidden') {
    later();
    return;
  }
  const onBack = () => {
    if (document.visibilityState === 'hidden') return;
    document.removeEventListener('visibilitychange', onBack);
    window.removeEventListener('focus', onBack);
    later();
  };
  document.addEventListener('visibilitychange', onBack);
  window.addEventListener('focus', onBack);
}

/** How a queued send ended: `go` once the uploads finished, `false` when cancelled. */
export interface UploadWait {
  go: boolean;
  /** Names of the attachments that failed (the send goes without them). */
  failed: string[];
  /** Mentions put into the text box while waiting (sessions the server does not type into). */
  mentions: string[];
}

/**
 * Attachments for the composer of a local session: the paperclip menu, the
 * upload chips, drag and drop of files and folders, and pasted files. Cloud
 * sessions have no project folder on the PC, so they get no attach button;
 * drops and pastes follow the paperclip and work only while it does.
 *
 * Inside a session pane (`LiveView`) the whole pane takes the drops; on its
 * own the composer does.
 */
export function useComposerAttachments(
  session: SessionInfo,
  { canPrefill, onMention, platform, codec, onSendAnyway, compact }: ComposerAttachmentsOptions,
): {
  available: boolean;
  button: ReactNode;
  tray: ReactNode;
  dropping: boolean;
  /** Items in the tray, and whether any is still preparing or uploading. */
  count: number;
  busy: boolean;
  /**
   * Call before sending text: `false` (and a prompt to retry or send without
   * them) while an attachment has failed, so the text never goes out silently
   * without the file it refers to.
   */
  allowSend(): boolean;
  /** Attachments ready to go out with the next prompt (uploaded and mentioned). */
  delivered(): { ids: string[]; attachments: SentAttachment[] };
  /** The prompt was delivered: these leave the tray (failed ones stay), notices go. */
  clearDelivered(ids: readonly string[]): void;
  /**
   * Send pressed while attachments are still uploading: resolves when they
   * are done (and their one mention is in), or with `go: false` when the
   * queued send is cancelled.
   */
  waitForUploads(): Promise<UploadWait>;
  /** A send is waiting for the uploads. */
  sendQueued: boolean;
  cancelQueuedSend(): void;
  /** Attachments finished (done or failed) of those in the tray. */
  progress: { done: number; total: number };
  /** A line above the tray until dismissed or the next send. */
  showNotice(text: string): void;
  dropProps: DropProps;
  /** Ctrl+V / Cmd+V of images or files attaches them (text pastes are untouched). */
  onPaste(e: ClipboardEvent): void;
} {
  const { t } = useI18n();
  const { machine } = useMachines();
  const actions = useActions();
  const connected = useAppState((s) => s.connection === 'connected');
  const client = useMemo(() => new Client(machine.transport), [machine.transport]);
  const [autoSend, setAutoSend] = useState(loadAutoSend);
  const [problem, setProblem] = useState<string | null>(null);
  /** Over the per-message caps, failed attachments left out of a send: stays until dismissed or sent. */
  const [notice, setNotice] = useState<string | null>(null);
  const [sendQueued, setSendQueued] = useState(false);
  const waitersRef = useRef<((wait: UploadWait) => void)[]>([]);
  const queuedMentionsRef = useRef<string[]>([]);
  const [sketching, setSketching] = useState(false);
  const available = !isCloud(session.origin);
  const enabled = available && connected && session.state.type !== 'exited';
  const [sendBlocked, setSendBlocked] = useState(false);
  const prefillRef = useRef({ canPrefill, onMention, onSendAnyway });
  useEffect(() => {
    prefillRef.current = { canPrefill, onMention, onSendAnyway };
  });

  const attachments = useAttachments({
    sessionId: session.id,
    request: (body) => client.request(body),
    t,
    // A queued send presses Enter after its text, not after the mention.
    submit: autoSend && !sendQueued,
    codec,
    onAttached: (result) => {
      if (!result.typed && result.mention && prefillRef.current.canPrefill) {
        prefillRef.current.onMention(result.mention);
        if (waitersRef.current.length > 0) queuedMentionsRef.current.push(result.mention);
      }
    },
  });

  const failed = attachments.items.filter((it) => it.status === 'error');
  const failedRef = useRef(failed.length);
  useEffect(() => {
    failedRef.current = failed.length;
  });
  const showSendBlock = sendBlocked && failed.length > 0;

  const allowSend = () => {
    if (failedRef.current === 0) return true;
    setSendBlocked(true);
    return false;
  };

  const itemsRef = useRef(attachments.items);
  useEffect(() => {
    itemsRef.current = attachments.items;
  });
  const delivered = () => {
    const done = itemsRef.current.filter((it) => it.status === 'done');
    return {
      ids: done.map((it) => it.id),
      attachments: done.map((it) => ({ name: it.name, kind: it.kind, thumbUrl: it.thumbUrl })),
    };
  };

  const settleQueued = (go: boolean) => {
    const waiters = waitersRef.current;
    if (waiters.length === 0) return;
    waitersRef.current = [];
    const failed = itemsRef.current.filter((it) => it.status === 'error').map((it) => it.name);
    const mentions = queuedMentionsRef.current;
    queuedMentionsRef.current = [];
    setSendQueued(false);
    for (const resolve of waiters) resolve({ go, failed, mentions });
  };
  // The uploads (and the batch's mention) are done: the queued send goes.
  useEffect(() => {
    if (sendQueued && !attachments.busy) settleQueued(true);
  });

  const waitForUploads = () =>
    new Promise<UploadWait>((resolve) => {
      waitersRef.current.push(resolve);
      setSendQueued(true);
    });

  const progress = {
    done: attachments.items.filter((it) => it.status === 'done' || it.status === 'error').length,
    total: attachments.items.filter((it) => it.status !== 'cancelled').length,
  };

  /** Queues a folder; returns why it was refused, if it was. */
  const queueFolder = (entries: { file: File; relativePath: string }[]): string | null => {
    const { selection } = selectFolder(entries);
    const issue = attachments.addFolder(selection);
    return issue ? folderProblemText(t, issue) : null;
  };

  const addFolderFiles = (entries: { file: File; relativePath: string }[]) => {
    setProblem(queueFolder(entries));
  };

  const onFiles = (files: File[]) => {
    setProblem(null);
    const { droppedImages, droppedFiles } = attachments.addFiles(files);
    const params = { images: MAX_IMAGES_PER_MESSAGE, files: MAX_FILES_PER_MESSAGE };
    const notes: string[] = [];
    const inline: string[] = [];
    if (droppedImages > 0) {
      notes.push(t('attach.limitImages', params));
      inline.push(
        t('attach.limitImagesInline', { max: MAX_IMAGES_PER_MESSAGE, dropped: droppedImages }),
      );
    }
    if (droppedFiles > 0) {
      notes.push(t('attach.limitFiles', params));
      inline.push(
        t('attach.limitFilesInline', { max: MAX_FILES_PER_MESSAGE, dropped: droppedFiles }),
      );
    }
    // Over the cap: the first ones are attached. The tray says so until
    // dismissed or sent (a toast alone was missed behind the photo picker).
    if (inline.length > 0) {
      setNotice(inline.join(' '));
      const toast = {
        id: `attach:limit:${Date.now()}:${++toastSeq}`,
        category: 'error' as const,
        sessionId: null,
        title: t('attach.dropFailed'),
        body: notes.join(' '),
      };
      whenVisible(() => actions.showToast(toast));
    }
  };

  /** Problems with dropped or pasted files go to a toast: the drop may be far from the composer. */
  const toastProblems = (problems: string[]) => {
    if (problems.length === 0) return;
    actions.showToast({
      id: `attach:${Date.now()}:${++toastSeq}`,
      category: 'error',
      sessionId: null,
      title: t('attach.dropFailed'),
      body: problems.join(' '),
    });
  };

  /**
   * Dropped or pasted files. Ones over the size limit are refused right away
   * (images are exempt: they are made smaller before the upload). Returns the
   * problems.
   */
  const takeFiles = (files: File[]): string[] => {
    const problems: string[] = [];
    const ok: File[] = [];
    for (const file of files) {
      if (!isResizable(file.type) && file.size > MAX_FILE_BYTES) {
        problems.push(
          t('attach.tooLarge', {
            name: file.name,
            size: formatBytes(file.size),
            max: formatBytes(MAX_FILE_BYTES),
          }),
        );
      } else ok.push(file);
    }
    if (ok.length) onFiles(ok);
    return problems;
  };

  const handleDrop = (data: DataTransfer) => {
    // Called inside the drop event: readDrop takes the items before they expire.
    void readDrop(data.items ?? null, data.files ?? null).then(
      (dropped) => {
        const problems = takeFiles(dropped.files);
        for (const folder of dropped.folders) {
          const issue = queueFolder(folder);
          if (issue) problems.push(issue);
        }
        for (const name of dropped.unreadable) {
          problems.push(t('attach.dropFolderUnreadable', { name }));
        }
        toastProblems(problems);
      },
      (err: Error) => toastProblems([err.message]),
    );
  };

  // The paperclip decides: without it (cloud) or with it dimmed, no drops.
  const dropHandler = enabled ? handleDrop : null;
  const inPane = useFileDropTarget(dropHandler);
  const own = useFileDrag(inPane ? null : dropHandler);
  const dropping = own.dragging;

  const onPaste = (e: ClipboardEvent) => {
    if (!enabled) return;
    const files = pastedFiles(e.clipboardData);
    if (files.length === 0) return;
    e.preventDefault();
    toastProblems(takeFiles(files));
  };

  // A drawing is just a PNG attachment: it goes through the same upload.
  const button = available ? (
    <>
      <AttachMenu
        disabled={!enabled}
        platform={platform}
        onFiles={onFiles}
        onFolder={(files) => addFolderFiles(fromDirectoryInput(files))}
        autoSend={autoSend}
        onAutoSendChange={(value) => {
          setAutoSend(value);
          saveAutoSend(value);
        }}
        onSketch={() => setSketching(true)}
        hasSketchDraft={() => hasDraft(session.id)}
      />
      {sketching && (
        <SketchSheet
          draftKey={session.id}
          codec={codec}
          onDone={(file) => {
            setSketching(false);
            onFiles([file]);
          }}
          onClose={() => setSketching(false)}
        />
      )}
    </>
  ) : null;

  const tray =
    available && (attachments.items.length > 0 || problem || notice || dropping) ? (
      <>
        {showSendBlock && (
          <div className={styles.sendBlock} role="alert">
            <span className={styles.sendBlockText}>
              {t('attach.sendBlocked', { count: failed.length })}
            </span>
            <span className={styles.sendBlockActions}>
              <button
                type="button"
                className="btn btn-sm"
                onClick={() => {
                  setSendBlocked(false);
                  for (const it of failed) attachments.retry(it.id);
                }}
              >
                {t('attach.retryFailed')}
              </button>
              <button
                type="button"
                className="btn btn-sm"
                onClick={() => {
                  setSendBlocked(false);
                  for (const it of failed) attachments.dismiss(it.id);
                  failedRef.current = 0;
                  prefillRef.current.onSendAnyway?.();
                }}
              >
                {t('attach.sendWithout')}
              </button>
            </span>
          </div>
        )}
        {dropping && <p className={styles.dropHint}>{t('attach.dropHint')}</p>}
        {problem && (
          <p className={styles.attachError} role="alert">
            {problem}
          </p>
        )}
        {notice && (
          <p className={styles.attachNotice} role="status" data-testid="attach-notice">
            <span>{notice}</span>
            <button
              type="button"
              className={styles.noticeClose}
              aria-label={t('attach.dismissNotice')}
              onClick={() => setNotice(null)}
            >
              <Icon name="close" size={14} />
            </button>
          </p>
        )}
        <AttachmentTray
          items={attachments.items}
          summary={compact}
          canPrefill={canPrefill}
          onCancel={attachments.cancel}
          onRetry={attachments.retry}
          onDismiss={attachments.dismiss}
        />
      </>
    ) : null;

  return {
    available,
    button,
    tray,
    dropping,
    count: attachments.items.length,
    busy: attachments.busy,
    allowSend,
    delivered,
    clearDelivered: (ids) => {
      setNotice(null);
      attachments.release(ids);
    },
    waitForUploads,
    sendQueued,
    cancelQueuedSend: () => settleQueued(false),
    progress,
    showNotice: setNotice,
    dropProps: own.dropProps,
    onPaste,
  };
}
