import { screen, render, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { loadDraft } from '../components/composerDrafts';
import { MachineRegistry } from '../native/registry';
import { NativeApp } from '../native/NativeApp';
import { MockTransport } from '../protocol/mockTransport';
import type { SpeechServices } from '../speech/context';
import { MIN_RECORDING_MS } from '../speech/dictation';
import { defaultSettings } from '../state/settings';
import { tauri } from '../test/tauriMock';
import {
  beginDemoStorage,
  createDemoMachines,
  createDemoRecorder,
  createDemoTranscriber,
  DEMO_SNAPSHOT_KEY,
  recoverDemoStorage,
} from './demoMode';

vi.mock('@tauri-apps/api/core', () => import('../test/tauriMock'));
vi.mock('@tauri-apps/plugin-deep-link', () => ({
  getCurrent: async () => null,
  onOpenUrl: async () => () => {},
}));

/** Every real transport built (the guard: none in the demo). */
const built = vi.hoisted(() => ({ tauri: 0 }));
vi.mock('../protocol/tauriTransport', async (importOriginal) => {
  const mod = await importOriginal<typeof import('../protocol/tauriTransport')>();
  class CountedTauriTransport extends mod.TauriTransport {
    constructor(...args: ConstructorParameters<typeof mod.TauriTransport>) {
      super(...args);
      built.tauri += 1;
    }
  }
  return { ...mod, TauriTransport: CountedTauriTransport };
});

/** The real app's speech: must never be used in the demo. */
const realSpeech: SpeechServices = {
  transcribe: vi.fn(async () => ({ text: 'REAL', corrections: [] })),
  createRecorder: vi.fn(() => ({
    start: async () => {},
    stop: async () => new Blob(),
    cancel: () => {},
  })),
  fallbackWithoutMic: false,
};

/** Native commands that may run while the demo is on screen (all local). */
const LOCAL_COMMANDS = new Set(['vca_machines', 'vca_platform', 'vca_launch_links']);

function renderNative() {
  return render(
    <NativeApp
      registry={new MachineRegistry()}
      speech={realSpeech}
      settings={{ ...defaultSettings(), language: 'en' }}
      host={null}
      demoTransport={{ latencyMs: 0, autoTick: false }}
    />,
  );
}

async function enterDemo() {
  const user = userEvent.setup();
  await user.click(await screen.findByRole('button', { name: 'Try the demo' }));
  await screen.findByTestId('session-card-wk-claude-api');
  return user;
}

describe('demo mode', () => {
  let fetchSpy: ReturnType<typeof vi.fn>;
  const realFetch = globalThis.fetch;
  const realWebSocket = globalThis.WebSocket;
  let sockets: number;

  beforeEach(() => {
    tauri.reset();
    tauri.handlers.set('vca_machines', () => []);
    tauri.handlers.set('vca_platform', () => 'ios');
    built.tauri = 0;
    localStorage.clear();
    fetchSpy = vi.fn(() => Promise.reject(new Error('network used in the demo')));
    globalThis.fetch = fetchSpy as unknown as typeof fetch;
    sockets = 0;
    globalThis.WebSocket = class {
      constructor() {
        sockets += 1;
        throw new Error('network used in the demo');
      }
    } as unknown as typeof WebSocket;
  });

  afterEach(() => {
    globalThis.fetch = realFetch;
    globalThis.WebSocket = realWebSocket;
    localStorage.clear();
  });

  /** Nothing went to the network, the shell's transport or the keychain. */
  function expectIsolated() {
    expect(fetchSpy).not.toHaveBeenCalled();
    expect(sockets).toBe(0);
    expect(built.tauri).toBe(0);
    expect(tauri.calls.map((c) => c.cmd).filter((cmd) => !LOCAL_COMMANDS.has(cmd))).toEqual([]);
  }

  it('offers the demo on the first-run pairing screen, above the pairing form', async () => {
    renderNative();
    const entry = await screen.findByTestId('demo-entry');
    expect(within(entry).getByRole('button', { name: 'Try the demo' })).toBeInTheDocument();
    const form = screen.getByRole('heading', { name: 'Pair a device' });
    expect(entry.compareDocumentPosition(form) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it('opens the sessions with sample machines and the Demo banner', async () => {
    renderNative();
    await enterDemo();
    const banner = screen.getByTestId('demo-banner');
    expect(banner).toHaveTextContent('Demo');
    expect(banner).toHaveTextContent('Sample data, not connected to a computer');
    expect(within(banner).getByRole('button', { name: 'Exit demo' })).toBeInTheDocument();
    // Several sessions wait for an answer.
    expect(screen.getAllByTestId(/^session-card-/).length).toBeGreaterThanOrEqual(5);
    // Plain machine names, no "(mock)".
    expect(document.body.textContent).not.toContain('(mock)');
    expect(screen.getByRole('option', { name: 'workstation' })).toBeInTheDocument();
    expect(screen.getByRole('option', { name: 'build-server' })).toBeInTheDocument();
    expectIsolated();
  });

  it('opens a session and answers its decision', async () => {
    renderNative();
    const user = await enterDemo();
    await user.click(screen.getByTestId('session-card-wk-claude-api'));
    // The banner stays on top inside a session too.
    expect(screen.getByTestId('demo-banner')).toBeInTheDocument();
    const sheet = await screen.findByRole('region', { name: 'Pending decision for payments-api' });
    await user.click(within(sheet).getByRole('button', { name: 'Yes' }));
    expect(await screen.findByText('Chose: Yes')).toBeInTheDocument();
    expectIsolated();
  });

  it('dictates a sample transcript without the microphone or the network', async () => {
    renderNative();
    const user = await enterDemo();
    await user.click(screen.getByTestId('session-card-wk-claude-api'));
    await user.click(await screen.findByRole('button', { name: /Tap to dictate/ }));
    const stop = await screen.findByRole('button', { name: /Dictating/ });
    // Longer than an accidental tap.
    await new Promise((resolve) => setTimeout(resolve, MIN_RECORDING_MS + 50));
    await user.click(stop);
    await waitFor(
      () =>
        expect(screen.getByRole('textbox', { name: 'Command for payments-api' })).toHaveValue(
          'Run the tests and tell me what failed',
        ),
      { timeout: 3000 },
    );
    expect(realSpeech.transcribe).not.toHaveBeenCalled();
    expect(realSpeech.createRecorder).not.toHaveBeenCalled();
    expectIsolated();
  });

  it('replaces pairing with a note while in the demo', async () => {
    renderNative();
    const user = await enterDemo();
    await user.selectOptions(
      screen.getByRole('combobox', { name: 'Machine' }),
      screen.getByRole('option', { name: 'Add computer…' }),
    );
    const note = await screen.findByTestId('demo-unavailable');
    expect(note).toHaveTextContent('Not available in the demo');
    expect(screen.queryByLabelText('Pairing link')).toBeNull();
    expectIsolated();
  });

  it('exits to the pairing screen and leaves nothing behind', async () => {
    localStorage.setItem('vca.keep', 'before');
    renderNative();
    const user = await enterDemo();
    await user.click(screen.getByTestId('session-card-wk-claude-api'));
    const box = await screen.findByRole('textbox', { name: 'Command for payments-api' });
    await user.type(box, 'draft in the demo');
    // Whatever the demo stores locally.
    localStorage.setItem('vca.keep', 'changed');
    localStorage.setItem('vca.demo-only', '1');
    await user.click(screen.getByRole('button', { name: 'Exit demo' }));
    expect(await screen.findByRole('heading', { name: 'Pair a device' })).toBeInTheDocument();
    expect(screen.queryByTestId('demo-banner')).toBeNull();
    expect(localStorage.getItem('vca.keep')).toBe('before');
    expect(localStorage.getItem('vca.demo-only')).toBeNull();
    expect(localStorage.getItem(DEMO_SNAPSHOT_KEY)).toBeNull();
    expect(loadDraft('wk-claude-api')).toBeFalsy();
    // A fresh start: back in, the draft is gone.
    await user.click(screen.getByRole('button', { name: 'Try the demo' }));
    await user.click(await screen.findByTestId('session-card-wk-claude-api'));
    expect(await screen.findByRole('textbox', { name: 'Command for payments-api' })).toHaveValue(
      '',
    );
    expectIsolated();
  });

  it('hands the app only simulated transports', async () => {
    renderNative();
    await enterDemo();
    // The two demo computers; no real transport was ever built.
    const select = screen.getByRole('combobox', { name: 'Machine' }) as HTMLSelectElement;
    const ids = [...select.options].map((o) => o.value).filter((v) => !v.startsWith('__'));
    expect(ids).toEqual(['workstation', 'build-server']);
    expectIsolated();
  });
});

describe('demo building blocks', () => {
  afterEach(() => localStorage.clear());

  it('builds simulated machines with plain names', () => {
    const machines = createDemoMachines({ latencyMs: 0, autoTick: false });
    expect(machines.map((m) => m.name)).toEqual(['workstation', 'build-server']);
    for (const m of machines) expect(m.transport).toBeInstanceOf(MockTransport);
  });

  it('answers with sample transcripts in the UI language', async () => {
    const ko = createDemoTranscriber(0);
    expect((await ko(new Blob(), { locale: 'ko', sessionId: null })).text).toBe(
      '테스트 돌리고 뭐가 실패했는지 알려줘',
    );
    const en = createDemoTranscriber(0);
    expect((await en(new Blob(), { locale: 'en', sessionId: null })).text).toBe(
      'Run the tests and tell me what failed',
    );
  });

  it('records without opening the microphone', async () => {
    const getUserMedia = vi.fn();
    Object.defineProperty(navigator, 'mediaDevices', {
      value: { getUserMedia },
      configurable: true,
    });
    const recorder = createDemoRecorder();
    await recorder.start();
    const clip = await recorder.stop();
    expect(clip.size).toBeGreaterThan(0);
    expect(getUserMedia).not.toHaveBeenCalled();
  });

  it('restores local storage after a demo the app was closed in', () => {
    localStorage.setItem('a', '1');
    beginDemoStorage();
    localStorage.setItem('a', '2');
    localStorage.setItem('b', '3');
    // The next start recovers it.
    expect(recoverDemoStorage()).toBe(true);
    expect(localStorage.getItem('a')).toBe('1');
    expect(localStorage.getItem('b')).toBeNull();
    expect(localStorage.getItem(DEMO_SNAPSHOT_KEY)).toBeNull();
    // Nothing to recover afterwards.
    expect(recoverDemoStorage()).toBe(false);
    expect(localStorage.getItem('a')).toBe('1');
  });
});
