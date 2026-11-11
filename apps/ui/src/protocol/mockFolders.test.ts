import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { MockTransport, type MockTransportOptions } from './mockTransport';
import type { ClientMsg, ServerMsg } from './types';

async function connected(options: MockTransportOptions = {}) {
  const t = new MockTransport({ latencyMs: 0, autoTick: false, ...options });
  const connecting = t.connect();
  await vi.runAllTimersAsync();
  await connecting;
  let id = 0;
  return async (body: ClientMsg): Promise<ServerMsg> => {
    const reply = t.send({ id: ++id, body });
    await vi.runAllTimersAsync();
    return reply;
  };
}

describe('mock project folders', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('serves a Windows-style tree on the workstation', async () => {
    const ask = await connected();
    const welcome = await ask({ type: 'hello', protocol_version: 1, app_version: 't' });
    expect(welcome.type === 'welcome' && welcome.features).toContain('project_folders');
    const folders = await ask({ type: 'project_folders' });
    if (folders.type !== 'project_folders') throw new Error(folders.type);
    expect(folders.os).toBe('windows');
    expect(folders.separator).toBe('\\');
    expect(folders.roots.map((r) => r.path)).toEqual(['C:\\Users\\dev', 'D:\\', 'E:\\']);
    expect(folders.recent[0]).toMatchObject({ name: 'payments-api', is_git_repo: true });

    const d = await ask({ type: 'list_folder', path: 'd:/' });
    if (d.type !== 'folder') throw new Error(d.type);
    // System folders never show; the server's own path form comes back.
    expect(d.entries.map((e) => e.name)).toEqual(['work']);
    expect(d).toMatchObject({ path: 'D:\\', name: 'D:\\', parent: null });
    const work = await ask({ type: 'list_folder', path: 'D:\\work' });
    expect(work).toMatchObject({
      parent: 'D:\\',
      breadcrumbs: [{ path: 'D:\\' }, { name: 'work' }],
    });

    expect(await ask({ type: 'list_folder', path: 'C:\\Windows' })).toMatchObject({
      code: 'forbidden',
    });
    expect(await ask({ type: 'list_folder', path: 'C:\\Users\\dev\\..\\other' })).toMatchObject({
      code: 'forbidden',
    });
    expect(await ask({ type: 'list_folder', path: 'C:\\Users\\dev\\nope' })).toMatchObject({
      code: 'not_found',
    });
    expect(
      await ask({ type: 'folder_path', parent: 'C:\\Users\\dev\\code', name: 'infra' }),
    ).toEqual({ type: 'folder_path', path: 'C:\\Users\\dev\\code\\infra', exists: true });
    expect(await ask({ type: 'folder_path', parent: 'D:\\', name: 'a?b' })).toMatchObject({
      code: 'invalid_request',
    });
  });

  it('serves a POSIX tree on the build server', async () => {
    const ask = await connected({ profile: 'build_server' });
    const folders = await ask({ type: 'project_folders' });
    if (folders.type !== 'project_folders') throw new Error(folders.type);
    expect(folders).toMatchObject({ os: 'linux', separator: '/' });
    const hidden = await ask({ type: 'list_folder', path: '/srv/work/monorepo' });
    const all = await ask({ type: 'list_folder', path: '/srv/work/monorepo', show_hidden: true });
    if (hidden.type !== 'folder' || all.type !== 'folder') throw new Error('expected folders');
    expect(hidden.entries.map((e) => e.name)).toEqual(['services', 'src']);
    expect(all.entries.map((e) => e.path)).toContain('/srv/work/monorepo/target');
    expect(await ask({ type: 'folder_path', parent: '/srv/work', name: 'new-app' })).toMatchObject({
      path: '/srv/work/new-app',
      exists: false,
    });
  });

  it('acts as an older server without the picker', async () => {
    const ask = await connected({ folderPicker: false });
    const welcome = await ask({ type: 'hello', protocol_version: 1, app_version: 't' });
    expect(welcome.type === 'welcome' && welcome.features).not.toContain('project_folders');
    expect(await ask({ type: 'project_folders' })).toMatchObject({ type: 'error' });
  });
});
