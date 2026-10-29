import { Client } from '../protocol/transport';
import { createNativeRecorder } from '../speech/recorder';
import { createServerTranscriber } from '../speech/transcribe';
import type { SpeechServices } from '../speech/context';
import type { Machine } from '../state/store';

/**
 * Native speech services for a machine: the shell's microphone plus that
 * machine's server-side speech pipeline. Clients on one transport share the
 * request id sequence, so this does not collide with the store's requests.
 */
export function nativeSpeech(machine: Machine): SpeechServices {
  return {
    transcribe: createServerTranscriber(() => new Client(machine.transport)),
    createRecorder: createNativeRecorder,
    fallbackWithoutMic: false,
  };
}
