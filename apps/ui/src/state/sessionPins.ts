/**
 * Sessions pinned in the quick switcher. Per device: kept in localStorage
 * (never sent to the PC), so each phone or desktop has its own short list.
 * Storage may be unavailable (private mode, full): pins then last until the
 * app closes.
 */
import { useSyncExternalStore } from 'react';

import type { SessionId } from '../protocol/types';

export const PINS_KEY = 'vca.switcher.pins';
/** More pins than this and the oldest one goes. */
const MAX_PINS = 50;

let pins: SessionId[] | null = null;
const listeners = new Set<() => void>();

function read(): SessionId[] {
  try {
    const raw = localStorage.getItem(PINS_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(parsed) ? parsed.filter((v): v is string => typeof v === 'string') : [];
  } catch {
    return [];
  }
}

export function loadPins(): SessionId[] {
  if (pins === null) pins = read();
  return pins;
}

function store(next: SessionId[]) {
  pins = next;
  try {
    localStorage.setItem(PINS_KEY, JSON.stringify(next));
  } catch {
    // Not remembered after a restart; still pinned for now.
  }
  for (const l of listeners) l();
}

export function isPinned(id: SessionId): boolean {
  return loadPins().includes(id);
}

/** Pins or unpins `id`; returns whether it is pinned now. */
export function togglePin(id: SessionId): boolean {
  const current = loadPins();
  if (current.includes(id)) {
    store(current.filter((x) => x !== id));
    return false;
  }
  store([...current, id].slice(-MAX_PINS));
  return true;
}

/** Forgets the cached pins so the next read comes from storage (tests). */
export function resetPinsCache(): void {
  pins = null;
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Pinned session ids on this device (re-renders on changes). */
export function usePins(): SessionId[] {
  return useSyncExternalStore(subscribe, loadPins, loadPins);
}
