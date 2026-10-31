import { describe, expect, it } from 'vitest';

import { decodeJson, decodeMsgpack, encodeJson, encodeMsgpack, ProtocolError } from './codec';
import type { ClientMsg, Envelope, LaunchRequest } from './types';

const hex = (s: string) => Uint8Array.from(s.match(/../g)!.map((b) => parseInt(b, 16)));

// Produced by rmp_serde::to_vec_named / serde_json from vca-core (see README "Protocol fixtures").
const FIXTURES = {
  serverEventMsgpack:
    '82a2696400a4626f647983a474797065a56576656e74aa73657373696f6e5f6964a27331a56576656e7484a474797065a66f7574707574a373657103a2617401a464617461c40968656c6c6f1b5b306d',
  sessionsMsgpack:
    '82a2696405a4626f647982a474797065a873657373696f6e73a873657373696f6e739189a26964a161a66f726967696e84a474797065a56c6f63616ca56167656e7482a474797065a767656e65726963a46e616d65a56d79636c69ab70726f6a6563745f646972a22f70a3706964c0ac646973706c61795f6e616d65a16ea5737461746582a474797065a6657869746564a4636f646501a7636f6e74726f6cab6d616e616765645f707479a770726f66696c65c0aa637265617465645f617401ad6c6173745f616374697669747902a6756e7265616400',
  okMsgpack: '82a2696409a4626f647981a474797065a26f6b',
  sessionsJson:
    '{"id":5,"body":{"type":"sessions","sessions":[{"id":"a","origin":{"type":"local","agent":{"type":"generic","name":"mycli"},"project_dir":"/p","pid":null},"display_name":"n","state":{"type":"exited","code":1},"control":"managed_pty","profile":null,"created_at":1,"last_activity":2,"unread":0}]}}',
  clientSubscribeJson: '{"id":7,"body":{"type":"subscribe","session_id":"s1","from_seq":null}}',
  clientLaunchJson:
    '{"id":2,"body":{"type":"launch","request":{"agent":{"type":"claude_code"},"project_dir":"~/x","create_dir":true,"profile":null,"args":[],"display_name":null}}}',
};

describe('codec (vca-core fixtures)', () => {
  it('decodes a msgpack output event with bin data as Uint8Array', () => {
    const env = decodeMsgpack(hex(FIXTURES.serverEventMsgpack));
    expect(env.id).toBe(0);
    expect(env.body.type).toBe('event');
    if (env.body.type !== 'event' || env.body.event.type !== 'output') throw new Error('shape');
    expect(env.body.session_id).toBe('s1');
    expect(env.body.event.seq).toBe(3);
    expect(env.body.event.data).toBeInstanceOf(Uint8Array);
    expect(new TextDecoder().decode(env.body.event.data)).toBe('hello\x1b[0m');
  });

  it('decodes sessions with a generic agent, null options and exited state', () => {
    const env = decodeMsgpack(hex(FIXTURES.sessionsMsgpack));
    expect(env).toEqual(JSON.parse(FIXTURES.sessionsJson));
  });

  it('decodes a unit variant', () => {
    expect(decodeMsgpack(hex(FIXTURES.okMsgpack))).toEqual({ id: 9, body: { type: 'ok' } });
  });

  it('encodes client messages exactly like serde_json', () => {
    const subscribe: Envelope<ClientMsg> = {
      id: 7,
      body: { type: 'subscribe', session_id: 's1', from_seq: null },
    };
    expect(encodeJson(subscribe)).toBe(FIXTURES.clientSubscribeJson);
    const request: LaunchRequest = {
      agent: { type: 'claude_code' },
      project_dir: '~/x',
      create_dir: true,
      profile: null,
      args: [],
      display_name: null,
    };
    expect(encodeJson({ id: 2, body: { type: 'launch', request } })).toBe(
      FIXTURES.clientLaunchJson,
    );
  });

  it('matches the tagged JSON shape asserted in protocol.rs', () => {
    const json = JSON.parse(
      encodeJson({
        id: 7,
        body: { type: 'send_input', session_id: 's1', input: { type: 'key', key: 'interrupt' } },
      }),
    );
    expect(json.body.type).toBe('send_input');
    expect(json.body.input.type).toBe('key');
    expect(json.body.input.key).toBe('interrupt');
  });
});

describe('codec round trips', () => {
  it('round-trips a client envelope through msgpack', () => {
    const env: Envelope<ClientMsg> = {
      id: 3,
      body: {
        type: 'send_input',
        session_id: 'abc',
        input: { type: 'choose', decision_id: 'd1', option_id: 'yes' },
      },
    };
    expect(decodeMsgpack(encodeMsgpack(env))).toEqual(env);
  });

  it('JSON output data: Uint8Array is written as a number array and read back as bytes', () => {
    const text = encodeJson({
      id: 0,
      body: {
        type: 'event',
        session_id: 's',
        event: { type: 'output', seq: 1, at: 2, data: Uint8Array.from([0, 255, 10]) },
      },
    });
    expect(text).toContain('"data":[0,255,10]');
    const env = decodeJson(text);
    if (env.body.type !== 'event' || env.body.event.type !== 'output') throw new Error('shape');
    expect(Array.from(env.body.event.data)).toEqual([0, 255, 10]);
  });

  it('rejects malformed envelopes', () => {
    expect(() => decodeJson('{"id":-1,"body":{"type":"ok"}}')).toThrow(ProtocolError);
    expect(() => decodeJson('{"id":1,"body":{}}')).toThrow(ProtocolError);
    expect(() => decodeJson('not json')).toThrow(ProtocolError);
    expect(() =>
      decodeJson(
        '{"id":0,"body":{"type":"event","session_id":"s","event":{"type":"output","seq":1,"at":1,"data":"x"}}}',
      ),
    ).toThrow(ProtocolError);
  });
});
