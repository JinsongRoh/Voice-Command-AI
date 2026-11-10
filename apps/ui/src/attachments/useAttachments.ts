import { useCallback, useEffect, useRef, useState } from 'react';

import { diag, errorInfo } from '../diag/log';
import type { TranslateFn } from '../i18n/i18n';
import { describeAttachError } from './errors';
import { checkFolder, MAX_FILE_BYTES, type FolderProblem, type FolderSelection } from './folder';
import { guessImageType, isResizable, prepareImageWithThumb, type ImageCodec } from './resize';
import {
  endBatch,
  uploadFile,
  uploadFolder,
  UploadCancelled,
  type AttachResult,
  type RequestFn,
} from './upload';

export type AttachmentStatus =
  'preparing' | 'queued' | 'uploading' | 'done' | 'error' | 'cancelled';

/** One chip in the attachment tray. */
export interface AttachmentItem {
  id: string;
  kind: 'file' | 'folder';
  name: string;
  /** Bytes to send (after resizing). */
  size: number;
  /** Files in a folder upload. */
  files: number;
  /** Hidden or system files left out of a folder. */
  skipped: number;
  /** Object URL of a small image preview (at most 256 px), when available. */
  thumbUrl: string | null;
  /** A photo or other image (counts against the photo cap). */
  image?: boolean;
  status: AttachmentStatus;
  sent: number;
  /** Short, translated reason for a failure. */
  error: string | null;
  /** The server's full message (may hold local paths): shown on request only. */
  errorDetail: string | null;
  result: AttachResult | null;
}

interface Job {
  file?: File;
  folder?: FolderSelection;
  controller: AbortController;
  /** Images are resized once, before the first attempt. */
  prepared: boolean;
  /** The webview could not decode this image, so it goes up unchanged. */
  undecodable?: boolean;
}

export interface UseAttachmentsOptions {
  sessionId: string;
  request: RequestFn;
  t: TranslateFn;
  /**
   * Called after each successful upload (file or whole folder), and once for
   * a batch of files (with its last item): only that result carries the
   * mention of the whole batch; the batch's own files come without one.
   */
  onAttached?: (result: AttachResult, item: AttachmentItem) => void;
  /** Press Enter after the mention (for a batch: once, after its mention). */
  submit?: boolean;
  maxFileBytes?: number;
  codec?: ImageCodec;
}

/** Per message: photos and other files are counted separately. */
export const MAX_IMAGES_PER_MESSAGE = 50;
export const MAX_FILES_PER_MESSAGE = 10;

/** What `addFiles` left out because of the caps. */
export interface AddFilesResult {
  added: number;
  droppedImages: number;
  droppedFiles: number;
}

/** Previews of unresizable images (GIF, SVG) are only taken from small files. */
const RAW_THUMB_MAX_BYTES = 1024 * 1024;

let counter = 0;
const nextId = () => `att${++counter}`;
let batchCounter = 0;
const nextBatchId = () =>
  `b${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}${++batchCounter}`;

/** Files uploaded in one run of the queue: mentioned once at the end. */
interface BatchRun {
  id: string;
  /** Items uploaded into the batch (their mention is still owed). */
  done: string[];
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  const mb = bytes / (1024 * 1024);
  return `${mb >= 10 ? Math.round(mb) : mb.toFixed(1)} MB`;
}

export function folderProblemText(t: TranslateFn, p: FolderProblem): string {
  switch (p.kind) {
    case 'empty':
      return t('attach.folderEmpty');
    case 'tooManyFiles':
      return t('attach.folderTooManyFiles', { count: p.count, max: p.max });
    case 'tooLarge':
      return t('attach.folderTooLarge', { size: formatBytes(p.bytes), max: formatBytes(p.max) });
    case 'fileTooLarge':
      return t('attach.tooLarge', {
        name: p.name,
        size: formatBytes(p.bytes),
        max: formatBytes(p.max),
      });
  }
}

function thumbnail(file: Blob): string | null {
  if (typeof URL === 'undefined' || typeof URL.createObjectURL !== 'function') return null;
  try {
    return URL.createObjectURL(file);
  } catch {
    return null;
  }
}

function revoke(url: string | null) {
  if (url && typeof URL.revokeObjectURL === 'function') URL.revokeObjectURL(url);
}

/** `image.jpg`, then `image (2).jpg`: pickers hand over many files with one name. */
function uniqueName(name: string, taken: Set<string>): string {
  if (!taken.has(name)) return name;
  const dot = name.lastIndexOf('.');
  const stem = dot > 0 ? name.slice(0, dot) : name;
  const ext = dot > 0 ? name.slice(dot) : '';
  for (let n = 2; ; n++) {
    const candidate = `${stem} (${n})${ext}`;
    if (!taken.has(candidate)) return candidate;
  }
}

/**
 * Upload queue for one session: files and folders go one after another,
 * with progress, cancel and retry per item.
 */
export function useAttachments({
  sessionId,
  request,
  t,
  onAttached,
  submit = false,
  maxFileBytes = MAX_FILE_BYTES,
  codec,
}: UseAttachmentsOptions) {
  const [items, setItems] = useState<AttachmentItem[]>([]);
  const itemsRef = useRef<AttachmentItem[]>([]);
  const jobs = useRef(new Map<string, Job>());
  const queue = useRef<string[]>([]);
  const running = useRef(false);
  const mounted = useRef(true);
  /** The mention of a finished batch is being typed: still busy. */
  const [ending, setEnding] = useState(false);
  const latest = useRef({ request, t, onAttached, submit, maxFileBytes, codec, sessionId });
  useEffect(() => {
    latest.current = { request, t, onAttached, submit, maxFileBytes, codec, sessionId };
  });

  const commit = useCallback((next: AttachmentItem[]) => {
    itemsRef.current = next;
    if (mounted.current) setItems(next);
  }, []);

  const patch = useCallback(
    (id: string, change: Partial<AttachmentItem>) => {
      if (change.status) diag('attach.status', { id, status: change.status });
      commit(itemsRef.current.map((it) => (it.id === id ? { ...it, ...change } : it)));
    },
    [commit],
  );

  const get = (id: string) => itemsRef.current.find((it) => it.id === id);

  const runOne = useCallback(
    async (id: string, batch: BatchRun | null) => {
      const job = jobs.current.get(id);
      const item = get(id);
      if (!job || !item || job.controller.signal.aborted) return;
      const { request, t, maxFileBytes, codec, sessionId, submit } = latest.current;
      const signal = job.controller.signal;
      try {
        let result: AttachResult;
        if (job.file) {
          if (!job.prepared) {
            patch(id, { status: 'preparing' });
            const before = job.file.size;
            const p0 = Date.now();
            let thumbUrl: string | null | undefined;
            let scaledDecode = false;
            if (isResizable(job.file.type)) {
              const prepared = await prepareImageWithThumb(job.file, codec);
              job.file = prepared.file;
              job.undecodable = prepared.undecodable;
              scaledDecode = prepared.scaledDecode;
              // The chip may have been removed while the image was decoded.
              if (get(id)) {
                if (prepared.thumb) thumbUrl = thumbnail(prepared.thumb);
                // Undecodable but small (a sketch, a tiny PNG): show the file itself.
                else if (job.file.size <= RAW_THUMB_MAX_BYTES) thumbUrl = thumbnail(job.file);
              }
            } else if (
              job.file.type.startsWith('image/') &&
              job.file.size <= RAW_THUMB_MAX_BYTES &&
              get(id)
            ) {
              thumbUrl = thumbnail(job.file);
            }
            diag('attach.prepared', {
              before,
              after: job.file.size,
              ms: Date.now() - p0,
              resizeWidth: scaledDecode,
            });
            job.prepared = true;
            patch(id, thumbUrl ? { size: job.file.size, thumbUrl } : { size: job.file.size });
          }
          if (job.file.size > maxFileBytes) {
            patch(id, {
              status: 'error',
              error: job.undecodable
                ? t('attach.errCannotShrink', { max: formatBytes(maxFileBytes) })
                : t('attach.errTooLargeSize', {
                    size: formatBytes(job.file.size),
                    max: formatBytes(maxFileBytes),
                  }),
              errorDetail: null,
            });
            return;
          }
          if (signal.aborted) throw new UploadCancelled();
          patch(id, { status: 'uploading', sent: 0, error: null, errorDetail: null });
          result = await uploadFile(request, job.file, {
            sessionId,
            // A batch presses Enter once, after its one mention.
            submit: batch ? false : submit && queue.current.length === 0,
            batch: batch?.id ?? null,
            signal,
            name: job.file.name,
            onProgress: (sent) => patch(id, { sent }),
          });
        } else if (job.folder) {
          patch(id, { status: 'uploading', sent: 0, error: null, errorDetail: null });
          result = await uploadFolder(request, job.folder.name, job.folder.files, {
            sessionId,
            submit,
            signal,
            onProgress: (sent) => patch(id, { sent }),
          });
        } else {
          return;
        }
        patch(id, { status: 'done', sent: result.size, result });
        const done = get(id);
        // In a batch the server answers without a mention (an older server
        // still mentions each file: that one is passed on as before).
        if (batch && job.file && !result.mention) batch.done.push(id);
        else if (done) latest.current.onAttached?.(result, done);
      } catch (err) {
        if (err instanceof UploadCancelled || signal.aborted) {
          patch(id, { status: 'cancelled', error: null, errorDetail: null });
        } else {
          const { message, detail } = describeAttachError(latest.current.t, err);
          patch(id, { status: 'error', error: message, errorDetail: detail });
        }
      }
    },
    [patch],
  );

  /** One mention for the batch's files; if that fails, each file's own (never none). */
  const finishBatch = useCallback(
    async (batch: BatchRun) => {
      if (batch.done.length === 0) return;
      const { request, submit } = latest.current;
      const last = batch.done[batch.done.length - 1]!;
      if (mounted.current) setEnding(true);
      try {
        const result = await endBatch(request, batch.id, { submit });
        // The chips say whether the (one) mention was typed into the session.
        for (const id of batch.done) {
          const it = get(id);
          if (it?.result) patch(id, { result: { ...it.result, typed: result.typed } });
        }
        const item = get(last);
        if (item) latest.current.onAttached?.(result, item);
      } catch (err) {
        diag('attach.batchEnd.error', { files: batch.done.length, ...errorInfo(err) });
        for (const id of batch.done) {
          const item = get(id);
          if (!item?.result) continue;
          latest.current.onAttached?.(
            { ...item.result, mention: `Attached file: @${item.result.path}`, typed: false },
            item,
          );
        }
      } finally {
        if (mounted.current) setEnding(false);
      }
    },
    [patch],
  );

  const pump = useCallback(async () => {
    if (running.current) return;
    running.current = true;
    try {
      // A multi-file pick queues all of its files first: they become one batch.
      await Promise.resolve();
      // Files added while a batch's mention is typed start the next batch.
      while (queue.current.length > 0) {
        let batch: BatchRun | null = null;
        for (;;) {
          const id = queue.current.shift();
          if (id === undefined) break;
          const isFile = Boolean(jobs.current.get(id)?.file);
          if (isFile && !batch && queue.current.some((q) => jobs.current.get(q)?.file)) {
            batch = { id: nextBatchId(), done: [] };
          }
          await runOne(id, isFile ? batch : null);
        }
        if (batch) await finishBatch(batch);
      }
    } finally {
      running.current = false;
    }
  }, [runOne, finishBatch]);

  const enqueue = useCallback(
    (item: AttachmentItem, job: Job) => {
      jobs.current.set(item.id, job);
      commit([...itemsRef.current, item]);
      queue.current.push(item.id);
      void pump();
    },
    [commit, pump],
  );

  const addFiles = useCallback(
    (input: File[]): AddFilesResult => {
      const existing = itemsRef.current.filter(
        (it) => it.kind === 'file' && it.status !== 'cancelled',
      );
      let images = existing.filter((it) => it.image).length;
      let others = existing.length - images;
      const taken = new Set(itemsRef.current.map((it) => it.name));
      const result: AddFilesResult = { added: 0, droppedImages: 0, droppedFiles: 0 };
      for (const original of input) {
        // Some pickers leave `type` empty (HEIC converted to JPEG, cloud files).
        const type = original.type || guessImageType(original.name);
        const image = type.startsWith('image/');
        if (image ? images >= MAX_IMAGES_PER_MESSAGE : others >= MAX_FILES_PER_MESSAGE) {
          if (image) result.droppedImages++;
          else result.droppedFiles++;
          continue;
        }
        if (image) images++;
        else others++;
        const name = uniqueName(original.name || (image ? 'photo.jpg' : 'file'), taken);
        taken.add(name);
        const file =
          name === original.name && type === original.type
            ? original
            : new File([original], name, { type, lastModified: original.lastModified });
        result.added++;
        const resizable = isResizable(file.type);
        enqueue(
          {
            id: nextId(),
            kind: 'file',
            name,
            size: file.size,
            files: 1,
            skipped: 0,
            thumbUrl: null,
            image,
            // Photos show "preparing" at once; the preview follows the decode.
            status: resizable ? 'preparing' : 'queued',
            sent: 0,
            error: null,
            errorDetail: null,
            result: null,
          },
          { file, controller: new AbortController(), prepared: !resizable },
        );
      }
      return result;
    },
    [enqueue],
  );

  /** Adds a folder; returns a problem (shown by the caller) instead when over the caps. */
  const addFolder = useCallback(
    (selection: FolderSelection): FolderProblem | null => {
      const problem = checkFolder(selection);
      if (problem) return problem;
      enqueue(
        {
          id: nextId(),
          kind: 'folder',
          name: selection.name,
          size: selection.totalBytes,
          files: selection.files.length,
          skipped: selection.skipped,
          thumbUrl: null,
          status: 'queued',
          sent: 0,
          error: null,
          errorDetail: null,
          result: null,
        },
        { folder: selection, controller: new AbortController(), prepared: true },
      );
      return null;
    },
    [enqueue],
  );

  const cancel = useCallback(
    (id: string) => {
      const job = jobs.current.get(id);
      const item = get(id);
      if (!job || !item) return;
      job.controller.abort();
      if (item.status === 'queued' || item.status === 'preparing') {
        queue.current = queue.current.filter((q) => q !== id);
        patch(id, { status: 'cancelled' });
      }
    },
    [patch],
  );

  const retry = useCallback(
    (id: string) => {
      const job = jobs.current.get(id);
      const item = get(id);
      if (!job || !item || (item.status !== 'error' && item.status !== 'cancelled')) return;
      job.controller = new AbortController();
      patch(id, { status: 'queued', sent: 0, error: null, errorDetail: null });
      queue.current.push(id);
      void pump();
    },
    [patch, pump],
  );

  const dismiss = useCallback(
    (id: string) => {
      const item = get(id);
      if (!item) return;
      jobs.current.get(id)?.controller.abort();
      jobs.current.delete(id);
      queue.current = queue.current.filter((q) => q !== id);
      revoke(item.thumbUrl);
      commit(itemsRef.current.filter((it) => it.id !== id));
    },
    [commit],
  );

  /**
   * Removes delivered items from the tray without revoking their previews:
   * the chat bubble of the prompt they went out with shows them now.
   */
  const release = useCallback(
    (ids: readonly string[]) => {
      const gone = new Set(ids);
      for (const id of ids) jobs.current.delete(id);
      commit(itemsRef.current.filter((it) => !gone.has(it.id)));
    },
    [commit],
  );

  // Leaving the session stops its uploads (the server deletes partial files).
  useEffect(() => {
    mounted.current = true;
    const all = jobs.current;
    return () => {
      mounted.current = false;
      for (const job of all.values()) job.controller.abort();
      for (const it of itemsRef.current) revoke(it.thumbUrl);
    };
  }, []);

  const busy =
    ending ||
    items.some(
      (it) => it.status === 'queued' || it.status === 'preparing' || it.status === 'uploading',
    );

  return { items, busy, addFiles, addFolder, cancel, retry, dismiss, release };
}
