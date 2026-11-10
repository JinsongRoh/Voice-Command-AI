/**
 * Short, quiet earcons of the Voco flow, synthesized with Web Audio (no
 * files): a rising two-note chime when the wake word is heard, a soft tick
 * per second of a scheduled-send countdown, a falling chime on cancel, a
 * bright one once a message was sent and one low note when a call-like word
 * was not the wake word. Silent where Web Audio is missing.
 */
import type { VocoChime } from './vocoFlow';

const NOTES: Record<VocoChime, { freq: number; at: number; dur: number; gain: number }[]> = {
  wake: [
    { freq: 880, at: 0, dur: 0.09, gain: 0.08 },
    { freq: 1320, at: 0.09, dur: 0.12, gain: 0.08 },
  ],
  tick: [{ freq: 1500, at: 0, dur: 0.03, gain: 0.04 }],
  sent: [
    { freq: 1047, at: 0, dur: 0.08, gain: 0.07 },
    { freq: 1568, at: 0.08, dur: 0.14, gain: 0.07 },
  ],
  cancel: [
    { freq: 660, at: 0, dur: 0.09, gain: 0.07 },
    { freq: 440, at: 0.09, dur: 0.14, gain: 0.07 },
  ],
  // One soft low note: "didn't catch the wake word".
  missed: [{ freq: 392, at: 0, dur: 0.14, gain: 0.05 }],
};

let context: AudioContext | null = null;

function audioContext(): AudioContext | null {
  if (context) return context;
  const Ctor =
    typeof window === 'undefined'
      ? undefined
      : (window.AudioContext ??
        (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext);
  if (!Ctor) return null;
  try {
    context = new Ctor();
  } catch {
    return null;
  }
  return context;
}

export function playChime(kind: VocoChime): void {
  const ctx = audioContext();
  if (!ctx) return;
  try {
    if (ctx.state === 'suspended') void ctx.resume().catch(() => {});
    const t0 = ctx.currentTime + 0.01;
    for (const n of NOTES[kind]) {
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = 'sine';
      osc.frequency.value = n.freq;
      gain.gain.setValueAtTime(0, t0 + n.at);
      gain.gain.linearRampToValueAtTime(n.gain, t0 + n.at + 0.01);
      gain.gain.exponentialRampToValueAtTime(0.0001, t0 + n.at + n.dur);
      osc.connect(gain).connect(ctx.destination);
      osc.start(t0 + n.at);
      osc.stop(t0 + n.at + n.dur + 0.02);
    }
  } catch {
    // Earcons are decoration; the screen shows the same state.
  }
}
