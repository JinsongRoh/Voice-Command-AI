import { createContext, useContext, type ReactNode } from 'react';

import type { DnsPilotService, MachineInfo } from './bridge';
import type { HostServices } from './host';
import type { PairLinks } from './pairLinks';
import type { PushService } from './push';
import type { QrScanner } from './qrScanner';
import type { SecurityServices } from './security';

/** Native-only services (pairing, forgetting machines). Absent in browser/mock mode. */
export interface NativeServices {
  pair(uri: string, deviceName: string, totp: string | null): Promise<MachineInfo>;
  /** The code to compare with the PC before pairing with `uri`. */
  pairingCode(uri: string): Promise<string>;
  forget(machineId: string): Promise<void>;
  /** 'windows' | 'macos' | 'linux' | 'android' | 'ios' */
  platform(): Promise<string>;
  /** `vca://pair` links that opened the app (pre-fill the pairing form). */
  pairLinks?: PairLinks;
  // Local machine management (MachinesPanel); absent on older shells.
  /** Sets the local alias (`null` clears it). */
  renameMachine?(machineId: string, alias: string | null): Promise<void>;
  /** Opens this machine on launch (`null` clears the choice). */
  setDefaultMachine?(machineId: string | null): Promise<void>;
  /** Moves a machine `offset` places (negative: up). */
  moveMachine?(machineId: string, offset: number): Promise<void>;
  /** "Connect DNSPilot" setup (keys and tokens stay native). */
  dnspilot?: DnsPilotService;
  /**
   * Camera QR scanning. Only used where `canScanQr(platform)` holds (the
   * phone apps); desktops paste the link.
   */
  qrScanner?: QrScanner;
  /** The VCA server on this PC: desktop only ("Connect a phone"). */
  host?: HostServices;
  /** Local verification, two-step verification and mail of this PC's server (desktop only). */
  security?: SecurityServices;
  /** Approval alerts through DNSPilot push (desktop settings, phone opt-in). */
  push?: PushService;
}

const NativeContext = createContext<NativeServices | null>(null);

export function NativeProvider({
  value,
  children,
}: {
  value: NativeServices | null;
  children: ReactNode;
}) {
  return <NativeContext.Provider value={value}>{children}</NativeContext.Provider>;
}

// eslint-disable-next-line react-refresh/only-export-components
export function useNative(): NativeServices | null {
  return useContext(NativeContext);
}
