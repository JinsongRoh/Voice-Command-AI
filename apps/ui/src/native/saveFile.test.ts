import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { translate, type MessageKey } from '../i18n/i18n';
import { tauri } from '../test/tauriMock';
import { NativeError } from './bridge';
import { describeSaveResult, resetSaveTarget, saveTarget, saveTextFile } from './saveFile';

vi.mock('@tauri-apps/api/core', () => import('../test/tauriMock'));

const t = (key: MessageKey, vars?: Record<string, string | number>) => translate('en', key, vars);

type Nav = Navigator & { share?: unknown; canShare?: unknown };

function stubShare(share: (data: ShareData) => Promise<void>, canShareFiles: boolean) {
  const nav = navigator as Nav;
  const spy = vi.fn(share);
  Object.defineProperty(nav, 'share', { value: spy, configurable: true, writable: true });
  Object.defineProperty(nav, 'canShare', {
    value: vi.fn(() => canShareFiles),
    configurable: true,
    writable: true,
  });
  return spy;
}

describe('saveTextFile', () => {
  beforeEach(() => {
    tauri.reset();
    resetSaveTarget();
  });

  afterEach(() => {
    const nav = navigator as Nav;
    Reflect.deleteProperty(nav, 'share');
    Reflect.deleteProperty(nav, 'canShare');
    vi.restoreAllMocks();
  });

  describe('desktop shell', () => {
    beforeEach(() => {
      tauri.handlers.set('vca_platform', () => 'windows');
    });

    it('asks the shell to show the Save as dialog and reports the file name', async () => {
      tauri.handlers.set('vca_save_text_file', () => ({ status: 'saved', fileName: 'codes.txt' }));
      expect(await saveTarget()).toBe('desktop');
      const res = await saveTextFile('vca-backup-codes.txt', 'AAAA\n');
      expect(res).toEqual({ status: 'saved', fileName: 'codes.txt' });
      expect(tauri.callsOf('vca_save_text_file')[0]?.args).toEqual({
        suggestedName: 'vca-backup-codes.txt',
        text: 'AAAA\n',
      });
      expect(describeSaveResult(t, res)).toBe('Saved to codes.txt.');
    });

    it('reports a closed dialog as cancelled', async () => {
      tauri.handlers.set('vca_save_text_file', () => ({ status: 'cancelled' }));
      const res = await saveTextFile('a.txt', 'x');
      expect(res).toEqual({ status: 'cancelled' });
      expect(describeSaveResult(t, res)).toBe('Cancelled.');
    });

    it('reports a write error', async () => {
      tauri.handlers.set('vca_save_text_file', () => {
        throw { code: 'save_failed', message: 'could not write the file: denied' };
      });
      const res = await saveTextFile('a.txt', 'x');
      expect(res.status).toBe('error');
      expect(describeSaveResult(t, res)).toBe(
        'Could not save the file: could not write the file: denied',
      );
    });

    it('never makes a blob download link', async () => {
      tauri.handlers.set('vca_save_text_file', () => ({ status: 'saved', fileName: 'a.txt' }));
      const create = vi.spyOn(document, 'createElement');
      await saveTextFile('a.txt', 'x');
      expect(create).not.toHaveBeenCalledWith('a');
    });
  });

  describe('phones', () => {
    beforeEach(() => {
      tauri.handlers.set('vca_platform', () => 'ios');
    });

    it('shares a file when the webview can', async () => {
      const share = stubShare(async () => {}, true);
      expect(await saveTarget()).toBe('phone');
      const res = await saveTextFile('vca-backup-codes.txt', 'AAAA\n');
      expect(res).toEqual({ status: 'shared', fileName: 'vca-backup-codes.txt' });
      const data = share.mock.calls[0]?.[0];
      expect(data?.files?.[0]?.name).toBe('vca-backup-codes.txt');
      expect(await data?.files?.[0]?.text()).toBe('AAAA\n');
      expect(data?.text).toBeUndefined();
      expect(tauri.callsOf('vca_save_text_file')).toHaveLength(0);
      expect(describeSaveResult(t, res)).toBe('Shared vca-backup-codes.txt.');
    });

    it('shares the text when files cannot be shared', async () => {
      const share = stubShare(async () => {}, false);
      await saveTextFile('vca-tasks.md', '# tasks');
      expect(share.mock.calls[0]?.[0]).toEqual({ title: 'vca-tasks.md', text: '# tasks' });
    });

    it('treats a dismissed share sheet as cancelled', async () => {
      stubShare(async () => {
        throw new DOMException('dismissed', 'AbortError');
      }, true);
      expect(await saveTextFile('a.txt', 'x')).toEqual({ status: 'cancelled' });
    });

    it('says so when the webview cannot share at all', async () => {
      const res = await saveTextFile('a.txt', 'x');
      expect(res).toEqual({ status: 'error', error: expect.any(NativeError) });
      expect(describeSaveResult(t, res)).toBe(
        "This device can't save files here. Copy the text instead.",
      );
    });
  });

  describe('plain browser', () => {
    beforeEach(() => {
      tauri.native = false;
    });

    it('keeps the blob download', async () => {
      vi.useFakeTimers();
      const created = vi.fn(() => 'blob:codes');
      const urls = { create: URL.createObjectURL, revoke: URL.revokeObjectURL };
      URL.createObjectURL = created;
      URL.revokeObjectURL = vi.fn();
      const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {});
      try {
        expect(await saveTarget()).toBe('browser');
        const res = await saveTextFile('vca-tasks.csv', 'a,b');
        expect(res).toEqual({ status: 'saved', fileName: 'vca-tasks.csv' });
        expect(created).toHaveBeenCalledTimes(1);
        const anchor = click.mock.instances[0] as unknown as HTMLAnchorElement;
        expect(anchor.download).toBe('vca-tasks.csv');
        expect(anchor.href).toBe('blob:codes');
        expect(tauri.calls).toHaveLength(0);
        vi.runAllTimers();
        expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:codes');
      } finally {
        vi.useRealTimers();
        URL.createObjectURL = urls.create;
        URL.revokeObjectURL = urls.revoke;
      }
    });
  });
});
