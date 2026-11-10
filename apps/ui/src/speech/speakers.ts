/**
 * Speaker choice of a PC TTS engine. An engine may list speakers (the owner's
 * `speakers` config); the user first picks a gender, then a voice of it.
 */
import type { TtsEngineInfo, TtsGender, TtsSpeaker } from '../protocol/types';

/** The genders a user can pick, in display order. */
export const PICKABLE_GENDERS: readonly TtsGender[] = ['female', 'male'];

export function speakersOf(engine: TtsEngineInfo | undefined): TtsSpeaker[] {
  return engine?.speakers ?? [];
}

/** The speaker used when nothing (valid) was chosen: the first female, else the first. */
export function defaultSpeaker(speakers: readonly TtsSpeaker[]): TtsSpeaker | null {
  return speakers.find((s) => s.gender === 'female') ?? speakers[0] ?? null;
}

/**
 * The speaker to use for a saved id: the saved one while the engine still
 * offers it, else the default. `null` when the engine lists no speakers.
 */
export function resolveSpeaker(
  engine: TtsEngineInfo | undefined,
  saved: string,
): TtsSpeaker | null {
  const speakers = speakersOf(engine);
  return speakers.find((s) => s.id === saved) ?? defaultSpeaker(speakers);
}

/** Speakers shown for a gender choice. `other` speakers only appear under `other`. */
export function speakersByGender(speakers: readonly TtsSpeaker[], gender: TtsGender): TtsSpeaker[] {
  return speakers.filter((s) => s.gender === gender);
}

/**
 * Genders offered for the first choice: female and male when they have
 * speakers, plus `other` when only such speakers exist or they are present.
 */
export function offeredGenders(speakers: readonly TtsSpeaker[]): TtsGender[] {
  const genders = (['female', 'male', 'other'] as const).filter((g) =>
    speakers.some((s) => s.gender === g),
  );
  return genders;
}
