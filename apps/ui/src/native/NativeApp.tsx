import { useEffect, useMemo, useState } from 'react';

import { App, type SpeechConfig } from '../App';
import type { AppLockVerifier } from './appLock';
import type { Settings } from '../state/settings';
import { bridge } from './bridge';
import type { NativeServices } from './context';
import { hostBridge, isDesktopPlatform, type HostServices } from './host';
import { listenForHostChanges } from './hostEvents';
import { listenForPairLinks, PairLinks } from './pairLinks';
import { pushBridge } from './push';
import { qrScanner as defaultQrScanner, type QrScanner } from './qrScanner';
import { securityBridge } from './security';
import { useMachineRegistry, type MachineRegistry } from './registry';
import { nativeSpeech } from './speech';

/** The app inside the Tauri shell: machines come from the native registry. */
export function NativeApp({
  registry,
  speech = nativeSpeech,
  settings,
  pairLinks: links,
  qrScanner = defaultQrScanner,
  host: hostOverride,
  appLock = null,
}: {
  registry: MachineRegistry;
  /**
   * Pairing links, already fed by `listenForPairLinks` (main.tsx starts it
   * before React renders so a cold-start link is not missed). Without it,
   * NativeApp listens itself.
   */
  pairLinks?: PairLinks;
  speech?: SpeechConfig;
  settings?: Settings;
  qrScanner?: QrScanner;
  /** The local server services (tests); by default detected from the platform. */
  host?: HostServices | null;
  /** The app lock (main.tsx passes the device sign-in); absent: no lock. */
  appLock?: AppLockVerifier | null;
}) {
  const { loaded, machines } = useMachineRegistry(registry);
  const [pairLinks] = useState(() => links ?? new PairLinks());
  const ownsLinks = !links;
  useEffect(() => {
    if (!ownsLinks) return;
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
  }, [pairLinks, ownsLinks]);
  // Desktop shells run (or use) a VCA server on this PC.
  const [desktop, setDesktop] = useState(false);
  useEffect(() => {
    if (hostOverride !== undefined) return;
    let active = true;
    bridge
      .platform()
      .then((p) => {
        if (active) setDesktop(isDesktopPlatform(p));
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [hostOverride]);
  const host = hostOverride !== undefined ? hostOverride : desktop ? hostBridge : null;
  // This PC appears as a machine once its server is reachable.
  useEffect(() => {
    if (!desktop) return;
    return listenForHostChanges(() => void registry.refresh());
  }, [desktop, registry]);
  const native = useMemo<NativeServices>(
    () => ({
      pair: (uri, deviceName, totp) => registry.pair(uri, deviceName, totp),
      pairingCode: (uri) => bridge.pairingCode(uri),
      forget: (id) => registry.forget(id),
      platform: () => bridge.platform(),
      pairLinks,
      renameMachine: (id, alias) => registry.rename(id, alias),
      setDefaultMachine: (id) => registry.setDefault(id),
      moveMachine: (id, offset) => registry.move(id, offset),
      dnspilot: bridge.dnspilot,
      push: pushBridge,
      qrScanner,
      ...(host ? { host, security: securityBridge } : {}),
    }),
    [registry, pairLinks, qrScanner, host],
  );
  // Avoid flashing the pairing screen before the stored machines are known.
  if (!loaded) return null;
  return (
    <App
      machines={machines}
      initialMachineId={registry.defaultId()}
      speech={speech}
      settings={settings}
      native={native}
      appLock={appLock}
    />
  );
}
