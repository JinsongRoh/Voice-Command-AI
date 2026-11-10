/**
 * Demo mode: the app against simulated computers, with no paired machine and
 * no network. Store reviewers and new users without a PC try it from the
 * pairing screen ("Try the demo").
 *
 * Isolation:
 * - Machines are `MockTransport`s (in memory); nothing goes to the native
 *   shell's transport, the keychain or the machine registry.
 * - Speech never leaves the device: the microphone is not opened and the
 *   transcriber answers with canned sample text.
 * - Local storage is snapshotted on entry and restored on exit, so nothing
 *   the demo changed (settings, pins, drafts, tasks) stays behind. The
 *   snapshot itself is kept until then: an app closed during the demo is
 *   restored on the next start (`recoverDemoStorage`), so the demo never
 *   survives a restart.
 */
import { MOCK_PROJECT_DIRS } from '../protocol/mockData';
import { mockRestoreLocal } from '../protocol/mockRestore';
import { MockTransport, type MockTransportOptions } from '../protocol/mockTransport';
import type { Recorder } from '../speech/recorder';
import type { SpeechServices } from '../speech/context';
import { stopVoice } from '../speech/handsFreeIO';
import { voiceQueue } from '../speech/voiceQueue';
import type { Transcript, TranscribeFn } from '../speech/transcribe';
import type { Locale } from '../i18n/i18n';
import type { Settings } from '../state/settings';
import type { Machine } from '../state/store';

type Profile = 'workstation' | 'build_server';

function simulatedMachine(
  id: string,
  name: string,
  connectivity: string,
  profile: Profile,
  options: MockTransportOptions,
): Machine {
  const transport = new MockTransport({ ...options, profile });
  return {
    id,
    name,
    connectivity,
    transport,
    projectDirs: MOCK_PROJECT_DIRS[profile],
    simulate: {
      drop: (ms, lose) => transport.simulateDrop(ms, lose),
      ...(profile === 'workstation' ? { restoreLocal: mockRestoreLocal() } : {}),
    },
  };
}

/** The simulated computers (browser build and demo mode). */
export function createDemoMachines(options: MockTransportOptions = {}): Machine[] {
  return [
    simulatedMachine('workstation', 'workstation', 'DNSPilot', 'workstation', options),
    simulatedMachine('build-server', 'build-server', 'Tailscale', 'build_server', options),
  ];
}

/** What the microphone "hears" in the demo, by UI language; the first one comes first. */
export const DEMO_TRANSCRIPTS: Record<Locale, readonly string[]> = {
  en: [
    'Run the tests and tell me what failed',
    'Summarize what changed in payments-api today',
    'Commit the changes and open a PR against main',
  ],
  ko: [
    '테스트 돌리고 뭐가 실패했는지 알려줘',
    '오늘 payments-api에서 바뀐 내용 요약해줘',
    '변경 사항 커밋하고 main 브랜치로 PR 올려줘',
  ],
  ja: [
    'テストを実行して、失敗したものを教えて',
    '今日の payments-api の変更点をまとめて',
    '変更をコミットして main に PR を作って',
  ],
  es: [
    'Ejecuta las pruebas y dime qué falló',
    'Resume lo que cambió hoy en payments-api',
    'Haz commit de los cambios y abre un PR contra main',
  ],
};

/** Canned transcripts in the UI language, in turn; nothing is sent anywhere. */
export function createDemoTranscriber(delayMs = 600): TranscribeFn {
  let i = 0;
  return (_audio, options) =>
    new Promise<Transcript>((resolve) => {
      const list = DEMO_TRANSCRIPTS[options.locale] ?? DEMO_TRANSCRIPTS.en;
      const text = list[i % list.length]!;
      i += 1;
      setTimeout(() => resolve({ text, corrections: [] }), delayMs);
    });
}

/**
 * A recorder that never opens the microphone (no permission prompt): it
 * pretends to listen and hands back a placeholder clip on stop.
 */
export function createDemoRecorder(): Recorder {
  let startedAt = 0;
  return {
    start: async () => {
      startedAt = Date.now();
    },
    stop: async () => new Blob(['demo'], { type: 'audio/webm' }),
    cancel: () => {},
    // A gentle, lively level so the mic button moves like it hears speech.
    level: () => 0.08 + 0.06 * Math.abs(Math.sin((Date.now() - startedAt) / 140)),
  };
}

export function demoSpeech(): SpeechServices {
  return {
    transcribe: createDemoTranscriber(),
    createRecorder: createDemoRecorder,
    fallbackWithoutMic: true,
  };
}

/**
 * Demo settings: the person's language and theme, but nothing that would
 * listen in the background or ask the computer for a voice.
 */
export function demoSettings(base: Settings): Settings {
  return { ...base, handsFree: false, ttsServerEngine: null, ttsServerVoice: '' };
}

/** Holds the local storage as it was before the demo (until exit or next start). */
export const DEMO_SNAPSHOT_KEY = 'vca.demo.snapshot';

function storage(): Storage | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

/** Saves the local storage before the demo starts changing it. */
export function beginDemoStorage(): void {
  const store = storage();
  if (!store || store.getItem(DEMO_SNAPSHOT_KEY) !== null) return;
  const saved: Record<string, string> = {};
  for (let i = 0; i < store.length; i += 1) {
    const key = store.key(i);
    if (key === null || key === DEMO_SNAPSHOT_KEY) continue;
    saved[key] = store.getItem(key) ?? '';
  }
  try {
    store.setItem(DEMO_SNAPSHOT_KEY, JSON.stringify(saved));
  } catch {
    // Full storage: the demo still runs, its changes may stay.
  }
}

/** Puts the local storage back as it was before the demo; true if there was a snapshot. */
export function recoverDemoStorage(): boolean {
  const store = storage();
  const raw = store?.getItem(DEMO_SNAPSHOT_KEY) ?? null;
  if (!store || raw === null) return false;
  let saved: Record<string, string> = {};
  try {
    const parsed: unknown = JSON.parse(raw);
    if (parsed && typeof parsed === 'object') saved = parsed as Record<string, string>;
  } catch {
    // A broken snapshot: clear what the demo wrote anyway.
  }
  const keys: string[] = [];
  for (let i = 0; i < store.length; i += 1) {
    const key = store.key(i);
    if (key !== null) keys.push(key);
  }
  for (const key of keys) if (!(key in saved)) store.removeItem(key);
  for (const [key, value] of Object.entries(saved)) {
    if (typeof value !== 'string') continue;
    if (store.getItem(key) !== value) store.setItem(key, value);
  }
  store.removeItem(DEMO_SNAPSHOT_KEY);
  return true;
}

/** A running demo: its machines and speech, created on entry. */
export interface DemoSession {
  machines: Machine[];
  speech: SpeechServices;
  settings: Settings;
}

/** Starts a demo from the person's current settings (`base`). */
export function startDemo(base: Settings, options: MockTransportOptions = {}): DemoSession {
  beginDemoStorage();
  return {
    machines: createDemoMachines(options),
    speech: demoSpeech(),
    settings: demoSettings(base),
  };
}

/** Ends a demo: closes the simulated machines and removes every trace. */
export function stopDemo(session: DemoSession, clearMemory: () => void = () => {}): void {
  for (const m of session.machines) m.transport.disconnect();
  // Nothing of the demo is said after it: the queue empties, both voices stop.
  voiceQueue.stopAll();
  stopVoice();
  clearMemory();
  recoverDemoStorage();
}
