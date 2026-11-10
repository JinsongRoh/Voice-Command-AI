/**
 * Pure helpers for the device and machine management panels: platform
 * grouping, device sorting and endpoint classification.
 */
import type { DeviceScope, PairedDevice } from '../protocol/types';
import type { IconName } from './Icon';

export const DEVICE_SCOPES: DeviceScope[] = ['view', 'command', 'admin'];

/**
 * An open device list refreshes this often. The server has no device push;
 * polling keeps presence fresh without a protocol change older apps would
 * not understand.
 */
export const DEVICES_REFRESH_MS = 10_000;

export type PlatformKind = 'ios' | 'android' | 'windows' | 'macos' | 'linux' | 'desktop';

/** Groups a reported platform string (`ios`, `Android 16`, `macOS 16`, ...). */
export function platformKind(platform: string): PlatformKind {
  const p = platform.toLowerCase();
  if (/\b(ios|ipados|iphone|ipad)/.test(p)) return 'ios';
  if (p.includes('android')) return 'android';
  if (p.includes('windows')) return 'windows';
  if (p.includes('mac') || p.includes('darwin')) return 'macos';
  if (p.includes('linux')) return 'linux';
  return 'desktop';
}

export function platformIcon(kind: PlatformKind): IconName {
  switch (kind) {
    case 'ios':
    case 'android':
      return 'phone';
    case 'macos':
      return 'laptop';
    default:
      return 'monitor';
  }
}

/** Online devices first, then the most recently seen. */
export function sortDevices(devices: PairedDevice[]): PairedDevice[] {
  return [...devices].sort(
    (a, b) => Number(!!b.online) - Number(!!a.online) || b.last_seen - a.last_seen,
  );
}

export type EndpointKind = 'lan' | 'tailscale' | 'dnspilot' | 'internet';

function ipv4(host: string): number[] | null {
  const parts = host.split('.');
  if (parts.length !== 4) return null;
  const nums = parts.map((p) => (/^\d{1,3}$/.test(p) ? Number(p) : NaN));
  return nums.every((n) => n >= 0 && n <= 255) ? nums : null;
}

/** How an endpoint URL reaches the machine (display only). */
export function endpointKind(url: string | undefined): EndpointKind {
  if (!url) return 'internet';
  let host: string;
  try {
    host = new URL(url).hostname.toLowerCase().replace(/^\[|\]$/g, '');
  } catch {
    return 'internet';
  }
  if (host.endsWith('.ts.net')) return 'tailscale';
  if (host.includes('dnspilot')) return 'dnspilot';
  const v4 = ipv4(host);
  if (v4) {
    const [a, b] = v4 as [number, number, number, number];
    if (a === 100 && b >= 64 && b <= 127) return 'tailscale';
    if (a === 10 || a === 127 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168))
      return 'lan';
    if (a === 169 && b === 254) return 'lan';
    return 'internet';
  }
  if (host.startsWith('fd7a:115c:a1e0:')) return 'tailscale';
  if (host === 'localhost' || host.endsWith('.local') || host.endsWith('.lan')) return 'lan';
  if (host === '::1' || host.startsWith('fe80:') || /^f[cd]/.test(host)) return 'lan';
  return 'internet';
}

/** Host (and port) of an endpoint URL, for display. */
export function endpointHost(url: string | undefined): string {
  if (!url) return '-';
  try {
    return new URL(url).host || url;
  } catch {
    return url;
  }
}
