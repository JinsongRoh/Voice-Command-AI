import { afterEach, describe, expect, it, vi } from 'vitest';

import { lockSession } from '../state/lockSession';
import {
  clearDiag,
  DIAG_CAPACITY,
  DIAG_MAX_AGE_MS,
  DIAG_RATE_PER_MIN,
  diag,
  diagEntries,
  diagRecords,
  diagReport,
  errorClass,
  errorInfo,
  flushDiag,
  formatDiag,
  redactText,
  setDiagEnabled,
  setDiagVerbose,
  subscribeDiag,
} from './log';

afterEach(() => {
  vi.useRealTimers();
  setDiagEnabled(true);
  setDiagVerbose(true);
  clearDiag();
});

describe('diagnostic log', () => {
  it('records events as one line each under a date line, without undefined fields', () => {
    setDiagEnabled(true);
    clearDiag();
    diag('stt.request', { bytes: 64044, final: true, note: undefined, label: 'a b' });
    expect(formatDiag()).toMatch(
      /^# \d{4}-\d\d-\d\d\n\d\d:\d\d:\d\d\.\d{3} stt\.request bytes=64044 final=true label="a b"$/,
    );
  });

  it('starts a new date line when the day changes', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 10, 3, 23, 59, 0));
    diag('mic.start.ok');
    vi.setSystemTime(new Date(2026, 10, 4, 0, 1, 0));
    diag('mic.stop.ok');
    const lines = formatDiag().split('\n');
    expect(lines[0]).toBe('# 2026-11-03');
    expect(lines[2]).toBe('# 2026-11-04');
    expect(lines).toHaveLength(4);
  });

  it('keeps only the newest entries and truncates long strings', () => {
    setDiagEnabled(true);
    clearDiag();
    for (let i = 0; i < DIAG_CAPACITY + 5; i += 1) diag('tick', { i });
    expect(diagEntries()).toHaveLength(DIAG_CAPACITY);
    expect(diagEntries()[0]?.data.i).toBe(5);
    diag('long', { label: 'x '.repeat(300) });
    expect(String(diagEntries().at(-1)?.data.label).length).toBeLessThan(210);
  });

  it('drops entries older than seven days', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 10, 1, 12, 0, 0));
    diag('mic.start.ok', { n: 1 });
    vi.setSystemTime(new Date(2026, 10, 5, 12, 0, 0));
    diag('mic.start.ok', { n: 2 });
    vi.setSystemTime(Date.now() + DIAG_MAX_AGE_MS - 3 * 24 * 3600_000 + 1);
    diag('mic.start.ok', { n: 3 });
    expect(diagEntries().map((e) => e.data.n)).toEqual([2, 3]);
  });

  it('records nothing when disabled', () => {
    setDiagEnabled(true);
    clearDiag();
    setDiagEnabled(false);
    diag('ignored');
    expect(diagEntries()).toHaveLength(0);
  });
});

describe('release builds', () => {
  it('keep the speech events and leave out chatter and other areas', () => {
    setDiagVerbose(false);
    for (const e of [
      'stt.request',
      'mic.start.ok',
      'voco.locked_wake',
      'lock.idle',
      'link.state',
    ]) {
      expect(diagRecords(e)).toBe(true);
    }
    for (const e of ['mic.peek', 'mic.native.stats', 'voco.vad', 'upload.chunk', 'attach.status']) {
      expect(diagRecords(e)).toBe(false);
    }
    diag('upload.begin', { bytes: 1 });
    diag('stt.response', { ms: 120 });
    expect(diagEntries().map((e) => e.event)).toEqual(['stt.response']);
  });

  it('rate limit each event name and report what was dropped', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 10, 5, 10, 0, 0));
    setDiagVerbose(false);
    for (let i = 0; i < DIAG_RATE_PER_MIN + 10; i += 1) diag('lifecycle.focus');
    expect(diagEntries()).toHaveLength(DIAG_RATE_PER_MIN);
    vi.setSystemTime(Date.now() + 61_000);
    diag('lifecycle.focus');
    const tail = diagEntries().slice(-2);
    expect(tail[0]).toMatchObject({
      event: 'diag.dropped',
      data: { event: 'lifecycle.focus', count: 10 },
    });
    expect(tail[1]?.event).toBe('lifecycle.focus');
  });
});

describe('privacy', () => {
  it('masks URLs, tokens, keys, addresses and paths', () => {
    const raw = [
      'GET https://stt.example.internal/v1?key=abc123 failed',
      'vca://pair?token=deadbeef',
      'Authorization: Bearer abc.def-ghi',
      'api_key=XYZ987 password: hunter2',
      'jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig',
      'sk-proj-ABCDEFGHIJKLMNOP',
      'mail alex@example.com',
      'C:\\Users\\alex\\voice.wav and /Users/alex/x.wav',
      'peer 192.168.0.12:7700',
      'blob QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo0NTY3ODkw',
    ].join(' | ');
    const out = redactText(raw);
    for (const leak of [
      'example.internal',
      'abc123',
      'deadbeef',
      'abc.def',
      'XYZ987',
      'hunter2',
      'eyJhbGci',
      'ABCDEFGHIJ',
      'alex',
      '192.168',
      'QUJDREVG',
    ]) {
      expect(out).not.toContain(leak);
    }
    expect(out).toContain('<url>');
    expect(out).toContain('<email>');
    expect(out).toContain('<path>');
    expect(out).toContain('<ip>');
  });

  it('drops content-like fields and redacts again on copy', () => {
    diag('stt.response', {
      text: '회의록 내용',
      transcript: 'secret words',
      textChars: 5,
      error: 'POST https://host/x?api_key=1 timed out',
    });
    const entry = diagEntries().at(-1)!;
    expect(entry.data.text).toBe('[redacted]');
    expect(entry.data.transcript).toBe('[redacted]');
    expect(entry.data.textChars).toBe(5);
    expect(entry.data.error).toBe('POST <url> timed out');
    const report = diagReport({ app: 'Voice Command AI', version: '0.1.0', entries: 1 });
    expect(report).toContain('version: 0.1.0');
    expect(report).not.toContain('회의록');
    expect(report).not.toContain('secret words');
    expect(report).not.toContain('api_key=1');
  });
});

describe('event hooks', () => {
  it('logs app lock changes and verifications', async () => {
    const session = lockSession({ verify: async () => 'verified' as const });
    session.decide(true);
    session.setLocked(false);
    session.setLocked(true, { awaitReturn: true });
    await session.verify({ reason: 'unlock' } as never, 'unlock');
    const events = diagEntries().map(
      (e) => `${e.event}${e.data.idle === undefined ? '' : `:${String(e.data.idle)}`}`,
    );
    expect(events).toEqual([
      'lock.decide',
      'lock.unlocked',
      'lock.locked:true',
      'lock.verify',
      'lock.unlocked',
    ]);
  });
});

describe('host names', () => {
  it('masks bare host names, with or without a port', () => {
    for (const host of [
      'api.example.com',
      'foo.internal:8443',
      'db-1.corp.example.net',
      'stt_box.lan',
      'gpu.home:9000',
      'node.unknown-tld:8443',
    ]) {
      expect(redactText(`connect to ${host} failed`)).toBe('connect to <host> failed');
    }
    expect(redactText('host=api.example.com,')).toBe('host=<host>,');
  });

  it('leaves versions, event labels and file names alone', () => {
    for (const text of [
      '1.2.3',
      'v1.2.3',
      '1.2.3-beta.1',
      '1.2.3.dev',
      'mic.native.start',
      'stt.transport.error',
      'lifecycle.online',
      'composer.dictate.discarded',
      'recording.wav',
      'main.rs',
      'App.test.tsx',
      'Node.js',
      'ratio 12.5 of 3.25',
    ]) {
      expect(redactText(text)).toBe(text);
    }
  });

  it('keeps the event name when a line is formatted', () => {
    diag('mic.native.start', { host: 'api.example.com' });
    const line = formatDiag().split('\n').at(-1)!;
    expect(line).toContain('mic.native.start host="<host>"');
  });
});

describe('release builds', () => {
  const release = () => {
    setDiagVerbose(false);
    clearDiag();
  };

  it('classifies errors instead of keeping their text', () => {
    release();
    diag('stt.transport.error', errorInfo(new Error('connect to api.example.com timed out')));
    const entry = diagEntries().at(-1)!;
    expect(entry.data.error).toBe('timeout');
    expect(entry.data.errorName).toBe('Error');
    expect(formatDiag()).not.toContain('connect to');
  });

  it('keeps short error codes and names, not arbitrary values', () => {
    release();
    const err = Object.assign(new Error('the microphone is taken by Zoom'), {
      name: 'NotAllowedError',
      code: 'mic_permission',
    });
    expect(errorInfo(err)).toEqual({
      error: 'permission',
      errorName: 'NotAllowedError',
      errorCode: 'mic_permission',
    });
    const odd = Object.assign(new Error('x'), { name: 'a b c', code: 'failed at /home/me' });
    expect(errorInfo(odd)).toEqual({ error: 'other', errorName: 'other', errorCode: 'other' });
  });

  it('classifies text passed under any error field', () => {
    release();
    diag('stt.server.error', { code: 'unavailable', error: 'speech_unreachable: no route to box' });
    diag('voco.utterance.error', { message: 'cannot parse the reply of box.local' });
    diag('mic.native.check', { lastError: 'device busy: Jane headset' });
    const [server, utterance, mic] = diagEntries();
    expect(server?.data).toEqual({ code: 'unavailable', error: 'network' });
    expect(utterance?.data.message).toBe('invalid');
    expect(mic?.data.lastError).toBe('busy');
  });

  it('gives the same class when an entry is cleaned again', () => {
    for (const cls of ['timeout', 'canceled', 'permission', 'network', 'not_found', 'busy']) {
      expect(errorClass(cls)).toBe(cls);
    }
    expect(errorClass('invalid')).toBe('invalid');
    expect(errorClass('other')).toBe('other');
    expect(errorClass('')).toBe('other');
  });

  it('keeps the text in development builds', () => {
    setDiagVerbose(true);
    expect(errorInfo(new Error('disk full')).error).toBe('disk full');
    diag('mic.native.check', { lastError: 'disk full' });
    expect(diagEntries().at(-1)?.data.lastError).toBe('disk full');
  });

  it('applies to entries stored before the rule, when copied', () => {
    setDiagVerbose(true);
    diag('stt.server.error', { error: 'refused by a customer box' });
    setDiagVerbose(false);
    expect(formatDiag()).toContain('error="network"');
    expect(formatDiag()).not.toContain('customer');
  });

  const route = (type: string, name: string) =>
    `route=<AVAudioSessionRouteDescription: 0x280c, inputs = ( "<AVAudioSessionPortDescription: 0x2, ` +
    `type = MicrophoneBuiltIn; name = ${name} Mic; UID = abc; selectedDataSource = Front>", ` +
    `"<AVAudioSessionPortDescription: 0x3, type = BluetoothHFP; name = ${name}; UID = 11:22; ` +
    `selectedDataSource = (null)>" ); outputs = ( "<AVAudioSessionPortDescription: 0x4, ` +
    `type = ${type}; name = ${name}; UID = 33:44; selectedDataSource = (null)>" )>`;

  it('reduces an iOS audio route to a fixed class', () => {
    release();
    const cases: [string, string][] = [
      ['Speaker', 'speaker'],
      ['Receiver', 'receiver'],
      ['BluetoothA2DPOutput', 'bluetooth'],
      ['BluetoothHFP', 'bluetooth'],
      ['BluetoothLE', 'bluetooth'],
      ['Headphones', 'wired'],
      ['LineOut', 'wired'],
      ['USBAudio', 'wired'],
      ['AirPlay', 'other'],
    ];
    for (const [type, cls] of cases) {
      clearDiag();
      const session = `category=PlayAndRecord mode=Default rate=48000 ${route(type, "Jane's AirPods")}`;
      diag('mic.native.start', { session });
      expect(diagEntries().at(-1)?.data.session).toBe(
        `category=PlayAndRecord mode=Default rate=48000 route=${cls}`,
      );
    }
  });

  it('removes both routes of an activation note and routes inside JSON', () => {
    release();
    const note = `before: mode=a ${route('Speaker', 'Jane')} | errors: [] | after: mode=b ${route('Receiver', 'Jane')}`;
    diag('mic.native.start', { note });
    expect(diagEntries().at(-1)?.data.note).toBe(
      'before: mode=a route=speaker | errors: [] | after: mode=b route=receiver',
    );
    diag('snapshot.mic', {
      json: JSON.stringify({ session: `mode=a ${route('Speaker', 'Jane')}`, frames: 3 }),
    });
    const json = String(diagEntries().at(-1)?.data.json);
    expect(json).toContain('route=speaker');
    expect(json).not.toContain('Jane');
  });

  it('keeps the route in development builds', () => {
    setDiagVerbose(true);
    diag('mic.native.start', { session: route('Speaker', 'Jane') });
    expect(diagEntries().at(-1)?.data.session).toContain('Jane');
  });
});

describe('off switch', () => {
  it('stops recording and clears the stored log', () => {
    diag('mic.start.ok');
    flushDiag();
    expect(localStorage.getItem('vca.diag.v1')).toContain('mic.start.ok');
    setDiagEnabled(false);
    expect(diagEntries()).toEqual([]);
    expect(localStorage.getItem('vca.diag.v1')).toBeNull();
    diag('mic.start.ok');
    flushDiag();
    expect(diagEntries()).toEqual([]);
    expect(localStorage.getItem('vca.diag.v1')).toBeNull();
  });

  it('does not let a pending save bring the log back', () => {
    vi.useFakeTimers();
    diag('mic.start.ok');
    setDiagEnabled(false);
    vi.advanceTimersByTime(5000);
    expect(localStorage.getItem('vca.diag.v1')).toBeNull();
  });

  it('records again when turned back on', () => {
    setDiagEnabled(false);
    setDiagEnabled(true);
    diag('mic.start.ok');
    expect(diagEntries()).toHaveLength(1);
  });

  it('tells the view that the log changed', () => {
    diag('mic.start.ok');
    const listener = vi.fn();
    const off = subscribeDiag(listener);
    setDiagEnabled(false);
    off();
    expect(listener).toHaveBeenCalled();
  });
});
