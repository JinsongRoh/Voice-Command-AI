import './styles/fonts.css';
import './styles/theme.css';

import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './App';
import { diag, installLifecycleDiag, setDiagEnabled } from './diag/log';
import { APP_VERSION } from './state/core';
import { trackVisualViewport } from './hooks/visualViewport';
import { nativeAppLock, simulatedAppLock } from './native/appLock';
import { isNative } from './native/bridge';
import { NativeApp } from './native/NativeApp';
import { listenForPairLinks, PairLinks } from './native/pairLinks';
import { MachineRegistry } from './native/registry';
import { MOCK_PROJECT_DIRS } from './protocol/mockData';
import { mockRestoreLocal } from './protocol/mockRestore';
import { MockTransport } from './protocol/mockTransport';
import { createMediaRecorder } from './speech/recorder';
import { createMockTranscriber } from './speech/transcribe';
import { loadSettings } from './state/settings';
import type { Machine } from './state/store';

// Before anything can record: the person may have turned the log off.
setDiagEnabled(loadSettings().diagnostics);

// In the browser (dev, demos) the app runs against simulated servers; inside
// the Tauri shell machines and speech come from the native side.
function mockMachine(
  id: string,
  name: string,
  connectivity: string,
  profile: 'workstation' | 'build_server',
): Machine {
  const transport = new MockTransport({ profile });
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

function mockApp() {
  const machines: Machine[] = [
    mockMachine('workstation', 'workstation (mock)', 'DNSPilot', 'workstation'),
    mockMachine('build-server', 'build-server (mock)', 'Tailscale', 'build_server'),
  ];
  return (
    <App
      machines={machines}
      speech={{
        transcribe: createMockTranscriber(),
        createRecorder: createMediaRecorder,
        fallbackWithoutMic: true,
      }}
      // No device sign-in in a browser: the lock is off, or simulated with ?applock=1.
      appLock={simulatedAppLock(window.location.search)}
    />
  );
}

function nativeApp() {
  // Listen for `vca://pair` links before React renders: a link that launched
  // the app (cold start) may be reported before any component is mounted.
  // PairLinks buffers it until the pairing form takes it.
  const pairLinks = new PairLinks();
  void listenForPairLinks(pairLinks);
  // Locked at launch: Windows Hello / Touch ID on PCs, biometrics on phones.
  return (
    <NativeApp registry={new MachineRegistry()} pairLinks={pairLinks} appLock={nativeAppLock()} />
  );
}

// Built once: machines and the registry must keep their identity across renders.
const app = isNative() ? nativeApp() : mockApp();

// The shell follows the visual viewport (on-screen keyboard, iOS shifts).
trackVisualViewport();
diag('app.start', { version: APP_VERSION });
installLifecycleDiag();

const root = document.getElementById('root');
if (!root) throw new Error('#root not found');

createRoot(root).render(<StrictMode>{app}</StrictMode>);
