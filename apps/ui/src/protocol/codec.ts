/**
 * (De)serialization of protocol envelopes.
 *
 * - MessagePack is the wire format inside the Noise channel (rmp_serde
 *   `to_vec_named`: structs as maps, `Vec<u8>` as `bin`).
 * - JSON is used for local IPC and debugging; there `Vec<u8>` is a number array.
 */
import { decode, encode } from '@msgpack/msgpack';

import type { ClientMsg, Envelope, ServerMsg, SessionEvent } from './types';

export class ProtocolError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'ProtocolError';
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** Structural check of an envelope; the body must carry a string `type` tag. */
export function assertEnvelope(value: unknown): Envelope<ServerMsg> {
  if (!isRecord(value)) throw new ProtocolError('envelope is not an object');
  const { id, body } = value;
  if (typeof id !== 'number' || !Number.isInteger(id) || id < 0) {
    throw new ProtocolError('envelope id must be a non-negative integer');
  }
  if (!isRecord(body) || typeof body.type !== 'string') {
    throw new ProtocolError('envelope body must be a tagged object');
  }
  return { id, body: normalizeServerMsg(body as unknown as ServerMsg) };
}

/** Ensures `Output.data` is a `Uint8Array`, whatever the source encoding. */
function normalizeEvent(event: SessionEvent): SessionEvent {
  if (event.type !== 'output') return event;
  const raw: unknown = event.data;
  if (raw instanceof Uint8Array) return event;
  if (Array.isArray(raw)) return { ...event, data: Uint8Array.from(raw as number[]) };
  throw new ProtocolError('output data must be bytes');
}

function normalizeServerMsg(msg: ServerMsg): ServerMsg {
  if (msg.type === 'event') return { ...msg, event: normalizeEvent(msg.event) };
  return msg;
}

export function encodeMsgpack(envelope: Envelope<ClientMsg>): Uint8Array {
  return encode(envelope);
}

export function decodeMsgpack(bytes: Uint8Array): Envelope<ServerMsg> {
  return assertEnvelope(decode(bytes));
}

function bytesReplacer(_key: string, value: unknown): unknown {
  return value instanceof Uint8Array ? Array.from(value) : value;
}

/** Serializes any envelope as JSON with byte arrays as number arrays (serde_json shape). */
export function encodeJson<T>(envelope: Envelope<T>): string {
  return JSON.stringify(envelope, bytesReplacer);
}

export function decodeJson(text: string): Envelope<ServerMsg> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (err) {
    throw new ProtocolError(`invalid JSON: ${(err as Error).message}`);
  }
  return assertEnvelope(parsed);
}
