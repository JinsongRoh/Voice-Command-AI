/**
 * In-memory stand-in for the server's attachment handling (vca-server
 * `attachments.rs`), used by `MockTransport`: chunk order and size checks,
 * name sanitizing and the `@path` mention, without storing any data.
 */
import type { ClientMsg, ErrorCode, ServerMsg, SessionInfo } from './types';
import { ATTACH_CHUNK_BYTES } from './types';

export type AttachMsg = Extract<
  ClientMsg,
  {
    type:
      | 'attach'
      | 'attach_begin'
      | 'attach_chunk'
      | 'attach_end'
      | 'attach_cancel'
      | 'attach_folder_begin'
      | 'attach_folder_end'
      | 'attach_batch_end';
  }
>;

export const MOCK_MAX_BYTES = 25 * 1024 * 1024;
export const MOCK_MAX_FOLDER_BYTES = 100 * 1024 * 1024;
export const MOCK_MAX_FOLDER_FILES = 500;

interface Upload {
  sessionId: string;
  name: string;
  size: number;
  received: number;
  note: string | null;
  submit: boolean;
  folderId: string | null;
  batchId?: string | null;
}

interface Batch {
  sessionId: string;
  dir: string;
  paths: string[];
  bytes: number;
}

/** Inline batch mentions longer than this name the folder instead (as the server). */
export const MOCK_MAX_INLINE_MENTION_CHARS = 400;

interface Folder {
  sessionId: string;
  dir: string;
  files: number;
  bytes: number;
  note: string | null;
  submit: boolean;
}

/** Same rules as the server's `sanitize_name`, simplified. */
export function mockSanitizeName(raw: string): string {
  const base = raw.split(/[/\\]/).pop() ?? '';
  let out = '';
  for (const c of base) {
    const ch = /[\p{L}\p{N}._-]/u.test(c) ? c : '-';
    if ((ch === '-' || ch === '.') && out.endsWith(ch)) continue;
    out += ch;
  }
  out = out.replace(/^[.-]+|[.-]+$/g, '');
  return out || 'file';
}

function stamp(now: number): string {
  const d = new Date(now);
  const p = (n: number) => String(n).padStart(2, '0');
  return (
    `${d.getUTCFullYear()}${p(d.getUTCMonth() + 1)}${p(d.getUTCDate())}-` +
    `${p(d.getUTCHours())}${p(d.getUTCMinutes())}${p(d.getUTCSeconds())}`
  );
}

export class MockAttachments {
  private readonly uploads = new Map<string, Upload>();
  private readonly folders = new Map<string, Folder>();
  private readonly batches = new Map<string, Batch>();
  private counter = 0;

  constructor(
    private readonly lookup: (sessionId: string) => SessionInfo | undefined,
    private readonly now: () => number,
    /** Called with the mention for sessions that accept input. */
    private readonly type: (sessionId: string, text: string, submit: boolean) => void,
  ) {}

  handle(msg: AttachMsg): ServerMsg {
    switch (msg.type) {
      case 'attach': {
        if (msg.data.length > ATTACH_CHUNK_BYTES) {
          return error('invalid_request', 'use a chunked upload');
        }
        const ready = this.begin(msg.session_id, msg.name, msg.data.length, msg.note, msg.submit);
        if (ready.type !== 'attach_ready') return ready;
        const chunk = this.chunk(ready.upload_id, 0, msg.data.length);
        if (chunk.type === 'error') return chunk;
        return this.end(ready.upload_id);
      }
      case 'attach_begin':
        if (msg.folder) {
          const folder = this.folders.get(msg.folder.folder_id);
          if (!folder) return error('not_found', 'no such folder upload');
          if (msg.folder.path.split('/').some((p) => p.startsWith('.') || p === '..')) {
            return error('invalid_request', 'hidden or system entry skipped');
          }
          return this.addUpload({
            sessionId: msg.session_id,
            name: msg.folder.path.split('/').map(mockSanitizeName).join('/'),
            size: msg.size,
            received: 0,
            note: null,
            submit: false,
            folderId: msg.folder.folder_id,
          });
        }
        if (msg.batch) {
          const refused = this.refuse(msg.session_id);
          if (refused) return refused;
          let batch = this.batches.get(msg.batch);
          if (!batch) {
            batch = {
              sessionId: msg.session_id,
              dir: `.vca/attachments/${stamp(this.now())}-batch`,
              paths: [],
              bytes: 0,
            };
            this.batches.set(msg.batch, batch);
          }
          return this.addUpload({
            sessionId: msg.session_id,
            name: mockSanitizeName(msg.name),
            size: msg.size,
            received: 0,
            note: null,
            submit: false,
            folderId: null,
            batchId: msg.batch,
          });
        }
        return this.begin(msg.session_id, msg.name, msg.size, msg.note, msg.submit);
      case 'attach_chunk':
        return this.chunk(msg.upload_id, msg.offset, msg.data.length);
      case 'attach_end':
        return this.end(msg.upload_id);
      case 'attach_cancel':
        if (this.uploads.delete(msg.upload_id) || this.folders.delete(msg.upload_id)) {
          return { type: 'ok' };
        }
        return error('not_found', 'no such upload');
      case 'attach_folder_begin': {
        const refused = this.refuse(msg.session_id);
        if (refused) return refused;
        if (msg.files > MOCK_MAX_FOLDER_FILES) return error('invalid_request', 'too many files');
        if (msg.total_bytes > MOCK_MAX_FOLDER_BYTES) {
          return error('invalid_request', 'the folder is too large');
        }
        const id = `fd_${++this.counter}`;
        this.folders.set(id, {
          sessionId: msg.session_id,
          dir: `.vca/attachments/${stamp(this.now())}-${mockSanitizeName(msg.name)}`,
          files: 0,
          bytes: 0,
          note: msg.note,
          submit: msg.submit,
        });
        return {
          type: 'attach_folder_ready',
          folder_id: id,
          max_files: MOCK_MAX_FOLDER_FILES,
          max_total_bytes: MOCK_MAX_FOLDER_BYTES,
        };
      }
      case 'attach_batch_end': {
        const batch = this.batches.get(msg.batch_id);
        if (!batch) return error('not_found', 'no such batch');
        this.batches.delete(msg.batch_id);
        if (batch.paths.length === 0) {
          return error('invalid_request', 'no file of the batch was uploaded');
        }
        if (batch.paths.length === 1) {
          return this.finish(
            batch.sessionId,
            batch.dir,
            batch.bytes,
            'Attached file',
            batch.paths[0]!,
            msg.note,
            msg.submit,
          );
        }
        const n = batch.paths.length;
        const inline = `Attached files (${n}): ${batch.paths.map((p) => `@${p}`).join(' ')}`;
        const text =
          inline.length <= MOCK_MAX_INLINE_MENTION_CHARS
            ? inline
            : `Attached ${n} files: @${batch.dir}/ (list: @${batch.dir}/files.txt)`;
        return this.finishText(batch.sessionId, batch.dir, batch.bytes, text, msg.note, msg.submit);
      }
      case 'attach_folder_end': {
        const folder = this.folders.get(msg.folder_id);
        if (!folder) return error('not_found', 'no such folder upload');
        this.folders.delete(msg.folder_id);
        if (folder.files === 0)
          return error('invalid_request', 'no file of the folder was uploaded');
        return this.finish(
          folder.sessionId,
          folder.dir,
          folder.bytes,
          'Attached folder',
          `${folder.dir}/`,
          folder.note,
          folder.submit,
        );
      }
    }
  }

  private refuse(sessionId: string): ServerMsg | null {
    const session = this.lookup(sessionId);
    if (!session) return error('not_found', `no session ${sessionId}`);
    if (session.origin.type === 'cloud') {
      return error(
        'unsupported',
        'this session has no project folder on this PC (cloud session); files can only be attached to local sessions',
      );
    }
    return null;
  }

  private begin(
    sessionId: string,
    name: string,
    size: number,
    note: string | null,
    submit: boolean,
  ): ServerMsg {
    const refused = this.refuse(sessionId);
    if (refused) return refused;
    if (size > MOCK_MAX_BYTES) return error('invalid_request', 'the file is too large');
    if (/\.(exe|bat|cmd|ps1|msi|dll|apk)$/i.test(name)) {
      return error('forbidden', `executable files cannot be attached: ${name}`);
    }
    return this.addUpload({
      sessionId,
      name: `${stamp(this.now())}-${mockSanitizeName(name)}`,
      size,
      received: 0,
      note,
      submit,
      folderId: null,
    });
  }

  private addUpload(upload: Upload): ServerMsg {
    const id = `up_${++this.counter}`;
    this.uploads.set(id, upload);
    return { type: 'attach_ready', upload_id: id, chunk_size: 512 * 1024 };
  }

  private chunk(id: string, offset: number, length: number): ServerMsg {
    const upload = this.uploads.get(id);
    if (!upload) return error('not_found', 'no such upload');
    if (offset !== upload.received || upload.received + length > upload.size) {
      this.uploads.delete(id);
      return error('invalid_request', 'chunk out of order');
    }
    upload.received += length;
    return { type: 'ok' };
  }

  private end(id: string): ServerMsg {
    const upload = this.uploads.get(id);
    if (!upload) return error('not_found', 'no such upload');
    this.uploads.delete(id);
    if (upload.received !== upload.size) return error('invalid_request', 'upload incomplete');
    if (upload.folderId) {
      const folder = this.folders.get(upload.folderId);
      if (!folder) return error('not_found', 'no such folder upload');
      folder.files += 1;
      folder.bytes += upload.size;
      return {
        type: 'attached',
        session_id: upload.sessionId,
        path: `${folder.dir}/${upload.name}`,
        size: upload.size,
        mention: '',
        typed: false,
      };
    }
    if (upload.batchId) {
      const batch = this.batches.get(upload.batchId);
      if (batch) {
        const path = `${batch.dir}/${upload.name}`;
        batch.paths.push(path);
        batch.bytes += upload.size;
        return {
          type: 'attached',
          session_id: upload.sessionId,
          path,
          size: upload.size,
          mention: '',
          typed: false,
        };
      }
    }
    const path = `.vca/attachments/${upload.name}`;
    return this.finish(
      upload.sessionId,
      path,
      upload.size,
      'Attached file',
      path,
      upload.note,
      upload.submit,
    );
  }

  private finish(
    sessionId: string,
    path: string,
    size: number,
    label: string,
    target: string,
    note: string | null,
    submit: boolean,
  ): ServerMsg {
    return this.finishText(sessionId, path, size, `${label}: @${target}`, note, submit);
  }

  private finishText(
    sessionId: string,
    path: string,
    size: number,
    text: string,
    note: string | null,
    submit: boolean,
  ): ServerMsg {
    const mention = `${text}${note ? ` ${note}` : ''}`;
    const session = this.lookup(sessionId);
    const typed = session?.control === 'managed_pty';
    if (typed) this.type(sessionId, submit ? mention : `${mention} `, submit);
    return { type: 'attached', session_id: sessionId, path, size, mention, typed };
  }
}

function error(code: ErrorCode, message: string): ServerMsg {
  return { type: 'error', code, message };
}
