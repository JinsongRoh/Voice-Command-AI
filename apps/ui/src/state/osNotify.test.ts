import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const plugin = vi.hoisted(() => ({
  native: true,
  granted: false,
  answer: 'granted' as 'granted' | 'denied' | 'default',
  isPermissionGranted: vi.fn(),
  requestPermission: vi.fn(),
  sendNotification: vi.fn(),
  onAction: vi.fn(),
  action: null as null | ((p: unknown) => void),
}));

vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => plugin.native }));
vi.mock('@tauri-apps/plugin-notification', () => ({
  isPermissionGranted: plugin.isPermissionGranted,
  requestPermission: plugin.requestPermission,
  sendNotification: plugin.sendNotification,
  onAction: plugin.onAction,
}));

async function load() {
  vi.resetModules();
  return import('./osNotify');
}

beforeEach(() => {
  localStorage.clear();
  plugin.native = true;
  plugin.granted = false;
  plugin.answer = 'granted';
  plugin.action = null;
  plugin.isPermissionGranted.mockReset().mockImplementation(async () => plugin.granted);
  plugin.requestPermission.mockReset().mockImplementation(async () => {
    plugin.granted = plugin.answer === 'granted';
    return plugin.answer;
  });
  plugin.sendNotification.mockReset();
  plugin.onAction.mockReset().mockImplementation(async (cb: (p: unknown) => void) => {
    plugin.action = cb;
    return { unregister: vi.fn() };
  });
  vi.spyOn(document, 'hasFocus').mockReturnValue(false);
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const flush = () => new Promise((r) => setTimeout(r, 0));

describe('native (plugin)', () => {
  it('asks for permission once', async () => {
    const os = await load();
    expect(await os.requestOsNotifications()).toBe('granted');
    plugin.granted = false;
    plugin.answer = 'denied';
    await os.requestOsNotifications();
    expect(plugin.requestPermission).toHaveBeenCalledTimes(1);
  });

  it('does not ask again after an earlier ask was dismissed', async () => {
    localStorage.setItem('vca.osNotifyAsked', '1');
    const os = await load();
    expect(await os.requestOsNotifications()).toBe('default');
    expect(plugin.requestPermission).not.toHaveBeenCalled();
  });

  it('sends only the title and generic body, with the tag in extra', async () => {
    plugin.granted = true;
    const os = await load();
    os.showOsNotification('Needs approval: api', 'Waiting for your decision', 'k1', vi.fn());
    await flush();
    expect(plugin.sendNotification).toHaveBeenCalledWith({
      title: 'Needs approval: api',
      body: 'Waiting for your decision',
      extra: { tag: 'k1' },
    });
  });

  it('stays silent without permission or while the app is in front', async () => {
    const os = await load();
    os.showOsNotification('t', 'b', 'k', vi.fn());
    await flush();
    expect(plugin.sendNotification).not.toHaveBeenCalled();
    plugin.granted = true;
    vi.spyOn(document, 'hasFocus').mockReturnValue(true);
    os.showOsNotification('t', 'b', 'k', vi.fn());
    await flush();
    expect(plugin.sendNotification).not.toHaveBeenCalled();
  });

  it('runs the click handler when the phone reports a tap', async () => {
    plugin.granted = true;
    const os = await load();
    const onClick = vi.fn();
    os.showOsNotification('t', 'b', 'k9', onClick);
    await flush();
    plugin.action?.({ actionId: 'tap', notification: { extra: { tag: 'k9' } } });
    expect(onClick).toHaveBeenCalledTimes(1);
    plugin.action?.({ actionId: 'tap', notification: { extra: { tag: 'k9' } } });
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it('survives a plugin without action events (desktop)', async () => {
    plugin.granted = true;
    plugin.onAction.mockRejectedValue(new Error('no listener'));
    const os = await load();
    os.showOsNotification('t', 'b', 'k', vi.fn());
    await flush();
    expect(plugin.sendNotification).toHaveBeenCalled();
  });

  it('falls back to silence when the plugin throws', async () => {
    plugin.isPermissionGranted.mockRejectedValue(new Error('boom'));
    const os = await load();
    os.showOsNotification('t', 'b', 'k', vi.fn());
    await flush();
    expect(plugin.sendNotification).not.toHaveBeenCalled();
  });
});

describe('browser / mock build', () => {
  beforeEach(() => {
    plugin.native = false;
  });

  it('is unavailable and silent without the web API', async () => {
    vi.stubGlobal('Notification', undefined);
    const os = await load();
    expect(os.osNotificationState()).toBe('unavailable');
    expect(await os.requestOsNotifications()).toBe('unavailable');
    os.showOsNotification('t', 'b', 'k', vi.fn());
    expect(plugin.sendNotification).not.toHaveBeenCalled();
  });

  it('uses the web API and jumps on click', async () => {
    const created: { title: string; opts: unknown; onclick?: () => void; close: () => void }[] = [];
    class FakeNotification {
      static permission = 'granted';
      onclick?: () => void;
      close = vi.fn();
      constructor(
        public title: string,
        public opts: unknown,
      ) {
        created.push(this);
      }
    }
    vi.stubGlobal('Notification', FakeNotification);
    const os = await load();
    const onClick = vi.fn();
    os.showOsNotification('t', 'generic', 'k', onClick);
    expect(created).toHaveLength(1);
    expect(created[0]?.opts).toEqual({ body: 'generic', tag: 'k' });
    created[0]?.onclick?.();
    expect(onClick).toHaveBeenCalled();
    expect(plugin.sendNotification).not.toHaveBeenCalled();
  });
});
