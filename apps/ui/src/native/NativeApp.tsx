import { useMemo } from 'react';

import { App, type SpeechConfig } from '../App';
import type { Settings } from '../state/settings';
import { bridge } from './bridge';
import type { NativeServices } from './context';
import { useMachineRegistry, type MachineRegistry } from './registry';
import { nativeSpeech } from './speech';

/** The app inside the Tauri shell: machines come from the native registry. */
export function NativeApp({
  registry,
  speech = nativeSpeech,
  settings,
}: {
  registry: MachineRegistry;
  speech?: SpeechConfig;
  settings?: Settings;
}) {
  const { loaded, machines } = useMachineRegistry(registry);
  const native = useMemo<NativeServices>(
    () => ({
      pair: (uri, deviceName, totp) => registry.pair(uri, deviceName, totp),
      forget: (id) => registry.forget(id),
      platform: () => bridge.platform(),
    }),
    [registry],
  );
  // Avoid flashing the pairing screen before the stored machines are known.
  if (!loaded) return null;
  return <App machines={machines} speech={speech} settings={settings} native={native} />;
}
