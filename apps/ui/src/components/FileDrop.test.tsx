import { act, fireEvent, screen, waitFor, within } from '@testing-library/react';
import { useEffect } from 'react';
import { describe, expect, it, vi } from 'vitest';

import { dragHasFiles, installFileDropGuard, pastedFiles } from '../attachments/fileDrop';
import { readDrop } from '../attachments/folder';
import { MockAttachments, type AttachMsg } from '../protocol/mockAttachments';
import type { ClientMsg, SessionInfo } from '../protocol/types';
import { useActions, useAppState } from '../state/store';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { cloudSession, localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { LiveView } from './LiveView';
import { Toasts } from './Toasts';

// xterm needs a real canvas; the terminal tab is not under test here.
vi.mock('./TerminalView', () => ({ default: () => <div data-testid="terminal" /> }));

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

/** A server that lists `session` and handles attachments like vca-server. */
function serverWith(session: SessionInfo) {
  const mock = new MockAttachments(
    (id) => (id === session.id ? session : undefined),
    () => Date.UTC(2026, 10, 1, 9, 0, 0),
    () => {},
  );
  return new FakeTransport((msg) => {
    if (msg.type === 'list_sessions') return { type: 'sessions', sessions: [session] };
    if (ATTACH_TYPES.has(msg.type)) return mock.handle(msg as AttachMsg);
    return defaultResponse(msg);
  });
}

/** Selects the session and renders its pane, with the app's toasts. */
function Selected({ session }: { session: SessionInfo }) {
  const actions = useActions();
  const selected = useAppState((s) => s.selectedId);
  const known = useAppState((s) => !!s.sessions[session.id]);
  useEffect(() => {
    if (known) actions.select(session.id);
  }, [actions, known, session.id]);
  return (
    <>
      {selected === session.id && <LiveView session={session} />}
      <Toasts onOpen={() => {}} />
    </>
  );
}

async function openPane(session = localSession()) {
  const transport = serverWith(session);
  renderWithApp(<Selected session={session} />, { transport });
  const pane = await screen.findByRole('region', { name: session.display_name });
  return { transport, pane };
}

async function paperclipReady() {
  await waitFor(() => expect(screen.getByRole('button', { name: 'Attach files' })).toBeEnabled());
}

const filesDrag = (files: File[], items: unknown = null) => ({
  types: ['Files'],
  files,
  items,
  dropEffect: 'none',
});

/** A folder as WebView2 hands it over without `webkitGetAsEntry`: a File that can't be read. */
function unreadableFolder(name: string): File {
  const dir = new File([], name);
  Object.defineProperty(dir, 'slice', {
    value: () => ({ arrayBuffer: () => Promise.reject(new Error('NotReadableError')) }),
  });
  return dir;
}

interface FakeEntry {
  isFile: boolean;
  isDirectory: boolean;
  name: string;
  file?(ok: (f: File) => void): void;
  createReader?(): { readEntries(ok: (e: FakeEntry[]) => void): void };
}

const fileEntry = (file: File): FakeEntry => ({
  isFile: true,
  isDirectory: false,
  name: file.name,
  file: (ok) => ok(file),
});

const dirEntry = (name: string, children: FakeEntry[]): FakeEntry => ({
  isFile: false,
  isDirectory: true,
  name,
  createReader: () => {
    let done = false;
    return {
      readEntries: (ok) => {
        ok(done ? [] : children);
        done = true;
      },
    };
  },
});

describe('dropping files on a session pane', () => {
  it('shows the overlay over the chat view while files are dragged, and hides it after', async () => {
    const { pane } = await openPane();
    await paperclipReady();
    const log = await screen.findByRole('log');
    const drag = filesDrag([new File(['a'], 'a.txt', { type: 'text/plain' })]);

    expect(fireEvent.dragEnter(log, { dataTransfer: drag })).toBe(false);
    const overlay = screen.getByTestId('drop-overlay');
    expect(overlay).toHaveTextContent('Drop to attach');
    expect(pane).toHaveAttribute('data-dropping', 'true');
    // dragover is accepted (as a copy), so the drop reaches the pane.
    expect(fireEvent.dragOver(log, { dataTransfer: drag })).toBe(false);
    expect(drag.dropEffect).toBe('copy');

    fireEvent.dragLeave(log, { dataTransfer: drag });
    expect(screen.queryByTestId('drop-overlay')).toBeNull();
    expect(pane).not.toHaveAttribute('data-dropping');
  });

  it('attaches several files dropped on the chat view, as the paperclip does', async () => {
    const { transport } = await openPane();
    await paperclipReady();
    const log = await screen.findByRole('log');
    const drag = filesDrag([
      new File(['{}'], 'data.json', { type: 'application/json' }),
      new File(['# hi'], 'notes.md', { type: 'text/markdown' }),
    ]);
    fireEvent.dragEnter(log, { dataTransfer: drag });
    await act(async () => {
      fireEvent.drop(log, { dataTransfer: drag });
    });
    expect(screen.queryByTestId('drop-overlay')).toBeNull();
    expect(await screen.findByText('Attached: data.json')).toBeInTheDocument();
    expect(await screen.findByText('Attached: notes.md')).toBeInTheDocument();
    expect(transport.bodies('attach_begin').map((b) => (b as { name: string }).name)).toEqual([
      'data.json',
      'notes.md',
    ]);
  });

  it('attaches files dropped on the composer, with one overlay for the pane', async () => {
    const { transport } = await openPane();
    await paperclipReady();
    const composer = screen.getByRole('region', { name: 'Command for payments-api' });
    const drag = filesDrag([new File(['x'], 'x.csv', { type: 'text/csv' })]);
    fireEvent.dragEnter(composer, { dataTransfer: drag });
    expect(screen.getAllByTestId('drop-overlay')).toHaveLength(1);
    // The composer's own small hint stays away while the pane shows the overlay.
    expect(screen.queryByText('Drop files or folders to attach them')).toBeNull();
    await act(async () => {
      fireEvent.drop(composer, { dataTransfer: drag });
    });
    expect(await screen.findByText('Attached: x.csv')).toBeInTheDocument();
    expect(transport.bodies('attach_begin')).toHaveLength(1);
  });

  it('ignores text and link drags', async () => {
    const { transport } = await openPane();
    await paperclipReady();
    const log = await screen.findByRole('log');
    const text = { types: ['text/plain', 'text/uri-list'], files: [], items: null };
    expect(fireEvent.dragEnter(log, { dataTransfer: text })).toBe(true);
    expect(fireEvent.dragOver(log, { dataTransfer: text })).toBe(true);
    expect(screen.queryByTestId('drop-overlay')).toBeNull();
    expect(fireEvent.drop(log, { dataTransfer: text })).toBe(true);
    await act(async () => {});
    expect(transport.bodies('attach_begin')).toHaveLength(0);
  });

  it('uploads a dropped folder as one mention', async () => {
    const { transport } = await openPane();
    await paperclipReady();
    const log = await screen.findByRole('log');
    const site = dirEntry('site', [
      fileEntry(new File(['<p>'], 'index.html', { type: 'text/html' })),
      dirEntry('.git', [fileEntry(new File(['x'], 'HEAD'))]),
    ]);
    const items = [{ kind: 'file', webkitGetAsEntry: () => site, getAsFile: () => null }];
    const drag = filesDrag([], items);
    fireEvent.dragEnter(log, { dataTransfer: drag });
    await act(async () => {
      fireEvent.drop(log, { dataTransfer: drag });
    });
    expect(await screen.findByText('Attached: site')).toBeInTheDocument();
    expect(transport.bodies('attach_folder_begin')).toMatchObject([{ name: 'site', files: 1 }]);
  });

  it('rejects a folder it cannot read with a clear toast', async () => {
    const { transport } = await openPane();
    await paperclipReady();
    const log = await screen.findByRole('log');
    const items = [{ kind: 'file', getAsFile: () => unreadableFolder('photos') }];
    const drag = filesDrag([], items);
    fireEvent.dragEnter(log, { dataTransfer: drag });
    await act(async () => {
      fireEvent.drop(log, { dataTransfer: drag });
    });
    const toasts = screen.getByRole('region', { name: 'Notifications' });
    expect(await within(toasts).findByText("Couldn't attach")).toBeInTheDocument();
    expect(
      within(toasts).getByText(
        "photos is a folder that can't be dropped here. Use Attach files > Folder instead.",
      ),
    ).toBeInTheDocument();
    expect(transport.bodies('attach_begin')).toHaveLength(0);
  });

  it('refuses files over the size limit with a toast and sends the rest', async () => {
    const { transport } = await openPane();
    await paperclipReady();
    const log = await screen.findByRole('log');
    const big = new File(['x'], 'dump.bin', { type: 'application/octet-stream' });
    Object.defineProperty(big, 'size', { value: 30 * 1024 * 1024 });
    const small = new File(['ok'], 'ok.txt', { type: 'text/plain' });
    const drag = filesDrag([big, small]);
    fireEvent.dragEnter(log, { dataTransfer: drag });
    await act(async () => {
      fireEvent.drop(log, { dataTransfer: drag });
    });
    expect(await screen.findByText('Attached: ok.txt')).toBeInTheDocument();
    expect(screen.getByText(/dump\.bin is too large \(30 MB; the limit is 25 MB\)/)).toBeVisible();
    expect(transport.bodies('attach_begin').map((b) => (b as { name: string }).name)).toEqual([
      'ok.txt',
    ]);
  });

  it('takes no drops where the paperclip is unavailable', async () => {
    const session = cloudSession();
    const { transport, pane } = await openPane(session);
    const drag = filesDrag([new File(['a'], 'a.txt', { type: 'text/plain' })]);
    expect(fireEvent.dragEnter(pane, { dataTransfer: drag })).toBe(true);
    expect(screen.queryByTestId('drop-overlay')).toBeNull();
    fireEvent.drop(pane, { dataTransfer: drag });
    await act(async () => {});
    expect(transport.bodies('attach_begin')).toHaveLength(0);
  });

  it('takes no drops once the session has ended', async () => {
    const { pane } = await openPane(localSession({ state: { type: 'exited', code: 0 } }));
    const drag = filesDrag([new File(['a'], 'a.txt', { type: 'text/plain' })]);
    expect(fireEvent.dragEnter(pane, { dataTransfer: drag })).toBe(true);
    expect(screen.queryByTestId('drop-overlay')).toBeNull();
  });
});

describe('pasting files into the composer', () => {
  it('attaches a pasted image', async () => {
    const { transport } = await openPane();
    await paperclipReady();
    const box = screen.getByRole('textbox', { name: 'Command for payments-api' });
    const shot = new File(['png'], 'image.gif', { type: 'image/gif' });
    let prevented = false;
    await act(async () => {
      prevented = !fireEvent.paste(box, { clipboardData: { types: ['Files'], files: [shot] } });
    });
    expect(prevented).toBe(true);
    expect(await screen.findByText('Attached: image.gif')).toBeInTheDocument();
    expect(transport.bodies('attach_begin')).toMatchObject([{ name: 'image.gif' }]);
  });

  it('leaves text pastes alone, even when they carry a picture', async () => {
    const { transport } = await openPane();
    await paperclipReady();
    const box = screen.getByRole('textbox', { name: 'Command for payments-api' });
    const cells = new File(['png'], 'image.png', { type: 'image/png' });
    const data = { types: ['text/plain', 'text/html', 'Files'], files: [cells] };
    expect(fireEvent.paste(box, { clipboardData: data })).toBe(true);
    await act(async () => {});
    expect(transport.bodies('attach_begin')).toHaveLength(0);
  });
});

describe('file drop helpers', () => {
  it('tells file drags from text drags', () => {
    expect(dragHasFiles({ types: ['Files'] })).toBe(true);
    expect(dragHasFiles({ types: ['text/plain'] })).toBe(false);
    expect(dragHasFiles(null)).toBe(false);
  });

  it('pastes files only without text', () => {
    const f = new File(['a'], 'a.png', { type: 'image/png' });
    expect(pastedFiles({ types: ['Files'], files: [f] } as unknown as DataTransfer)).toEqual([f]);
    expect(
      pastedFiles({ types: ['text/plain', 'Files'], files: [f] } as unknown as DataTransfer),
    ).toEqual([]);
    expect(pastedFiles(null)).toEqual([]);
  });

  it('keeps file drops elsewhere in the window from opening the file', () => {
    const remove = installFileDropGuard(window);
    try {
      const files = { types: ['Files'], dropEffect: 'copy' };
      expect(fireEvent.dragOver(document.body, { dataTransfer: files })).toBe(false);
      expect(files.dropEffect).toBe('none');
      expect(fireEvent.drop(document.body, { dataTransfer: files })).toBe(false);
      // Text drags (into text boxes) keep working.
      const text = { types: ['text/plain'], dropEffect: 'copy' };
      expect(fireEvent.dragOver(document.body, { dataTransfer: text })).toBe(true);
      expect(fireEvent.drop(document.body, { dataTransfer: text })).toBe(true);
    } finally {
      remove();
    }
    expect(fireEvent.drop(document.body, { dataTransfer: { types: ['Files'] } })).toBe(true);
  });

  it('reads unreadable dropped folders out of the file list', async () => {
    const ok = new File(['x'], 'Makefile');
    const dropped = await readDrop(null, [ok, unreadableFolder('docs')]);
    expect(dropped.files).toEqual([ok]);
    expect(dropped.unreadable).toEqual(['docs']);
  });
});
