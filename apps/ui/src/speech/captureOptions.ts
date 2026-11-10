/**
 * Microphone options every recorder reads when it starts. The store sets
 * them from Settings, so all recorders (dictation, Voco wake listening,
 * hands-free, pronunciation) follow one switch without passing it around.
 */
export interface CaptureOptions {
  /** Reduce background noise (native: RNNoise; browser: the WebView's own). */
  noiseSuppression: boolean;
}

let current: CaptureOptions = { noiseSuppression: true };

export function captureOptions(): CaptureOptions {
  return current;
}

export function setCaptureOptions(next: CaptureOptions): void {
  current = { ...next };
}
