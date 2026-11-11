import {
  cancel,
  checkPermissions,
  Format,
  openAppSettings,
  requestPermissions,
  scan,
} from '@tauri-apps/plugin-barcode-scanner';

import { classifyLink } from './pairLinks';

/** Outcome of scanning the pairing QR code with the camera. */
export type QrScanResult =
  | { kind: 'link'; uri: string }
  /** A QR code was read, but it is neither a `vca://pair` link nor a DNSPilot invite. */
  | { kind: 'invalid' }
  | { kind: 'cancelled' }
  /** Camera permission was refused (now or earlier, in the system settings). */
  | { kind: 'denied' }
  | { kind: 'error'; message: string };

/** Camera QR scanning, provided by the mobile app only. */
export interface QrScanner {
  scan(): Promise<QrScanResult>;
  /** Stops a running scan; its `scan()` resolves as `cancelled`. */
  cancel(): Promise<void>;
  /** Opens the app's system settings (to allow the camera after a refusal). */
  openSettings(): Promise<void>;
}

/** Platforms (`vca_platform`) whose app bundles the barcode scanner plugin. */
export function canScanQr(platform: string | null): boolean {
  return platform === 'ios' || platform === 'android';
}

function errorText(err: unknown): string {
  if (typeof err === 'string') return err;
  if (err instanceof Error) return err.message;
  if (typeof err === 'object' && err !== null && 'message' in err) {
    return String((err as { message: unknown }).message);
  }
  return String(err);
}

/**
 * Asks for the camera if needed, then scans one QR code in windowed mode: the
 * camera is drawn behind the (transparent) webview, so the UI keeps its own
 * overlay with a Cancel button. The plugin's full-screen mode has no way out
 * on iOS.
 *
 * The scanned link is returned, never used here: the pairing screen decides
 * what it does.
 */
async function scanPairingQr(): Promise<QrScanResult> {
  try {
    let state = await checkPermissions();
    if (state !== 'granted') state = await requestPermissions();
    if (state !== 'granted') return { kind: 'denied' };
    const { content } = await scan({ windowed: true, formats: [Format.QRCode] });
    const uri = content.trim();
    return classifyLink(uri) ? { kind: 'link', uri } : { kind: 'invalid' };
  } catch (err) {
    const message = errorText(err);
    // Rejections of the iOS and Android plugins.
    if (/cancel/i.test(message)) return { kind: 'cancelled' };
    if (/permission|denied/i.test(message)) return { kind: 'denied' };
    return { kind: 'error', message };
  }
}

export const qrScanner: QrScanner = {
  scan: scanPairingQr,
  cancel: async () => {
    try {
      await cancel();
    } catch {
      // Nothing to cancel.
    }
  },
  openSettings: async () => {
    try {
      await openAppSettings();
    } catch {
      // Settings could not be opened; the message already explains the way.
    }
  },
};
