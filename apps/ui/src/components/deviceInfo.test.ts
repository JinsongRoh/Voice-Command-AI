import { describe, expect, it } from 'vitest';

import type { PairedDevice } from '../protocol/types';
import { endpointHost, endpointKind, platformIcon, platformKind, sortDevices } from './deviceInfo';

describe('deviceInfo', () => {
  it('groups platform strings', () => {
    expect(platformKind('ios')).toBe('ios');
    expect(platformKind('iPadOS 19')).toBe('ios');
    expect(platformKind('Android 16')).toBe('android');
    expect(platformKind('windows')).toBe('windows');
    expect(platformKind('macOS 16')).toBe('macos');
    expect(platformKind('linux')).toBe('linux');
    expect(platformKind('test')).toBe('desktop');
    expect(platformIcon('android')).toBe('phone');
    expect(platformIcon('macos')).toBe('laptop');
    expect(platformIcon('desktop')).toBe('monitor');
  });

  it('sorts online devices first, then by last seen', () => {
    const d = (id: string, online: boolean, last_seen: number): PairedDevice => ({
      id,
      name: id,
      platform: 'ios',
      paired_at: 0,
      last_seen,
      current: false,
      online,
    });
    const sorted = sortDevices([d('old', false, 1), d('on', true, 0), d('recent', false, 5)]);
    expect(sorted.map((x) => x.id)).toEqual(['on', 'recent', 'old']);
  });

  it('classifies endpoints', () => {
    expect(endpointKind('wss://desk.tail1234.ts.net/v1/ws')).toBe('tailscale');
    expect(endpointKind('ws://100.101.7.12:7300/v1/ws')).toBe('tailscale');
    expect(endpointKind('wss://abc.dnspilot.kr/v1/ws')).toBe('dnspilot');
    expect(endpointKind('ws://192.168.0.10:7300/v1/ws')).toBe('lan');
    expect(endpointKind('ws://10.0.0.2:7300/v1/ws')).toBe('lan');
    expect(endpointKind('ws://172.20.1.1:7300/v1/ws')).toBe('lan');
    expect(endpointKind('ws://desk.local:7300/v1/ws')).toBe('lan');
    expect(endpointKind('ws://[fd12::1]:7300/v1/ws')).toBe('lan');
    expect(endpointKind('wss://example.com/v1/ws')).toBe('internet');
    expect(endpointKind('ws://8.8.8.8/v1/ws')).toBe('internet');
    expect(endpointKind('not a url')).toBe('internet');
    expect(endpointKind(undefined)).toBe('internet');
    expect(endpointHost('ws://192.168.0.10:7300/v1/ws')).toBe('192.168.0.10:7300');
    expect(endpointHost(undefined)).toBe('-');
  });
});
