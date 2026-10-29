import { useEffect, useMemo, useState } from 'react';

import { App, type SpeechConfig } from '../App';
import type { Settings } from '../state/settings';
import { bridge } from './bridge';
import type { NativeServices } from './context';
import { listenForPairLinks, PairLinks } from './pairLinks';
import { useMachineRegistry, type MachineRegistry } from './registry';
import { nativeSpeech } from './speech';

/** The app inside the Tauri shell: machines come from the native registry. */
export function NativeApp({
  registry,
  speech = nativeSpeech,
  settings,
  pairLinks: links,
}: {
  registry: MachineRegistry;
  pairLinks?: PairLinks;
  speech?: SpeechConfig;
  settings?: Settings;
}) {
  const { loaded, machines } = useMachineRegistry(registry);
  const [pairLinks] = useState(() => links ?? new PairLinks());
  useEffect(() => {
    let stop: (() => void) | null = null;
    let active = true;
    void listenForPairLinks(pairLinks).then((unsubscribe) => {
      if (active) stop = unsubscribe;
      else unsubscribe();
    });
    return () => {
      active = false;
      stop?.();
    };
  }, [pairLinks]);
  const native = useMemo<NativeServices>(
    () => ({
      pair: (uri, deviceName, totp) => registry.pair(uri, deviceName, totp),
      forget: (id) => registry.forget(id),
      platform: () => bridge.platform(),
      pairLinks,
    }),
    [registry, pairLinks],
  );
  // Avoid flashing the pairing screen before the stored machines are known.
  if (!loaded) return null;
  return <App machines={machines} speech={speech} settings={settings} native={native} />;
}
