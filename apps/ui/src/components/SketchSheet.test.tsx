import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ImageCodec } from '../attachments/resize';
import { I18nProvider } from '../i18n/context';
import { MockAttachments, type AttachMsg } from '../protocol/mockAttachments';
import type { ClientMsg, SessionInfo } from '../protocol/types';
import { clearDrafts, hasDraft } from '../sketch/drafts';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { Composer } from './Composer';
import { SketchSheet } from './SketchSheet';

/** A 2D context that records calls (jsdom has no canvas). */
function fakeContext() {
  const calls: string[] = [];
  const target: Record<string | symbol, unknown> = { calls };
  return new Proxy(target, {
    get(t, key) {
      if (key in t) return t[key];
      const fn = (..._args: unknown[]) => {
        calls.push(String(key));
      };
      t[key] = fn;
      return fn;
    },
    set(t, key, value) {
      t[key] = value;
      return true;
    },
  }) as unknown as CanvasRenderingContext2D & { calls: string[] };
}

const contexts: (CanvasRenderingContext2D & { calls: string[] })[] = [];
const blobTypes: string[] = [];
const proto = HTMLCanvasElement.prototype;
const original = {
  getContext: proto.getContext,
  toBlob: proto.toBlob,
  rect: HTMLElement.prototype.getBoundingClientRect,
};

beforeAll(() => {
  proto.getContext = function () {
    const ctx = fakeContext();
    contexts.push(ctx);
    return ctx;
  } as unknown as typeof proto.getContext;
  proto.toBlob = function (cb: BlobCallback, type?: string) {
    blobTypes.push(type ?? 'image/png');
    cb(new Blob([new Uint8Array([137, 80, 78, 71])], { type: type ?? 'image/png' }));
  };
  // The stage and canvases measure 300 x 400.
  HTMLElement.prototype.getBoundingClientRect = function () {
    return {
      x: 0,
      y: 0,
      left: 0,
      top: 0,
      right: 300,
      bottom: 400,
      width: 300,
      height: 400,
      toJSON: () => ({}),
    } as DOMRect;
  };
});

afterAll(() => {
  proto.getContext = original.getContext;
  proto.toBlob = original.toBlob;
  HTMLElement.prototype.getBoundingClientRect = original.rect;
});

beforeEach(() => {
  // Sheets unmounted by the previous test's cleanup may have saved a draft.
  clearDrafts();
  contexts.length = 0;
  blobTypes.length = 0;
});

afterEach(() => {
  clearDrafts();
});

function draw(canvas: HTMLElement, points: [number, number][], pointerType = 'mouse') {
  const [first, ...rest] = points;
  fireEvent.pointerDown(canvas, {
    pointerId: 1,
    pointerType,
    button: 0,
    clientX: first![0],
    clientY: first![1],
  });
  for (const [x, y] of rest) {
    fireEvent.pointerMove(canvas, { pointerId: 1, pointerType, clientX: x, clientY: y });
  }
  const last = points[points.length - 1]!;
  fireEvent.pointerUp(canvas, { pointerId: 1, pointerType, clientX: last[0], clientY: last[1] });
}

function renderSheet(props: Partial<Parameters<typeof SketchSheet>[0]> = {}) {
  const onDone = vi.fn();
  const onClose = vi.fn();
  const view = render(
    <I18nProvider locale="en">
      <SketchSheet draftKey="s1" onDone={onDone} onClose={onClose} {...props} />
    </I18nProvider>,
  );
  return { onDone, onClose, ...view };
}

const canvas = () => screen.getByTestId('sketch-canvas');
const pressed = (name: string) =>
  screen.getByRole('button', { name }).getAttribute('aria-pressed') === 'true';

describe('SketchSheet', () => {
  it('is a labelled modal dialog with the pen selected and focused', () => {
    renderSheet();
    const dialog = screen.getByRole('dialog', { name: 'Sketch' });
    expect(dialog).toHaveAttribute('aria-modal', 'true');
    expect(pressed('Pen')).toBe(true);
    expect(screen.getByRole('button', { name: 'Pen' })).toHaveFocus();
    // Nothing drawn yet: Done, undo and clear are off.
    expect(screen.getByRole('button', { name: 'Done' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Undo' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Clear all' })).toBeDisabled();
  });

  it('switches tools, colors, widths and pages', async () => {
    renderSheet();
    await userEvent.click(screen.getByRole('button', { name: 'Highlighter' }));
    expect(pressed('Highlighter')).toBe(true);
    expect(pressed('Pen')).toBe(false);
    await userEvent.click(screen.getByRole('button', { name: 'Red' }));
    expect(pressed('Red')).toBe(true);
    await userEvent.click(screen.getByRole('button', { name: 'Thick' }));
    expect(pressed('Thick')).toBe(true);
    expect(pressed('Medium')).toBe(false);
    await userEvent.click(screen.getByRole('button', { name: 'Grid page' }));
    expect(pressed('Grid page')).toBe(true);
  });

  it('draws strokes and shapes, with undo and redo', async () => {
    renderSheet();
    draw(canvas(), [
      [10, 10],
      [40, 30],
      [80, 60],
      [120, 70],
    ]);
    expect(canvas()).toHaveAccessibleName('Drawing canvas, 1 items');
    await userEvent.click(screen.getByRole('button', { name: 'Arrow' }));
    draw(canvas(), [
      [20, 200],
      [200, 250],
    ]);
    expect(canvas()).toHaveAccessibleName('Drawing canvas, 2 items');
    // A shape without length is dropped.
    draw(canvas(), [[50, 50]]);
    expect(canvas()).toHaveAccessibleName('Drawing canvas, 2 items');

    await userEvent.click(screen.getByRole('button', { name: 'Undo' }));
    expect(canvas()).toHaveAccessibleName('Drawing canvas, 1 items');
    await userEvent.click(screen.getByRole('button', { name: 'Redo' }));
    expect(canvas()).toHaveAccessibleName('Drawing canvas, 2 items');
    // Keyboard shortcuts on desktop.
    await userEvent.keyboard('{Control>}z{/Control}');
    expect(canvas()).toHaveAccessibleName('Drawing canvas, 1 items');
    await userEvent.keyboard('{Control>}{Shift>}z{/Shift}{/Control}');
    expect(canvas()).toHaveAccessibleName('Drawing canvas, 2 items');
    // Strokes reach the canvas as smoothed curves.
    expect(contexts.some((c) => c.calls.includes('quadraticCurveTo'))).toBe(true);
  });

  it('places a text label on tap', async () => {
    renderSheet();
    await userEvent.click(screen.getByRole('button', { name: 'Text' }));
    draw(canvas(), [[60, 80]]);
    const input = await screen.findByRole('textbox', { name: 'Text label' });
    expect(input).toHaveFocus();
    await userEvent.type(input, 'Login button{Enter}');
    expect(screen.queryByRole('textbox', { name: 'Text label' })).toBeNull();
    expect(canvas()).toHaveAccessibleName('Drawing canvas, 1 items');
    expect(contexts.some((c) => c.calls.includes('fillText'))).toBe(true);
  });

  it('clears after a confirmation, and the clear can be undone', async () => {
    renderSheet();
    draw(canvas(), [
      [10, 10],
      [50, 50],
    ]);
    await userEvent.click(screen.getByRole('button', { name: 'Clear all' }));
    const confirm = screen.getByRole('dialog', { name: 'Clear the canvas?' });
    await userEvent.click(within(confirm).getByRole('button', { name: 'Clear all' }));
    expect(canvas()).toHaveAccessibleName('Drawing canvas, 0 items');
    await userEvent.click(screen.getByRole('button', { name: 'Undo' }));
    expect(canvas()).toHaveAccessibleName('Drawing canvas, 1 items');
  });

  it('cancel closes at once when nothing is drawn', async () => {
    const { onClose } = renderSheet();
    await userEvent.click(screen.getByRole('button', { name: 'Cancel drawing' }));
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole('dialog')).toBeInTheDocument();
  });

  it('cancel asks before discarding a drawing', async () => {
    const { onClose, unmount } = renderSheet();
    draw(canvas(), [
      [10, 10],
      [50, 50],
    ]);
    await userEvent.click(screen.getByRole('button', { name: 'Cancel drawing' }));
    const confirm = screen.getByRole('dialog', { name: 'Discard this drawing?' });
    // Escape closes only the confirmation; the drawing stays.
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('dialog', { name: 'Discard this drawing?' })).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
    expect(confirm).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole('button', { name: 'Cancel drawing' }));
    await userEvent.click(screen.getByRole('button', { name: 'Discard' }));
    expect(onClose).toHaveBeenCalledTimes(1);
    unmount();
    expect(hasDraft('s1')).toBe(false);
  });

  it('Escape closes and keeps the drawing as a draft', async () => {
    const first = renderSheet();
    draw(canvas(), [
      [10, 10],
      [50, 50],
    ]);
    await userEvent.keyboard('{Escape}');
    expect(first.onClose).toHaveBeenCalledTimes(1);
    first.unmount();
    expect(hasDraft('s1')).toBe(true);

    renderSheet();
    expect(canvas()).toHaveAccessibleName('Drawing canvas, 1 items');
    expect(screen.getByRole('button', { name: 'Done' })).toBeEnabled();
  });

  it('traps focus inside the sheet', async () => {
    renderSheet();
    draw(canvas(), [
      [10, 10],
      [50, 50],
    ]);
    screen.getByRole('button', { name: 'Clear all' }).focus();
    await userEvent.tab();
    expect(screen.getByRole('button', { name: 'Cancel drawing' })).toHaveFocus();
    await userEvent.tab({ shift: true });
    expect(screen.getByRole('button', { name: 'Clear all' })).toHaveFocus();
  });

  it('exports a PNG on Done', async () => {
    const { onDone } = renderSheet();
    draw(canvas(), [
      [10, 10],
      [50, 50],
      [90, 20],
    ]);
    await userEvent.click(screen.getByRole('button', { name: 'Done' }));
    await waitFor(() => expect(onDone).toHaveBeenCalledTimes(1));
    const file = onDone.mock.calls[0]![0] as File;
    expect(file.type).toBe('image/png');
    expect(file.name).toMatch(/^sketch-\d{8}-\d{6}\.png$/);
    expect(blobTypes).toEqual(['image/png']);
  });

  it('annotate mode draws on top of a picked photo', async () => {
    const source = document.createElement('canvas');
    const close = vi.fn();
    const codec: ImageCodec = {
      decode: vi.fn(async () => ({ width: 4000, height: 3000, source, close })),
      encode: vi.fn(),
    };
    const { onDone } = renderSheet({ codec });
    const photo = new File(['jpeg'], 'screen.jpg', { type: 'image/jpeg' });
    await act(async () => {
      fireEvent.change(screen.getByTestId('sketch-photo'), { target: { files: [photo] } });
    });
    expect(codec.decode).toHaveBeenCalledWith(photo);
    expect(await screen.findByRole('dialog', { name: 'Mark up photo' })).toBeInTheDocument();
    // Scaled down to 2048 px once; the full-size bitmap is freed.
    expect(close).toHaveBeenCalled();
    expect(contexts.some((c) => c.calls.includes('drawImage'))).toBe(true);
    // Pages make no sense on a photo; red is the default ink there.
    expect(screen.queryByRole('button', { name: 'Grid page' })).toBeNull();
    expect(pressed('Red')).toBe(true);
    // A photo alone can be sent.
    expect(screen.getByRole('button', { name: 'Done' })).toBeEnabled();
    draw(canvas(), [
      [100, 100],
      [150, 160],
    ]);
    await userEvent.click(screen.getByRole('button', { name: 'Done' }));
    await waitFor(() => expect(onDone).toHaveBeenCalledTimes(1));
    expect((onDone.mock.calls[0]![0] as File).type).toBe('image/png');
  });

  it('shows an error when a photo cannot be opened', async () => {
    const codec: ImageCodec = {
      decode: vi.fn(async () => {
        throw new Error('HEIC');
      }),
      encode: vi.fn(),
    };
    renderSheet({ codec });
    await act(async () => {
      fireEvent.change(screen.getByTestId('sketch-photo'), {
        target: { files: [new File(['x'], 'a.heic', { type: 'image/heic' })] },
      });
    });
    expect(screen.getByRole('alert')).toHaveTextContent('That image could not be opened.');
  });
});

const ATTACH_TYPES = new Set<ClientMsg['type']>([
  'attach',
  'attach_begin',
  'attach_chunk',
  'attach_end',
  'attach_cancel',
]);

function attachTransport(session: SessionInfo) {
  const mock = new MockAttachments(
    (id) => (id === session.id ? session : undefined),
    () => Date.UTC(2026, 9, 31, 8, 0, 0),
    () => {},
  );
  return new FakeTransport((msg) =>
    ATTACH_TYPES.has(msg.type) ? mock.handle(msg as AttachMsg) : defaultResponse(msg),
  );
}

describe('Sketch in the composer', () => {
  it('attaches the drawing as a PNG through the normal upload', async () => {
    const session = localSession();
    const transport = attachTransport(session);
    const created = vi.fn((_: Blob | MediaSource) => 'blob:thumb');
    const revoke = vi.fn();
    const urls = { create: URL.createObjectURL, revoke: URL.revokeObjectURL };
    URL.createObjectURL = created;
    URL.revokeObjectURL = revoke;
    try {
      renderWithApp(<Composer session={session} />, { transport });
      const attach = screen.getByRole('button', { name: 'Attach files' });
      await waitFor(() => expect(attach).toBeEnabled());
      await userEvent.click(attach);
      const item = screen.getByRole('menuitem', { name: /Sketch/ });
      expect(item).toHaveTextContent('Draw, or mark up a photo');
      await userEvent.click(item);
      expect(screen.getByRole('dialog', { name: 'Sketch' })).toBeInTheDocument();

      // Closed by accident: the drawing waits as a draft.
      draw(canvas(), [
        [10, 10],
        [60, 90],
      ]);
      await userEvent.keyboard('{Escape}');
      expect(screen.queryByRole('dialog', { name: 'Sketch' })).toBeNull();
      expect(attach).toHaveFocus();
      await userEvent.click(attach);
      expect(screen.getByRole('menuitem', { name: /Sketch/ })).toHaveTextContent(
        'Continue your unsent drawing',
      );
      await userEvent.click(screen.getByRole('menuitem', { name: /Sketch/ }));
      expect(canvas()).toHaveAccessibleName('Drawing canvas, 1 items');

      await userEvent.click(screen.getByRole('button', { name: 'Done' }));
      expect(await screen.findByText(/^Attached: sketch-\d{8}-\d{6}\.png$/)).toBeInTheDocument();
      expect(screen.queryByRole('dialog', { name: 'Sketch' })).toBeNull();
      const begin = transport.bodies('attach_begin')[0] as Extract<
        ClientMsg,
        { type: 'attach_begin' }
      >;
      expect(begin.name).toMatch(/^sketch-.*\.png$/);
      expect(begin.mime).toBe('image/png');
      // The chip shows a thumbnail of the drawing.
      expect(created).toHaveBeenCalled();
      expect(
        screen.getByRole('list', { name: 'Attachments' }).querySelector('img'),
      ).toHaveAttribute('src', 'blob:thumb');
      // Sent: the draft is gone.
      expect(hasDraft(session.id)).toBe(false);
    } finally {
      URL.createObjectURL = urls.create;
      URL.revokeObjectURL = urls.revoke;
    }
  });
});
