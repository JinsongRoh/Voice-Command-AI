import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { MockProfile } from '../protocol/mockData';
import { MockFolderError, MockFolders } from '../protocol/mockFolders';
import type { ClientMsg, LaunchRequest, ServerMsg } from '../protocol/types';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { shortenPath } from './FolderPicker';
import { NewSessionDialog } from './NewSessionDialog';

/** A server with the folder picker, backed by the demo folder tree. */
function pickerServer(profile: MockProfile = 'workstation') {
  const folders = new MockFolders(profile);
  return new FakeTransport((msg: ClientMsg): ServerMsg => {
    try {
      switch (msg.type) {
        case 'hello':
          return { ...defaultResponse(msg), features: ['project_folders'] } as ServerMsg;
        case 'project_folders':
          return { type: 'project_folders', ...folders.projectFolders(Date.now()) };
        case 'list_folder':
          return { type: 'folder', ...folders.listFolder(msg.path, msg.show_hidden) };
        case 'folder_path':
          return { type: 'folder_path', ...folders.folderPath(msg.parent, msg.name) };
        case 'launch':
          return { type: 'launched', session: localSession() };
        default:
          return defaultResponse(msg);
      }
    } catch (err) {
      if (err instanceof MockFolderError)
        return { type: 'error', code: err.code, message: err.message };
      throw err;
    }
  });
}

function renderDialog(transport: FakeTransport) {
  const onLaunched = vi.fn();
  renderWithApp(<NewSessionDialog projectDirs={[]} onClose={() => {}} onLaunched={onLaunched} />, {
    transport,
  });
  return onLaunched;
}

function launches(transport: FakeTransport): LaunchRequest[] {
  return transport
    .bodies('launch')
    .map((b) => (b as Extract<ClientMsg, { type: 'launch' }>).request);
}

const listedPaths = (transport: FakeTransport) =>
  transport
    .bodies('list_folder')
    .map((b) => (b as Extract<ClientMsg, { type: 'list_folder' }>).path);

beforeEach(() => {
  localStorage.clear();
});

describe('shortenPath', () => {
  it('keeps the drive or root and the last folders', () => {
    expect(shortenPath('C:\\Users\\dev\\code\\payments-api', '\\')).toBe(
      'C:\\…\\code\\payments-api',
    );
    expect(shortenPath('/srv/work/monorepo/services', '/')).toBe('/srv/…/monorepo/services');
    expect(shortenPath('E:\\Voice-Command-AI', '\\')).toBe('E:\\Voice-Command-AI');
  });
});

describe('folder picker in the new session dialog', () => {
  it('lists recent projects and launches in the chosen one', async () => {
    const transport = pickerServer();
    const onLaunched = renderDialog(transport);
    const recent = await screen.findByRole('list', { name: 'Recent project folders' });
    const rows = within(recent).getAllByRole('button');
    expect(rows[0]).toHaveTextContent('payments-api');
    expect(rows[0]).toHaveTextContent('C:\\…\\code\\payments-api');
    expect(within(rows[0]!).getByText('(Git repository)')).toBeInTheDocument();
    const launch = screen.getByRole('button', { name: 'Launch' });
    expect(launch).toBeDisabled();
    await userEvent.click(rows[1]!);
    expect(rows[1]).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByText('Selected: C:\\Users\\dev\\code\\web-dashboard')).toBeInTheDocument();
    await userEvent.click(launch);
    await waitFor(() => expect(onLaunched).toHaveBeenCalled());
    expect(launches(transport)[0]).toMatchObject({
      project_dir: 'C:\\Users\\dev\\code\\web-dashboard',
      create_dir: false,
    });
    // Continuing the folder's conversation is the default.
    expect(launches(transport)[0]).not.toHaveProperty('fresh');
  });

  it('continues the folder conversation by default and can start a new one', async () => {
    const transport = pickerServer();
    const onLaunched = renderDialog(transport);
    const recent = await screen.findByRole('list', { name: 'Recent project folders' });
    await userEvent.click(within(recent).getAllByRole('button')[0]!);
    const fresh = screen.getByRole('checkbox', { name: 'Start a new conversation' });
    expect(fresh).not.toBeChecked();
    expect(screen.getByText(/continues where you left off \(claude -c\)/)).toBeInTheDocument();
    await userEvent.click(fresh);
    expect(screen.getByText(/new conversation even if/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Launch' }));
    await waitFor(() => expect(onLaunched).toHaveBeenCalledTimes(1));
    expect(launches(transport)[0]).toMatchObject({ fresh: true });
  });

  it('uses the opened folder itself without picking a subfolder', async () => {
    const transport = pickerServer();
    renderDialog(transport);
    await userEvent.click(await screen.findByRole('tab', { name: 'Browse' }));
    const roots = screen.getByRole('list', { name: 'Locations' });
    await userEvent.click(within(roots).getByRole('button', { name: 'Open C:\\Users\\dev' }));
    const home = await screen.findByRole('list', { name: 'Folders in dev' });
    await userEvent.click(within(home).getByRole('button', { name: 'Open code' }));
    await screen.findByRole('list', { name: 'Folders in code' });
    expect(screen.getByRole('status', { name: '' })).toHaveTextContent(
      'This folder will be used: C:\\Users\\dev\\code',
    );
    await userEvent.click(screen.getByRole('button', { name: 'Launch' }));
    await waitFor(() => expect(launches(transport)).toHaveLength(1));
    expect(launches(transport)[0]).toMatchObject({
      project_dir: 'C:\\Users\\dev\\code',
      create_dir: false,
    });
  });

  it('browses from the roots with breadcrumbs and selects a folder', async () => {
    const transport = pickerServer();
    renderDialog(transport);
    await userEvent.click(await screen.findByRole('tab', { name: 'Browse' }));
    const roots = screen.getByRole('list', { name: 'Locations' });
    expect(
      within(roots)
        .getAllByRole('button')
        .map((b) => b.textContent),
    ).toEqual(['C:\\Users\\dev', 'D:\\', 'E:\\']);
    await userEvent.click(within(roots).getByRole('button', { name: 'Open C:\\Users\\dev' }));
    // Hidden and noise folders stay out; the server's paths are used as is.
    const home = await screen.findByRole('list', { name: 'Folders in dev' });
    expect(within(home).queryByText('.vscode')).toBeNull();
    await userEvent.click(within(home).getByRole('button', { name: 'Open code' }));
    const code = await screen.findByRole('list', { name: 'Folders in code' });
    expect(listedPaths(transport)).toEqual(['C:\\Users\\dev', 'C:\\Users\\dev\\code']);

    const crumbs = screen.getByRole('navigation', { name: 'Folder path' });
    expect(
      within(crumbs)
        .getAllByRole('button')
        .map((b) => b.textContent),
    ).toEqual(['Locations', 'C:\\…\\dev', 'code']);
    expect(within(crumbs).getByRole('button', { name: 'code' })).toHaveAttribute(
      'aria-current',
      'location',
    );

    await userEvent.click(within(code).getByRole('button', { name: 'Select infra' }));
    await userEvent.click(screen.getByRole('button', { name: 'Launch' }));
    await waitFor(() => expect(launches(transport)).toHaveLength(1));
    expect(launches(transport)[0]).toMatchObject({
      project_dir: 'C:\\Users\\dev\\code\\infra',
      create_dir: false,
    });

    // Back up through a breadcrumb, then to the locations.
    await userEvent.click(within(crumbs).getByRole('button', { name: 'C:\\…\\dev' }));
    await screen.findByRole('list', { name: 'Folders in dev' });
    await userEvent.click(
      within(screen.getByRole('navigation', { name: 'Folder path' })).getByRole('button', {
        name: 'Locations',
      }),
    );
    expect(screen.getByRole('list', { name: 'Locations' })).toBeInTheDocument();
  });

  it('selects the browsed folder itself and reports truncation', async () => {
    const transport = pickerServer();
    renderDialog(transport);
    await userEvent.click(await screen.findByRole('tab', { name: 'Browse' }));
    await userEvent.click(screen.getByRole('button', { name: 'Open C:\\Users\\dev' }));
    await userEvent.click(
      within(await screen.findByRole('list', { name: 'Folders in dev' })).getByRole('button', {
        name: 'Open Pictures',
      }),
    );
    const pictures = await screen.findByRole('list', { name: 'Folders in Pictures' });
    expect(within(pictures).getAllByRole('listitem')).toHaveLength(500);
    expect(screen.getByText(/Only the first 500 folders are shown/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Use this folder (Pictures)' }));
    expect(screen.getByText('Selected: C:\\Users\\dev\\Pictures')).toBeInTheDocument();
  });

  it('filters the current folder', async () => {
    const transport = pickerServer();
    renderDialog(transport);
    await userEvent.click(await screen.findByRole('tab', { name: 'Browse' }));
    await userEvent.click(screen.getByRole('button', { name: 'Open C:\\Users\\dev' }));
    await userEvent.click(
      within(await screen.findByRole('list', { name: 'Folders in dev' })).getByRole('button', {
        name: 'Open code',
      }),
    );
    await screen.findByRole('list', { name: 'Folders in code' });
    await userEvent.type(screen.getByRole('searchbox', { name: 'Filter folders' }), 'DASH');
    const list = screen.getByRole('list', { name: 'Folders in code' });
    expect(within(list).getAllByRole('listitem')).toHaveLength(1);
    expect(within(list).getByText('web-dashboard')).toBeInTheDocument();
    await userEvent.clear(screen.getByRole('searchbox', { name: 'Filter folders' }));
    await userEvent.type(screen.getByRole('searchbox', { name: 'Filter folders' }), 'zzz');
    expect(screen.getByText('No folder matches "zzz".')).toBeInTheDocument();
  });

  it('moves between folders with the arrow keys', async () => {
    const transport = pickerServer();
    renderDialog(transport);
    const recent = await screen.findByRole('list', { name: 'Recent project folders' });
    const rows = within(recent).getAllByRole('button');
    rows[0]!.focus();
    await userEvent.keyboard('{ArrowDown}');
    expect(rows[1]).toHaveFocus();
    await userEvent.keyboard('{End}');
    expect(rows[rows.length - 1]).toHaveFocus();
    await userEvent.keyboard('{Home}');
    expect(rows[0]).toHaveFocus();
    await userEvent.keyboard('{Enter}');
    expect(rows[0]).toHaveAttribute('aria-pressed', 'true');
  });

  it('creates a new folder inside the browsed one, path built by the server', async () => {
    const transport = pickerServer();
    renderDialog(transport);
    await userEvent.click(await screen.findByRole('tab', { name: 'Browse' }));
    await userEvent.click(screen.getByRole('button', { name: 'Open D:\\' }));
    await userEvent.click(
      within(await screen.findByRole('list', { name: 'Folders in D:\\' })).getByRole('button', {
        name: 'Open work',
      }),
    );
    await screen.findByRole('list', { name: 'Folders in work' });
    await userEvent.click(screen.getByRole('tab', { name: 'New folder' }));
    expect(screen.getByText('Create inside D:\\work')).toBeInTheDocument();
    const name = screen.getByLabelText('New folder name');
    await userEvent.type(name, 'bad:name');
    expect(await screen.findByText(/That name cannot be used/)).toBeInTheDocument();
    expect(name).toHaveAttribute('aria-invalid', 'true');
    expect(screen.getByRole('button', { name: 'Launch' })).toBeDisabled();
    await userEvent.clear(name);
    await userEvent.type(name, 'new-app');
    expect(await screen.findByText('Will create D:\\work\\new-app')).toBeInTheDocument();
    expect(transport.bodies('folder_path').at(-1)).toEqual({
      type: 'folder_path',
      parent: 'D:\\work',
      name: 'new-app',
    });
    await userEvent.click(screen.getByRole('button', { name: 'Launch' }));
    await waitFor(() => expect(launches(transport)).toHaveLength(1));
    expect(launches(transport)[0]).toMatchObject({
      project_dir: 'D:\\work\\new-app',
      create_dir: true,
    });
  });

  it('remembers the last browsed folder per machine', async () => {
    const first = pickerServer();
    const view = renderWithApp(
      <NewSessionDialog projectDirs={[]} onClose={() => {}} onLaunched={() => {}} />,
      { transport: first },
    );
    await userEvent.click(await screen.findByRole('tab', { name: 'Browse' }));
    await userEvent.click(screen.getByRole('button', { name: 'Open E:\\' }));
    await screen.findByRole('list', { name: 'Folders in E:\\' });
    expect(localStorage.getItem('vca.folderPicker.last.m')).toBe('E:\\');
    view.unmount();

    const second = pickerServer();
    renderDialog(second);
    await userEvent.click(await screen.findByRole('tab', { name: 'Browse' }));
    expect(await screen.findByRole('list', { name: 'Folders in E:\\' })).toBeInTheDocument();
    expect(listedPaths(second)[0]).toBe('E:\\');
  });

  it('shows POSIX paths of a Linux server', async () => {
    const transport = pickerServer('build_server');
    renderDialog(transport);
    const recent = await screen.findByRole('list', { name: 'Recent project folders' });
    expect(within(recent).getAllByRole('button')[0]).toHaveTextContent('/srv/work/monorepo');
    await userEvent.click(screen.getByRole('tab', { name: 'Browse' }));
    await userEvent.click(screen.getByRole('button', { name: 'Open /srv/work' }));
    const work = await screen.findByRole('list', { name: 'Folders in work' });
    await userEvent.click(within(work).getByRole('button', { name: 'Open monorepo' }));
    await screen.findByRole('list', { name: 'Folders in monorepo' });
    expect(listedPaths(transport).at(-1)).toBe('/srv/work/monorepo');
    // `target` and dot folders are hidden by default.
    expect(screen.queryByText('target')).toBeNull();
  });

  it('falls back to a typed path on older servers', async () => {
    const transport = new FakeTransport((msg) =>
      msg.type === 'launch' ? { type: 'launched', session: localSession() } : defaultResponse(msg),
    );
    renderDialog(transport);
    const path = await screen.findByLabelText('New folder path');
    expect(transport.bodies('project_folders')).toEqual([]);
    expect(transport.bodies('list_folder')).toEqual([]);
    await userEvent.type(path, '/home/dev/new-thing');
    await userEvent.click(screen.getByRole('button', { name: 'Launch' }));
    await waitFor(() => expect(launches(transport)).toHaveLength(1));
    expect(launches(transport)[0]).toMatchObject({
      project_dir: '/home/dev/new-thing',
      create_dir: true,
    });
  });

  it('is a full-screen sheet on phones', async () => {
    renderDialog(pickerServer());
    const dialog = await screen.findByRole('dialog', { name: 'New session' });
    expect(dialog.className).toContain('full');
  });
});
