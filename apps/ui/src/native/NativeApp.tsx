import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { App, type SpeechConfig } from '../App';
import { clearAllDrafts } from '../components/composerDrafts';
import type { DemoControls } from '../demo/context';
import { startDemo, stopDemo, type DemoSession } from '../demo/demoMode';
import type { MockTransportOptions } from '../protocol/mockTransport';
import { loadSettings, type Settings } from '../state/settings';
import type { AppLockVerifier } from './appLock';
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
  demoTransport,
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
  /** Options of the demo's simulated machines (tests: no latency or ticks). */
  demoTransport?: MockTransportOptions;
}) {
  const { loaded, machines } = useMachineRegistry(registry);
  // Demo mode ("Try the demo" on the pairing screen): simulated machines
  // only. The registry's machines are not handed to the app meanwhile, so no
  // real transport connects, and none of the native services are offered.
  // Kept in memory only: a restart opens the pairing screen again.
  const [demo, setDemo] = useState<DemoSession | null>(null);
  const demoRef = useRef<DemoSession | null>(null);
  const enterDemo = useCallback(() => {
    if (demoRef.current) return;
    const session = startDemo(settings ?? loadSettings(), demoTransport);
    demoRef.current = session;
    setDemo(session);
  }, [settings, demoTransport]);
  const demoControls = useMemo<DemoControls>(
    () => ({
      exit: () => {
        const session = demoRef.current;
        if (!session) return;
        demoRef.current = null;
        setDemo(null);
        stopDemo(session, clearAllDrafts);
        // A server started on this PC meanwhile shows up now.
        void registry.refresh();
      },
    }),
    [registry],
  );
  // Ending the app inside the demo (unmount) leaves nothing behind either.
  useEffect(
    () => () => {
      const session = demoRef.current;
      demoRef.current = null;
      if (session) stopDemo(session, clearAllDrafts);
    },
    [],
  );
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
    // Nothing real is loaded during the demo.
    if (!desktop || demo) return;
    return listenForHostChanges(() => void registry.refresh());
  }, [desktop, registry, demo]);
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
  if (demo) {
    return (
      <App
        key="demo"
        machines={demo.machines}
        speech={demo.speech}
        settings={demo.settings}
        demo={demoControls}
      />
    );
  }
  return (
    <App
      machines={machines}
      initialMachineId={registry.defaultId()}
      speech={speech}
      settings={settings}
      native={native}
      appLock={appLock}
      // Only while nothing is paired: the first-run screen.
      onTryDemo={machines.length === 0 ? enterDemo : undefined}
    />
  );
}
