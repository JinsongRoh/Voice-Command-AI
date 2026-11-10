import { describe, expect, it, vi } from 'vitest';

import { MockAttachments } from '../protocol/mockAttachments';
import type { ClientMsg, ServerMsg, SessionInfo } from '../protocol/types';
import { RequestError } from '../state/core';
import { localSession } from '../test/fixtures';
import { uploadFile, uploadFolder, UploadCancelled, type RequestFn } from './upload';

function server(session: SessionInfo = localSession()) {
  const typed: string[] = [];
  const mock = new MockAttachments(
    (id) => (id === session.id ? session : undefined),
    () => Date.UTC(2026, 9, 30, 8, 34, 56),
    (_id, text) => typed.push(text),
  );
  const sent: ClientMsg[] = [];
  const request = vi.fn(async (body: ClientMsg): Promise<ServerMsg> => {
    sent.push(body);
    return mock.handle(body as Parameters<MockAttachments['handle']>[0]);
  });
  return { request: request as RequestFn & typeof request, sent, typed };
}

const bytes = (n: number) => new Uint8Array(Array.from({ length: n }, (_, i) => i % 256));

describe('uploadFile', () => {
  it('sends ordered chunks and reports progress', async () => {
    const { request, sent, typed } = server();
    const file = new File([bytes(1_300_000)], 'diagram one.png', { type: 'image/png' });
    const progress: number[] = [];
    const result = await uploadFile(request, file, {
      sessionId: 's1',
      onProgress: (n) => progress.push(n),
    });
    expect(result).toEqual({
      path: '.vca/attachments/20261030-083456-diagram-one.png',
      size: 1_300_000,
      mention: 'Attached file: @.vca/attachments/20261030-083456-diagram-one.png',
      typed: true,
    });
    const chunks = sent.filter((m) => m.type === 'attach_chunk');
    // The mock server suggests 512 KiB chunks.
    expect(chunks.map((c) => c.offset)).toEqual([0, 524_288, 1_048_576]);
    expect(progress).toEqual([524_288, 1_048_576, 1_300_000]);
    expect(sent[0]).toMatchObject({
      type: 'attach_begin',
      name: 'diagram one.png',
      mime: 'image/png',
      size: 1_300_000,
      submit: false,
      folder: null,
    });
    expect(typed).toEqual([`${result.mention} `]);
  });

  it('cancels between chunks and tells the server', async () => {
    const { request, sent } = server();
    const controller = new AbortController();
    const file = new File([bytes(1_200_000)], 'a.pdf', { type: 'application/pdf' });
    const p = uploadFile(request, file, {
      sessionId: 's1',
      signal: controller.signal,
      onProgress: () => controller.abort(),
    });
    await expect(p).rejects.toBeInstanceOf(UploadCancelled);
    await vi.waitFor(() => expect(sent.at(-1)?.type).toBe('attach_cancel'));
    expect(sent.filter((m) => m.type === 'attach_chunk')).toHaveLength(1);
    expect(sent.some((m) => m.type === 'attach_end')).toBe(false);
  });

  it('turns server errors into RequestErrors', async () => {
    const { request } = server();
    const exe = new File([bytes(4)], 'setup.exe', { type: 'application/x-msdownload' });
    await expect(uploadFile(request, exe, { sessionId: 's1' })).rejects.toMatchObject({
      code: 'forbidden',
    });
    const offline: RequestFn = async () => {
      throw new Error('not connected');
    };
    const err = await uploadFile(offline, exe, { sessionId: 's1' }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(RequestError);
    expect((err as RequestError).code).toBe('transport');
  });

  it('observed sessions only save the file', async () => {
    const { request, typed } = server(localSession({ control: 'observe_only' }));
    const r = await uploadFile(request, new File(['x'], 'n.md', { type: 'text/markdown' }), {
      sessionId: 's1',
    });
    expect(r.typed).toBe(false);
    expect(r.mention).toMatch(/^Attached file: @\.vca\/attachments\/.*-n\.md$/);
    expect(typed).toEqual([]);
  });
});

describe('uploadFolder', () => {
  it('uploads files with their relative paths and mentions the folder once', async () => {
    const { request, sent, typed } = server();
    const progress: number[] = [];
    const result = await uploadFolder(
      request,
      'My Docs',
      [
        { file: new Blob([bytes(10)]), path: 'README.md' },
        { file: new Blob([bytes(20)]), path: 'src/lib/main.rs' },
      ],
      { sessionId: 's1', onProgress: (n) => progress.push(n) },
    );
    expect(sent[0]).toMatchObject({ type: 'attach_folder_begin', files: 2, total_bytes: 30 });
    const begins = sent.filter((m) => m.type === 'attach_begin');
    expect(begins.map((b) => b.folder?.path)).toEqual(['README.md', 'src/lib/main.rs']);
    expect(progress).toEqual([10, 30]);
    expect(result.path).toBe('.vca/attachments/20261030-083456-My-Docs');
    expect(result.mention).toBe('Attached folder: @.vca/attachments/20261030-083456-My-Docs/');
    expect(typed).toHaveLength(1);
  });

  it('removes a partly uploaded folder when a file fails', async () => {
    const { request, sent } = server();
    await expect(
      uploadFolder(
        request,
        'docs',
        [
          { file: new Blob(['a']), path: 'a.txt' },
          { file: new Blob(['b']), path: '.git/config' },
        ],
        { sessionId: 's1' },
      ),
    ).rejects.toMatchObject({ code: 'invalid_request' });
    await vi.waitFor(() =>
      expect(sent.at(-1)).toMatchObject({ type: 'attach_cancel', upload_id: 'fd_1' }),
    );
  });
});
