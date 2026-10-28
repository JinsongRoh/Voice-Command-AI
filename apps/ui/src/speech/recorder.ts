/** Push-to-talk audio capture (VOC-04). */
export interface Recorder {
  start(): Promise<void>;
  /** Stops capturing, releases the microphone and returns the audio. */
  stop(): Promise<Blob>;
  /** Stops without producing audio. */
  cancel(): void;
}

export type RecorderFactory = () => Recorder;

/** MediaRecorder-based recorder. The microphone is released on stop/cancel. */
export function createMediaRecorder(): Recorder {
  let stream: MediaStream | null = null;
  let recorder: MediaRecorder | null = null;
  let chunks: Blob[] = [];

  const release = () => {
    stream?.getTracks().forEach((t) => t.stop());
    stream = null;
    recorder = null;
  };

  return {
    async start() {
      if (typeof navigator === 'undefined' || !navigator.mediaDevices?.getUserMedia) {
        throw new Error('audio capture is not supported here');
      }
      if (typeof MediaRecorder === 'undefined') throw new Error('MediaRecorder is not supported');
      stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      chunks = [];
      recorder = new MediaRecorder(stream);
      recorder.ondataavailable = (e) => {
        if (e.data.size > 0) chunks.push(e.data);
      };
      recorder.start();
    },
    stop() {
      return new Promise<Blob>((resolve) => {
        const rec = recorder;
        if (!rec || rec.state === 'inactive') {
          release();
          resolve(new Blob(chunks));
          return;
        }
        rec.onstop = () => {
          const blob = new Blob(chunks, { type: rec.mimeType });
          release();
          resolve(blob);
        };
        rec.stop();
      });
    },
    cancel() {
      if (recorder && recorder.state !== 'inactive') recorder.stop();
      release();
      chunks = [];
    },
  };
}
