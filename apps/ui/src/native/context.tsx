import { createContext, useContext, type ReactNode } from 'react';

import type { MachineInfo } from './bridge';
import type { PairLinks } from './pairLinks';

/** Native-only services (pairing, forgetting machines). Absent in browser/mock mode. */
export interface NativeServices {
  pair(uri: string, deviceName: string, totp: string | null): Promise<MachineInfo>;
  forget(machineId: string): Promise<void>;
  /** 'windows' | 'macos' | 'linux' | 'android' | 'ios' */
  platform(): Promise<string>;
  /** `vca://pair` links that opened the app (pre-fill the pairing form). */
  pairLinks?: PairLinks;
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
