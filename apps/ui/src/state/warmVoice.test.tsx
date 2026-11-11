import { waitFor } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import type { ClientMsg } from '../protocol/types';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { renderWithApp } from '../test/render';

function pc() {
  return new FakeTransport((msg: ClientMsg) => {
    if (msg.type === 'list_tts_engines') {
      return {
        type: 'tts_engines',
        engines: [
          {
            name: 'local-tts',
            languages: ['ko'],
            default: true,
            speakers: [{ id: 'F1', label: 'Female 1', gender: 'female' }],
          },
        ],
      };
    }
    if (msg.type === 'synthesize') {
      return { type: 'audio', format: 'mp3', data: new Uint8Array([1]) };
    }
    return defaultResponse(msg);
  });
}

describe('warming the PC voice', () => {
  it('synthesizes "네" first once connected, with the chosen speaker', async () => {
    const transport = pc();
    renderWithApp(<div />, {
      transport,
      settings: { handsFree: true, ttsServerEngine: 'local-tts', language: 'ko' },
    });
    await waitFor(() => expect(transport.bodies('synthesize').length).toBeGreaterThan(0), {
      timeout: 5_000,
    });
    expect(transport.bodies('synthesize')[0]).toMatchObject({
      text: '네',
      language: 'ko-KR',
      engine: 'local-tts',
      voice: 'F1',
    });
  });

  it('asks for nothing with the device voice or without hands-free mode', async () => {
    const device = pc();
    renderWithApp(<div />, {
      transport: device,
      settings: { handsFree: true, ttsServerEngine: null, language: 'ko' },
    });
    const off = pc();
    renderWithApp(<div />, {
      transport: off,
      settings: { handsFree: false, ttsServerEngine: 'local-tts', language: 'ko' },
    });
    await new Promise((r) => setTimeout(r, 2_500));
    expect(device.bodies('synthesize')).toEqual([]);
    expect(off.bodies('synthesize')).toEqual([]);
  });
});
