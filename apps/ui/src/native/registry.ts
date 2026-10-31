/**
 * Paired machines known to the native shell. Loads `vca_machines`, keeps one
 * `TauriTransport` per machine (stable across refreshes) and refreshes after
 * pairing or forgetting a machine.
 */
import { useEffect, useSyncExternalStore } from 'react';

import { TauriTransport, type TauriTransportOptions } from '../protocol/tauriTransport';
import type { Machine } from '../state/store';
import { bridge, type MachineInfo } from './bridge';

export interface RegistrySnapshot {
  /** False until the first `vca_machines` call settled. */
  loaded: boolean;
  machines: Machine[];
  error: string | null;
}

function sameInfo(a: MachineInfo, b: MachineInfo): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/** Display string for the connectivity row (the first endpoint's host). */
export function describeEndpoints(endpoints: string[]): string {
  const first = endpoints[0];
  if (!first) return '-';
  try {
    return new URL(first).host || first;
  } catch {
    return first;
  }
}

export class MachineRegistry {
  private snapshot: RegistrySnapshot = { loaded: false, machines: [], error: null };
  private readonly listeners = new Set<() => void>();
  private readonly entries = new Map<string, { machine: Machine; transport: TauriTransport }>();
  private loading: Promise<void> | null = null;

  constructor(private readonly transportOptions: TauriTransportOptions = {}) {}

  getSnapshot = (): RegistrySnapshot => this.snapshot;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  /** Reloads the machine list; concurrent calls share one request. */
  refresh(): Promise<void> {
    if (!this.loading) {
      const run = bridge
        .machines()
        .then(
          (infos) => this.apply(infos, null),
          (err: Error) => this.update({ ...this.snapshot, loaded: true, error: err.message }),
        )
        .finally(() => {
          if (this.loading === run) this.loading = null;
        });
      this.loading = run;
    }
    return this.loading;
  }

  /** Pairs with a machine and resolves once it is in the list. */
  async pair(uri: string, deviceName: string, totp: string | null): Promise<MachineInfo> {
    const info = await bridge.pair(uri, deviceName, totp);
    await this.refresh();
    if (!this.entries.has(info.id)) this.apply([...this.infos(), info], null);
    return info;
  }

  async forget(machineId: string): Promise<void> {
    await bridge.forget(machineId);
    await this.refresh();
    if (this.entries.has(machineId)) {
      this.apply(
        this.infos().filter((i) => i.id !== machineId),
        null,
      );
    }
  }

  private infos(): MachineInfo[] {
    return this.snapshot.machines.flatMap((m) => (m.native ? [m.native.info] : []));
  }

  private apply(infos: MachineInfo[], error: string | null): void {
    const seen = new Set<string>();
    const machines = infos.map((info) => {
      seen.add(info.id);
      const existing = this.entries.get(info.id);
      if (existing && existing.machine.native && sameInfo(existing.machine.native.info, info)) {
        return existing.machine;
      }
      const transport = existing?.transport ?? new TauriTransport(info.id, this.transportOptions);
      const machine: Machine = {
        id: info.id,
        name: info.name,
        connectivity: describeEndpoints(info.endpoints),
        transport,
        native: {
          info,
          linkStatus: () => transport.status,
          onLinkStatus: (listener) => transport.onStatus(listener),
          login: (totp, backupCode) => bridge.login(info.id, totp, backupCode),
        },
      };
      this.entries.set(info.id, { machine, transport });
      return machine;
    });
    for (const [id, entry] of [...this.entries]) {
      if (seen.has(id)) continue;
      this.entries.delete(id);
      entry.transport.disconnect();
    }
    this.update({ loaded: true, machines, error });
  }

  private update(next: RegistrySnapshot): void {
    this.snapshot = next;
    for (const l of [...this.listeners]) l();
  }
}

/** Subscribes to a registry and triggers the first load. */
export function useMachineRegistry(registry: MachineRegistry): RegistrySnapshot {
  const snapshot = useSyncExternalStore(registry.subscribe, registry.getSnapshot);
  useEffect(() => {
    if (!registry.getSnapshot().loaded) void registry.refresh();
  }, [registry]);
  return snapshot;
}
