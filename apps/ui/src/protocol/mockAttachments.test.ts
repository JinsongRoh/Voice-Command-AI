import { describe, expect, it } from 'vitest';

import { uploadFile } from '../attachments/upload';
import { mockSanitizeName } from './mockAttachments';
import { MockTransport } from './mockTransport';
import { Client } from './transport';
import type { ServerMsg } from './types';

describe('MockTransport attachments', () => {
  it('saves uploads for local sessions, types into managed ones and refuses cloud sessions', async () => {
    const t = new MockTransport({ latencyMs: 0, autoTick: false, now: () => 1_700_000_000_000 });
    await t.connect();
    const client = new Client(t);
    const list = await client.request({ type: 'list_sessions' });
    if (list.type !== 'sessions') throw new Error('expected sessions');
    const managed = list.sessions.find((s) => s.control === 'managed_pty')!;
    const cloud = list.sessions.find((s) => s.origin.type === 'cloud')!;
    const pushed: ServerMsg[] = [];
    t.onMessage((e) => pushed.push(e.body));
    await client.request({ type: 'subscribe', session_id: managed.id, from_seq: null });

    const request = (body: Parameters<Client['request']>[0]) => client.request(body);
    const file = new File(['hello'], 'a b.txt', { type: 'text/plain' });
    const r = await uploadFile(request, file, { sessionId: managed.id });
    expect(r.typed).toBe(true);
    expect(r.path).toBe('.vca/attachments/20231114-221320-a-b.txt');
    const echoed = pushed.some(
      (m) =>
        m.type === 'event' &&
        m.event.type === 'output' &&
        new TextDecoder().decode(m.event.data).includes(r.mention),
    );
    expect(echoed).toBe(true);

    await expect(uploadFile(request, file, { sessionId: cloud.id })).rejects.toMatchObject({
      code: 'unsupported',
    });
    t.disconnect();
  });

  it('sanitizes names like the server', () => {
    expect(mockSanitizeName('../x/../IMG 1.jpg')).toBe('IMG-1.jpg');
    expect(mockSanitizeName('..')).toBe('file');
  });
});
