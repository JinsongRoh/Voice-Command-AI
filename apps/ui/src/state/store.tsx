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
import {
  FEATURE_CONDUCTOR,
  FEATURE_DISMISS_SESSION,
  FEATURE_DISMISS_STOP,
  FEATURE_PROJECT_FOLDERS,
  FEATURE_SCAN_SESSIONS,
  FEATURE_SESSION_ORDER,
  type ClientMsg,
  type CloudLaunchRequest,
  type ConductorState,
  type FolderListing,
  type ProjectFolders,
  type ServerMsg,
} from '../protocol/types';
import { Client, type Transport } from '../protocol/transport';
import { FEATURE_HUMAN_TASKS, type TaskClientMsg } from '../protocol/tasks';
import {
  FEATURE_PERSONAL_TERMS,
  type PersonalTerm,
  type PersonalTermsClientMsg,
} from '../protocol/personalTerms';
import {
  FEATURE_PRONUNCIATIONS,
  type Pronunciation,
  type PronunciationClientMsg,
} from '../protocol/pronunciations';
import { setPronunciations } from '../speech/pronunciation';
import {
  PROTOCOL_VERSION,
  type AuditEntry,
  type ControlKey,
  type DangerLevel,
  type DeviceScope,
  type LaunchRequest,
  type QuestionAnswer,
  type ArrangeEdit,
  type ArrangeMonitorPlan,
  type ArrangeResult,
  type RestoreEdit,
  type RestoreJob,
  type RestoreOverview,
  type RestoreRunRequest,
  type SessionAction,
  type SessionEvent,
  type SessionId,
  type SessionInfo,
  type ShellRequest,
  type TtsEngineInfo,
} from '../protocol/types';
import {
  createAlertVoice,
  type AlertVoicePrefs,
  type SpeakOutcome,
  type Synthesize,
} from '../speech/alertVoice';
import { decisionVoiceClaimed } from '../speech/decisionVoice';
import { fixedPhrases } from '../speech/fixedPhrases';
import { resolveSpeaker } from '../speech/speakers';
import { voiceQueue, type VoicePriority } from '../speech/voiceQueue';
import { alertsFor } from './alerts';
import { isAppLocked, onAppLockChange } from './appLock';
import { createConductorStore, type ConductorStore } from './conductor';
import { approvalTitle, freshAlerts, type ApprovalAlert } from './approvals';
import { inputBusy } from './inputGuard';
import { showOsNotification } from './osNotify';
import { historyPageSize, sessionNumber, workOrder } from './reducer';
import type { AppState, DeckFilter, SentAttachment, Toast } from './reducer';
import { shouldSummarize, speakResultSummary } from './resultSummary';
import { completionLine, errorLine, priorityOf, spokenTopic, type Translate } from './spokenAlerts';
import { normalizeKoreanNumbers } from '../speech/koreanNumbers';
import { plainSpeech } from '../speech/plainSpeech';
import { spokenSessionName } from '../speech/sessionNames';
import { voiceLang } from '../speech/handsFree';
import { APP_VERSION, createStore, RequestError, ShellConfirmNeeded, type Store } from './core';

/** How long "Remove from VCA" waits for a stopped session to exit (older servers). */
const STOP_WAIT_MS = 15_000;
/** Longest wait for the PC's TTS engine list before speaking with its default voice. */
const SPEAKER_LIST_WAIT_MS = 3_000;
/** Warming starts this long after the app connected (the sync goes first). */
const WARM_DELAY_MS = 2_000;
/** The same voice choice is warmed at most this often. */
const WARM_MIN_INTERVAL_MS = 60_000;

/** Resolves once `id` has exited or left the list; rejects after `ms`. */
function waitForExit(store: Store, id: SessionId, ms = STOP_WAIT_MS): Promise<void> {
  const done = () => {
    const session = store.getState().sessions[id];
    return !session || session.state.type === 'exited';
  };
  if (done()) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const off = store.subscribe(() => {
      if (!done()) return;
      clearTimeout(timer);
      off();
      resolve();
    });
    const timer = setTimeout(() => {
      off();
      reject(new RequestError('unavailable', 'the agent did not stop in time'));
    }, ms);
  });
}

import {
  inQuietHours,
  loadSettings,
  saveSettings,
  shouldSpeak,
  shouldToast,
  type Settings,
} from './settings';
import { setCaptureOptions } from '../speech/captureOptions';

/** The server's `speech_options` reply. */
export type SpeechOptions = Extract<ServerMsg, { type: 'speech_options' }>;

/** The server's `command_danger` reply (classification of a command line). */
export type CommandDanger = Extract<ServerMsg, { type: 'command_danger' }>;

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
  simulate?: {
    drop(durationMs: number, loseHistory: boolean): void;
    /** Stands in for the desktop app's shortcut and sign-in commands. */
    restoreLocal?: RestoreLocalServices;
  };
  /** Present on machines reached through the native shell (Tauri). */
  native?: NativeMachine;
}

/** Native link details of a paired machine. */
/** Desktop shortcut and restore at sign-in: only on the computer itself. */
export interface RestoreLocalServices {
  status(): Promise<RestoreLocalStatus>;
  createShortcut(name: string): Promise<RestoreLocalStatus>;
  setSignIn(enabled: boolean, delaySecs: number): Promise<RestoreLocalStatus>;
}

export interface RestoreLocalStatus {
  supported: boolean;
  sign_in: boolean;
  sign_in_delay_secs: number;
}

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
  /**
   * Streams one more session next to the selected one (a session's Shell tab
   * shows the project's remote shell inside the agent's view). `null` stops
   * it. Only one such companion at a time; the newest call wins.
   */
  watch(id: SessionId | null): void;
  setFilter(filter: DeckFilter): void;
  /**
   * `attachments`: already uploaded files mentioned in the text (shown in the
   * bubble). `typed`: a person typed this by hand, so it may answer a prompt
   * the agent has open; voice and other automatic sends leave it out.
   */
  sendText(
    sessionId: SessionId,
    text: string,
    attachments?: SentAttachment[],
    typed?: boolean,
  ): Promise<void>;
  sendKey(sessionId: SessionId, key: ControlKey): Promise<void>;
  /** Types text without Enter (paste, Ctrl combinations); no chat echo. */
  sendRaw(sessionId: SessionId, text: string): Promise<void>;
  choose(sessionId: SessionId, decisionId: string, optionId: string): Promise<void>;
  /** Answers a question form; resolves once the agent took the answers. */
  answer(sessionId: SessionId, decisionId: string, answers: QuestionAnswer[]): Promise<void>;
  control(sessionId: SessionId, action: SessionAction): Promise<void>;
  /**
   * "Remove from list" of an ended session: `dismiss_session` on servers
   * that announce it, otherwise hidden on this device until the server
   * drops it (it does after a while).
   */
  dismissSession(sessionId: SessionId): Promise<void>;
  /**
   * "Remove from VCA": an ended session goes as with `dismissSession`; a
   * running session VCA started is stopped first. Servers with
   * `dismiss_stop` do both in one request (it completes even if this device
   * goes away); older ones get Stop, a wait for the exit, then the removal.
   */
  removeFromVca(sessionId: SessionId): Promise<void>;
  /** "Keep open" after a remote shell's idle-close notice (resets its idle timer). */
  keepShellOpen(sessionId: SessionId): Promise<void>;
  /** `null` restores the automatic name; the card updates via `session_updated`. */
  renameSession(sessionId: SessionId, name: string | null): Promise<void>;
  /**
   * Puts the cards in this order (first card first) on the server, for every
   * client. Shown at once; on failure the server's order comes back.
   */
  setSessionOrder(ids: SessionId[]): Promise<void>;
  /** Back to the default order (oldest first). */
  resetSessionOrder(): Promise<void>;
  launch(request: LaunchRequest): Promise<SessionInfo>;
  launchCloud(request: CloudLaunchRequest): Promise<SessionInfo>;
  /** Opens a remote shell (admin, fresh TOTP code). */
  openShell(request: ShellRequest): Promise<SessionInfo>;
  /** Classifies a command line without running it. */
  classifyCommand(command: string): Promise<CommandDanger>;
  /**
   * Types a confirmed command and Enter into a shell session. Rejects with
   * `RequestError('totp_required')` when a dangerous command needs a code,
   * and with `ShellConfirmNeeded` for several lines sent without `confirm`.
   */
  shellRun(
    sessionId: SessionId,
    command: string,
    totp: string | null,
    confirm?: boolean,
  ): Promise<DangerLevel>;
  /**
   * Joins a shell another device opened (fresh TOTP code), then streams it
   * again if it is on screen.
   */
  joinShell(sessionId: SessionId, totp: string): Promise<void>;
  /** Newest remote shell audit entries (admin). */
  listAudit(limit?: number): Promise<AuditEntry[]>;
  refreshResources(): Promise<void>;
  reorderQueue(jobId: string, position: number): Promise<void>;
  cancelQueued(jobId: string): Promise<void>;
  refreshDevices(): Promise<void>;
  /** Fetches the page of events before the oldest one of the session's view. */
  loadOlder(sessionId: SessionId): Promise<void>;
  /**
   * A page of a session's events before `beforeSeq` (`null`: the newest),
   * without putting it into the session's view (a to-do's source context).
   */
  fetchHistory(
    sessionId: SessionId,
    beforeSeq: number | null,
    limit: number,
  ): Promise<SessionEvent[]>;
  /** Speech engine and correction LLM profiles of the server. */
  listSpeechOptions(): Promise<SpeechOptions>;
  /** TTS engines on the PC (names and languages); empty when there are none. */
  listTtsEngines(): Promise<TtsEngineInfo[]>;
  /**
   * Speaks at once with the chosen voice source (the app's one speaker:
   * whatever it was saying stops); the device voice only when the PC cannot
   * before any PC audio played. `prefs.serverVoice` is the saved speaker,
   * checked against what the engine offers. `signal` stops it. Resolves
   * when it has been said, with the source that spoke.
   */
  speakAlert(
    text: string,
    lang: string,
    prefs: AlertVoicePrefs,
    signal?: AbortSignal,
  ): Promise<SpeakOutcome>;
  /** Saves this device's engine and LLM choice; resolves with the new options. */
  setSpeechPreferences(engine: string | null, llm: string | null): Promise<SpeechOptions>;
  revokeDevice(deviceId: string): Promise<void>;
  renameDevice(deviceId: string, name: string): Promise<void>;
  setDeviceScope(deviceId: string, scope: DeviceScope): Promise<void>;
  killSwitch(): Promise<void>;
  reconnect(): Promise<void>;
  dismissToast(id: string): void;
  /** Opens the session of an alert at its decision card. */
  openAlert(alert: Pick<ApprovalAlert, 'sessionId'>): void;
  /** The alert is dealt with: it leaves the count and does not come back. */
  markAlertHandled(key: string): void;
  /** Undo of `markAlertHandled`: back on the list if the session still waits. */
  reopenAlert(alert: Pick<ApprovalAlert, 'key' | 'sessionId'>): void;
  /** The decision card took the focus. */
  finishJump(): void;
  refreshRestore(): Promise<void>;
  restoreCapture(profile: string | null): Promise<void>;
  restoreEdit(edit: RestoreEdit): Promise<RestoreOverview>;
  restoreImport(layout: string, profile: string | null): Promise<void>;
  /**
   * Plans (`dry_run`) or starts a restore. Rejects with
   * `RequestError('totp_required')` when a remote run needs a code.
   */
  restoreRun(request: RestoreRunRequest): Promise<RestoreJob>;
  refreshArrange(): Promise<void>;
  /**
   * Moves windows; the result says what moved and what was skipped, and why.
   * Like `restoreRun`, rejects with `RequestError('totp_required')` when a
   * paired device needs a code; `totp` sends one.
   */
  arrangeApply(monitors: ArrangeMonitorPlan[], totp?: string): Promise<ArrangeResult>;
  arrangeApplyLayout(layout: string, totp?: string): Promise<ArrangeResult>;
  /** Puts the windows of the last arrangement back (a code as above). */
  arrangeUndo(totp?: string): Promise<ArrangeResult>;
  arrangeEdit(edit: ArrangeEdit): Promise<void>;
  /**
   * Brings a session's terminal window to the front, on a click in this
   * computer's own app. Rejects (`unavailable`) when Windows keeps the focus.
   */
  focusTerminal(sessionId: SessionId): Promise<void>;
  /** Shows an in-app toast raised by the UI itself (not by a server message). */
  showToast(toast: Toast): void;
  /** The PC's spoken status of its sessions (`briefing`), numbered in card order. */
  briefing(language: string): Promise<string>;
  /** Speaks text with the voice chosen for spoken alerts (device or PC engine). */
  speak(text: string): void;
  /**
   * Folder picker. Each rejects with `RequestError('unsupported')` without a
   * request when the server's `welcome` does not list `project_folders`.
   */
  projectFolders(): Promise<ProjectFolders>;
  listFolder(path: string, showHidden?: boolean): Promise<FolderListing>;
  /** The server-built path of a new folder `name` inside `parent` (nothing is created). */
  folderPath(parent: string, name: string): Promise<{ path: string; exists: boolean }>;
  /**
   * Session discovery: AI sessions running on the PC now, and hiding or
   * listing them again. Each rejects with `RequestError('unsupported')`
   * without a request when the server's `welcome` does not list `scan_sessions`.
   */
  scanSessions(): Promise<ScanReport>;
  hideObserved(target: { sessionId?: SessionId; pid?: number }): Promise<ScanReport>;
  unhideObserved(target: { pid?: number; all?: boolean }): Promise<ScanReport>;
  /**
   * A human task request ("할 일"). The answer also goes into the store
   * (`tasks`, `task_saved`, `task_result_sent`); errors reject with `RequestError`.
   */
  task(body: TaskClientMsg): Promise<ServerMsg>;
  /**
   * A conductor request (`conductor_*`); resolves with the new state, which
   * also reaches `useConductor`. Rejects with `RequestError('unsupported')`
   * without a request on servers without the conductor.
   */
  conductor(body: ConductorRequestMsg): Promise<ConductorState>;
  /**
   * A pronunciation dictionary request; resolves with every entry (which
   * spoken texts then use). Rejects with `RequestError('unsupported')`
   * without a request on servers without the dictionary.
   */
  pronunciation(body: PronunciationClientMsg): Promise<Pronunciation[]>;
  /**
   * A request of the commonly misheard words; resolves with the stored list.
   * Rejects with `RequestError('unsupported')` without a request on servers
   * without them.
   */
  personalTerms(body: PersonalTermsClientMsg): Promise<PersonalTerm[]>;
}

/** The conductor requests of the protocol. */
export type ConductorRequestMsg = Extract<ClientMsg, { type: `conductor_${string}` | 'cc_pause' }>;

/** The server's `scan_report` answer. */
export type ScanReport = Extract<ServerMsg, { type: 'scan_report' }>;

interface StoreContextValue {
  store: Store;
  conductor: ConductorStore;
  actions: Actions;
  machines: Machine[];
  machine: Machine;
  switchMachine(id: string): void;
  settings: Settings;
  updateSettings(patch: Partial<Settings>): void;
}

let promptCounter = 0;

/**
 * A folder picker request, or `unsupported` when the server did not offer
 * the picker (older servers drop the connection on unknown requests).
 */
function folderRequest(features: string[], body: ClientMsg): ClientMsg {
  if (!features.includes(FEATURE_PROJECT_FOLDERS))
    throw new RequestError('unsupported', 'this server has no folder picker');
  return body;
}

/** A session discovery request, or `unsupported` on servers without it. */
function scanRequest(features: string[], body: ClientMsg): ClientMsg {
  if (!features.includes(FEATURE_SCAN_SESSIONS))
    throw new RequestError('unsupported', 'this server cannot look for sessions');
  return body;
}

/** Whether `sessionId` is the selected session and the app is visible right now. */
function onScreenSession(selectedId: string | null, sessionId: string): boolean {
  return (
    selectedId === sessionId &&
    typeof document !== 'undefined' &&
    document.visibilityState === 'visible'
  );
}

/** Whether the app window has the keyboard focus (true where unknown). */
function documentFocused(): boolean {
  if (typeof document === 'undefined') return false;
  return typeof document.hasFocus !== 'function' || document.hasFocus();
}

/** Unique id for a locally echoed prompt. */
function nextPromptId(): string {
  promptCounter += 1;
  return `p${promptCounter}`;
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
  const [conductorStore] = useState(() => createConductorStore());
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
    // Every recorder reads the microphone options when it starts.
    setCaptureOptions({ noiseSuppression: settings.noiseSuppression });
  }, [settings, persistSettings]);

  /** Tells the user about a new approval alert, as the setting says. */
  const announceRef = useRef<(alert: ApprovalAlert) => void>(() => {});

  /** The session `watch` asked to stream next to the selected one. */
  const companionWishRef = useRef<SessionId | null>(null);
  /** Applies `companionWishRef` (set by the connection effect). */
  const followCompanionRef = useRef<() => void>(() => {});

  /** Reloads the session list (set by the connection effect). */
  const refreshSessionsRef = useRef<(staleId?: SessionId) => Promise<void>>(async () => {});
  /** `welcome.features` of the current connection (`null` before the welcome). */
  const featuresRef = useRef<string[] | null>(null);
  const featureWaitersRef = useRef<(() => void)[]>([]);
  /** The server's features, waiting a few seconds for the first welcome. */
  const serverFeatures = useCallback(async (): Promise<string[]> => {
    if (featuresRef.current) return featuresRef.current;
    await new Promise<void>((resolve) => {
      const timer = setTimeout(resolve, 5000);
      featureWaitersRef.current.push(() => {
        clearTimeout(timer);
        resolve();
      });
    });
    return featuresRef.current ?? [];
  }, []);
  const request = useCallback(async (body: ClientMsg): Promise<ServerMsg> => {
    let response: ServerMsg;
    try {
      response = await clientRef.current.request(body);
    } catch (err) {
      throw new RequestError('transport', (err as Error).message);
    }
    if (response.type === 'error') {
      // The card points at a session the server no longer knows (e.g. the PC
      // restarted and rediscovered its terminals under new ids): reload the
      // list so the stale card goes away.
      if (response.code === 'not_found' && 'session_id' in body) {
        void refreshSessionsRef.current(body.session_id ?? undefined).catch(() => {});
      }
      throw new RequestError(response.code, response.message);
    }
    return response;
  }, []);

  /** Spoken alerts: the device voice, or the PC's TTS engine with fallback. */
  const [alertVoice] = useState(() => createAlertVoice());
  const synthesize = useCallback<Synthesize>(
    async (text, language, engine, voice) => {
      const res = await request({ type: 'synthesize', text, language, engine, voice });
      if (res.type !== 'audio') throw new RequestError('internal', `unexpected ${res.type}`);
      const data = res.data as Uint8Array | number[];
      return { format: res.format, data: Array.isArray(data) ? Uint8Array.from(data) : data };
    },
    [request],
  );
  /** The PC's TTS engines, fetched once per connection (cleared with the cached audio). */
  const ttsEnginesRef = useRef<Promise<TtsEngineInfo[]> | null>(null);
  const ttsEngines = useCallback((): Promise<TtsEngineInfo[]> => {
    if (ttsEnginesRef.current === null) {
      const pending = request({ type: 'list_tts_engines' }).then((res) => {
        if (res.type !== 'tts_engines')
          throw new RequestError('internal', `unexpected ${res.type}`);
        return res.engines;
      });
      ttsEnginesRef.current = pending;
      pending.catch(() => {
        if (ttsEnginesRef.current === pending) ttsEnginesRef.current = null;
      });
    }
    return ttsEnginesRef.current;
  }, [request]);
  /**
   * The PC speaker for a saved id, checked against what the engine offers
   * now (the same for every path, so every prompt has the same voice). A
   * list that cannot be fetched in time leaves the engine's configured voice.
   */
  const serverVoiceFor = useCallback(
    async (engine: string | null, saved: string | null | undefined): Promise<string | null> => {
      if (engine === null) return null;
      let timer: ReturnType<typeof setTimeout> | undefined;
      const late = new Promise<null>((resolve) => {
        timer = setTimeout(() => resolve(null), SPEAKER_LIST_WAIT_MS);
      });
      const list = await Promise.race([ttsEngines().catch(() => null), late]);
      clearTimeout(timer);
      if (!list) return null;
      return (
        resolveSpeaker(
          list.find((e) => e.name === engine),
          saved ?? '',
        )?.id ?? null
      );
    },
    [ttsEngines],
  );
  /**
   * Queues `text` on the one voice queue (`voiceQueue.ts`): it plays after
   * what is already playing, most urgent first, with the chosen voice.
   */
  const speakAlert = useCallback(
    (text: string, current: Settings, opts: { priority?: VoicePriority; key?: string } = {}) => {
      // Locked: nothing is read aloud (names and summaries are content).
      if (isAppLocked()) return;
      const prefs: AlertVoicePrefs = {
        serverEngine: current.ttsServerEngine,
        voice: current.ttsVoice,
        rate: current.ttsRate,
      };
      const engine = current.ttsServerEngine;
      /** The playback in progress; a replay after `exclusive` gets its own. */
      let active: AbortController | null = null;
      voiceQueue.enqueue({
        key: opts.key,
        priority: opts.priority ?? 'completion',
        // Stops this item only, also while its audio is still being made:
        // a clip that arrives after the stop is never played.
        stop: () => active?.abort(),
        play: async () => {
          const controller = new AbortController();
          active = controller;
          const serverVoice = await serverVoiceFor(engine, current.ttsServerVoice);
          if (controller.signal.aborted) return 'stopped';
          return alertVoice.play(
            text,
            current.language,
            { ...prefs, serverVoice },
            synthesize,
            controller.signal,
          );
        },
      });
    },
    [alertVoice, synthesize, serverVoiceFor],
  );

  // Fixed prompts ("네", "보냈습니다.", ...) of hands-free mode are
  // synthesized ahead in the PC voice: when the app connects and when the
  // engine, speaker or language changes, or hands-free mode is turned on.
  // At most once a minute per choice, one phrase at a time.
  const warmedAt = useRef(new Map<string, number>());
  const handsFreeOn = settings.handsFree;
  useEffect(() => {
    const engine = settings.ttsServerEngine;
    if (engine === null || !handsFreeOn) return;
    const lang = settings.language;
    const saved = settings.ttsServerVoice;
    const transport = machine.transport;
    const key = JSON.stringify([machine.id, engine, saved, lang]);
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const schedule = () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        if (controller.signal.aborted || transport.state !== 'connected') return;
        const last = warmedAt.current.get(key);
        if (last !== undefined && Date.now() - last < WARM_MIN_INTERVAL_MS) return;
        warmedAt.current.set(key, Date.now());
        void serverVoiceFor(engine, saved)
          .then((serverVoice) =>
            controller.signal.aborted
              ? undefined
              : alertVoice.warm(
                  fixedPhrases(voiceLang(lang)),
                  lang,
                  { serverEngine: engine, serverVoice },
                  synthesize,
                  { signal: controller.signal },
                ),
          )
          .catch(() => {})
          .finally(() => {
            // Cut short: the next chance tries again.
            if (controller.signal.aborted) warmedAt.current.delete(key);
          });
      }, WARM_DELAY_MS);
    };
    const off = transport.onStateChange((state) => {
      if (state === 'connected') schedule();
    });
    schedule();
    return () => {
      controller.abort();
      clearTimeout(timer);
      off();
    };
  }, [
    machine,
    handsFreeOn,
    settings.ttsServerEngine,
    settings.ttsServerVoice,
    settings.language,
    alertVoice,
    synthesize,
    serverVoiceFor,
  ]);

  // Connection lifecycle for the selected machine.
  useEffect(() => {
    const transport = machine.transport;
    const client = new Client(transport);
    clientRef.current = client;
    // Another PC has other engines: cached alert audio is not reused.
    alertVoice.clear();
    ttsEnginesRef.current = null;
    store.dispatch({ type: 'reset' });
    let active = true;
    /** The one session whose event stream is subscribed: the selected one. */
    let subscribed: SessionId | null = null;
    let synced = false;

    /** The second stream `watch` asked for (never the selected session). */
    let companion: SessionId | null = null;
    /** A shell another device opened: this device has to join it first. */
    const noteJoinNeeded = (id: SessionId, res: ServerMsg) => {
      if (res.type === 'error' && res.code === 'totp_required' && /join/i.test(res.message)) {
        store.dispatch({ type: 'shell_join_needed', id });
      }
    };
    const followCompanion = () => {
      if (!synced || transport.state !== 'connected') return;
      const wish = companionWishRef.current;
      const want = wish && wish !== subscribed ? wish : null;
      if (want === companion) return;
      if (companion && companion !== subscribed) {
        client.request({ type: 'unsubscribe', session_id: companion }).catch(() => {});
      }
      companion = want;
      if (!want) return;
      store.dispatch({ type: 'view_opening', sessionId: want });
      const fromSeq = store.getState().views[want]?.lastSeq ?? 0;
      client
        .request({ type: 'subscribe', session_id: want, from_seq: fromSeq })
        .then((res) => noteJoinNeeded(want, res))
        .catch(() => {
          if (companion === want) companion = null;
        });
    };
    followCompanionRef.current = followCompanion;

    // Cards use `SessionInfo.preview`, so only the selected session is streamed
    // (plus the companion of its Shell tab, if any).
    const followSelection = () => {
      if (!synced || transport.state !== 'connected') return;
      const selected = store.getState().selectedId;
      if (selected === subscribed) return followCompanion();
      if (subscribed && subscribed !== companion) {
        client.request({ type: 'unsubscribe', session_id: subscribed }).catch(() => {});
      }
      subscribed = selected;
      if (selected && selected === companion) {
        // Already streamed as the companion: it only changes roles.
        companion = null;
        return followCompanion();
      }
      followCompanion();
      if (!selected) return;
      store.dispatch({ type: 'view_opening', sessionId: selected });
      // Resume from the last seen seq (CON-04); the server replays or sends
      // stream_gap. With nothing seen yet ask for the whole retained history:
      // `null` would mean "new events only" and leave the view empty.
      const fromSeq = store.getState().views[selected]?.lastSeq ?? 0;
      client
        .request({ type: 'subscribe', session_id: selected, from_seq: fromSeq })
        .then((res) => noteJoinNeeded(selected, res))
        .catch(() => {
          if (subscribed === selected) subscribed = null;
        });
    };
    const offStore = store.subscribe(followSelection);

    const sync = async () => {
      synced = false;
      subscribed = null;
      companion = null;
      const welcome = await client.request({
        type: 'hello',
        protocol_version: PROTOCOL_VERSION,
        app_version: APP_VERSION,
      });
      if (!active) return;
      featuresRef.current = welcome.type === 'welcome' ? (welcome.features ?? []) : [];
      for (const wake of featureWaitersRef.current.splice(0)) wake();
      store.dispatch({ type: 'server', msg: welcome });
      // The conductor's state, on servers that have one.
      conductorStore.set(null);
      if (featuresRef.current.includes(FEATURE_CONDUCTOR)) {
        client
          .request({ type: 'conductor_get' })
          .then((res) => res.type === 'conductor' && conductorStore.set(res.state))
          .catch(() => {});
      }
      // Result summaries stay off on the PC until this device opts in again.
      const voice = settingsRef.current;
      if (voice.resultSummary) {
        client
          .request({
            type: 'set_voice_summary',
            enabled: true,
            details: voice.resultSummaryDetails,
          })
          .catch(() => {});
      }
      const list = await client.request({ type: 'list_sessions' });
      if (!active) return;
      store.dispatch({ type: 'server', msg: list });
      // The card order chosen on the PC; changes are pushed.
      if (featuresRef.current.includes(FEATURE_SESSION_ORDER)) {
        const order = await client.request({ type: 'get_session_order' }).catch(() => null);
        if (!active) return;
        if (order?.type === 'session_order') store.dispatch({ type: 'server', msg: order });
      }
      synced = true;
      followSelection();
      const resources = await client.request({ type: 'get_resources' });
      if (active && resources.type === 'resources')
        store.dispatch({ type: 'server', msg: resources });
      // Device management may be disabled on the server; ignore failures.
      const devices = await client.request({ type: 'list_devices' }).catch(() => null);
      if (active && devices?.type === 'devices') store.dispatch({ type: 'server', msg: devices });
      // The PC's to-do list (servers that offer it); changes are pushed.
      if (featuresRef.current?.includes(FEATURE_HUMAN_TASKS)) {
        const tasks = await client.request({ type: 'task_list' }).catch(() => null);
        if (active && tasks?.type === 'tasks') store.dispatch({ type: 'server', msg: tasks });
      }
      // The PC's pronunciation dictionary, used by every spoken text; pushed on change.
      if (featuresRef.current?.includes(FEATURE_PRONUNCIATIONS)) {
        const list = await client
          .request({ type: 'list_pronunciations', language: null })
          .catch(() => null);
        if (active && list?.type === 'pronunciations') setPronunciations(list.entries);
      } else if (active) {
        setPronunciations([]);
      }
    };
    refreshSessionsRef.current = async (staleId) => {
      if (!synced || transport.state !== 'connected') return;
      const list = await client.request({ type: 'list_sessions' });
      if (!active || list.type !== 'sessions') return;
      store.dispatch({ type: 'server', msg: list });
      if (staleId && !list.sessions.some((x) => x.id === staleId)) {
        // Its view closes with the card, so say what happened in a toast.
        const lang = settingsRef.current.language;
        store.dispatch({
          type: 'toast',
          toast: {
            id: `gone:${staleId}:${Date.now()}`,
            category: 'error',
            sessionId: null,
            title: translate(lang, 'error.sessionNotFound'),
            body: translate(lang, 'error.sessionRemoved'),
            // The person just acted on it: shown whatever the alert settings say.
            security: true,
          },
        });
      }
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
      // Conductor replies are read aloud when spoken alerts are on.
      if (msg.type === 'pronunciations') setPronunciations(msg.entries);
      conductorStore.handle(msg, (text, at) => {
        if (settingsRef.current.spokenAlerts)
          speakAlert(plainSpeech(text), settingsRef.current, {
            priority: 'briefing',
            key: `conductor:${at}`,
          });
      });
      const now = new Date();
      const after = store.getState();
      const fresh =
        msg.type === 'session_updated' || msg.type === 'launched'
          ? freshAlerts(before.alerts, after.alerts)
          : [];
      for (const alert of fresh) announceRef.current(alert);
      const lang = settingsRef.current.language;
      const tr: Translate = (k, v) => translate(lang, k, v);
      /** A session in a spoken alert: by the name the user gave it. */
      const spokenName = (id: string) => {
        const s = after.sessions[id];
        return s ? spokenSessionName(s, after.sessions, workOrder(after), voiceLang(lang)) : null;
      };
      // A finished turn is read aloud as a short summary instead (opt-in).
      const summarize =
        msg.type === 'session_updated' &&
        !!before.sessions[msg.session.id] &&
        // The conductor's replies are read aloud as `conductor_reply`.
        !msg.session.conductor &&
        shouldSummarize(
          settingsRef.current,
          before.sessions[msg.session.id]!.state,
          msg.session.state,
          now,
        );
      for (const toast of toasts) {
        // Approval alerts have their own path (`announce`), once per decision.
        if ((toast.category === 'decision' || toast.category === 'input') && toast.sessionId)
          continue;
        // A finished task in the session being looked at is already on screen.
        const seen =
          toast.category === 'completed' &&
          !!toast.sessionId &&
          onScreenSession(store.getState().selectedId, toast.sessionId) &&
          documentFocused();
        // Remote shell notices are security relevant: always shown.
        if (toast.security || (!seen && shouldToast(settingsRef.current, toast.category)))
          store.dispatch({ type: 'toast', toast });
        // Activity titles come from the agent and may hold commands or
        // paths, so they are shown but never read aloud.
        if (
          toast.category === 'activity' ||
          !shouldSpeak(settingsRef.current, toast.category, now) ||
          (toast.category === 'completed' && summarize)
        ) {
          continue;
        }
        // Say what finished or failed: the card number, a safe topic or a
        // generic reason; never the agent's text.
        const session = toast.sessionId ? after.sessions[toast.sessionId] : undefined;
        const number = sessionNumber(after, toast.sessionId);
        let text = toast.title;
        if (session && toast.category === 'completed') {
          text = completionLine(tr, number, spokenTopic(session), spokenName(session.id));
        } else if (session && toast.category === 'error') {
          text = errorLine(tr, number, session.state, spokenName(session.id));
        }
        speakAlert(normalizeKoreanNumbers(text, lang), settingsRef.current, {
          priority: priorityOf(toast.category),
          key: toast.id,
        });
      }
      if (summarize && msg.type === 'session_updated') {
        const number = sessionNumber(after, msg.session.id);
        void speakResultSummary(
          {
            request: (body) => client.request(body),
            speak: (text) =>
              speakAlert(plainSpeech(text), settingsRef.current, {
                priority: 'completion',
                key: `summary:${msg.session.id}:${msg.session.last_activity}`,
              }),
            language: lang,
          },
          msg.session.id,
          number,
          completionLine(tr, number, spokenTopic(msg.session), spokenName(msg.session.id)),
        );
      }
      if (msg.type === 'session_updated' || msg.type === 'launched') {
        if (msg.session.id === before.selectedId && msg.session.unread > 0) {
          client.request({ type: 'mark_read', session_id: msg.session.id }).catch(() => {});
        }
      }
    });

    let previous = transport.state;
    /** The first connection syncs from `connect()` below; later ones from here. */
    let initialDone = false;
    const offState = transport.onStateChange((state) => {
      store.dispatch({ type: 'connection', state });
      // CON-04: after any reconnect, reload the session list (it replaces
      // the cards, so none survive a server restart) and resubscribe from
      // the last seen seq.
      if (state === 'connected' && previous !== 'connected' && initialDone) void safeSync();
      previous = state;
    });
    store.dispatch({ type: 'connection', state: transport.state });
    transport.connect().then(
      () => {
        initialDone = true;
        return safeSync();
      },
      (err: Error) => {
        initialDone = true;
        store.dispatch({ type: 'error', message: err.message });
      },
    );

    return () => {
      active = false;
      followCompanionRef.current = () => {};
      offStore();
      offMessage();
      offState();
      transport.disconnect();
    };
  }, [machine, store, alertVoice, speakAlert, conductorStore]);

  const actions = useMemo<Actions>(() => {
    const arranged = (res: ServerMsg): ArrangeResult => {
      if (res.type !== 'arrange_done') {
        throw new RequestError('internal', `unexpected ${res.type}`);
      }
      store.dispatch({ type: 'server', msg: res });
      return res.result;
    };
    const withAck = async (sessionId: SessionId, body: ClientMsg) => {
      store.dispatch({
        type: 'delivery',
        sessionId,
        delivery: { status: 'sending', at: Date.now() },
      });
      try {
        const res = await request(body);
        if (res.type === 'shell_confirm') {
          // Several lines: nothing was typed; the person confirms them first.
          throw new ShellConfirmNeeded(res.session_id, res.command, res.lines);
        }
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
      watch(id) {
        companionWishRef.current = id;
        followCompanionRef.current();
      },
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
      async sendText(sessionId, text, attachments, typed) {
        // Local echo for the chat view: the protocol has no event for user input.
        const id = nextPromptId();
        store.dispatch({ type: 'prompt_sent', sessionId, id, text, at: Date.now(), attachments });
        try {
          await withAck(sessionId, {
            type: 'send_input',
            session_id: sessionId,
            input: { type: 'text', text, submit: true, ...(typed ? { at_prompt: true } : {}) },
          });
        } catch (err) {
          store.dispatch({
            type: 'prompt_status',
            sessionId,
            id,
            status: 'failed',
            error: (err as Error).message,
          });
          throw err;
        }
      },
      sendKey: (sessionId, key) =>
        withAck(sessionId, {
          type: 'send_input',
          session_id: sessionId,
          input: { type: 'key', key },
        }),
      sendRaw: (sessionId, text) =>
        withAck(sessionId, {
          type: 'send_input',
          session_id: sessionId,
          // Terminal keys a person tapped: they may answer a prompt.
          input: { type: 'text', text, submit: false, at_prompt: true },
        }),
      async choose(sessionId, decisionId, optionId) {
        await withAck(sessionId, {
          type: 'send_input',
          session_id: sessionId,
          input: { type: 'choose', decision_id: decisionId, option_id: optionId },
        });
        store.dispatch({ type: 'decision_answered', sessionId });
      },
      async answer(sessionId, decisionId, answers) {
        await withAck(sessionId, {
          type: 'send_input',
          session_id: sessionId,
          input: { type: 'answer', decision_id: decisionId, answers },
        });
        store.dispatch({ type: 'decision_answered', sessionId });
      },
      async control(sessionId, action) {
        await request({ type: 'control', session_id: sessionId, action });
      },
      async dismissSession(sessionId) {
        if (store.getState().serverFeatures.includes(FEATURE_DISMISS_SESSION)) {
          await request({ type: 'dismiss_session', session_id: sessionId });
        }
        // Gone from this list at once (the server's `session_removed` follows).
        store.dispatch({ type: 'hide_session', id: sessionId });
      },
      async removeFromVca(sessionId) {
        const state = store.getState();
        const session = state.sessions[sessionId];
        if (session && session.state.type !== 'exited') {
          if (state.serverFeatures.includes(FEATURE_DISMISS_STOP)) {
            await request({ type: 'dismiss_session', session_id: sessionId, stop: true });
            // `ok` means the server removed it (its `session_removed` follows).
            store.dispatch({
              type: 'server',
              msg: { type: 'session_removed', session_id: sessionId },
            });
            return;
          }
          await request({ type: 'control', session_id: sessionId, action: 'stop' });
          await waitForExit(store, sessionId);
        }
        if (store.getState().serverFeatures.includes(FEATURE_DISMISS_SESSION)) {
          await request({ type: 'dismiss_session', session_id: sessionId });
        }
        store.dispatch({ type: 'hide_session', id: sessionId });
      },
      async keepShellOpen(sessionId) {
        await request({ type: 'shell_keep_alive', session_id: sessionId });
        store.dispatch({ type: 'closing_cleared', id: sessionId });
      },
      async renameSession(sessionId, name) {
        await request({ type: 'rename_session', session_id: sessionId, name });
      },
      async setSessionOrder(ids) {
        store.dispatch({ type: 'reorder', order: ids });
        try {
          const res = await request({ type: 'set_session_order', session_ids: ids });
          if (res.type === 'session_order') store.dispatch({ type: 'server', msg: res });
        } catch (err) {
          // Show the PC's order again.
          const res = await request({ type: 'get_session_order' }).catch(() => null);
          if (res?.type === 'session_order') store.dispatch({ type: 'server', msg: res });
          throw err;
        }
      },
      async resetSessionOrder() {
        const res = await request({ type: 'reset_session_order' });
        if (res.type === 'session_order') store.dispatch({ type: 'server', msg: res });
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
      async openShell(req) {
        const res = await request({ type: 'open_shell', request: req });
        if (res.type !== 'launched') throw new RequestError('internal', `unexpected ${res.type}`);
        store.dispatch({ type: 'server', msg: res });
        return res.session;
      },
      async classifyCommand(command) {
        const res = await request({ type: 'classify_command', command });
        if (res.type !== 'command_danger') {
          throw new RequestError('internal', `unexpected ${res.type}`);
        }
        return res;
      },
      async shellRun(sessionId, command, totp, confirm = false) {
        const res = await request({
          type: 'shell_run',
          session_id: sessionId,
          command,
          totp,
          ...(confirm ? { confirm: true } : {}),
        });
        if (res.type === 'shell_confirm') {
          throw new ShellConfirmNeeded(res.session_id, res.command, res.lines);
        }
        if (res.type !== 'shell_ran') throw new RequestError('internal', `unexpected ${res.type}`);
        return res.danger;
      },
      async joinShell(sessionId, totp) {
        const res = await request({ type: 'shell_join', session_id: sessionId, totp });
        if (res.type !== 'ok') throw new RequestError('internal', `unexpected ${res.type}`);
        store.dispatch({ type: 'shell_joined', id: sessionId });
        // The refused stream starts now (the selected session or a Shell tab).
        const state = store.getState();
        if (state.selectedId === sessionId || companionWishRef.current === sessionId) {
          const fromSeq = state.views[sessionId]?.lastSeq ?? 0;
          await request({ type: 'subscribe', session_id: sessionId, from_seq: fromSeq });
        }
      },
      async listAudit(limit = 200) {
        const res = await request({ type: 'list_audit', limit });
        if (res.type !== 'audit') throw new RequestError('internal', `unexpected ${res.type}`);
        return res.entries;
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
      async loadOlder(sessionId) {
        const view = store.getState().views[sessionId];
        if (!view || view.loadingOlder || view.more === false) return;
        const before = view.events[0]?.seq ?? view.gapFrom;
        if (before == null) return;
        store.dispatch({ type: 'older_loading', sessionId });
        try {
          const res = await request({
            type: 'history',
            session_id: sessionId,
            before_seq: before,
            limit: historyPageSize(view.events),
          });
          if (res.type !== 'history') throw new RequestError('internal', `unexpected ${res.type}`);
          store.dispatch({ type: 'server', msg: res });
        } catch (err) {
          store.dispatch({ type: 'older_failed', sessionId });
          throw err;
        }
      },
      async fetchHistory(sessionId, beforeSeq, limit) {
        const res = await request({
          type: 'history',
          session_id: sessionId,
          before_seq: beforeSeq,
          limit,
        });
        if (res.type !== 'history') throw new RequestError('internal', `unexpected ${res.type}`);
        return res.events;
      },
      async refreshDevices() {
        store.dispatch({ type: 'server', msg: await request({ type: 'list_devices' }) });
      },
      async listSpeechOptions() {
        const res = await request({ type: 'list_speech_options' });
        if (res.type !== 'speech_options') {
          throw new RequestError('internal', `unexpected ${res.type}`);
        }
        return res;
      },
      async listTtsEngines() {
        ttsEnginesRef.current = null;
        return ttsEngines();
      },
      async speakAlert(text, lang, prefs, signal) {
        const serverVoice =
          prefs.serverEngine === null
            ? null
            : await serverVoiceFor(prefs.serverEngine, prefs.serverVoice);
        if (signal?.aborted) return 'stopped';
        return alertVoice.speak(text, lang, { ...prefs, serverVoice }, synthesize, signal);
      },
      async setSpeechPreferences(engine, llm) {
        const res = await request({ type: 'set_speech_preferences', engine, llm });
        if (res.type !== 'speech_options') {
          throw new RequestError('internal', `unexpected ${res.type}`);
        }
        return res;
      },
      async revokeDevice(deviceId) {
        const reply = await request({ type: 'revoke_device', device_id: deviceId });
        // The server answers `ok`; reload the list so the device disappears.
        const msg = reply.type === 'devices' ? reply : await request({ type: 'list_devices' });
        store.dispatch({ type: 'server', msg });
      },
      async renameDevice(deviceId, name) {
        store.dispatch({
          type: 'server',
          msg: await request({ type: 'rename_device', device_id: deviceId, name }),
        });
      },
      async setDeviceScope(deviceId, scope) {
        store.dispatch({
          type: 'server',
          msg: await request({ type: 'set_device_scope', device_id: deviceId, scope }),
        });
      },
      async killSwitch() {
        await request({ type: 'kill_switch' });
      },
      reconnect: () => syncRef.current(),
      dismissToast(id) {
        store.dispatch({ type: 'dismiss_toast', id });
      },
      openAlert({ sessionId }) {
        const session = store.getState().sessions[sessionId];
        store.dispatch({ type: 'select', id: sessionId });
        if (session && session.unread > 0) {
          request({ type: 'mark_read', session_id: sessionId }).catch(() => {});
        }
        store.dispatch({ type: 'jump', sessionId });
      },
      markAlertHandled(key) {
        store.dispatch({ type: 'alert_handled', key });
      },
      reopenAlert({ key, sessionId }) {
        store.dispatch({ type: 'alert_reopened', key, sessionId });
      },
      finishJump() {
        store.dispatch({ type: 'jump_done' });
      },
      async refreshRestore() {
        store.dispatch({ type: 'server', msg: await request({ type: 'restore_list' }) });
      },
      async restoreCapture(profile) {
        store.dispatch({
          type: 'server',
          msg: await request({ type: 'restore_capture', profile }),
        });
      },
      async restoreEdit(edit) {
        const res = await request({ type: 'restore_edit', edit });
        if (res.type !== 'restore_list') {
          throw new RequestError('internal', `unexpected ${res.type}`);
        }
        store.dispatch({ type: 'server', msg: res });
        return res.overview;
      },
      async restoreImport(layout, profile) {
        store.dispatch({
          type: 'server',
          msg: await request({ type: 'restore_import', layout, profile }),
        });
      },
      async restoreRun(req) {
        const res = await request({ type: 'restore_run', request: req });
        if (res.type !== 'restore_progress') {
          throw new RequestError('internal', `unexpected ${res.type}`);
        }
        store.dispatch({ type: 'server', msg: res });
        return res.job;
      },
      async refreshArrange() {
        store.dispatch({ type: 'server', msg: await request({ type: 'arrange_list' }) });
      },
      async arrangeApply(monitors, totp) {
        return arranged(
          await request({ type: 'arrange_apply', monitors, ...(totp ? { totp } : {}) }),
        );
      },
      async arrangeApplyLayout(layout, totp) {
        return arranged(
          await request({ type: 'arrange_apply_layout', layout, ...(totp ? { totp } : {}) }),
        );
      },
      async arrangeUndo(totp) {
        return arranged(await request({ type: 'arrange_undo', ...(totp ? { totp } : {}) }));
      },
      async arrangeEdit(edit) {
        store.dispatch({ type: 'server', msg: await request({ type: 'arrange_edit', edit }) });
      },
      async focusTerminal(sessionId) {
        await request({ type: 'focus_terminal', session_id: sessionId });
      },
      showToast(toast) {
        store.dispatch({ type: 'toast', toast });
      },
      async briefing(language) {
        const res = await request({
          type: 'briefing',
          language,
          order: workOrder(store.getState()),
        });
        if (res.type !== 'briefing') throw new RequestError('internal', `unexpected ${res.type}`);
        return res.text;
      },
      speak(text) {
        speakAlert(plainSpeech(text), settingsRef.current, { priority: 'briefing' });
      },
      async projectFolders() {
        const res = await request(
          folderRequest(await serverFeatures(), { type: 'project_folders' }),
        );
        if (res.type !== 'project_folders')
          throw new RequestError('internal', `unexpected ${res.type}`);
        return res;
      },
      async listFolder(path, showHidden = false) {
        const res = await request(
          folderRequest(await serverFeatures(), {
            type: 'list_folder',
            path,
            show_hidden: showHidden,
          }),
        );
        if (res.type !== 'folder') throw new RequestError('internal', `unexpected ${res.type}`);
        return res;
      },
      async folderPath(parent, name) {
        const res = await request(
          folderRequest(await serverFeatures(), { type: 'folder_path', parent, name }),
        );
        if (res.type !== 'folder_path')
          throw new RequestError('internal', `unexpected ${res.type}`);
        return { path: res.path, exists: res.exists };
      },
      async scanSessions() {
        const res = await request(scanRequest(await serverFeatures(), { type: 'scan_sessions' }));
        if (res.type !== 'scan_report')
          throw new RequestError('internal', `unexpected ${res.type}`);
        // New cards also arrive as `session_updated`; this catches removals.
        void refreshSessionsRef.current().catch(() => {});
        return res;
      },
      async hideObserved({ sessionId, pid }) {
        const res = await request(
          scanRequest(await serverFeatures(), {
            type: 'hide_observed',
            session_id: sessionId ?? null,
            pid: pid ?? null,
          }),
        );
        if (res.type !== 'scan_report')
          throw new RequestError('internal', `unexpected ${res.type}`);
        void refreshSessionsRef.current().catch(() => {});
        return res;
      },
      async unhideObserved({ pid, all }) {
        const res = await request(
          scanRequest(await serverFeatures(), {
            type: 'unhide_observed',
            pid: pid ?? null,
            all: all ?? false,
          }),
        );
        if (res.type !== 'scan_report')
          throw new RequestError('internal', `unexpected ${res.type}`);
        void refreshSessionsRef.current().catch(() => {});
        return res;
      },
      async pronunciation(body) {
        if (!(await serverFeatures()).includes(FEATURE_PRONUNCIATIONS))
          throw new RequestError('unsupported', 'this server has no pronunciation dictionary');
        const res = await request(body);
        if (res.type !== 'pronunciations')
          throw new RequestError('internal', `unexpected ${res.type}`);
        setPronunciations(res.entries);
        return res.entries;
      },
      async personalTerms(body) {
        if (!(await serverFeatures()).includes(FEATURE_PERSONAL_TERMS))
          throw new RequestError('unsupported', 'this server has no word list');
        const res = await request(body);
        if (res.type !== 'personal_terms')
          throw new RequestError('internal', `unexpected ${res.type}`);
        return res.terms;
      },
      async task(body) {
        if (!(await serverFeatures()).includes(FEATURE_HUMAN_TASKS))
          throw new RequestError('unsupported', 'this server has no to-do list');
        const res = await request(body);
        store.dispatch({ type: 'server', msg: res });
        return res;
      },
      async conductor(body) {
        if (!(await serverFeatures()).includes(FEATURE_CONDUCTOR))
          throw new RequestError('unsupported', 'this server has no conductor');
        const res = await request(body);
        if (res.type !== 'conductor') throw new RequestError('internal', `unexpected ${res.type}`);
        conductorStore.set(res.state);
        return res.state;
      },
    };
  }, [
    request,
    store,
    alertVoice,
    synthesize,
    ttsEngines,
    serverVoiceFor,
    speakAlert,
    serverFeatures,
    conductorStore,
  ]);

  // Turning spoken alerts off (Settings or the quick toggle) silences the
  // voice at once: what plays stops and nothing waiting is said.
  // The app lock silences the voice at once, too.
  useEffect(
    () =>
      onAppLockChange((locked) => {
        if (locked) voiceQueue.stopAll();
      }),
    [],
  );
  const spokenBefore = useRef(settings.spokenAlerts);
  useEffect(() => {
    if (spokenBefore.current && !settings.spokenAlerts) voiceQueue.stopAll();
    spokenBefore.current = settings.spokenAlerts;
  }, [settings.spokenAlerts]);

  // The PC keeps a device's summary opt-in only while it is connected; a
  // change is sent at once (and again after every reconnect, in `sync`).
  const summaryOptIn = useRef<string | null>(null);
  useEffect(() => {
    const key = `${settings.resultSummary}:${settings.resultSummaryDetails}`;
    const previous = summaryOptIn.current;
    summaryOptIn.current = key;
    if (previous === null || previous === key || machine.transport.state !== 'connected') return;
    request({
      type: 'set_voice_summary',
      enabled: settings.resultSummary,
      details: settings.resultSummaryDetails,
    }).catch(() => {});
  }, [settings.resultSummary, settings.resultSummaryDetails, machine, request]);

  useEffect(() => {
    announceRef.current = (alert) => {
      const current = settingsRef.current;
      // Watched: the alert's session is open, the app visible and focused.
      // Then the session itself shows the alert and no toast pops over it.
      const watched =
        onScreenSession(store.getState().selectedId, alert.sessionId) && documentFocused();
      // A question from the session being watched is read there: the Alerts
      // list keeps it, but it does not interrupt.
      if (alert.kind === 'question' && watched) return;
      const now = new Date();
      // "Switch automatically": never takes the screen while the person is
      // typing or dictating, and never answers anything: the card only opens.
      // The spoken alert and the OS notification still go out below, so an
      // alert is not missed while the window is hidden or in the background.
      const opened = current.approvalMode === 'auto' && !inputBusy(now.getTime());
      if (opened) actions.openAlert(alert);
      const lang = current.language;
      const title = approvalTitle(alert, (k, v) => translate(lang, k, v));
      if (!opened && !watched && shouldToast(current, alert.kind)) {
        // "Don't switch" (`off`): a plain notice that never offers Go / Later.
        const prompt = current.approvalMode !== 'off';
        store.dispatch({
          type: 'toast',
          toast: {
            id: `alert:${alert.key}`,
            category: alert.kind,
            sessionId: alert.sessionId,
            title,
            body: alert.request,
            ...(prompt ? { alertKey: alert.key } : {}),
          },
        });
      }
      // Hands-free reads a decision on screen itself (with its choices); the
      // alert would only say the same thing twice.
      const onScreen = onScreenSession(store.getState().selectedId, alert.sessionId);
      const readOnScreen = current.handsFree && alert.kind === 'decision' && onScreen;
      // Hands-free mode already reads this decision (`decisionVoice.ts`).
      const claimed = decisionVoiceClaimed(`alert:${alert.key}`);
      if (!readOnScreen && !claimed && shouldSpeak(current, alert.kind, now)) {
        speakAlert(normalizeKoreanNumbers(title, lang), current, {
          priority: 'decision',
          key: `alert:${alert.key}`,
        });
      }
      // `showOsNotification` itself shows nothing while the app is in front.
      if (current.notifications[alert.kind] && !inQuietHours(current.quietHours, now)) {
        // Only the session name (in the title) and a generic line: the
        // request text can hold commands or paths and stays in the app.
        const generic = translate(
          lang,
          alert.kind === 'decision'
            ? 'alerts.needsDecision'
            : alert.kind === 'question'
              ? 'alerts.hasQuestion'
              : 'alerts.needsInput',
        );
        showOsNotification(title, generic, alert.key, () => actions.openAlert(alert));
      }
    };
  }, [actions, store, speakAlert]);

  const updateSettings = useCallback((patch: Partial<Settings>) => {
    setSettings((s) => ({ ...s, ...patch }));
  }, []);

  const value = useMemo<StoreContextValue>(
    () => ({
      store,
      conductor: conductorStore,
      actions,
      machines,
      machine,
      switchMachine: setMachineId,
      settings,
      updateSettings,
    }),
    [store, conductorStore, actions, machines, machine, settings, updateSettings],
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

/**
 * Reads the current app state outside rendering (cleanups, event handlers),
 * where a value from `useAppState` may be one render behind.
 */
// eslint-disable-next-line react-refresh/only-export-components
export function useAppStateGetter(): () => AppState {
  const { store } = useStoreContext();
  return store.getState;
}

/** The conductor's state on the selected machine (`null`: none or not loaded). */
// eslint-disable-next-line react-refresh/only-export-components
export function useConductor(): ConductorState | null {
  const { conductor } = useStoreContext();
  return useSyncExternalStore(conductor.subscribe, conductor.get);
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
