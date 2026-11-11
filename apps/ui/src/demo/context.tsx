import { createContext, useContext, type ReactNode } from 'react';

/** Present while the app runs the demo (simulated computers, no network). */
export interface DemoControls {
  /** Leaves the demo and returns to the pairing screen. */
  exit(): void;
}

const DemoContext = createContext<DemoControls | null>(null);

export function DemoProvider({
  value,
  children,
}: {
  value: DemoControls | null;
  children: ReactNode;
}) {
  return <DemoContext.Provider value={value}>{children}</DemoContext.Provider>;
}

/** The demo's controls, or `null` outside the demo. */
// eslint-disable-next-line react-refresh/only-export-components
export function useDemo(): DemoControls | null {
  return useContext(DemoContext);
}
