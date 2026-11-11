/**
 * Starting points for a new engine profile. A preset only fills in the form
 * (every field stays editable) and never contains a key: keys are typed by
 * the owner and stored on the PC.
 */
import { emptyForm, type EngineForm, type EngineType } from '../native/engines';

export interface EnginePreset {
  /** Stable id, also the i18n suffix (`engines.preset.<id>`). */
  id: string;
  /** Needs a key from the provider. */
  needsKey: boolean;
  form: EngineForm;
}

function preset(
  id: string,
  needsKey: boolean,
  form: Partial<EngineForm> & Pick<EngineForm, 'id'>,
): EnginePreset {
  return { id, needsKey, form: { ...emptyForm(), ...form } };
}

/**
 * Local servers listen on this PC (Ollama, LM Studio, vLLM, LocalAI,
 * faster-whisper-server / Speaches, openedai-speech, the reference TTS server
 * in `tools/tts-server`). Change the port to the one your server uses.
 */
export const ENGINE_PRESETS: Record<EngineType, readonly EnginePreset[]> = {
  stt: [
    preset('local', false, {
      id: 'local-stt',
      label: 'Speech server (this PC)',
      kind: 'openai_compatible',
      base_url: 'http://127.0.0.1:8000/v1',
      model: 'Systran/faster-whisper-small',
    }),
    preset('openai', true, {
      id: 'openai-stt',
      label: 'OpenAI',
      kind: 'openai',
      base_url: 'https://api.openai.com/v1',
      model: 'gpt-4o-transcribe',
    }),
    preset('google', true, {
      id: 'google-stt',
      label: 'Google Cloud',
      kind: 'google',
      model: 'long',
      location: 'global',
    }),
  ],
  llm: [
    preset('local', false, {
      id: 'local-llm',
      label: 'Local LLM (this PC)',
      kind: 'openai_compatible',
      base_url: 'http://127.0.0.1:11434/v1',
      model: 'qwen2.5:1.5b',
    }),
    preset('openai', true, {
      id: 'openai-llm',
      label: 'OpenAI',
      kind: 'openai_compatible',
      base_url: 'https://api.openai.com/v1',
      model: 'gpt-4o-mini',
    }),
    preset('anthropic', true, {
      id: 'claude',
      label: 'Anthropic',
      kind: 'anthropic',
    }),
  ],
  tts: [
    preset('local', false, {
      id: 'local-tts',
      kind: 'openai_compatible',
      base_url: 'http://127.0.0.1:8880/v1',
      model: 'tts-1',
      // WAV needs no encoder on the server (the reference server makes MP3
      // only with ffmpeg installed).
      format: 'wav',
    }),
    preset('openai', true, {
      id: 'openai-tts',
      kind: 'openai_compatible',
      base_url: 'https://api.openai.com/v1',
      model: 'gpt-4o-mini-tts',
      voice: 'alloy',
      format: 'mp3',
    }),
  ],
};

/** A copy of a preset's form, so edits never change the preset. */
export function presetForm(kind: EngineType, presetId: string): EngineForm | null {
  const found = ENGINE_PRESETS[kind].find((p) => p.id === presetId);
  return found ? { ...found.form, speakers: [...found.form.speakers] } : null;
}
