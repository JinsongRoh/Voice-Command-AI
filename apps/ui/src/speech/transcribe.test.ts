import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { LinkStatus } from '../native/bridge';
import { TauriTransport } from '../protocol/tauriTransport';
import { Client, type ConnectionState } from '../protocol/transport';
import type { ClientMsg, Envelope, ServerMsg } from '../protocol/types';
import { FakeTransport } from '../test/fakeTransport';
import {
  decodeClient,
  serverBytes,
  status,
  tauri,
  type FakeChannel,
  type Link,
} from '../test/tauriMock';
import { createNativeRecorder } from './recorder';
import { RequestError } from '../state/core';
import {
  audioEncodingOf,
  correctionSpans,
  createServerTranscriber,
  isNetworkError,
  isSpeechUnreachableError,
  RECONNECT_WAIT_MS,
} from './transcribe';

vi.mock('@tauri-apps/api/core', () => import('../test/tauriMock'));

const TRANSCRIPT: ServerMsg = {
  type: 'transcript',
  text: 'run pnpm test',
  raw_text: 'run p n p m test',
  language: 'en',
  corrections: [
    { from: 'p n p m', to: 'pnpm', layer: 'base' },
    { from: 'tst', to: 'test', layer: 'personal' },
    { from: 'x', to: 'y', layer: 'team' as 'base' },
  ],
  intent: { type: 'prompt' },
  engine: 'whisper',
};

const OPTIONS = { locale: 'ko' as const, sessionId: 's1' };

describe('server transcriber', () => {
  it('sends transcribe through the client and maps the transcript', async () => {
    const transport = new FakeTransport((msg: ClientMsg) =>
      msg.type === 'transcribe' ? TRANSCRIPT : { type: 'ok' },
    );
    const transcribe = createServerTranscriber(() => new Client(transport));
    const audio = new Blob([new Uint8Array([1, 2, 3])], { type: 'audio/webm;codecs=opus' });
    const result = await transcribe(audio, OPTIONS);
    expect(result).toEqual({
      text: 'run pnpm test',
      rawText: 'run p n p m test',
      corrections: [
        { from: 'p n p m', to: 'pnpm', layer: 'base' },
        { from: 'tst', to: 'test', layer: 'personal' },
        { from: 'x', to: 'y', layer: 'base' },
      ],
    });
    const [sent] = transport.bodies('transcribe');
    expect(sent).toMatchObject({
      type: 'transcribe',
      format: 'webm',
      sample_rate: null,
      // The UI locale is never forced on the recognizer: the server decides.
      language: null,
      session_id: 's1',
    });
    expect(Array.from((sent as { audio: Uint8Array }).audio)).toEqual([1, 2, 3]);
    expect(sent).not.toHaveProperty('context');
  });

  it('sends a speech language only when the user picked one', async () => {
    const transport = new FakeTransport((msg: ClientMsg) =>
      msg.type === 'transcribe' ? TRANSCRIPT : { type: 'ok' },
    );
    const transcribe = createServerTranscriber(() => new Client(transport));
    const audio = new Blob([new Uint8Array([1])], { type: 'audio/wav' });
    await transcribe(audio, { ...OPTIONS, locale: 'en', language: null });
    await transcribe(audio, { ...OPTIONS, locale: 'en', language: 'ko' });
    expect(
      transport.bodies('transcribe').map((b) => (b as { language: unknown }).language),
    ).toEqual([null, 'ko']);
  });

  it('drops a lone stock phrase on quiet WAV audio only', async () => {
    const wav = (level: number): Blob => {
      const out = new Uint8Array(44 + 64000);
      const v = new DataView(out.buffer);
      [...'RIFF'].forEach((c, i) => (out[i] = c.charCodeAt(0)));
      [...'WAVEfmt '].forEach((c, i) => (out[8 + i] = c.charCodeAt(0)));
      v.setUint32(16, 16, true);
      v.setUint16(20, 1, true);
      v.setUint16(22, 1, true);
      v.setUint32(24, 16000, true);
      v.setUint16(34, 16, true);
      [...'data'].forEach((c, i) => (out[36 + i] = c.charCodeAt(0)));
      v.setUint32(40, 64000, true);
      for (let i = 0; i < 32000; i += 1) v.setInt16(44 + i * 2, level, true);
      return new Blob([out], { type: 'audio/wav' });
    };
    const say = (text: string): Client =>
      new Client(
        new FakeTransport((msg: ClientMsg) =>
          msg.type === 'transcribe' ? { ...TRANSCRIPT, text, raw_text: text } : { type: 'ok' },
        ),
      );
    const run = (text: string, level: number) =>
      createServerTranscriber(() => say(text))(wav(level), OPTIONS);
    expect((await run('감사합니다.', 20)).text).toBe('');
    expect((await run('감사합니다.', 4000)).text).toBe('감사합니다.');
    expect((await run('도와주셔서 감사합니다 다음 작업 진행해', 20)).text).toBe(
      '도와주셔서 감사합니다 다음 작업 진행해',
    );
  });

  it('locates correction terms in raw and corrected text', () => {
    const c = [
      { from: 'p n p m', to: 'pnpm', layer: 'base' as const },
      { from: 'the bug', to: 'the race condition', layer: 'context' as const },
    ];
    expect(correctionSpans('run p n p m and fix the bug', c, 'from')).toEqual([
      { start: 4, end: 11, correction: c[0] },
      { start: 20, end: 27, correction: c[1] },
    ]);
    expect(correctionSpans('run pnpm, pnpm', c, 'to').map((s) => s.start)).toEqual([4, 10]);
  });

  it('sends preceding text as context only when present', async () => {
    const transport = new FakeTransport((msg: ClientMsg) =>
      msg.type === 'transcribe' ? TRANSCRIPT : { type: 'ok' },
    );
    const transcribe = createServerTranscriber(() => new Client(transport));
    const audio = new Blob([new Uint8Array([1])], { type: 'audio/wav' });
    await transcribe(audio, { ...OPTIONS, context: 'Fix the tests' });
    await transcribe(audio, { ...OPTIONS, context: null });
    const [withContext, withoutContext] = transport.bodies('transcribe');
    expect(withContext).toMatchObject({ format: 'wav', context: 'Fix the tests' });
    expect(withoutContext).not.toHaveProperty('context');
  });

  it('sends final only for final chunks and maps the topic and context layer', async () => {
    const transport = new FakeTransport((msg: ClientMsg) =>
      msg.type === 'transcribe'
        ? {
            ...TRANSCRIPT,
            corrections: [{ from: 'sub mitt', to: 'submit', layer: 'context' }],
            topic: msg.final ? ' frontend/React ' : null,
          }
        : { type: 'ok' },
    );
    const transcribe = createServerTranscriber(() => new Client(transport));
    const audio = new Blob([new Uint8Array([1])], { type: 'audio/wav' });
    const interim = await transcribe(audio, OPTIONS);
    const final = await transcribe(audio, { ...OPTIONS, final: true });
    const [interimMsg, finalMsg] = transport.bodies('transcribe');
    expect(interimMsg).not.toHaveProperty('final');
    expect(finalMsg).toMatchObject({ final: true });
    expect(interim).not.toHaveProperty('topic');
    expect(final.topic).toBe('frontend/React');
    expect(final.corrections).toEqual([{ from: 'sub mitt', to: 'submit', layer: 'context' }]);
  });

  it('surfaces error responses and transport failures as readable errors', async () => {
    const off = new FakeTransport(() => ({
      type: 'error',
      code: 'unavailable',
      message: 'speech recognition is disabled on this server',
    }));
    const audio = new Blob([new Uint8Array([1])], { type: 'audio/wav' });
    await expect(createServerTranscriber(() => new Client(off))(audio, OPTIONS)).rejects.toThrow(
      'speech recognition is disabled on this server',
    );
    await expect(createServerTranscriber(() => null)(audio, OPTIONS)).rejects.toThrow(/no machine/);
    const ok = new FakeTransport();
    await expect(
      createServerTranscriber(() => new Client(ok))(new Blob([], { type: 'video/x-foo' }), OPTIONS),
    ).rejects.toThrow(/unsupported audio format/);
    expect(ok.sent).toHaveLength(0);
  });

  it('maps MIME types to audio encodings', () => {
    expect(audioEncodingOf('audio/wav')).toBe('wav');
    expect(audioEncodingOf('audio/x-wav')).toBe('wav');
    expect(audioEncodingOf('audio/webm;codecs=opus')).toBe('webm');
    expect(audioEncodingOf('audio/ogg; codecs=opus')).toBe('ogg');
    expect(audioEncodingOf('audio/mp4')).toBe('m4a');
    expect(audioEncodingOf('audio/x-m4a')).toBe('m4a');
    expect(audioEncodingOf('')).toBeNull();
  });
});

/** A link that can drop: `answers` script each `send` (a reply, or an error to throw). */
class FlakyLink extends FakeTransport {
  constructor(private readonly answers: (ServerMsg | Error)[]) {
    super();
    this.state = 'connected';
  }
  override async send(envelope: Envelope<ClientMsg>) {
    this.sent.push(envelope);
    const next = this.answers.shift() ?? TRANSCRIPT;
    if (next instanceof Error) {
      // Like TauriTransport: a drop fails the pending requests and the link
      // reports `reconnecting` before anyone sees the failure.
      if (/connection lost/.test(next.message)) this.go('reconnecting');
      throw next;
    }
    return next;
  }
  go(state: ConnectionState) {
    if (state === 'connected') void this.connect();
    else if (state === 'disconnected') this.disconnect();
    else {
      this.state = state;
      // FakeTransport has no public way to announce other states.
      (this as unknown as { setState(s: ConnectionState): void }).setState(state);
    }
  }
}

describe('server transcriber on a weak link', () => {
  const wav = new Blob([new Uint8Array(44 + 32_000)], { type: 'audio/wav' });
  const FINAL = { ...OPTIONS, final: true };

  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('sends a final chunk again once a dropped link is back', async () => {
    const link = new FlakyLink([new Error('connection lost (transport_error)'), TRANSCRIPT]);
    const transcribe = createServerTranscriber(() => new Client(link));
    const result = transcribe(wav, FINAL);
    await vi.advanceTimersByTimeAsync(3000);
    expect(link.state).toBe('reconnecting');
    expect(link.bodies('transcribe')).toHaveLength(1);
    link.go('connected');
    await expect(result).resolves.toMatchObject({ text: 'run pnpm test' });
    const sent = link.bodies('transcribe') as { audio: Uint8Array }[];
    expect(sent).toHaveLength(2);
    // The same recording both times: nothing has to be said again.
    expect(sent[1]!.audio).toEqual(sent[0]!.audio);
    expect(sent[1]!.audio.length).toBe(wav.size);
  });

  it('waits for a reconnecting link before sending a final chunk', async () => {
    const link = new FlakyLink([TRANSCRIPT]);
    link.go('reconnecting');
    const result = createServerTranscriber(() => new Client(link))(wav, FINAL);
    await vi.advanceTimersByTimeAsync(5000);
    expect(link.sent).toHaveLength(0);
    link.go('connected');
    await expect(result).resolves.toMatchObject({ text: 'run pnpm test' });
    expect(link.sent).toHaveLength(1);
  });

  it('gives up after the reconnect wait with a network error', async () => {
    const link = new FlakyLink([new Error('not connected')]);
    link.go('reconnecting');
    const result = createServerTranscriber(() => new Client(link))(wav, FINAL);
    const assertion = expect(result).rejects.toSatisfy(isNetworkError);
    await vi.advanceTimersByTimeAsync(RECONNECT_WAIT_MS);
    // Sent once anyway (it fails fast while the link is down), never twice.
    await vi.advanceTimersByTimeAsync(RECONNECT_WAIT_MS);
    await assertion;
    expect(link.sent).toHaveLength(1);
  });

  it('does not repeat a request that timed out on a live link, nor previews', async () => {
    const slow = new FlakyLink([new Error('request timed out (transcribe)')]);
    const a = createServerTranscriber(() => new Client(slow))(wav, FINAL);
    await expect(a).rejects.toSatisfy(isNetworkError);
    expect(slow.sent).toHaveLength(1);

    const dropped = new FlakyLink([new Error('connection lost (transport_error)')]);
    const b = createServerTranscriber(() => new Client(dropped))(wav, OPTIONS);
    await expect(b).rejects.toSatisfy(isNetworkError);
    dropped.go('connected');
    expect(dropped.sent).toHaveLength(1);
  });

  it('keeps server answers apart from network errors', async () => {
    const off = new FlakyLink([
      { type: 'error', code: 'unavailable', message: 'speech_unreachable: try again' },
    ]);
    const err = await createServerTranscriber(() => new Client(off))(wav, FINAL).catch((e) => e);
    expect(isNetworkError(err)).toBe(false);
    expect(isSpeechUnreachableError(err)).toBe(true);
    expect(off.sent).toHaveLength(1);
  });
});

describe('server transcriber over the native link', () => {
  const wav = new Blob([new Uint8Array(44 + 32_000)], { type: 'audio/wav' });
  const FINAL = { ...OPTIONS, final: true };
  const TIMEOUT_MS = 5000;

  /** Records `vca_connect` channels; the test answers on them. */
  function captureLinks() {
    const links: Link[] = [];
    tauri.handlers.set('vca_connect', (args) => {
      const { onStatus, onMessage } = args as {
        onStatus: FakeChannel<LinkStatus>;
        onMessage: FakeChannel<unknown>;
      };
      links.push({ status: onStatus, message: onMessage });
      return undefined;
    });
    tauri.handlers.set('vca_send', () => undefined);
    tauri.handlers.set('vca_disconnect', () => undefined);
    return links;
  }

  async function setup() {
    const links = captureLinks();
    const transport = new TauriTransport('m1', {
      transcribeTimeoutMs: TIMEOUT_MS,
      quietProbeMs: 0,
    });
    const p = transport.connect();
    await vi.advanceTimersByTimeAsync(0);
    links[0]!.status.onmessage(status('connected'));
    await p;
    return { links, transport, client: new Client(transport) };
  }

  const sentTranscribes = () =>
    tauri
      .callsOf('vca_send')
      .map((c) => decodeClient(c.args))
      .filter((e) => e.body.type === 'transcribe');

  beforeEach(() => {
    vi.useFakeTimers();
    tauri.reset();
  });
  afterEach(() => vi.useRealTimers());

  it('recycles the link when a final times out and sends it once more', async () => {
    const { links, transport, client } = await setup();
    const result = createServerTranscriber(() => client)(wav, FINAL);
    await vi.advanceTimersByTimeAsync(TIMEOUT_MS);
    // No answer: the link is replaced instead of left "connected".
    expect(transport.state).toBe('reconnecting');
    expect(links).toHaveLength(2);
    expect(sentTranscribes()).toHaveLength(1);
    links[1]!.status.onmessage(status('connected'));
    await vi.advanceTimersByTimeAsync(0);
    const sent = sentTranscribes();
    expect(sent).toHaveLength(2);
    expect((sent[1]!.body as { audio: Uint8Array }).audio).toEqual(
      (sent[0]!.body as { audio: Uint8Array }).audio,
    );
    // A late answer on the old link is dropped; the new one counts, once.
    links[0]!.message.onmessage(serverBytes({ id: sent[0]!.id, body: TRANSCRIPT }));
    links[1]!.message.onmessage(serverBytes({ id: sent[1]!.id, body: TRANSCRIPT }));
    await expect(result).resolves.toMatchObject({ text: 'run pnpm test' });
    expect(sentTranscribes()).toHaveLength(2);
  });

  it('retries a timed-out final only once and keeps it a network error', async () => {
    const { links, client } = await setup();
    const result = createServerTranscriber(() => client)(wav, FINAL);
    const assertion = expect(result).rejects.toSatisfy(isNetworkError);
    await vi.advanceTimersByTimeAsync(TIMEOUT_MS);
    links[1]!.status.onmessage(status('connected'));
    await vi.advanceTimersByTimeAsync(TIMEOUT_MS);
    await assertion;
    expect(sentTranscribes()).toHaveLength(2);
    links[2]?.status.onmessage(status('connected'));
    await vi.advanceTimersByTimeAsync(TIMEOUT_MS);
    expect(sentTranscribes()).toHaveLength(2);
  });

  it('recycles the link for a timed-out preview but does not resend it', async () => {
    const { links, transport, client } = await setup();
    const result = createServerTranscriber(() => client)(wav, OPTIONS);
    const assertion = expect(result).rejects.toSatisfy(isNetworkError);
    await vi.advanceTimersByTimeAsync(TIMEOUT_MS);
    await assertion;
    expect(transport.state).toBe('reconnecting');
    links[1]!.status.onmessage(status('connected'));
    await vi.advanceTimersByTimeAsync(TIMEOUT_MS);
    expect(sentTranscribes()).toHaveLength(1);
  });
});

describe('isSpeechUnreachableError', () => {
  it('recognizes an unreachable speech server, new and old wording', () => {
    const msg =
      'speech_unreachable: the speech server could not be reached (error sending request)';
    expect(isSpeechUnreachableError(new RequestError('unavailable', msg))).toBe(true);
    expect(
      isSpeechUnreachableError(
        new RequestError('unavailable', 'http error: error sending request'),
      ),
    ).toBe(true);
    // A recognizer that stayed busy (vcad gives up waiting for a slot).
    expect(
      isSpeechUnreachableError(
        new RequestError('unavailable', 'speech_busy: the speech server is busy'),
      ),
    ).toBe(true);
  });

  it('leaves answered errors and other codes alone', () => {
    expect(
      isSpeechUnreachableError(
        new RequestError('unavailable', 'speech service returned 500: boom'),
      ),
    ).toBe(false);
    expect(isSpeechUnreachableError(new RequestError('internal', 'speech_unreachable: x'))).toBe(
      false,
    );
    expect(isSpeechUnreachableError(new Error('speech_unreachable: x'))).toBe(false);
  });
});

describe('native recorder', () => {
  beforeEach(() => tauri.reset());

  it('records through vca_mic_* and returns a WAV blob', async () => {
    tauri.handlers.set('vca_mic_start', () => undefined);
    tauri.handlers.set('vca_mic_stop', () => [82, 73, 70, 70]); // number[] (Android IPC)
    const rec = createNativeRecorder();
    await rec.start();
    const blob = await rec.stop();
    expect(blob.type).toBe('audio/wav');
    expect(Array.from(new Uint8Array(await blob.arrayBuffer()))).toEqual([82, 73, 70, 70]);
    // The debug log's counter snapshots (`vca_mic_diag`) are not part of the recording.
    expect(tauri.calls.map((c) => c.cmd).filter((c) => c !== 'vca_mic_diag')).toEqual([
      'vca_mic_start',
      'vca_mic_stop',
    ]);
  });

  it('peeks and splits through vca_mic_peek / vca_mic_split', async () => {
    tauri.handlers.set('vca_mic_peek', () => new Uint8Array([82, 73]).buffer);
    tauri.handlers.set('vca_mic_split', () => new Uint8Array(0).buffer); // not enough audio yet
    const rec = createNativeRecorder();
    const peeked = await rec.peek!();
    expect(peeked.type).toBe('audio/wav');
    expect(Array.from(new Uint8Array(await peeked.arrayBuffer()))).toEqual([82, 73]);
    const empty = await rec.split!(15);
    expect(empty.size).toBe(0);
    tauri.handlers.set('vca_mic_split', () => [82, 73, 70, 70]); // number[] (Android IPC)
    const chunk = await rec.split!(15);
    expect(chunk.type).toBe('audio/wav');
    expect(chunk.size).toBe(4);
    tauri.handlers.set('vca_mic_split', () => null);
    expect((await rec.split!(15)).size).toBe(0);
    expect(tauri.callsOf('vca_mic_split')[0]!.args).toEqual({ minSeconds: 15 });
    expect(tauri.calls.map((c) => c.cmd)).toEqual([
      'vca_mic_peek',
      'vca_mic_split',
      'vca_mic_split',
      'vca_mic_split',
    ]);
  });

  it('cancels without throwing and propagates start failures', async () => {
    tauri.handlers.set('vca_mic_start', () => {
      throw { code: 'permission_denied', message: 'microphone permission denied' };
    });
    const rec = createNativeRecorder();
    await expect(rec.start()).rejects.toThrow('microphone permission denied');
    rec.cancel(); // a missing command rejects; must be swallowed
    await Promise.resolve();
    expect(tauri.callsOf('vca_mic_cancel')).toHaveLength(1);
  });
});
