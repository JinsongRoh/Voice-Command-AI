import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { diagEntries, clearDiag, setDiagEnabled } from '../diag/log';
import { defaultSettings, parseSettings, type Settings } from '../state/settings';
import { useSettings } from '../state/store';
import { renderWithApp } from '../test/render';
import { tauri } from '../test/tauriMock';
import { captureOptions, setCaptureOptions } from './captureOptions';
import { createMediaRecorder, createNativeRecorder } from './recorder';

vi.mock('@tauri-apps/api/core', () => import('../test/tauriMock'));

beforeEach(() => {
  tauri.reset();
  setCaptureOptions({ noiseSuppression: true });
  // The log is opt-in; these tests check what it records.
  setDiagEnabled(true);
  clearDiag();
});
afterEach(() => {
  vi.unstubAllGlobals();
  setCaptureOptions({ noiseSuppression: true });
});

describe('noise suppression setting', () => {
  it('is on by default and survives a reload', () => {
    const defaults = defaultSettings();
    expect(defaults.noiseSuppression).toBe(true);
    expect(parseSettings('{}', defaults).noiseSuppression).toBe(true);
    expect(parseSettings('{"noiseSuppression":false}', defaults).noiseSuppression).toBe(false);
    expect(parseSettings('{"noiseSuppression":"no"}', defaults).noiseSuppression).toBe(true);
  });

  it('reaches the native microphone start', async () => {
    tauri.handlers.set('vca_mic_start', () => undefined);
    tauri.handlers.set('vca_mic_cancel', () => undefined);
    const rec = createNativeRecorder();
    await rec.start();
    rec.cancel();
    setCaptureOptions({ noiseSuppression: false });
    const rec2 = createNativeRecorder();
    await rec2.start();
    rec2.cancel();
    expect(tauri.callsOf('vca_mic_start').map((c) => c.args)).toEqual([
      { noiseSuppression: true },
      { noiseSuppression: false },
    ]);
  });

  it('asks the browser for explicit constraints', async () => {
    const stop = vi.fn();
    const getUserMedia = vi.fn(async () => ({
      getTracks: () => [{ stop }],
      getAudioTracks: () => [{ getSettings: () => ({ noiseSuppression: false }) }],
    }));
    vi.stubGlobal('navigator', { ...navigator, mediaDevices: { getUserMedia } });
    vi.stubGlobal(
      'MediaRecorder',
      class {
        state = 'recording';
        ondataavailable: unknown = null;
        start() {}
        stop() {}
      },
    );
    setCaptureOptions({ noiseSuppression: false });
    const rec = createMediaRecorder();
    await rec.start();
    rec.cancel();
    expect(getUserMedia).toHaveBeenCalledWith({
      audio: {
        echoCancellation: true,
        noiseSuppression: false,
        autoGainControl: true,
        channelCount: 1,
      },
    });
  });

  it('follows the Settings switch through the store', async () => {
    function Toggle() {
      const { settings, updateSettings } = useSettings();
      return (
        <button
          type="button"
          onClick={() => updateSettings({ noiseSuppression: !settings.noiseSuppression })}
        >
          toggle
        </button>
      );
    }
    renderWithApp(<Toggle />, { settings: { noiseSuppression: true } as Partial<Settings> });
    await waitFor(() => expect(captureOptions().noiseSuppression).toBe(true));
    await userEvent.click(screen.getByRole('button', { name: 'toggle' }));
    await waitFor(() => expect(captureOptions().noiseSuppression).toBe(false));
  });

  it('logs the native capture counters after stop, with the device kind only', async () => {
    tauri.handlers.set('vca_mic_start', () => undefined);
    tauri.handlers.set('vca_mic_stop', () => [82, 73, 70, 70]);
    tauri.handlers.set('vca_mic_diag', () =>
      JSON.stringify({
        capture: {
          device: 'Bluetooth',
          config: '48000 Hz, 1 ch, F32',
          noiseSuppression: true,
          callbacks: 0,
          samples: 0,
          peakPpm: 0,
          outPeakPpm: 0,
          suppressMs: 0,
          streamErrors: 0,
          lastError: '',
          ageMs: 900,
        },
        note: 'session - | ns on | start ok',
      }),
    );
    const rec = createNativeRecorder();
    await rec.start();
    await rec.stop();
    await waitFor(() =>
      expect(diagEntries().find((e) => e.event === 'mic.native.stop')?.data).toMatchObject({
        device: 'Bluetooth',
        callbacks: 0,
        samples: 0,
        noiseSuppression: true,
      }),
    );
  });
});
