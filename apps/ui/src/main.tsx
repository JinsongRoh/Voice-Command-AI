import './styles/theme.css';

import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './App';
import { MOCK_PROJECT_DIRS } from './protocol/mockData';
import { MockTransport } from './protocol/mockTransport';
import { createMediaRecorder } from './speech/recorder';
import { createMockTranscriber } from './speech/transcribe';
import type { Machine } from './state/store';

// Until the Tauri bridge exists, the app runs against simulated servers.
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

const machines: Machine[] = [
  mockMachine('workstation', 'workstation (mock)', 'DNS Pilot', 'workstation'),
  mockMachine('build-server', 'build-server (mock)', 'Tailscale', 'build_server'),
];

const root = document.getElementById('root');
if (!root) throw new Error('#root not found');

createRoot(root).render(
  <StrictMode>
    <App
      machines={machines}
      speech={{
        transcribe: createMockTranscriber(),
        createRecorder: createMediaRecorder,
        fallbackWithoutMic: true,
      }}
    />
  </StrictMode>,
);
