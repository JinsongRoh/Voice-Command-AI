import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  classifyLink,
  hasInvite,
  isConsoleEnrollUrl,
  isPairingUri,
  listenForPairLinks,
  PairLinks,
  type PairLinkSources,
} from './pairLinks';

vi.mock('@tauri-apps/plugin-deep-link', () => ({
  getCurrent: async () => null,
  onOpenUrl: async () => () => {},
}));

const A = 'vca://pair?v=1&k=a';
const B = 'vca://pair?v=1&k=b';

describe('PairLinks', () => {
  it('keeps only pairing links and hands each out once', () => {
    const links = new PairLinks();
    const seen: string[] = [];
    const stop = links.subscribe((u) => seen.push(u));
    links.push('https://evil.example/pair');
    expect(links.hasPending).toBe(false);
    links.push(' vca://pair?v=1&k=x ');
    expect(seen).toEqual(['vca://pair?v=1&k=x']);
    expect(links.take()).toBe('vca://pair?v=1&k=x');
    expect(links.take()).toBeNull();
    stop();
    links.push('vca://pair?v=1&k=y');
    expect(seen).toHaveLength(1);
  });

  it('recognizes pairing URIs', () => {
    expect(isPairingUri('vca://pair?v=1')).toBe(true);
    expect(isPairingUri('VCA://pair/?v=1')).toBe(true);
    expect(isPairingUri('vca://other?v=1')).toBe(false);
  });

  it('buffers a link that arrives before anyone subscribes', () => {
    const links = new PairLinks();
    links.pushLaunch(A);
    expect(links.hasPending).toBe(true);
    const seen: string[] = [];
    links.subscribe((u) => seen.push(u));
    expect(seen).toEqual([]);
    expect(links.take()).toBe(A);
  });

  it('takes a launch link only once, but a reopened link every time', () => {
    const links = new PairLinks();
    links.pushLaunch(A);
    expect(links.take()).toBe(A);
    // The launch link is re-read later: it must not refill the form.
    links.pushLaunch(A);
    links.pushLaunch(` ${A} `);
    expect(links.hasPending).toBe(false);
    // A link opened while the app runs is honored even if seen before...
    links.push(A);
    expect(links.take()).toBe(A);
    // ...and a link opened as an event is not reported again as the launch link.
    links.push(B);
    links.take();
    links.pushLaunch(B);
    expect(links.hasPending).toBe(false);
  });
});

/** Scriptable stand-in for the deep-link plugin and `vca_launch_links`. */
function fakeSources() {
  const calls: string[] = [];
  const state = {
    current: null as string[] | null,
    captured: [] as string[],
    open: null as ((urls: string[]) => void) | null,
    unlistened: false,
  };
  const sources: PairLinkSources = {
    getCurrent: async () => {
      calls.push('getCurrent');
      return state.current;
    },
    onOpenUrl: async (handler) => {
      calls.push('onOpenUrl');
      state.open = handler;
      return () => {
        state.unlistened = true;
      };
    },
    launchLinks: async () => {
      calls.push('launchLinks');
      // Handed out once, like the native command.
      const out = state.captured;
      state.captured = [];
      return out;
    },
  };
  return { sources, state, calls };
}

describe('listenForPairLinks', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('registers the event listener before reading the launch link', async () => {
    const { sources, calls } = fakeSources();
    const stop = await listenForPairLinks(new PairLinks(), { sources, recheckMs: [] });
    expect(calls[0]).toBe('onOpenUrl');
    expect(calls).toContain('getCurrent');
    stop();
  });

  it('picks up the launch link from the plugin', async () => {
    const { sources, state } = fakeSources();
    state.current = ['https://example.com/ignored', A];
    const links = new PairLinks();
    const stop = await listenForPairLinks(links, { sources, recheckMs: [] });
    expect(links.take()).toBe(A);
    stop();
  });

  it('picks up a launch link captured natively (iOS cold start)', async () => {
    const { sources, state } = fakeSources();
    state.captured = [A];
    const links = new PairLinks();
    const stop = await listenForPairLinks(links, { sources, recheckMs: [] });
    expect(links.take()).toBe(A);
    stop();
  });

  it('reads the launch link again after a delay', async () => {
    const { sources, state } = fakeSources();
    const links = new PairLinks();
    const stop = await listenForPairLinks(links, { sources, recheckMs: [500] });
    expect(links.hasPending).toBe(false);
    // The plugin learns the URL only after the webview has loaded.
    state.current = [A];
    await vi.advanceTimersByTimeAsync(500);
    expect(links.take()).toBe(A);
    // Later checks report the same launch link: not filled again.
    document.dispatchEvent(new Event('visibilitychange'));
    await vi.advanceTimersByTimeAsync(0);
    expect(links.hasPending).toBe(false);
    stop();
  });

  it('reads the launch link again when the app returns to the foreground', async () => {
    const { sources, state } = fakeSources();
    const links = new PairLinks();
    const stop = await listenForPairLinks(links, { sources, recheckMs: [] });
    state.captured = [B];
    window.dispatchEvent(new Event('focus'));
    await vi.advanceTimersByTimeAsync(0);
    expect(links.take()).toBe(B);
    stop();
  });

  it('forwards links opened while running and ignores nulls', async () => {
    const { sources, state } = fakeSources();
    const links = new PairLinks();
    const seen: string[] = [];
    links.subscribe((u) => seen.push(u));
    const stop = await listenForPairLinks(links, { sources, recheckMs: [] });
    state.open!([null as unknown as string, A]);
    expect(seen).toEqual([A]);
    stop();
    expect(state.unlistened).toBe(true);
  });

  it('stops rechecking once unsubscribed', async () => {
    const { sources, state, calls } = fakeSources();
    const stop = await listenForPairLinks(new PairLinks(), { sources, recheckMs: [500] });
    stop();
    const before = calls.length;
    state.current = [A];
    await vi.advanceTimersByTimeAsync(1000);
    window.dispatchEvent(new Event('focus'));
    expect(calls).toHaveLength(before);
  });

  it('survives a platform without deep-link support', async () => {
    const links = new PairLinks();
    const stop = await listenForPairLinks(links, {
      sources: {
        getCurrent: () => Promise.reject(new Error('unsupported')),
        onOpenUrl: () => Promise.reject(new Error('unsupported')),
        launchLinks: () => Promise.reject(new Error('unknown command')),
      },
      recheckMs: [],
    });
    expect(links.hasPending).toBe(false);
    stop();
  });
});

describe('invite links', () => {
  const code = `dpt_inv_${'aB3_-'.repeat(8)}xyz`;
  const good = `https://dns.dnspilot.kr/enroll#v=1&h=vca.example.com&t=12&c=${code}`;
  const pair = 'vca://pair?v=1&k=abc&id=00&s=c2Vj&x=1&e=wss%3A%2F%2Fa.example.com';

  it('accepts the console link exactly', () => {
    expect(isConsoleEnrollUrl(good)).toBe(true);
    expect(isConsoleEnrollUrl(` ${good} `)).toBe(true);
    expect(isConsoleEnrollUrl(good.replace('vca.example.com', 'vca%2Eexample%2Ecom'))).toBe(true);
    expect(classifyLink(good)).toBe('enroll');
  });

  it.each([
    ['http', good.replace('https://', 'http://')],
    ['another host', good.replace('dns.dnspilot.kr', 'evil.example')],
    ['a suffix host', good.replace('dns.dnspilot.kr', 'dns.dnspilot.kr.evil.example')],
    ['a user name', good.replace('dns.dnspilot.kr', 'dns.dnspilot.kr@evil.example')],
    ['a port', good.replace('dns.dnspilot.kr', 'dns.dnspilot.kr:8443')],
    ['upper case', good.replace('dns.dnspilot.kr', 'DNS.DNSPILOT.KR')],
    ['another path', good.replace('/enroll', '/other')],
    ['a query', good.replace('/enroll#', '/enroll?x=1#')],
    ['no fragment', good.replace('#', '?')],
    ['version 2', good.replace('v=1', 'v=2')],
    ['no version', good.replace('v=1&', '')],
    ['a bad tunnel id', good.replace('t=12', 't=abc')],
    ['tunnel id zero', good.replace('t=12', 't=0')],
    ['no host name', good.replace('h=vca.example.com&', '')],
    ['a code prefix', good.replace('dpt_inv_', 'dpt_app_')],
    ['a short code', good.replace(code, 'dpt_inv_short')],
    ['a repeated code', `${good}&c=${code}`],
  ])('rejects %s', (_name, link) => {
    expect(isConsoleEnrollUrl(link)).toBe(false);
    expect(classifyLink(link)).toBeNull();
  });

  it('tells pairing links with an invite from plain ones', () => {
    expect(classifyLink(pair)).toBe('pair');
    expect(hasInvite(pair)).toBe(false);
    const withInvite = `${pair}&h=vca.example.com&t=12&c=${code}`;
    expect(classifyLink(withInvite)).toBe('pair_invite');
    expect(isPairingUri(withInvite)).toBe(true);
    // A partial invite goes to Rust, which refuses it.
    expect(classifyLink(`${pair}&c=x`)).toBe('pair_invite');
    expect(classifyLink('https://example.com/?c=x')).toBeNull();
  });

  it('buffers console links but not other https links', () => {
    const links = new PairLinks();
    links.push('https://example.com/');
    expect(links.hasPending).toBe(false);
    links.push(good);
    expect(links.take()).toBe(good);
  });
});
