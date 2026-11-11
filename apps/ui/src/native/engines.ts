/**
 * Speech engine setup of this PC's own VCA server (desktop shell only):
 * speech-to-text engines, transcript-correction LLMs and TTS engines in its
 * `config.toml`. Mirrors `apps/shell/src/host/engines.rs` and
 * `crates/vca-server/src/engines.rs`.
 *
 * Keys are sent once (save or test) and stored by the server in its
 * credential store. The server never sends a key back, so the app can only
 * say whether one is stored. The same goes for the gateway token of an
 * engine behind a gateway or reverse proxy that authenticates with its own
 * bearer token.
 */
import { call } from './bridge';

export type EngineType = 'stt' | 'llm' | 'tts';

export const ENGINE_TYPES: readonly EngineType[] = ['stt', 'llm', 'tts'];

export type SpeakerGender = 'female' | 'male' | 'other';

export interface EngineSpeaker {
  id: string;
  label: string;
  gender: SpeakerGender;
}

/** One engine profile as the server stores it (never a key). */
export interface EngineForm {
  /** Profile id (the TTS engine name): a-z, 0-9, `-` and `_`. */
  id: string;
  /** Speech engines and LLMs only. */
  label: string | null;
  kind: string;
  base_url: string | null;
  model: string | null;
  /** TTS only. */
  voice: string | null;
  /** TTS only. */
  speakers: EngineSpeaker[];
  /** TTS only: mp3, opus, aac, flac or wav. */
  format: string | null;
  /** Name of the stored key; the id when null. */
  credential: string | null;
  /** Environment variable holding the key on the PC. */
  api_key_env: string | null;
  /** Google speech-to-text only. */
  project_id: string | null;
  location: string | null;
  /**
   * OpenAI-compatible engines only: the header carrying the key (sent raw)
   * instead of `Authorization: Bearer`, e.g. `X-VCA-Key`.
   */
  api_key_header: string | null;
  /** Environment variable holding the gateway token on the PC. */
  gateway_token_env: string | null;
  /**
   * The key may go over plain http to this private network address. Turning
   * it on asks for local verification.
   */
  allow_plain_http: boolean;
}

export type KeySource = 'stored' | 'env' | 'none';

export interface EngineEntry {
  profile: EngineForm;
  key: KeySource;
  /** Where the gateway token comes from (`none`: no gateway). */
  gateway?: KeySource;
  /** A key is available, or none is needed. */
  ready: boolean;
  default: boolean;
  /** From an older single `[speech]` setting: edit it in config.toml. */
  read_only: boolean;
}

export interface EngineSetup {
  config_path: string;
  stt: EngineEntry[];
  llm: EngineEntry[];
  tts: EngineEntry[];
  /** The file has engine changes the running server does not use yet. */
  restart_required: boolean;
}

export interface EngineTestResult {
  ok: boolean;
  latency_ms: number;
  code?: string;
  message: string;
  chars?: number;
  audio_bytes?: number;
}

/** What a save does with the stored gateway token. */
export interface GatewayChange {
  /** A new token to store (replacing the old one), or null. */
  token: string | null;
  /** Delete the stored token (ignored with `token`). */
  clear: boolean;
}

/** What to test: a draft form, or a saved profile. */
export type EngineTestTarget = { profile: EngineForm } | { profileId: string };

export interface EngineServices {
  get(): Promise<EngineSetup>;
  /** Adds (`originalId` null) or changes a profile; `secret` is stored, never echoed. */
  save(
    kind: EngineType,
    originalId: string | null,
    profile: EngineForm,
    secret: string | null,
    makeDefault: boolean,
    gateway?: GatewayChange,
  ): Promise<EngineSetup>;
  remove(kind: EngineType, id: string, deleteKey: boolean): Promise<EngineSetup>;
  /** `null`: the first engine, or no correction LLM. */
  setDefault(kind: EngineType, id: string | null): Promise<EngineSetup>;
  test(
    kind: EngineType,
    target: EngineTestTarget,
    secret: string | null,
    gatewayToken?: string | null,
  ): Promise<EngineTestResult>;
}

export const engineBridge: EngineServices = {
  get: () => call<EngineSetup>('vca_host_engines'),
  save: (kind, originalId, profile, secret, makeDefault, gateway) =>
    call<EngineSetup>('vca_host_engine_save', {
      kind,
      originalId,
      profile,
      secret,
      makeDefault,
      gatewayToken: gateway?.token ?? null,
      clearGatewayToken: gateway?.clear ?? false,
    }),
  remove: (kind, id, deleteKey) =>
    call<EngineSetup>('vca_host_engine_remove', { kind, profileId: id, deleteKey }),
  setDefault: (kind, id) =>
    call<EngineSetup>('vca_host_engine_set_default', { kind, profileId: id }),
  test: (kind, target, secret, gatewayToken) =>
    call<EngineTestResult>('vca_host_engine_test', {
      kind,
      profile: 'profile' in target ? target.profile : null,
      profileId: 'profileId' in target ? target.profileId : null,
      secret,
      gatewayToken: gatewayToken ?? null,
    }),
};

/** An empty form. */
export function emptyForm(): EngineForm {
  return {
    id: '',
    label: null,
    kind: 'openai_compatible',
    base_url: null,
    model: null,
    voice: null,
    speakers: [],
    format: null,
    credential: null,
    api_key_env: null,
    project_id: null,
    location: null,
    api_key_header: null,
    gateway_token_env: null,
    allow_plain_http: false,
  };
}

/** The API takes a key header and a gateway token (OpenAI-compatible only). */
export function supportsGateway(kind: EngineType, apiKind: string): boolean {
  return kind === 'stt' ? apiKind !== 'google' : apiKind === 'openai_compatible';
}

/** API kinds per engine type (the server rejects anything else). */
export const ENGINE_KINDS: Record<EngineType, readonly string[]> = {
  stt: ['openai_compatible', 'openai', 'google'],
  llm: ['openai_compatible', 'anthropic'],
  tts: ['openai_compatible'],
};

/** TTS audio formats. */
export const TTS_FORMATS = ['mp3', 'opus', 'aac', 'flac', 'wav'] as const;

/** Profile ids the server accepts. */
export function isValidProfileId(id: string): boolean {
  return /^[a-z0-9_-]{1,64}$/.test(id) && id !== 'none';
}

/** A profile id from a free label: lowercase, a-z0-9 and dashes. */
export function slugifyId(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 64);
}

/** Speakers as editable text, one per line: `id | label | gender`. */
export function speakersToText(speakers: EngineSpeaker[]): string {
  return speakers.map((s) => `${s.id} | ${s.label} | ${s.gender}`).join('\n');
}

/** Parses `id | label | gender` lines (label and gender optional). */
export function parseSpeakers(text: string): EngineSpeaker[] {
  return text
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
    .map((line) => {
      const [id = '', label, gender] = line.split('|').map((p) => p.trim());
      const g = (gender ?? '').toLowerCase();
      return {
        id,
        label: label || id,
        gender: g === 'female' || g === 'male' ? g : 'other',
      };
    });
}
