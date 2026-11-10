import { describe, expect, it } from 'vitest';

import {
  formatCountdown,
  isDesktopPlatform,
  isLocalMachine,
  isLoopbackListen,
  secondsLeft,
  svgDataUri,
} from './host';

describe('host helpers', () => {
  it('counts whole seconds down to zero', () => {
    expect(secondsLeft(1_000, 700_000)).toBe(300);
    expect(secondsLeft(1_000, 700_400)).toBe(299);
    expect(secondsLeft(1_000, 1_000_000)).toBe(0);
    expect(secondsLeft(1_000, 2_000_000)).toBe(0);
    expect(formatCountdown(300)).toBe('5:00');
    expect(formatCountdown(61)).toBe('1:01');
    expect(formatCountdown(-3)).toBe('0:00');
  });

  it('turns SVG into a data URI', () => {
    const uri = svgDataUri('<svg a="1"/>');
    expect(uri).toBe('data:image/svg+xml;charset=utf-8,%3Csvg%20a%3D%221%22%2F%3E');
  });

  it('recognizes loopback listen addresses', () => {
    expect(isLoopbackListen('127.0.0.1:47800')).toBe(true);
    expect(isLoopbackListen('[::1]:47800')).toBe(true);
    expect(isLoopbackListen('localhost:47800')).toBe(true);
    expect(isLoopbackListen('0.0.0.0:47800')).toBe(false);
    expect(isLoopbackListen('192.168.0.10:47800')).toBe(false);
    expect(isLoopbackListen(null)).toBe(false);
  });

  it('knows desktop platforms and the local machine', () => {
    expect(isDesktopPlatform('windows')).toBe(true);
    expect(isDesktopPlatform('linux')).toBe(true);
    expect(isDesktopPlatform('ios')).toBe(false);
    expect(isLocalMachine('local')).toBe(true);
    expect(isLocalMachine('0011223344556677')).toBe(false);
  });
});
