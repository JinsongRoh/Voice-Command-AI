/**
 * Known Whisper hallucinations, the client twin of the phrase pass in
 * `vca_speech::cleanup` (crates/vca-speech/src/cleanup.rs). On silence or
 * noise Whisper invents stock phrases from its YouTube training data:
 * "감사합니다.", "Thank you.", "ご視聴ありがとうございました". The phrases live in
 * `hallucinationPhrases.json`, shared with Rust and the reference STT server.
 *
 * A phrase is removed ONLY when the whole chunk is that phrase (ignoring case,
 * punctuation and spacing) and the audio is short or quiet. A phrase inside a
 * longer sentence is never touched. Without audio information nothing is
 * removed.
 *
 * The other invention is the prompt itself: on little or no speech a Whisper
 * engine can return the end of the context it was given, as if it had been
 * said. `isContextEcho` tells that from a real repeat by what the live
 * previews of the chunk heard (see there).
 */
import data from './hallucinationPhrases.json';

/** A plain-text chunk shorter than this is too short to hold the phrase... */
const SHORT_CHUNK_SECS = 1.2;
/** ...and one with a root-mean-square level (0..1) below this is quiet. */
const QUIET_RMS = 0.012;
/** Longest free part ("*") of a phrase, in normalized characters. */
const MAX_WILDCARD_CHARS = 30;
/** Only chunks up to this long can be dropped as a context echo. */
const ECHO_MAX_SECS = 5;
/** An echo has at least this many normalized characters: words do recur. */
const ECHO_MIN_CHARS = 4;
/** How far from the end of the context an echo may be found, at least. */
const ECHO_TAIL_CHARS = 60;
/** Normalized characters per second nobody speaks faster than. */
const MAX_SPOKEN_CHARS_PER_SEC = 20;

/** How long and how loud a chunk's audio is. */
export interface AudioLevel {
  durationSecs: number;
  /** Root-mean-square level of the samples, 0 to 1. */
  rms: number;
}

interface Phrase {
  prefix: string;
  suffix: string | null;
}

const normalize = (text: string): string =>
  Array.from(text.toLowerCase())
    .filter((c) => /[\p{L}\p{N}]/u.test(c))
    .join('');

const PHRASES: Phrase[] = Object.values(data.phrases)
  .flat()
  .map((entry) => {
    const star = entry.indexOf('*');
    return star < 0
      ? { prefix: normalize(entry), suffix: null }
      : { prefix: normalize(entry.slice(0, star)), suffix: normalize(entry.slice(star + 1)) };
  });

/** Whether the whole `text` is a known hallucination phrase. */
export function isHallucinationPhrase(text: string): boolean {
  const norm = normalize(text);
  if (norm === '') return false;
  return PHRASES.some((p) => {
    if (p.suffix === null) return norm === p.prefix;
    if (!norm.startsWith(p.prefix)) return false;
    const rest = norm.slice(p.prefix.length);
    if (!rest.endsWith(p.suffix)) return false;
    return Array.from(rest.slice(0, rest.length - p.suffix.length)).length <= MAX_WILDCARD_CHARS;
  });
}

/**
 * Empties `text` when the whole chunk is one known hallucination phrase and
 * its audio is short or quiet; otherwise returns it unchanged.
 */
export function dropLoneHallucination(text: string, audio: AudioLevel | null): string {
  if (!audio || !isHallucinationPhrase(text)) return text;
  return audio.durationSecs < SHORT_CHUNK_SECS || audio.rms < QUIET_RMS ? '' : text;
}

/** What the live previews of a chunk found. */
export interface ChunkPreview {
  /** Some preview of the chunk returned text. */
  heard: boolean;
  /** The longest stretch from the chunk's start that a preview answered for. */
  ms: number;
}

/** Whether `text`, or one piece of it said over and over, is in the tail of `context`. */
export function repeatsContextTail(text: string, context: string | null): boolean {
  const norm = normalize(text);
  const unit = norm.slice(0, (norm + norm).indexOf(norm, 1));
  if (Array.from(unit).length < ECHO_MIN_CHARS) return false;
  const tail = normalize(context ?? '').slice(-Math.max(ECHO_TAIL_CHARS, unit.length * 2));
  return tail.includes(unit);
}

/**
 * Whether a final chunk's `text`, already known to repeat the tail of the
 * context (`repeatsContextTail`), was not spoken. People do repeat themselves,
 * so the repeat alone proves nothing. It takes a short chunk that no preview
 * heard anything in, and audio that is quiet or too short for the text: the
 * part no preview answered for could not hold that many characters.
 */
export function isContextEcho(
  text: string,
  audio: AudioLevel | null,
  preview: ChunkPreview,
): boolean {
  if (!audio || preview.heard || audio.durationSecs > ECHO_MAX_SECS) return false;
  if (audio.rms < QUIET_RMS) return true;
  const unheardSecs = Math.max(0, audio.durationSecs - preview.ms / 1000);
  return Array.from(normalize(text)).length > unheardSecs * MAX_SPOKEN_CHARS_PER_SEC;
}

/**
 * Duration and level of a PCM16 WAV file, or null for any other format. Used
 * for the phone path, where the engine returns plain text only.
 */
export function wavLevel(bytes: Uint8Array): AudioLevel | null {
  if (bytes.length < 44) return null;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const tag = (at: number): string => String.fromCharCode(...bytes.subarray(at, at + 4));
  if (tag(0) !== 'RIFF' || tag(8) !== 'WAVE') return null;
  let channels = 0;
  let rate = 0;
  let bits = 0;
  let format = 0;
  let pos = 12;
  while (pos + 8 <= bytes.length) {
    const id = tag(pos);
    const size = view.getUint32(pos + 4, true);
    const body = pos + 8;
    if (id === 'fmt ' && body + 16 <= bytes.length) {
      format = view.getUint16(body, true);
      channels = view.getUint16(body + 2, true);
      rate = view.getUint32(body + 4, true);
      bits = view.getUint16(body + 14, true);
    } else if (id === 'data') {
      if (format !== 1 || bits !== 16 || channels < 1 || rate < 1) return null;
      const end = Math.min(bytes.length, body + size);
      const samples = Math.floor((end - body) / 2);
      if (samples < 1) return null;
      let sum = 0;
      for (let i = 0; i < samples; i += 1) {
        const v = view.getInt16(body + i * 2, true) / 32768;
        sum += v * v;
      }
      return { durationSecs: samples / channels / rate, rms: Math.sqrt(sum / samples) };
    }
    pos = body + size + (size % 2);
  }
  return null;
}
