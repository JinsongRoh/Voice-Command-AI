import { createContext, useContext, type ReactNode } from 'react';

import { createMediaRecorder, type RecorderFactory } from './recorder';
import { createMockTranscriber, type TranscribeFn } from './transcribe';

export interface SpeechServices {
  transcribe: TranscribeFn;
  createRecorder: RecorderFactory;
  /**
   * When the microphone cannot be opened, still call `transcribe` with an
   * empty clip (demo/mock mode only) so the flow can be exercised.
   */
  fallbackWithoutMic: boolean;
}

const SpeechContext = createContext<SpeechServices>({
  transcribe: createMockTranscriber(),
  createRecorder: createMediaRecorder,
  fallbackWithoutMic: false,
});

export function SpeechProvider({
  value,
  children,
}: {
  value: SpeechServices;
  children: ReactNode;
}) {
  return <SpeechContext.Provider value={value}>{children}</SpeechContext.Provider>;
}

// eslint-disable-next-line react-refresh/only-export-components
export function useSpeech(): SpeechServices {
  return useContext(SpeechContext);
}
