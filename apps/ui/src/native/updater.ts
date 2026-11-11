/**
 * In-app updates of the desktop app from GitHub Releases (desktop shell
 * only; mirrors `apps/shell/src/updater.rs`). The shell checks the
 * signature of every download; nothing is installed until the person
 * clicks "Restart to update" and confirms. Installing first stops the VCA
 * server this app started, then reopens the app.
 *
 * The phone apps are updated by their stores and the browser mock has no
 * shell, so neither has these services.
 */
import { Channel } from '@tauri-apps/api/core';
import { useSyncExternalStore } from 'react';

import { call, toNativeError } from './bridge';

/** `auto`: the app installs it. `manual`: download it from the release page. */
export type InstallKind = 'auto' | 'manual';

export interface AvailableUpdate {
  version: string;
  notes: string | null;
  /** RFC 3339, when the release says. */
  date: string | null;
  install: InstallKind;
  /** The release page of this version. */
  release_url: string;
}

export interface UpdateCheck {
  current_version: string;
  /** `null`: this is the latest version. */
  available: AvailableUpdate | null;
}

export type DownloadEvent =
  { event: 'progress'; downloaded: number; total: number | null } | { event: 'finished' };

export interface UpdatePreflight {
  /** The VCA server runs and this app started it: installing stops it. */
  managed_server: boolean;
  /** Running sessions VCA started (they may end with the server). */
  managed_sessions: number;
}

export interface UpdaterServices {
  check(): Promise<UpdateCheck>;
  /** Downloads and verifies the update found by the last check. */
  download(onEvent: (event: DownloadEvent) => void): Promise<void>;
  preflight(): Promise<UpdatePreflight>;
  /** Stops the app's VCA server, installs and relaunches (never returns on success). */
  install(): Promise<void>;
}

export const updaterBridge: UpdaterServices = {
  check: () => call<UpdateCheck>('vca_update_check'),
  download: (onEvent) => {
    const channel = new Channel<DownloadEvent>();
    channel.onmessage = onEvent;
    return call<void>('vca_update_download', { onEvent: channel });
  },
  preflight: () => call<UpdatePreflight>('vca_update_preflight'),
  install: () => call<void>('vca_update_install'),
};

export type UpdateStatus =
  | { kind: 'idle' }
  | { kind: 'checking' }
  | { kind: 'latest' }
  | { kind: 'available'; update: AvailableUpdate }
  | { kind: 'downloading'; update: AvailableUpdate; downloaded: number; total: number | null }
  | { kind: 'ready'; update: AvailableUpdate }
  | { kind: 'installing'; update: AvailableUpdate }
  /** `update`: the update the failed step was for (a retry continues with it). */
  | {
      kind: 'error';
      step: UpdateStep;
      code: string | null;
      message: string;
      update: AvailableUpdate | null;
    };

export type UpdateStep = 'check' | 'download' | 'install';

export interface UpdateState {
  status: UpdateStatus;
  /** The installed version, once a check reported it. */
  currentVersion: string | null;
  /** "Check for updates automatically" (on by default). */
  autoCheck: boolean;
}

const AUTO_CHECK_KEY = 'vca.updates.autoCheck';
/** First automatic check after startup. */
export const FIRST_CHECK_MS = 30_000;
/** Automatic checks after that. */
export const CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000;

function loadAutoCheck(): boolean {
  try {
    return globalThis.localStorage?.getItem(AUTO_CHECK_KEY) !== 'false';
  } catch {
    return true;
  }
}

function saveAutoCheck(on: boolean) {
  try {
    globalThis.localStorage?.setItem(AUTO_CHECK_KEY, on ? 'true' : 'false');
  } catch {
    // Not persisted (private mode): the choice holds for this run.
  }
}

/** The update state shared by the settings panel and the background check. */
export class UpdateController {
  private state: UpdateState;
  private readonly listeners = new Set<() => void>();

  constructor(private readonly services: UpdaterServices) {
    this.state = { status: { kind: 'idle' }, currentVersion: null, autoCheck: loadAutoCheck() };
  }

  getState = (): UpdateState => this.state;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  private set(patch: Partial<UpdateState>) {
    this.state = { ...this.state, ...patch };
    for (const l of this.listeners) l();
  }

  private fail(step: UpdateStep, err: unknown, update: AvailableUpdate | null) {
    const e = toNativeError(err);
    this.set({ status: { kind: 'error', step, code: e.code, message: e.message, update } });
  }

  /** True while a step runs or an update is downloaded (a check would drop it). */
  get busy(): boolean {
    const k = this.state.status.kind;
    return k === 'checking' || k === 'downloading' || k === 'ready' || k === 'installing';
  }

  setAutoCheck(on: boolean) {
    saveAutoCheck(on);
    this.set({ autoCheck: on });
  }

  /** Checks for an update; resolves with it (`null`: none or failed). */
  async check(): Promise<AvailableUpdate | null> {
    if (this.busy) return null;
    this.set({ status: { kind: 'checking' } });
    try {
      const result = await this.services.check();
      this.set({
        currentVersion: result.current_version,
        status: result.available
          ? { kind: 'available', update: result.available }
          : { kind: 'latest' },
      });
      return result.available;
    } catch (err) {
      this.fail('check', err, null);
      return null;
    }
  }

  /** Downloads and verifies the update found (the person clicked "Download"). */
  async download(): Promise<void> {
    const s = this.state.status;
    const update =
      s.kind === 'available' || (s.kind === 'error' && s.step === 'download') ? s.update : null;
    if (!update || update.install !== 'auto') return;
    this.set({ status: { kind: 'downloading', update, downloaded: 0, total: null } });
    try {
      await this.services.download((event) => {
        const cur = this.state.status;
        if (cur.kind !== 'downloading') return;
        if (event.event === 'progress') {
          this.set({ status: { ...cur, downloaded: event.downloaded, total: event.total } });
        }
      });
      this.set({ status: { kind: 'ready', update } });
    } catch (err) {
      this.fail('download', err, update);
    }
  }

  preflight(): Promise<UpdatePreflight> {
    return this.services.preflight();
  }

  /** Installs the downloaded update (the person confirmed). */
  async install(): Promise<void> {
    const s = this.state.status;
    if (s.kind !== 'ready') return;
    this.set({ status: { kind: 'installing', update: s.update } });
    try {
      await this.services.install();
    } catch (err) {
      this.fail('install', err, s.update);
    }
  }

  /** After a failed install: the shell keeps the download, so it is ready again. */
  backToReady() {
    const s = this.state.status;
    if (s.kind === 'error' && s.step === 'install' && s.update) {
      this.set({ status: { kind: 'ready', update: s.update } });
    }
  }
}

const controllers = new WeakMap<UpdaterServices, UpdateController>();

/** The one controller of these services (settings and notifier share it). */
export function updateControllerFor(services: UpdaterServices): UpdateController {
  let c = controllers.get(services);
  if (!c) {
    c = new UpdateController(services);
    controllers.set(services, c);
  }
  return c;
}

export function useUpdateState(controller: UpdateController): UpdateState {
  return useSyncExternalStore(controller.subscribe, controller.getState);
}

/**
 * Automatic checks while "Check for updates automatically" is on: once
 * `FIRST_CHECK_MS` after startup, then every `CHECK_INTERVAL_MS`. Only
 * checks: `onFound` tells the person, who decides. Each version is
 * announced once per run. Returns the stop function.
 */
export function startAutoCheck(
  controller: UpdateController,
  onFound: (update: AvailableUpdate) => void,
  { firstMs = FIRST_CHECK_MS, intervalMs = CHECK_INTERVAL_MS } = {},
): () => void {
  let announced: string | null = null;
  const tick = () => {
    if (!controller.getState().autoCheck) return;
    void controller.check().then((update) => {
      if (update && update.version !== announced) {
        announced = update.version;
        onFound(update);
      }
    });
  };
  let interval: ReturnType<typeof setInterval> | null = null;
  const first = setTimeout(() => {
    tick();
    interval = setInterval(tick, intervalMs);
  }, firstMs);
  return () => {
    clearTimeout(first);
    if (interval !== null) clearInterval(interval);
  };
}
