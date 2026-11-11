import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { describe, expect, it, vi } from 'vitest';

import { trackVisualViewport } from '../hooks/visualViewport';
import { MockAttachments, type AttachMsg } from '../protocol/mockAttachments';
import type { ClientMsg, ServerMsg, SessionInfo } from '../protocol/types';
import { cloudSession, localSession } from '../test/fixtures';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { installFakeViewport } from '../test/fakeViewport';
import { renderWithApp } from '../test/render';
import { I18nProvider } from '../i18n/context';
import { useAppState } from '../state/store';
import { AttachMenu } from './AttachMenu';
import { ChatView } from './ChatView';
import { Composer } from './Composer';

/** The chat of a session: its sent prompts (with their attachments). */
function SentBubbles({ sessionId }: { sessionId: string }) {
  const sent = useAppState((s) => s.sent[sessionId]);
  return <ChatView events={[]} gapFrom={null} sent={sent} />;
}

const ATTACH_TYPES = new Set<ClientMsg['type']>([
  'attach',
  'attach_begin',
  'attach_chunk',
  'attach_end',
  'attach_cancel',
  'attach_folder_begin',
  'attach_folder_end',
  'attach_batch_end',
]);

/** A fake server that handles attachments like vca-server (in memory). */
function attachTransport(session: SessionInfo, override?: (msg: ClientMsg) => ServerMsg | null) {
  const mock = new MockAttachments(
    (id) => (id === session.id ? session : undefined),
    () => Date.UTC(2026, 9, 30, 8, 34, 56),
    () => {},
  );
  return new FakeTransport((msg) => {
    const custom = override?.(msg);
    if (custom) return custom;
    if (ATTACH_TYPES.has(msg.type)) return mock.handle(msg as AttachMsg);
    return defaultResponse(msg);
  });
}

type AttachBegin = Extract<ClientMsg, { type: 'attach_begin' }>;

const beginBodies = (t: FakeTransport) =>
  t.sent.map((e) => e.body).filter((b): b is AttachBegin => b.type === 'attach_begin');

const DESKTOP = { mobile: false, folders: true };
const PHONE = { mobile: true, folders: false };

function renderMenu(platform = DESKTOP) {
  const onFiles = vi.fn();
  const onFolder = vi.fn();
  render(
    <I18nProvider locale="en">
      <AttachMenu
        platform={platform}
        onFiles={onFiles}
        onFolder={onFolder}
        autoSend={false}
        onAutoSendChange={() => {}}
        onSketch={() => {}}
      />
    </I18nProvider>,
  );
  return { onFiles, onFolder };
}

describe('AttachMenu', () => {
  it('offers photos, files and folder on desktop', async () => {
    renderMenu(DESKTOP);
    await userEvent.click(screen.getByRole('button', { name: 'Attach files' }));
    const menu = screen.getByRole('menu');
    const items = within(menu)
      .getAllByRole('menuitem')
      .map((b) => b.textContent);
    expect(items).toEqual([
      'Photo library',
      'SketchDraw, or mark up a photo',
      'FilesDevice and cloud documents',
      'FolderSend a whole folder',
    ]);
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('menu')).toBeNull();
  });

  it('offers the camera and hides folders on phones', async () => {
    renderMenu(PHONE);
    await userEvent.click(screen.getByRole('button', { name: 'Attach files' }));
    const names = screen.getAllByRole('menuitem').map((b) => b.textContent);
    expect(names).toContain('Take photo');
    expect(names.some((n) => n?.startsWith('Folder'))).toBe(false);
    expect(screen.queryByTestId('attach-folder')).toBeNull();
  });

  it('uses inputs that open the right system pickers', () => {
    renderMenu(DESKTOP);
    const photos = screen.getByTestId('attach-photos');
    expect(photos).toHaveAttribute('accept', 'image/*');
    expect(photos).toHaveAttribute('multiple');
    expect(photos).not.toHaveAttribute('capture');
    const camera = screen.getByTestId('attach-camera');
    expect(camera).toHaveAttribute('capture', 'environment');
    expect(camera).toHaveAttribute('accept', 'image/*');
    // Files: device storage and cloud providers, so no filter and no capture.
    const files = screen.getByTestId('attach-files');
    expect(files).not.toHaveAttribute('accept');
    expect(files).not.toHaveAttribute('capture');
    expect(files).toHaveAttribute('multiple');
    expect(screen.getByTestId('attach-folder')).toHaveAttribute('webkitdirectory');
  });

  it('passes picked files and folders on', async () => {
    const { onFiles, onFolder } = renderMenu(DESKTOP);
    const a = new File(['a'], 'a.txt', { type: 'text/plain' });
    const b = new File(['b'], 'b.png', { type: 'image/png' });
    fireEvent.change(screen.getByTestId('attach-files'), { target: { files: [a, b] } });
    expect(onFiles).toHaveBeenCalledWith([a, b]);
    fireEvent.change(screen.getByTestId('attach-folder'), { target: { files: [a] } });
    expect(onFolder).toHaveBeenCalledWith([a]);
  });

  it('copies the FileList before the input is reset (WebKit empties it)', () => {
    const { onFiles } = renderMenu(PHONE);
    const a = new File(['a'], 'image.jpg', { type: 'image/jpeg' });
    const b = new File(['b'], 'image.jpg', { type: 'image/jpeg' });
    const live = [a, b];
    const input = screen.getByTestId('attach-photos');
    Object.defineProperty(input, 'files', { get: () => live, configurable: true });
    Object.defineProperty(input, 'value', {
      get: () => '',
      set: () => {
        live.length = 0;
      },
      configurable: true,
    });
    fireEvent.change(input);
    expect(onFiles).toHaveBeenCalledWith([a, b]);
  });

  it('allows several photos and no capture on the library input', () => {
    renderMenu(PHONE);
    const photos = screen.getByTestId('attach-photos');
    expect(photos).toHaveAttribute('multiple');
    expect(photos).not.toHaveAttribute('capture');
  });
});

function ToastProbe() {
  const toasts = useAppState((s) => s.toasts);
  return <output data-testid="toasts">{toasts.map((x) => x.body).join('|')}</output>;
}

async function pick(testId: string, files: File[]) {
  const input = await screen.findByTestId(testId);
  await act(async () => {
    fireEvent.change(input, { target: { files } });
  });
}

async function ready() {
  await waitFor(() => expect(screen.getByRole('button', { name: 'Attach files' })).toBeEnabled());
}

describe('Composer attachments', () => {
  it('uploads a file and shows the attached chip (managed session types the mention)', async () => {
    const session = localSession();
    const transport = attachTransport(session);
    renderWithApp(<Composer session={session} />, { transport });
    await ready();
    await pick('attach-files', [new File(['hello'], 'notes.md', { type: 'text/markdown' })]);
    expect(await screen.findByText('Attached: notes.md')).toBeInTheDocument();
    expect(
      screen.getByText('Mentioned in the session; add instructions and send'),
    ).toBeInTheDocument();
    expect(transport.bodies('attach_begin')).toMatchObject([{ name: 'notes.md', submit: false }]);
    // The server typed the mention, so the text box stays as it was.
    expect(screen.getByRole('textbox', { name: 'Command for payments-api' })).toHaveValue('');
  });

  it('prefills the mention when the server did not type it', async () => {
    const session = localSession({ control: 'terminal_api' });
    const transport = attachTransport(session);
    renderWithApp(<Composer session={session} />, { transport });
    await ready();
    await pick('attach-photos', [new File(['img'], 'shot.gif', { type: 'image/gif' })]);
    expect(await screen.findByText('Mention added to your message')).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.getByRole('textbox', { name: 'Command for payments-api' })).toHaveValue(
        'Attached file: @.vca/attachments/20261030-083456-shot.gif ',
      ),
    );
  });

  it('observed sessions show where the file was saved', async () => {
    const session = localSession({ control: 'observe_only' });
    renderWithApp(<Composer session={session} />, { transport: attachTransport(session) });
    await ready();
    await pick('attach-files', [new File(['x'], 'log.txt', { type: 'text/plain' })]);
    expect(
      await screen.findByText(
        'Saved to .vca/attachments/20261030-083456-log.txt; mention it in your next message',
      ),
    ).toBeInTheDocument();
  });

  it('shows errors with retry, and refuses files over the limit before sending', async () => {
    const session = localSession();
    let fail = true;
    const transport = attachTransport(session, (msg) =>
      msg.type === 'attach_begin' && fail
        ? { type: 'error', code: 'unavailable', message: 'disk full' }
        : null,
    );
    renderWithApp(<Composer session={session} />, { transport });
    await ready();
    await pick('attach-files', [new File(['x'], 'a.txt', { type: 'text/plain' })]);
    // A short, translated reason; the server's own text only under "Details".
    expect(await screen.findByText('Upload failed')).toBeInTheDocument();
    expect(screen.queryByText('disk full')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Details' }));
    expect(screen.getByText('disk full')).toBeInTheDocument();
    fail = false;
    await userEvent.click(screen.getByRole('button', { name: 'Retry a.txt' }));
    expect(await screen.findByText('Attached: a.txt')).toBeInTheDocument();

    const huge = new File(['x'], 'huge.pdf', { type: 'application/pdf' });
    Object.defineProperty(huge, 'size', { value: 30 * 1024 * 1024 });
    await pick('attach-files', [huge]);
    expect(await screen.findByText('Too large (30 MB; the limit is 25 MB).')).toBeInTheDocument();
    expect(beginBodies(transport).filter((b) => b.name === 'huge.pdf')).toEqual([]);
  });

  it('shows a short reason for a folder outside the workspace roots, with details on demand', async () => {
    const session = localSession();
    const path = 'E:\\Voice-Command-AI\\.claude\\worktrees\\agent-a6b89483909a3f5af\\project';
    const transport = attachTransport(session, (msg) =>
      msg.type === 'attach_begin'
        ? {
            type: 'error',
            code: 'forbidden',
            message: `the project folder ${path} is outside the allowed workspace roots`,
          }
        : null,
    );
    renderWithApp(<Composer session={session} />, { transport });
    await ready();
    const name = 'a-very-long-screenshot-name-from-the-phone-2026-10-31.png';
    await pick('attach-files', [new File(['x'], name, { type: 'text/plain' })]);
    expect(
      await screen.findByText(
        "This session's folder is outside the PC's approved folders, so files can't be sent to it.",
      ),
    ).toBeInTheDocument();
    // The long path is not inline; it is in the tooltip and under "Details".
    expect(screen.queryByText(new RegExp('worktrees'))).toBeNull();
    // Long names are shortened in the middle, keeping the extension.
    const chipName = screen.getByTitle(name);
    expect(chipName.textContent).toMatch(/^a-very-lon.*….*.png$/);
    expect(chipName.textContent!.length).toBeLessThanOrEqual(28);
    // Only retry and remove.
    expect(screen.getByRole('button', { name: `Retry ${name}` })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: `Remove ${name}` })).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Details' }));
    expect(screen.getByText(new RegExp('worktrees'))).toBeInTheDocument();
  });

  it('will not send the text silently while an attachment failed', async () => {
    const session = localSession();
    const transport = attachTransport(session, (msg) =>
      msg.type === 'attach_begin'
        ? {
            type: 'error',
            code: 'unsupported',
            message: 'this file type (video/mp4) cannot be attached',
          }
        : null,
    );
    renderWithApp(<Composer session={session} />, { transport });
    await ready();
    await pick('attach-files', [new File(['x'], 'clip.mp4', { type: 'video/mp4' })]);
    expect(await screen.findByText("This file type can't be attached.")).toBeInTheDocument();
    const box = screen.getByRole('textbox', { name: 'Command for payments-api' });
    await userEvent.type(box, 'look at the clip');
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    expect(
      screen.getByText(
        'Attachments failed to upload (1). Retry, or send your message without them.',
      ),
    ).toBeInTheDocument();
    expect(transport.bodies('send_input')).toEqual([]);
    await userEvent.click(screen.getByRole('button', { name: 'Send without them' }));
    await waitFor(() => expect(transport.bodies('send_input')).toHaveLength(1));
    expect(screen.queryByText('clip.mp4')).toBeNull();
  });

  it('clears delivered attachments from the tray and shows them in the sent bubble', async () => {
    const session = localSession();
    const transport = attachTransport(session);
    renderWithApp(
      <>
        <Composer session={session} />
        <SentBubbles sessionId={session.id} />
      </>,
      { transport },
    );
    await ready();
    await pick('attach-files', [new File(['hello'], 'notes.md', { type: 'text/markdown' })]);
    expect(await screen.findByText('Attached: notes.md')).toBeInTheDocument();
    await userEvent.type(
      screen.getByRole('textbox', { name: 'Command for payments-api' }),
      'summarize the notes',
    );
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(screen.queryByText('Attached: notes.md')).toBeNull());
    expect(screen.queryByRole('list', { name: 'Attachments' })).toBeNull();
    const files = await screen.findByRole('list', { name: 'Attached files' });
    expect(within(files).getByText('notes.md')).toBeInTheDocument();
  });

  it('cancels a running upload', async () => {
    const session = localSession();
    let release: () => void = () => {};
    const gate = new Promise<void>((r) => {
      release = r;
    });
    const base = attachTransport(session);
    const transport = new FakeTransport((msg) => defaultResponse(msg));
    // Hold the first chunk until the user cancels.
    transport.send = async (env) => {
      transport.sent.push(env);
      if (env.body.type === 'attach_chunk') await gate;
      return base.send(env);
    };
    renderWithApp(<Composer session={session} />, { transport });
    await ready();
    await pick('attach-files', [new File(['abc'], 'slow.txt', { type: 'text/plain' })]);
    const cancel = await screen.findByRole('button', { name: 'Cancel slow.txt' });
    await userEvent.click(cancel);
    release();
    expect(await screen.findByText('Cancelled')).toBeInTheDocument();
    await waitFor(() => expect(transport.bodies('attach_cancel')).toHaveLength(1));
    expect(transport.bodies('attach_end')).toEqual([]);
  });

  it('attaches the first 10 documents and says so in the tray and (after the picker) a toast', async () => {
    const session = localSession();
    const transport = attachTransport(session);
    renderWithApp(
      <>
        <Composer session={session} />
        <ToastProbe />
      </>,
      { transport },
    );
    await ready();
    await pick(
      'attach-files',
      Array.from({ length: 11 }, (_, i) => new File(['x'], `d${i}.txt`, { type: 'text/plain' })),
    );
    expect(screen.getByTestId('attach-notice')).toHaveTextContent(
      'Up to 10 files can be attached at once — 1 not attached.',
    );
    // The toast waits for the picker sheet to close.
    expect(screen.getByTestId('toasts').textContent).toBe('');
    await waitFor(
      () => expect(screen.getByTestId('toasts').textContent).toContain('only the first 10 files'),
      { timeout: 3000 },
    );
    expect(
      screen.getAllByRole('listitem').filter((li) => li.className.includes('chip')),
    ).toHaveLength(10);
  });

  it('picking 51 photos keeps an inline notice until dismissed', async () => {
    const session = localSession();
    renderWithApp(<Composer session={session} />, { transport: attachTransport(session) });
    await ready();
    await pick(
      'attach-photos',
      Array.from({ length: 51 }, (_, i) => new File(['x'], `IMG_${i}.gif`, { type: 'image/gif' })),
    );
    const notice = screen.getByTestId('attach-notice');
    expect(notice).toHaveTextContent('Up to 50 photos can be attached at once — 1 not attached.');
    expect(notice).toHaveAttribute('role', 'status');
    // It stays while the uploads run and finish.
    await waitFor(() => expect(screen.getAllByText(/^Attached: IMG_/)).toHaveLength(50), {
      timeout: 5000,
    });
    expect(screen.getByTestId('attach-notice')).toBeInTheDocument();
    await userEvent.click(within(notice).getByRole('button', { name: 'Dismiss' }));
    expect(screen.queryByTestId('attach-notice')).toBeNull();
  });

  it('the over-limit notice is cleared by the send', async () => {
    const session = localSession();
    const transport = attachTransport(session);
    renderWithApp(<Composer session={session} />, { transport });
    await ready();
    await pick(
      'attach-files',
      Array.from({ length: 11 }, (_, i) => new File(['x'], `d${i}.txt`, { type: 'text/plain' })),
    );
    expect(screen.getByTestId('attach-notice')).toBeInTheDocument();
    await waitFor(() => expect(screen.getAllByText(/^Attached: d/)).toHaveLength(10));
    await userEvent.type(
      screen.getByRole('textbox', { name: 'Command for payments-api' }),
      'read them',
    );
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(transport.bodies('send_input')).toHaveLength(1));
    await waitFor(() => expect(screen.queryByTestId('attach-notice')).toBeNull());
  });

  /** Holds every `attach_chunk` until `release()`; handles the rest like vca-server. */
  function gatedTransport(session: SessionInfo, override?: (msg: ClientMsg) => ServerMsg | null) {
    let release: () => void = () => {};
    const gate = new Promise<void>((r) => {
      release = r;
    });
    const base = attachTransport(session, override);
    const transport = new FakeTransport((msg) => defaultResponse(msg));
    transport.send = async (env) => {
      transport.sent.push(env);
      if (env.body.type === 'attach_chunk') await gate;
      return base.send(env);
    };
    return { transport, release: () => release() };
  }

  it('holds a typed send when a decision opened during the upload wait', async () => {
    const session = localSession({ control: 'terminal_api' });
    const { transport, release } = gatedTransport(session);
    const waiting: SessionInfo = {
      ...session,
      state: { type: 'awaiting_decision' },
      pending_decision: {
        at: 100,
        decision: { id: 'd1', prompt: 'Allow?', options: [{ id: 'yes', label: 'Yes' }] },
      },
    };
    function Live() {
      const [shown, set] = useState(session);
      return (
        <>
          <button type="button" onClick={() => set(waiting)}>
            open a decision
          </button>
          <Composer session={shown} />
        </>
      );
    }
    renderWithApp(<Live />, { transport });
    await ready();
    await pick('attach-files', [new File(['1'], 'a.txt', { type: 'text/plain' })]);
    const box = screen.getByRole('textbox', { name: 'Command for payments-api' });
    await userEvent.type(box, 'compare these');
    await userEvent.click(await screen.findByRole('button', { name: /^Send after upload/ }));
    await userEvent.click(screen.getByRole('button', { name: 'open a decision' }));
    release();
    expect(await screen.findByText(/waiting for a decision/)).toBeInTheDocument();
    expect(transport.bodies('send_input')).toEqual([]);
    expect(box).not.toHaveValue('');
  });

  it('Send while a batch uploads waits, then sends ONE prompt with the one batch mention', async () => {
    const session = localSession({ control: 'terminal_api' });
    const { transport, release } = gatedTransport(session);
    renderWithApp(<Composer session={session} />, { transport });
    await ready();
    await pick('attach-files', [
      new File(['1'], 'a.txt', { type: 'text/plain' }),
      new File(['2'], 'b.txt', { type: 'text/plain' }),
    ]);
    const box = screen.getByRole('textbox', { name: 'Command for payments-api' });
    await userEvent.type(box, 'compare these');
    const send = await screen.findByRole('button', { name: /^Send after upload · 0\/2/ });
    await userEvent.click(send);
    // Queued: nothing went out, and the button now cancels.
    expect(
      screen.getByRole('button', {
        name: 'Sends when the upload finishes. Cancel the queued send',
      }),
    ).toBeInTheDocument();
    expect(transport.bodies('send_input')).toEqual([]);
    release();
    await waitFor(() => expect(transport.bodies('send_input')).toHaveLength(1));
    const [sent] = transport.bodies('send_input') as Extract<ClientMsg, { type: 'send_input' }>[];
    const dir = '.vca/attachments/20261030-083456-batch';
    expect(sent!.input).toEqual({
      type: 'text',
      text: `compare these Attached files (2): @${dir}/a.txt @${dir}/b.txt`,
      submit: true,
      at_prompt: true,
    });
    // One batch end, after the files; no per-file mentions.
    expect(transport.bodies('attach_batch_end')).toHaveLength(1);
    await waitFor(() => expect(box).toHaveValue(''));
    expect(screen.getByRole('button', { name: 'Send' })).toBeInTheDocument();
  });

  it('a queued send can be cancelled', async () => {
    const session = localSession({ control: 'terminal_api' });
    const { transport, release } = gatedTransport(session);
    renderWithApp(<Composer session={session} />, { transport });
    await ready();
    await pick('attach-files', [
      new File(['1'], 'a.txt', { type: 'text/plain' }),
      new File(['2'], 'b.txt', { type: 'text/plain' }),
    ]);
    const box = screen.getByRole('textbox', { name: 'Command for payments-api' });
    await userEvent.type(box, 'compare these');
    await userEvent.click(await screen.findByRole('button', { name: /^Send after upload/ }));
    await userEvent.click(
      screen.getByRole('button', {
        name: 'Sends when the upload finishes. Cancel the queued send',
      }),
    );
    release();
    await waitFor(() => expect(transport.bodies('attach_batch_end')).toHaveLength(1));
    await waitFor(() => expect((box as HTMLTextAreaElement).value).toContain('Attached files (2)'));
    expect(transport.bodies('send_input')).toEqual([]);
    expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled();
  });

  it('a failed attachment does not hold back a queued send; the tray names it', async () => {
    const session = localSession();
    const { transport, release } = gatedTransport(session, (msg) =>
      msg.type === 'attach_begin' && msg.name === 'bad.txt'
        ? { type: 'error', code: 'unavailable', message: 'disk full' }
        : null,
    );
    renderWithApp(<Composer session={session} />, { transport });
    await ready();
    await pick('attach-files', [
      new File(['1'], 'bad.txt', { type: 'text/plain' }),
      new File(['2'], 'good.txt', { type: 'text/plain' }),
    ]);
    await userEvent.type(screen.getByRole('textbox', { name: 'Command for payments-api' }), 'look');
    await userEvent.click(await screen.findByRole('button', { name: /^Send after upload/ }));
    release();
    await waitFor(() => expect(transport.bodies('send_input')).toHaveLength(1));
    expect(await screen.findByTestId('attach-notice')).toHaveTextContent(
      'Sent without 1 attachment that failed to upload: bad.txt',
    );
    // The failed chip stays for a retry.
    expect(screen.getByRole('button', { name: 'Retry bad.txt' })).toBeInTheDocument();
  });

  it('uploads several files one after another', async () => {
    const session = localSession();
    const transport = attachTransport(session);
    renderWithApp(<Composer session={session} />, { transport });
    await ready();
    await pick('attach-photos', [
      new File(['1'], 'one.gif', { type: 'image/gif' }),
      new File(['2'], 'two.gif', { type: 'image/gif' }),
    ]);
    expect(await screen.findByText('Attached: two.gif')).toBeInTheDocument();
    expect(screen.getByText('Attached: one.gif')).toBeInTheDocument();
    const order = transport.sent
      .map((e) => e.body)
      .filter((b) => b.type === 'attach_begin' || b.type === 'attach_end')
      .map((b) => b.type);
    expect(order).toEqual(['attach_begin', 'attach_end', 'attach_begin', 'attach_end']);
  });

  it('shows the tray as one summary line while typing with the keyboard open', async () => {
    const viewport = installFakeViewport(390, 844);
    const stop = trackVisualViewport(window, { touch: true });
    try {
      const session = localSession();
      const transport = attachTransport(session);
      renderWithApp(<Composer session={session} />, { transport });
      await ready();
      await pick('attach-photos', [
        new File(['1'], 'one.gif', { type: 'image/gif' }),
        new File(['2'], 'two.gif', { type: 'image/gif' }),
      ]);
      expect(await screen.findByText('Attached: two.gif')).toBeInTheDocument();
      act(() => {
        screen.getByRole('textbox', { name: 'Command for payments-api' }).focus();
        viewport.keyboard(500);
      });
      expect(screen.getByTestId('attach-summary')).toHaveTextContent('Photos attached: 2');
      expect(screen.queryByText('Attached: two.gif')).not.toBeInTheDocument();
      act(() => viewport.close());
      expect(screen.queryByTestId('attach-summary')).not.toBeInTheDocument();
      expect(screen.getByText('Attached: two.gif')).toBeInTheDocument();
    } finally {
      stop();
      viewport.restore();
    }
  });

  it('accepts dropped files', async () => {
    const session = localSession();
    const transport = attachTransport(session);
    renderWithApp(<Composer session={session} />, { transport });
    await ready();
    const region = screen.getByRole('region', { name: 'Command for payments-api' });
    const file = new File(['d'], 'dropped.json', { type: 'application/json' });
    const dataTransfer = { types: ['Files'], files: [file], items: null, dropEffect: 'none' };
    fireEvent.dragEnter(region, { dataTransfer });
    expect(screen.getByText('Drop files or folders to attach them')).toBeInTheDocument();
    await act(async () => {
      fireEvent.drop(region, { dataTransfer });
    });
    expect(await screen.findByText('Attached: dropped.json')).toBeInTheDocument();
  });

  it('uploads a picked folder as one mention', async () => {
    if (!('webkitdirectory' in HTMLInputElement.prototype)) {
      Object.defineProperty(HTMLInputElement.prototype, 'webkitdirectory', {
        configurable: true,
        value: false,
        writable: true,
      });
    }
    const session = localSession();
    const transport = attachTransport(session);
    renderWithApp(<Composer session={session} />, { transport });
    await ready();
    const make = (path: string) => {
      const f = new File(['x'], path.split('/').pop()!, { type: 'text/plain' });
      Object.defineProperty(f, 'webkitRelativePath', { value: path });
      return f;
    };
    await pick('attach-folder', [
      make('site/index.html'),
      make('site/.git/HEAD'),
      make('site/css/a.css'),
    ]);
    expect(await screen.findByText('Attached: site')).toBeInTheDocument();
    expect(screen.getByText(/1 hidden or system file skipped/)).toBeInTheDocument();
    expect(transport.bodies('attach_folder_begin')).toMatchObject([{ name: 'site', files: 2 }]);
    expect(beginBodies(transport).map((b) => b.folder?.path)).toEqual(['index.html', 'css/a.css']);
  });

  it('cloud sessions have no attach button', () => {
    renderWithApp(<Composer session={cloudSession()} />);
    expect(screen.queryByRole('button', { name: 'Attach files' })).toBeNull();
  });
});

describe('Composer dictation after attaching a photo', () => {
  const header = () => new Blob([new Uint8Array(44)], { type: 'audio/wav' });
  const speech = (
    recorder: object,
    transcribe = vi.fn(async () => ({ text: 'hi', corrections: [] })),
  ) => ({
    createRecorder: () => recorder as never,
    transcribe,
  });
  const photo = () => new File(['img'], 'shot.gif', { type: 'image/gif' });

  async function attachThenStart(recorder: object) {
    const session = localSession();
    renderWithApp(<Composer session={session} />, {
      transport: attachTransport(session),
      speech: speech(recorder),
    });
    await ready();
    await pick('attach-photos', [photo()]);
    expect(await screen.findByText('Attached: shot.gif')).toBeInTheDocument();
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Tap to dictate/ }));
    });
  }

  it('shows the dictated text after an attachment', async () => {
    const recorder = {
      start: vi.fn(async () => {}),
      stop: vi.fn(async () => new Blob([new Uint8Array(44 + 64_000)], { type: 'audio/wav' })),
      cancel: vi.fn(),
      peek: vi.fn(async () => new Blob([new Uint8Array(44 + 64_000)], { type: 'audio/wav' })),
      split: vi.fn(async () => new Blob([], { type: 'audio/wav' })),
    };
    await attachThenStart(recorder);
    expect(recorder.start).toHaveBeenCalled();
    await waitFor(() =>
      expect(screen.getByRole('textbox', { name: 'Command for payments-api' })).toHaveValue('hi'),
    );
    expect(screen.getByRole('button', { name: 'Attach files' })).toBeInTheDocument();
  });

  it('gives up on a microphone that delivers no audio and frees the composer', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const recorder = {
        start: vi.fn(async () => {}),
        stop: vi.fn(async () => header()),
        cancel: vi.fn(),
        peek: vi.fn(async () => header()),
        split: vi.fn(async () => new Blob([], { type: 'audio/wav' })),
      };
      await attachThenStart(recorder);
      expect(screen.getByRole('button', { name: /Dictating/ })).toBeInTheDocument();
      await act(async () => {
        await vi.advanceTimersByTimeAsync(9000);
      });
      expect(screen.getByText('Dictation could not start. Please try again.')).toBeInTheDocument();
      expect(recorder.cancel).toHaveBeenCalled();
      expect(screen.getByRole('button', { name: /Tap to dictate/ })).toBeEnabled();
      expect(screen.getByRole('textbox', { name: 'Command for payments-api' })).toBeEnabled();
      // The attachment is still there.
      expect(screen.getByText('Attached: shot.gif')).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it('can always be cancelled, even while the recorder hangs on stop', async () => {
    const recorder = {
      start: vi.fn(async () => {}),
      stop: vi.fn(() => new Promise<Blob>(() => {})),
      cancel: vi.fn(),
      peek: vi.fn(async () => new Blob([new Uint8Array(44 + 64_000)], { type: 'audio/wav' })),
      split: vi.fn(async () => new Blob([], { type: 'audio/wav' })),
    };
    await attachThenStart(recorder);
    await act(async () => {
      await new Promise((r) => setTimeout(r, 400));
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Dictating/ }));
    });
    // Stuck in "transcribing": the cancel button is still there.
    const cancel = await screen.findByRole('button', { name: 'Cancel recording' });
    await act(async () => {
      fireEvent.click(cancel);
    });
    expect(screen.getByRole('button', { name: /Tap to dictate/ })).toBeEnabled();
    expect(recorder.cancel).toHaveBeenCalled();
  });
});

describe('Composer dictation while a photo is uploading', () => {
  const audio = () => new Blob([new Uint8Array(44 + 64_000)], { type: 'audio/wav' });
  const recorder = () => ({
    start: vi.fn(async () => {}),
    stop: vi.fn(async () => audio()),
    cancel: vi.fn(),
    peek: vi.fn(async () => audio()),
    split: vi.fn(async () => new Blob([], { type: 'audio/wav' })),
  });

  /** An upload whose `attach_end` answer is held back until `release()`. */
  function gatedTransport(session: SessionInfo) {
    const inner = attachTransport(session);
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const send = inner.send.bind(inner);
    inner.send = async (envelope) => {
      if (envelope.body.type === 'attach_end') await gate;
      return send(envelope);
    };
    return { transport: inner, release };
  }

  const box = () => screen.getByRole('textbox', { name: 'Command for payments-api' });

  async function start(session: SessionInfo, transport: FakeTransport, rec: object) {
    renderWithApp(<Composer session={session} />, {
      transport,
      speech: {
        createRecorder: () => rec as never,
        transcribe: vi.fn(async () => ({ text: 'hi', corrections: [] })),
      },
    });
    await ready();
    await pick('attach-photos', [new File(['img'], 'shot.gif', { type: 'image/gif' })]);
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Tap to dictate/ }));
    });
  }

  it('shows dictated text while the upload is still in flight', async () => {
    const session = localSession({ control: 'terminal_api' });
    const { transport, release } = gatedTransport(session);
    await start(session, transport, recorder());
    await waitFor(() => expect(box()).toHaveValue('hi'));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 400));
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Dictating/ }));
    });
    await waitFor(() =>
      expect(screen.getByRole('button', { name: /Tap to dictate/ })).toBeEnabled(),
    );
    expect(box()).toHaveValue('hi');
    release();
    await waitFor(() =>
      expect((box() as HTMLTextAreaElement).value).toContain('@.vca/attachments/'),
    );
    expect((box() as HTMLTextAreaElement).value).toContain('hi');
  });

  it('slims Send on phones while dictating during an upload, so Cancel fits beside it', async () => {
    const session = localSession({ control: 'terminal_api' });
    const { transport, release } = gatedTransport(session);
    renderWithApp(<Composer session={session} layout="phone" />, {
      transport,
      speech: {
        createRecorder: () => recorder() as never,
        transcribe: vi.fn(async () => ({ text: 'hi', corrections: [] })),
      },
    });
    await ready();
    await pick('attach-photos', [new File(['img'], 'shot.gif', { type: 'image/gif' })]);
    const send = await screen.findByRole('button', { name: /^Send after upload/ });
    expect(send).toHaveAttribute('data-state', 'uploading');
    expect(send).not.toHaveAttribute('data-slim');
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Tap to dictate/ }));
    });
    expect(screen.getByRole('button', { name: 'Cancel recording' })).toBeInTheDocument();
    expect(send).toHaveAttribute('data-slim', 'true');
    release();
  });

  it('keeps the attachment mention that arrives in the middle of a dictation', async () => {
    const session = localSession({ control: 'terminal_api' });
    const { transport, release } = gatedTransport(session);
    await start(session, transport, recorder());
    await waitFor(() => expect(box()).toHaveValue('hi'));
    release();
    await screen.findByText('Mention added to your message');
    // The next live update must not wipe the mention.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 900));
    });
    expect((box() as HTMLTextAreaElement).value).toContain('@.vca/attachments/');
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /Dictating/ }));
    });
    await waitFor(() =>
      expect(screen.getByRole('button', { name: /Tap to dictate/ })).toBeEnabled(),
    );
    const value = (box() as HTMLTextAreaElement).value;
    expect(value).toContain('@.vca/attachments/');
    expect(value).toContain('hi');
  });

  it('transcribes when the app is hidden (camera, picker) in the middle of a dictation', async () => {
    const session = localSession();
    const { transport, release } = gatedTransport(session);
    const rec = recorder();
    await start(session, transport, rec);
    await waitFor(() => expect(box()).toHaveValue('hi'));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 400));
    });
    const state = Object.getOwnPropertyDescriptor(Document.prototype, 'visibilityState');
    Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true });
    try {
      await act(async () => {
        document.dispatchEvent(new Event('visibilitychange'));
      });
      await waitFor(() => expect(rec.stop).toHaveBeenCalled());
      await waitFor(() =>
        expect(screen.getByRole('button', { name: /Tap to dictate/ })).toBeEnabled(),
      );
      expect(box()).toHaveValue('hi');
    } finally {
      delete (document as unknown as Record<string, unknown>).visibilityState;
      if (state) Object.defineProperty(Document.prototype, 'visibilityState', state);
      release();
    }
  });
});
