import { beforeEach, describe, expect, it, vi } from 'vitest';

import { Client } from '../protocol/transport';
import type { ClientMsg, ServerMsg } from '../protocol/types';
import { FakeTransport } from '../test/fakeTransport';
import { tauri } from '../test/tauriMock';
import { createNativeRecorder } from './recorder';
import { audioEncodingOf, createServerTranscriber } from './transcribe';

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

const OPTIONS = { engine: 'whisper_server' as const, locale: 'ko' as const, sessionId: 's1' };

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
      language: 'ko',
      session_id: 's1',
    });
    expect(Array.from((sent as { audio: Uint8Array }).audio)).toEqual([1, 2, 3]);
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
    expect(tauri.calls.map((c) => c.cmd)).toEqual(['vca_mic_start', 'vca_mic_stop']);
  });

  it('cancels without throwing and propagates start failures', async () => {
    tauri.handlers.set('vca_mic_start', () => {
      throw { code: 'permission_denied', message: 'microphone permission denied' };
    });
    const rec = createNativeRecorder();
    await expect(rec.start()).rejects.toThrow('microphone permission denied');
    rec.cancel(); // unhandled command rejects; must be swallowed
    await Promise.resolve();
    expect(tauri.callsOf('vca_mic_cancel')).toHaveLength(1);
  });
});
