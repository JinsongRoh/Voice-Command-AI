/**
 * Application store: owns the connection to the selected machine, feeds
 * server messages into the reducer, raises alerts and exposes actions.
 *
 * State lives in a tiny external store (read with `useSyncExternalStore`) so
 * that transport callbacks always see the latest state synchronously.
 */
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from 'react';

import { translate } from '../i18n/i18n';
import type { LinkStatus, MachineInfo } from '../native/bridge';
import type { ClientMsg, CloudLaunchRequest, ServerMsg } from '../protocol/types';
import { Client, type Transport } from '../protocol/transport';
import {
  PROTOCOL_VERSION,
  type ControlKey,
  type LaunchRequest,
  type SessionAction,
  type SessionId,
  type SessionInfo,
} from '../protocol/types';
import { speak } from '../speech/tts';
import { alertsFor } from './alerts';
import type { AppState, DeckFilter } from './reducer';
import { APP_VERSION, createStore, RequestError, type Store } from './core';
import { loadSettings, saveSettings, shouldSpeak, shouldToast, type Settings } from './settings';

export { APP_VERSION };

export interface Machine {
  id: string;
  name: string;
  /** Connectivity provider used to reach it (display only). */
  connectivity: string;
  transport: Transport;
  /** Folder suggestions for the new-session dialog. */
  projectDirs?: string[];
  /** Present on simulated machines only. */
  simulate?: { drop(durationMs: number, loseHistory: boolean): void };
  /** Present on machines reached through the native shell (Tauri). */
  native?: NativeMachine;
}

/** Native link details of a paired machine. */
export interface NativeMachine {
  info: MachineInfo;
  /** Last native link status (`login_required` is not a ConnectionState). */
  linkStatus(): LinkStatus | null;
  onLinkStatus(listener: (status: LinkStatus) => void): () => void;
  /** Re-authenticates the device with a TOTP or backup code. */
  login(totp: string | null, backupCode: string | null): Promise<void>;
}

export interface Actions {
  select(id: SessionId | null): void;
  setFilter(filter: DeckFilter): void;
  sendText(sessionId: SessionId, text: string): Promise<void>;
  sendKey(sessionId: SessionId, key: ControlKey): Promise<void>;
  choose(sessionId: SessionId, decisionId: string, optionId: string): Promise<void>;
  control(sessionId: SessionId, action: SessionAction): Promise<void>;
  launch(request: LaunchRequest): Promise<SessionInfo>;
  launchCloud(request: CloudLaunchRequest): Promise<SessionInfo>;
  refreshResources(): Promise<void>;
  reorderQueue(jobId: string, position: number): Promise<void>;
  cancelQueued(jobId: string): Promise<void>;
  refreshDevices(): Promise<void>;
  revokeDevice(deviceId: string): Promise<void>;
  killSwitch(): Promise<void>;
  reconnect(): Promise<void>;
  dismissToast(id: string): void;
}

interface StoreContextValue {
  store: Store;
  actions: Actions;
  machines: Machine[];
  machine: Machine;
  switchMachine(id: string): void;
  settings: Settings;
  updateSettings(patch: Partial<Settings>): void;
}

const StoreContext = createContext<StoreContextValue | null>(null);

export interface AppProviderProps {
  machines: Machine[];
  initialMachineId?: string;
  /** Initial settings; defaults to the persisted ones. */
  settings?: Settings;
  /** Persist settings changes (default true). */
  persistSettings?: boolean;
  children: ReactNode;
}

export function AppProvider({
  machines,
  initialMachineId,
  settings: initialSettings,
  persistSettings = true,
  children,
}: AppProviderProps) {
  const [store] = useState(() => createStore());
  const [machineId, setMachineId] = useState(initialMachineId ?? machines[0]?.id);
  const machine = machines.find((m) => m.id === machineId) ?? machines[0];
  if (!machine) throw new Error('AppProvider needs at least one machine');
  const [settings, setSettings] = useState<Settings>(() => initialSettings ?? loadSettings());
  const settingsRef = useRef(settings);
  const clientRef = useRef<Client>(new Client(machine.transport));
  const syncRef = useRef<() => Promise<void>>(async () => {});

  useEffect(() => {
    settingsRef.current = settings;
    if (persistSettings) saveSettings(settings);
  }, [settings, persistSettings]);

  const request = useCallback(async (body: ClientMsg): Promise<ServerMsg> => {
    let response: ServerMsg;
    try {
      response = await clientRef.current.request(body);
    } catch (err) {
      throw new RequestError('transport', (err as Error).message);
    }
    if (response.type === 'error') throw new RequestError(response.code, response.message);
    return response;
  }, []);

  // Connection lifecycle for the selected machine.
  useEffect(() => {
    const transport = machine.transport;
    const client = new Client(transport);
    clientRef.current = client;
    store.dispatch({ type: 'reset' });
    let active = true;
    /** The one session whose event stream is subscribed: the selected one. */
    let subscribed: SessionId | null = null;
    let synced = false;

    // Cards use `SessionInfo.preview`, so only the selected session is streamed.
    const followSelection = () => {
      if (!synced || transport.state !== 'connected') return;
      const selected = store.getState().selectedId;
      if (selected === subscribed) return;
      if (subscribed) {
        client.request({ type: 'unsubscribe', session_id: subscribed }).catch(() => {});
      }
      subscribed = selected;
      if (!selected) return;
      // Resume from the last seen seq (CON-04); the server replays or sends stream_gap.
      const fromSeq = store.getState().views[selected]?.lastSeq ?? null;
      client.request({ type: 'subscribe', session_id: selected, from_seq: fromSeq }).catch(() => {
        if (subscribed === selected) subscribed = null;
      });
    };
    const offStore = store.subscribe(followSelection);

    const sync = async () => {
      synced = false;
      subscribed = null;
      const welcome = await client.request({
        type: 'hello',
        protocol_version: PROTOCOL_VERSION,
        app_version: APP_VERSION,
      });
      if (!active) return;
      store.dispatch({ type: 'server', msg: welcome });
      const list = await client.request({ type: 'list_sessions' });
      if (!active) return;
      store.dispatch({ type: 'server', msg: list });
      synced = true;
      followSelection();
      const resources = await client.request({ type: 'get_resources' });
      if (active && resources.type === 'resources')
        store.dispatch({ type: 'server', msg: resources });
      // Device management may be disabled on the server; ignore failures.
      const devices = await client.request({ type: 'list_devices' }).catch(() => null);
      if (active && devices?.type === 'devices') store.dispatch({ type: 'server', msg: devices });
    };
    const safeSync = () =>
      sync().catch((err: Error) => {
        if (active) store.dispatch({ type: 'error', message: err.message });
      });
    syncRef.current = async () => {
      await transport.connect();
      await safeSync();
    };

    const offMessage = transport.onMessage(({ body: msg }) => {
      const before = store.getState();
      const toasts = alertsFor(before, msg, (k, v) =>
        translate(settingsRef.current.language, k, v),
      );
      store.dispatch({ type: 'server', msg });
      const now = new Date();
      for (const toast of toasts) {
        if (shouldToast(settingsRef.current, toast.category))
          store.dispatch({ type: 'toast', toast });
        if (shouldSpeak(settingsRef.current, toast.category, now)) {
          speak(toast.title, settingsRef.current.language);
        }
      }
      if (msg.type === 'session_updated' || msg.type === 'launched') {
        if (msg.session.id === before.selectedId && msg.session.unread > 0) {
          client.request({ type: 'mark_read', session_id: msg.session.id }).catch(() => {});
        }
      }
    });

    let previous = transport.state;
    const offState = transport.onStateChange((state) => {
      store.dispatch({ type: 'connection', state });
      // CON-04: after a reconnect, resubscribe from the last seen seq.
      if (state === 'connected' && previous === 'reconnecting') void safeSync();
      previous = state;
    });
    store.dispatch({ type: 'connection', state: transport.state });
    transport
      .connect()
      .then(safeSync, (err: Error) => store.dispatch({ type: 'error', message: err.message }));

    return () => {
      active = false;
      offStore();
      offMessage();
      offState();
      transport.disconnect();
    };
  }, [machine, store]);

  const actions = useMemo<Actions>(() => {
    const withAck = async (sessionId: SessionId, body: ClientMsg) => {
      store.dispatch({
        type: 'delivery',
        sessionId,
        delivery: { status: 'sending', at: Date.now() },
      });
      try {
        const res = await request(body);
        store.dispatch({ type: 'server', msg: res });
      } catch (err) {
        store.dispatch({
          type: 'delivery',
          sessionId,
          delivery: { status: 'failed', at: Date.now(), error: (err as Error).message },
        });
        throw err;
      }
    };
    return {
      select(id) {
        store.dispatch({ type: 'select', id });
        const session = id ? store.getState().sessions[id] : undefined;
        if (session && session.unread > 0) {
          request({ type: 'mark_read', session_id: session.id }).catch(() => {});
        }
      },
      setFilter(filter) {
        store.dispatch({ type: 'filter', filter });
      },
      sendText: (sessionId, text) =>
        withAck(sessionId, {
          type: 'send_input',
          session_id: sessionId,
          input: { type: 'text', text, submit: true },
        }),
      sendKey: (sessionId, key) =>
        withAck(sessionId, {
          type: 'send_input',
          session_id: sessionId,
          input: { type: 'key', key },
        }),
      async choose(sessionId, decisionId, optionId) {
        await withAck(sessionId, {
          type: 'send_input',
          session_id: sessionId,
          input: { type: 'choose', decision_id: decisionId, option_id: optionId },
        });
        store.dispatch({ type: 'decision_answered', sessionId });
      },
      async control(sessionId, action) {
        await request({ type: 'control', session_id: sessionId, action });
      },
      async launch(req) {
        const res = await request({ type: 'launch', request: req });
        if (res.type !== 'launched') throw new RequestError('internal', `unexpected ${res.type}`);
        store.dispatch({ type: 'server', msg: res });
        return res.session;
      },
      async launchCloud(req) {
        const res = await request({ type: 'launch_cloud', request: req });
        if (res.type !== 'launched') throw new RequestError('internal', `unexpected ${res.type}`);
        store.dispatch({ type: 'server', msg: res });
        return res.session;
      },
      async refreshResources() {
        store.dispatch({ type: 'server', msg: await request({ type: 'get_resources' }) });
      },
      async reorderQueue(jobId, position) {
        await request({ type: 'reorder_queue', job_id: jobId, position });
      },
      async cancelQueued(jobId) {
        await request({ type: 'cancel_queued', job_id: jobId });
      },
      async refreshDevices() {
        store.dispatch({ type: 'server', msg: await request({ type: 'list_devices' }) });
      },
      async revokeDevice(deviceId) {
        store.dispatch({
          type: 'server',
          msg: await request({ type: 'revoke_device', device_id: deviceId }),
        });
      },
      async killSwitch() {
        await request({ type: 'kill_switch' });
      },
      reconnect: () => syncRef.current(),
      dismissToast(id) {
        store.dispatch({ type: 'dismiss_toast', id });
      },
    };
  }, [request, store]);

  const updateSettings = useCallback((patch: Partial<Settings>) => {
    setSettings((s) => ({ ...s, ...patch }));
  }, []);

  const value = useMemo<StoreContextValue>(
    () => ({
      store,
      actions,
      machines,
      machine,
      switchMachine: setMachineId,
      settings,
      updateSettings,
    }),
    [store, actions, machines, machine, settings, updateSettings],
  );

  return <StoreContext.Provider value={value}>{children}</StoreContext.Provider>;
}

function useStoreContext(): StoreContextValue {
  const ctx = useContext(StoreContext);
  if (!ctx) throw new Error('useStore must be used inside <AppProvider>');
  return ctx;
}

/** Subscribes to a slice of the app state. Selectors must return stable values. */
// eslint-disable-next-line react-refresh/only-export-components
export function useAppState<T>(selector: (state: AppState) => T): T {
  const { store } = useStoreContext();
  return useSyncExternalStore(store.subscribe, () => selector(store.getState()));
}

// eslint-disable-next-line react-refresh/only-export-components
export function useActions(): Actions {
  return useStoreContext().actions;
}

// eslint-disable-next-line react-refresh/only-export-components
export function useMachines() {
  const { machines, machine, switchMachine } = useStoreContext();
  return { machines, machine, switchMachine };
}

// eslint-disable-next-line react-refresh/only-export-components
export function useSettings() {
  const { settings, updateSettings } = useStoreContext();
  return { settings, updateSettings };
}
