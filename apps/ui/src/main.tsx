import './styles/fonts.css';
import './styles/theme.css';

import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './App';
import { isNative } from './native/bridge';
import { NativeApp } from './native/NativeApp';
import { MachineRegistry } from './native/registry';
import { MOCK_PROJECT_DIRS } from './protocol/mockData';
import { MockTransport } from './protocol/mockTransport';
import { createMediaRecorder } from './speech/recorder';
import { createMockTranscriber } from './speech/transcribe';
import type { Machine } from './state/store';

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
    simulate: { drop: (ms, lose) => transport.simulateDrop(ms, lose) },
  };
}

function mockApp() {
  const machines: Machine[] = [
    mockMachine('workstation', 'workstation (mock)', 'DNS Pilot', 'workstation'),
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
    />
  );
}

// Built once: machines and the registry must keep their identity across renders.
const app = isNative() ? <NativeApp registry={new MachineRegistry()} /> : mockApp();

const root = document.getElementById('root');
if (!root) throw new Error('#root not found');

createRoot(root).render(<StrictMode>{app}</StrictMode>);
