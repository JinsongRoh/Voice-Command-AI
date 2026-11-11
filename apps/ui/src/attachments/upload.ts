/**
 * Chunked uploads of files and folders into a session (vca-server
 * `attachments.rs`): `attach_begin`, ordered `attach_chunk`s of at most
 * `ATTACH_CHUNK_BYTES`, then `attach_end`. Folders wrap their files in
 * `attach_folder_begin` / `attach_folder_end`, which mentions the folder once.
 * Several files picked together share a batch id: the server saves them in
 * one folder and `attach_batch_end` types ONE short mention for all of them
 * (many separate mentions made a long burst that lost text in the terminal).
 */
import { ATTACH_CHUNK_BYTES, type ClientMsg, type ServerMsg } from '../protocol/types';
import { diag, errorInfo } from '../diag/log';
import { RequestError } from '../state/core';

export type RequestFn = (body: ClientMsg) => Promise<ServerMsg>;

/** The server's answer to a finished upload. */
export interface AttachResult {
  path: string;
  size: number;
  mention: string;
  typed: boolean;
}

export interface UploadOptions {
  sessionId: string;
  note?: string | null;
  /** Press Enter after the mention (default false: the user completes the line). */
  submit?: boolean;
  signal?: AbortSignal;
  /** Bytes sent so far, after every chunk. */
  onProgress?: (sent: number, total: number) => void;
}

/** One file of a folder upload with its path relative to the folder. */
export interface FolderFile {
  file: Blob;
  path: string;
}

export class UploadCancelled extends Error {
  constructor() {
    super('upload cancelled');
    this.name = 'UploadCancelled';
  }
}

async function call(request: RequestFn, body: ClientMsg): Promise<ServerMsg> {
  let response: ServerMsg;
  try {
    response = await request(body);
  } catch (err) {
    if (err instanceof RequestError) throw err;
    throw new RequestError('transport', (err as Error).message);
  }
  if (response.type === 'error') throw new RequestError(response.code, response.message);
  return response;
}

function unexpected(response: ServerMsg): never {
  throw new RequestError('internal', `unexpected response: ${response.type}`);
}

async function blobBytes(blob: Blob): Promise<Uint8Array> {
  if (typeof blob.arrayBuffer === 'function') return new Uint8Array(await blob.arrayBuffer());
  return new Uint8Array(await new Response(blob).arrayBuffer());
}

function fileName(file: Blob, fallback: string): string {
  return 'name' in file && typeof file.name === 'string' && file.name ? file.name : fallback;
}

interface SendOptions extends UploadOptions {
  name: string;
  folder: { folder_id: string; path: string } | null;
  batch?: string | null;
  /** Bytes already sent by earlier files of the same folder (progress only). */
  base?: number;
  total?: number;
}

async function send(request: RequestFn, file: Blob, opts: SendOptions): Promise<AttachResult> {
  const { signal } = opts;
  if (signal?.aborted) throw new UploadCancelled();
  const t0 = Date.now();
  diag('upload.begin', { bytes: file.size, mime: file.type });
  const ready = await call(request, {
    type: 'attach_begin',
    session_id: opts.sessionId,
    name: opts.name,
    mime: file.type,
    size: file.size,
    note: opts.note ?? null,
    submit: opts.submit ?? false,
    folder: opts.folder,
    ...(opts.batch ? { batch: opts.batch } : {}),
  });
  if (ready.type !== 'attach_ready') unexpected(ready);
  const uploadId = ready.upload_id;
  const chunkSize = Math.max(
    1,
    Math.min(ready.chunk_size || ATTACH_CHUNK_BYTES, ATTACH_CHUNK_BYTES),
  );
  const base = opts.base ?? 0;
  const total = opts.total ?? file.size;
  const cancel = () => request({ type: 'attach_cancel', upload_id: uploadId }).catch(() => {});
  let offset = 0;
  try {
    while (offset < file.size) {
      if (signal?.aborted) throw new UploadCancelled();
      const end = Math.min(offset + chunkSize, file.size);
      const data = await blobBytes(file.slice(offset, end));
      const c0 = Date.now();
      const ok = await call(request, { type: 'attach_chunk', upload_id: uploadId, offset, data });
      if (ok.type !== 'ok') unexpected(ok);
      offset = end;
      diag('upload.chunk', { sent: offset, of: file.size, ms: Date.now() - c0 });
      opts.onProgress?.(base + offset, total);
    }
    if (signal?.aborted) throw new UploadCancelled();
  } catch (err) {
    // The server drops broken uploads itself; a cancel also covers the
    // case where the client stopped between chunks.
    diag('upload.error', { sent: offset, of: file.size, ...errorInfo(err) });
    void cancel();
    throw err;
  }
  const done = await call(request, { type: 'attach_end', upload_id: uploadId });
  if (done.type !== 'attached') unexpected(done);
  diag('upload.done', { bytes: file.size, ms: Date.now() - t0, typed: done.typed });
  if (file.size === 0) opts.onProgress?.(base, total);
  return { path: done.path, size: done.size, mention: done.mention, typed: done.typed };
}

/**
 * Uploads one file; resolves with where the server saved it. With `batch`
 * the server does not mention it (its `mention` is empty): `endBatch` does.
 */
export function uploadFile(
  request: RequestFn,
  file: Blob,
  options: UploadOptions & { name?: string; batch?: string | null },
): Promise<AttachResult> {
  return send(request, file, {
    ...options,
    name: options.name ?? fileName(file, 'file'),
    folder: null,
  });
}

/** Mentions every uploaded file of a batch once (typed, or returned to show). */
export async function endBatch(
  request: RequestFn,
  batchId: string,
  options: { note?: string | null; submit?: boolean } = {},
): Promise<AttachResult> {
  const done = await call(request, {
    type: 'attach_batch_end',
    batch_id: batchId,
    note: options.note ?? null,
    submit: options.submit ?? false,
  });
  if (done.type !== 'attached') unexpected(done);
  diag('upload.batchEnd', { chars: done.mention.length, typed: done.typed });
  return { path: done.path, size: done.size, mention: done.mention, typed: done.typed };
}

/** Uploads a folder's files one after another, then mentions the folder once. */
export async function uploadFolder(
  request: RequestFn,
  name: string,
  files: FolderFile[],
  options: UploadOptions,
): Promise<AttachResult> {
  const total = files.reduce((n, f) => n + f.file.size, 0);
  if (options.signal?.aborted) throw new UploadCancelled();
  const ready = await call(request, {
    type: 'attach_folder_begin',
    session_id: options.sessionId,
    name,
    files: files.length,
    total_bytes: total,
    note: options.note ?? null,
    submit: options.submit ?? false,
  });
  if (ready.type !== 'attach_folder_ready') unexpected(ready);
  const folderId = ready.folder_id;
  let sent = 0;
  try {
    for (const entry of files) {
      await send(request, entry.file, {
        sessionId: options.sessionId,
        signal: options.signal,
        onProgress: options.onProgress,
        name: entry.path.split('/').pop() ?? 'file',
        folder: { folder_id: folderId, path: entry.path },
        base: sent,
        total,
      });
      sent += entry.file.size;
    }
    if (options.signal?.aborted) throw new UploadCancelled();
  } catch (err) {
    // Removes the partly uploaded folder on the server.
    void request({ type: 'attach_cancel', upload_id: folderId }).catch(() => {});
    throw err;
  }
  const done = await call(request, { type: 'attach_folder_end', folder_id: folderId });
  if (done.type !== 'attached') unexpected(done);
  return { path: done.path, size: done.size, mention: done.mention, typed: done.typed };
}
