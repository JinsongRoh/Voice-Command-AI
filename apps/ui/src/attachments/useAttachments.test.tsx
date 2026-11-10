import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { TranslateFn } from '../i18n/i18n';
import type { ClientMsg, ServerMsg } from '../protocol/types';
import type { ImageCodec } from './resize';
import { MAX_FILES_PER_MESSAGE, MAX_IMAGES_PER_MESSAGE, useAttachments } from './useAttachments';

const t = ((key: string) => key) as unknown as TranslateFn;

/**
 * A server that accepts everything; records the order of the calls. Files
 * of a batch get no mention; `attach_batch_end` answers with one for all
 * (`legacy`: an older server that mentions every file and knows no batches).
 */
function fakeServer(opts: { failName?: string; legacy?: boolean; failBatchEnd?: boolean } = {}) {
  const log: string[] = [];
  const begins: Extract<ClientMsg, { type: 'attach_begin' }>[] = [];
  const batchEnds: Extract<ClientMsg, { type: 'attach_batch_end' }>[] = [];
  const batchOf = new Map<string, string | null>();
  const batchFiles = new Map<string, string[]>();
  let open = 0;
  let maxOpen = 0;
  let n = 0;
  const request = vi.fn(async (body: ClientMsg): Promise<ServerMsg> => {
    log.push(body.type);
    switch (body.type) {
      case 'attach_begin': {
        if (body.name === opts.failName) {
          return { type: 'error', code: 'invalid_request', message: 'refused' } as ServerMsg;
        }
        begins.push(body);
        open++;
        maxOpen = Math.max(maxOpen, open);
        const id = `u${++n}`;
        batchOf.set(id, opts.legacy ? null : (body.batch ?? null));
        return { type: 'attach_ready', upload_id: id, chunk_size: 524_288 } as ServerMsg;
      }
      case 'attach_chunk':
        return { type: 'ok' } as ServerMsg;
      case 'attach_end': {
        open--;
        const k = body.upload_id.slice(1);
        const batch = batchOf.get(body.upload_id);
        if (batch) batchFiles.set(batch, [...(batchFiles.get(batch) ?? []), `b/f${k}`]);
        return {
          type: 'attached',
          path: batch ? `b/f${k}` : `.vca/attachments/f${k}`,
          size: 1,
          mention: batch ? '' : `Attached file: @f${k}`,
          typed: !batch,
        } as ServerMsg;
      }
      case 'attach_batch_end': {
        batchEnds.push(body);
        if (opts.legacy || opts.failBatchEnd) {
          return { type: 'error', code: 'invalid_request', message: 'unknown' } as ServerMsg;
        }
        const files = batchFiles.get(body.batch_id) ?? [];
        return {
          type: 'attached',
          path: 'b',
          size: files.length,
          mention: `Attached files (${files.length}): ${files.map((f) => `@${f}`).join(' ')}`,
          typed: false,
        } as ServerMsg;
      }
      default:
        return { type: 'ok' } as ServerMsg;
    }
  });
  return { request, log, begins, batchEnds, maxOpen: () => maxOpen };
}

/** Counts live bitmaps: decode opens one, close frees it. */
function trackedCodec(width = 4000, height = 3000) {
  const state = { alive: 0, peak: 0, decodes: 0, options: [] as unknown[] };
  const codec: ImageCodec = {
    decode: vi.fn(async (_f, options) => {
      state.decodes++;
      state.options.push(options);
      state.alive++;
      state.peak = Math.max(state.peak, state.alive);
      await Promise.resolve();
      return {
        width,
        height,
        source: {} as CanvasImageSource,
        close: () => {
          state.alive--;
        },
      };
    }),
    encode: vi.fn(async (_i, _w, _h, type) => new Blob([new Uint8Array(10)], { type })),
  };
  return { codec, state };
}

const jpg = (name: string, type = 'image/jpeg', bytes = 100) =>
  new File([new Uint8Array(bytes)], name, { type });

/** A file that reports a large size without holding the bytes. */
const fat = (name: string, bytes: number) => {
  const f = jpg(name);
  Object.defineProperty(f, 'size', { value: bytes });
  return f;
};

let urls = 0;
let created: string[];
let revoked: string[];
beforeEach(() => {
  created = [];
  revoked = [];
  urls = 0;
  URL.createObjectURL = vi.fn(() => {
    const u = `blob:t/${++urls}`;
    created.push(u);
    return u;
  });
  URL.revokeObjectURL = vi.fn((u: string) => {
    revoked.push(u);
  });
});
afterEach(() => vi.restoreAllMocks());

function setup(server = fakeServer(), codec?: ImageCodec, submit = false) {
  const mentions: string[] = [];
  const hook = renderHook(() =>
    useAttachments({
      sessionId: 's1',
      request: server.request,
      t,
      codec,
      submit,
      onAttached: (result) => {
        if (result.mention) mentions.push(result.mention);
      },
    }),
  );
  return { ...hook, server, mentions };
}

const settled = (r: { result: { current: { busy: boolean } } }) =>
  waitFor(() => expect(r.result.current.busy).toBe(false), { timeout: 5000 });

describe('useAttachments batches', () => {
  it('attaches 5 images as 5 chips at once and uploads them one after another', async () => {
    const { codec } = trackedCodec();
    const r = setup(undefined, codec);
    const files = [1, 2, 3, 4, 5].map((i) => jpg(`p${i}.jpg`));
    act(() => {
      r.result.current.addFiles(files);
    });
    expect(r.result.current.items).toHaveLength(5);
    expect(r.result.current.items[0]?.status).toBe('preparing');
    await settled(r);
    expect(r.result.current.items.map((i) => i.status)).toEqual(Array(5).fill('done'));
    const kinds = r.server.log.filter((k) => k === 'attach_begin' || k === 'attach_end');
    expect(kinds).toEqual(Array(5).fill(['attach_begin', 'attach_end']).flat());
    expect(r.server.maxOpen()).toBe(1);
  });

  it('keeps duplicate names distinct (iOS names every pick image.jpg)', async () => {
    const r = setup(undefined, trackedCodec().codec);
    act(() => {
      r.result.current.addFiles([jpg('image.jpg'), jpg('image.jpg'), jpg('image.jpg')]);
    });
    await settled(r);
    const names = r.result.current.items.map((i) => i.name);
    expect(names).toEqual(['image.jpg', 'image (2).jpg', 'image (3).jpg']);
    expect(new Set(r.result.current.items.map((i) => i.id)).size).toBe(3);
    expect(r.server.begins.map((b) => b.name)).toEqual(names);
  });

  it('treats an empty MIME type with a .jpg/.heic name as an image', async () => {
    const { codec, state } = trackedCodec();
    const r = setup(undefined, codec);
    act(() => {
      r.result.current.addFiles([jpg('a.JPG', ''), jpg('b.heic', '')]);
    });
    await settled(r);
    expect(r.result.current.items.every((i) => i.image)).toBe(true);
    expect(state.decodes).toBe(2);
    expect(r.server.begins.map((b) => b.mime)).toEqual(['image/jpeg', 'image/jpeg']);
  });

  it('one failing file does not drop the others', async () => {
    const r = setup(fakeServer({ failName: 'bad.jpg' }), trackedCodec().codec);
    act(() => {
      r.result.current.addFiles([jpg('a.jpg'), jpg('bad.jpg'), jpg('c.jpg')]);
    });
    await settled(r);
    expect(r.result.current.items.map((i) => [i.name, i.status])).toEqual([
      ['a.jpg', 'done'],
      ['bad.jpg', 'error'],
      ['c.jpg', 'done'],
    ]);
    expect(r.result.current.items[1]?.error).toBeTruthy();
  });

  it('prepares 50 large photos with one bitmap alive at a time and small previews', async () => {
    const { codec, state } = trackedCodec(8000, 6000);
    const r = setup(undefined, codec);
    let res: ReturnType<typeof r.result.current.addFiles> | undefined;
    act(() => {
      res = r.result.current.addFiles(
        Array.from({ length: 50 }, (_, i) => fat(`IMG_${i}.jpg`, 12_000_000)),
      );
    });
    expect(res).toEqual({ added: 50, droppedImages: 0, droppedFiles: 0 });
    expect(r.result.current.items).toHaveLength(50);
    await settled(r);
    expect(r.result.current.items.filter((i) => i.status === 'done')).toHaveLength(50);
    expect(state.peak).toBe(1);
    expect(state.alive).toBe(0);
    expect(r.server.maxOpen()).toBe(1);
    // Previews come from the 256 px encode, never from the full file.
    expect(created).toHaveLength(50);
    const thumbCalls = vi
      .mocked(codec.encode)
      .mock.calls.filter(([, w, h]) => Math.max(w, h) <= 256);
    expect(thumbCalls).toHaveLength(50);
  });

  it('refuses the 51st photo, 11th document, and counts the kinds separately', () => {
    const { codec } = trackedCodec();
    const r = setup(undefined, codec);
    let res: ReturnType<typeof r.result.current.addFiles> | undefined;
    act(() => {
      res = r.result.current.addFiles(Array.from({ length: 51 }, (_, i) => jpg(`p${i}.jpg`)));
    });
    expect(res).toEqual({ added: MAX_IMAGES_PER_MESSAGE, droppedImages: 1, droppedFiles: 0 });
    act(() => {
      res = r.result.current.addFiles(
        Array.from({ length: 11 }, (_, i) => jpg(`d${i}.pdf`, 'application/pdf')),
      );
    });
    expect(res).toEqual({ added: MAX_FILES_PER_MESSAGE, droppedImages: 0, droppedFiles: 1 });
    expect(r.result.current.items).toHaveLength(60);
    act(() => {
      res = r.result.current.addFiles([jpg('extra.jpg')]);
    });
    expect(res?.droppedImages).toBe(1);
  });

  it('accepts 50 photos together with 10 documents, and videos count as documents', () => {
    const r = setup(undefined, trackedCodec().codec);
    let res: ReturnType<typeof r.result.current.addFiles> | undefined;
    act(() => {
      res = r.result.current.addFiles([
        ...Array.from({ length: 50 }, (_, i) => jpg(`p${i}.jpg`)),
        ...Array.from({ length: 9 }, (_, i) => jpg(`d${i}.pdf`, 'application/pdf')),
        jpg('clip.mov', 'video/quicktime'),
      ]);
    });
    expect(res).toEqual({ added: 60, droppedImages: 0, droppedFiles: 0 });
  });

  it('presses Enter (submit) once, after the one mention of a batch', async () => {
    const r = setup(undefined, trackedCodec().codec, true);
    act(() => {
      r.result.current.addFiles([jpg('a.jpg'), jpg('b.jpg'), jpg('c.jpg')]);
    });
    await settled(r);
    expect(r.server.begins.map((b) => b.submit)).toEqual([false, false, false]);
    expect(r.server.batchEnds).toMatchObject([{ submit: true }]);
  });

  it('mentions a multi-photo pick once, with one batch id for all its files', async () => {
    const r = setup(undefined, trackedCodec().codec);
    act(() => {
      r.result.current.addFiles([jpg('a.jpg'), jpg('b.jpg'), jpg('c.jpg')]);
    });
    await settled(r);
    const ids = new Set(r.server.begins.map((b) => b.batch));
    expect(ids.size).toBe(1);
    expect([...ids][0]).toMatch(/^[A-Za-z0-9_-]{1,64}$/);
    expect(r.server.batchEnds).toHaveLength(1);
    expect(r.server.batchEnds[0]?.batch_id).toBe([...ids][0]);
    // The end comes after the last file, and is the only mention.
    expect(r.server.log.at(-1)).toBe('attach_batch_end');
    expect(r.mentions).toEqual(['Attached files (3): @b/f1 @b/f2 @b/f3']);
  });

  it('mentions a 50-photo batch once', async () => {
    const r = setup(undefined, trackedCodec().codec);
    act(() => {
      r.result.current.addFiles(Array.from({ length: 50 }, (_, i) => jpg(`IMG_${i}.jpg`)));
    });
    await settled(r);
    expect(r.server.begins).toHaveLength(50);
    expect(new Set(r.server.begins.map((b) => b.batch)).size).toBe(1);
    expect(r.server.batchEnds).toHaveLength(1);
    expect(r.mentions).toHaveLength(1);
    expect(r.result.current.items.every((i) => i.status === 'done')).toBe(true);
  });

  it('a single file is not a batch', async () => {
    const r = setup(undefined, trackedCodec().codec);
    act(() => {
      r.result.current.addFiles([jpg('a.jpg')]);
    });
    await settled(r);
    expect(r.server.begins[0]?.batch).toBeUndefined();
    expect(r.server.batchEnds).toHaveLength(0);
    expect(r.mentions).toEqual(['Attached file: @f1']);
  });

  it('a batch whose end fails still mentions every file (never none)', async () => {
    const r = setup(fakeServer({ failBatchEnd: true }), trackedCodec().codec);
    act(() => {
      r.result.current.addFiles([jpg('a.jpg'), jpg('b.jpg')]);
    });
    await settled(r);
    expect(r.mentions).toEqual(['Attached file: @b/f1', 'Attached file: @b/f2']);
  });

  it('an older server without batches mentions each file as before', async () => {
    const r = setup(fakeServer({ legacy: true }), trackedCodec().codec);
    act(() => {
      r.result.current.addFiles([jpg('a.jpg'), jpg('b.jpg')]);
    });
    await settled(r);
    expect(r.mentions).toEqual(['Attached file: @f1', 'Attached file: @f2']);
    // Nothing owed: no batch end is asked for.
    expect(r.server.batchEnds).toHaveLength(0);
  });

  it('stays busy until the batch mention is in', async () => {
    let release: () => void = () => {};
    const gate = new Promise<void>((done) => {
      release = done;
    });
    const server = fakeServer();
    const inner = server.request.getMockImplementation()!;
    let asked = 0;
    server.request.mockImplementation(async (body: ClientMsg) => {
      if (body.type === 'attach_batch_end') {
        asked++;
        await gate;
      }
      return inner(body);
    });
    const r = setup(server, trackedCodec().codec);
    act(() => {
      r.result.current.addFiles([jpg('a.jpg'), jpg('b.jpg')]);
    });
    await waitFor(() => expect(asked).toBe(1));
    expect(r.result.current.items.every((i) => i.status === 'done')).toBe(true);
    expect(r.result.current.busy).toBe(true);
    release();
    await settled(r);
    expect(r.mentions).toHaveLength(1);
  });

  it('revokes the preview when a chip is removed, and on unmount', async () => {
    const r = setup(undefined, trackedCodec().codec);
    act(() => {
      r.result.current.addFiles([jpg('a.jpg'), jpg('b.jpg')]);
    });
    await settled(r);
    const [a, b] = r.result.current.items;
    expect(a?.thumbUrl).toBeTruthy();
    act(() => r.result.current.dismiss(a!.id));
    expect(revoked).toContain(a!.thumbUrl);
    r.unmount();
    expect(revoked).toContain(b!.thumbUrl);
  });

  it('does not leak a preview for a chip removed while its photo is being decoded', async () => {
    const r = setup(undefined, trackedCodec().codec);
    act(() => {
      r.result.current.addFiles([jpg('a.jpg')]);
    });
    act(() => r.result.current.dismiss(r.result.current.items[0]!.id));
    await settled(r);
    expect(created).toHaveLength(0);
  });

  it('shows the clear error when an undecodable HEIC is too large to send as is', async () => {
    const failing: ImageCodec = {
      decode: async () => {
        throw new Error('no HEIC');
      },
      encode: async () => new Blob(),
    };
    const r = setup(undefined, failing);
    act(() => {
      r.result.current.addFiles([
        jpg('big.heic', '', 30_000_000),
        jpg('ok.heic', 'image/heic', 5_000_000),
      ]);
    });
    await settled(r);
    const [big, ok] = r.result.current.items;
    expect(big?.status).toBe('error');
    expect(big?.error).toBe('attach.errCannotShrink');
    // A smaller one goes up as the original.
    expect(ok?.status).toBe('done');
    expect(r.server.begins.at(-1)?.size).toBe(5_000_000);
  });

  it('rejects a 30 MB DNG with a per-file message and still sends the rest', async () => {
    const r = setup(undefined, trackedCodec().codec);
    act(() => {
      r.result.current.addFiles([jpg('RAW.dng', 'image/x-adobe-dng', 30_000_000), jpg('a.jpg')]);
    });
    await settled(r);
    expect(r.result.current.items[0]?.error).toBe('attach.errTooLargeSize');
    expect(r.result.current.items[1]?.status).toBe('done');
  });
});
