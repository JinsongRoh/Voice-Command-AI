import { beforeEach, describe, expect, it, vi } from 'vitest';

import { canScanQr, qrScanner } from './qrScanner';

const plugin = vi.hoisted(() => ({
  checkPermissions: vi.fn(),
  requestPermissions: vi.fn(),
  scan: vi.fn(),
  cancel: vi.fn(),
  openAppSettings: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-barcode-scanner', () => ({
  Format: { QRCode: 'QR_CODE' },
  ...plugin,
}));

vi.mock('@tauri-apps/plugin-deep-link', () => ({
  getCurrent: async () => null,
  onOpenUrl: async () => () => {},
}));

const URI = 'vca://pair?v=1&pk=abc';

describe('qrScanner', () => {
  beforeEach(() => {
    for (const fn of Object.values(plugin)) fn.mockReset();
    plugin.checkPermissions.mockResolvedValue('granted');
    plugin.requestPermissions.mockResolvedValue('granted');
    plugin.scan.mockResolvedValue({ content: ` ${URI} `, format: 'QR_CODE', bounds: null });
    plugin.cancel.mockResolvedValue(undefined);
    plugin.openAppSettings.mockResolvedValue(undefined);
  });

  it('scans only QR codes in windowed mode and returns the pairing link', async () => {
    expect(await qrScanner.scan()).toEqual({ kind: 'link', uri: URI });
    expect(plugin.scan).toHaveBeenCalledWith({ windowed: true, formats: ['QR_CODE'] });
    expect(plugin.requestPermissions).not.toHaveBeenCalled();
  });

  it('accepts the DNSPilot console QR and refuses look-alikes', async () => {
    const code = `dpt_inv_${'a'.repeat(43)}`;
    const good = `https://dns.dnspilot.kr/enroll#v=1&h=vca.example.com&t=12&c=${code}`;
    plugin.scan.mockResolvedValue({ content: good, format: 'QR_CODE', bounds: null });
    expect(await qrScanner.scan()).toEqual({ kind: 'link', uri: good });
    plugin.scan.mockResolvedValue({
      content: good.replace('dns.dnspilot.kr', 'dns.dnspilot.kr.evil.example'),
      format: 'QR_CODE',
      bounds: null,
    });
    expect(await qrScanner.scan()).toEqual({ kind: 'invalid' });
  });

  it('asks for the camera first when not yet granted', async () => {
    plugin.checkPermissions.mockResolvedValue('prompt');
    expect(await qrScanner.scan()).toEqual({ kind: 'link', uri: URI });
    expect(plugin.requestPermissions).toHaveBeenCalledOnce();
  });

  it('reports a refused camera without scanning', async () => {
    plugin.checkPermissions.mockResolvedValue('prompt');
    plugin.requestPermissions.mockResolvedValue('denied');
    expect(await qrScanner.scan()).toEqual({ kind: 'denied' });
    expect(plugin.scan).not.toHaveBeenCalled();
  });

  it('maps permission rejections from the plugin to denied', async () => {
    plugin.scan.mockRejectedValue('Camera permission denied or not yet requested');
    expect(await qrScanner.scan()).toEqual({ kind: 'denied' });
  });

  it('rejects QR codes that are not pairing links', async () => {
    plugin.scan.mockResolvedValue({ content: 'https://example.com', format: 'QR_CODE' });
    expect(await qrScanner.scan()).toEqual({ kind: 'invalid' });
  });

  it('reports a cancelled scan', async () => {
    plugin.scan.mockRejectedValue('cancelled');
    expect(await qrScanner.scan()).toEqual({ kind: 'cancelled' });
  });

  it('reports other failures with their message', async () => {
    plugin.scan.mockRejectedValue(new Error('No camera available on this device'));
    expect(await qrScanner.scan()).toEqual({
      kind: 'error',
      message: 'No camera available on this device',
    });
  });

  it('cancels and opens settings without throwing', async () => {
    plugin.cancel.mockRejectedValue('nothing to cancel');
    await expect(qrScanner.cancel()).resolves.toBeUndefined();
    plugin.openAppSettings.mockRejectedValue('unsupported');
    await expect(qrScanner.openSettings()).resolves.toBeUndefined();
  });

  it('is offered on phones only', () => {
    expect(canScanQr('ios')).toBe(true);
    expect(canScanQr('android')).toBe(true);
    expect(canScanQr('windows')).toBe(false);
    expect(canScanQr('macos')).toBe(false);
    expect(canScanQr(null)).toBe(false);
  });
});
