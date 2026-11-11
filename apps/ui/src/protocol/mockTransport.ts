/**
 * In-memory simulation of a VCA Server, used for development, demos and tests.
 *
 * It implements the same request/response and push semantics a real server
 * has: per-session event logs with monotonically increasing `seq`, a bounded
 * replay buffer (so `subscribe { from_seq }` can produce `stream_gap`),
 * delivery acknowledgements, decisions, an admission queue and resource
 * snapshots. Timing is driven by `tick()`, which runs on an interval unless
 * `autoTick` is false (tests call `tick()` directly).
 */
import type {
  ClientMsg,
  ControlKey,
  Decision,
  Envelope,
  ErrorCode,
  Input,
  PairedDevice,
  QueueEntry,
  ResourceSnapshot,
  ServerMsg,
  SessionAction,
  SessionEvent,
  SessionInfo,
  SessionState,
  ShellRequest,
  SpeechProfileInfo,
  UnixMillis,
} from './types';
import { createScenario, launchScript, type MockProfile, type ScriptStep } from './mockData';
import { MockConductor, type MockConductorOptions } from './mockConductor';
import { MockPersonalTerms } from './mockPersonalTerms';
import { MockPronunciations } from './mockPronunciations';
import { MockTasks, type MockTasksOptions } from './mockTasks';
import { MockDiscovery } from './mockDiscovery';
import { mockBriefing, mockSummary } from './mockVoice';
import type { ConnectionState, Transport, Unsubscribe } from './transport';
import { PROTOCOL_VERSION, SPEECH_LLM_NONE } from './types';

/** Speech profiles of the simulated server (one of each needs a key on the PC). */
const MOCK_ENGINES: SpeechProfileInfo[] = [
  {
    id: 'whisper-local',
    label: 'Whisper (this PC)',
    kind: 'openai_compatible',
    configured: true,
    default: true,
  },
  { id: 'openai', label: 'OpenAI', kind: 'openai', configured: false, default: false },
];
const MOCK_LLMS: SpeechProfileInfo[] = [
  {
    id: 'local-llm',
    label: 'Local LLM',
    kind: 'openai_compatible',
    configured: true,
    default: false,
  },
  { id: 'claude', label: 'Claude', kind: 'anthropic', configured: false, default: false },
];
import { checkSessionName, eventPreview, needsAttention, shellLabel } from './helpers';
import { MockAttachments } from './mockAttachments';
import { MockFolderError, MockFolders } from './mockFolders';
import { MockArrange } from './mockArrange';
import { MockRestore } from './mockRestore';
import { classifyCommand, MockAudit, MockTotp } from './mockShell';
import { commandProblem, SHELLS, submissionLines } from './shell';

/** Remote shell timeouts of the simulated server (vcad defaults). */
/** Events replayed on subscribe (vcad: the newest page). */
export const MOCK_PAGE_EVENTS = 60;
const SHELL_IDLE_MS = 15 * 60_000;
const SHELL_LIFETIME_MS = 60 * 60_000;

export interface MockTransportOptions {
  profile?: MockProfile;
  /** Clock; defaults to `Date.now`. */
  now?: () => UnixMillis;
  /** Interval between simulation steps. */
  tickMs?: number;
  /** Simulated round-trip latency for requests. */
  latencyMs?: number;
  /** Number of events kept per session for replay. */
  bufferSize?: number;
  /** Run `tick()` on a timer after connecting. */
  autoTick?: boolean;
  /**
   * Remote shell support: `disabled` (shells off in the config),
   * `no_totp` (TOTP not enrolled) or `on` (default).
   */
  shells?: 'on' | 'disabled' | 'no_totp';
  /** Offer the project folder picker (default true; false acts as an older server). */
  folderPicker?: boolean;
  /** Answer `scan_sessions` (default true; false acts as an older server). */
  sessionScan?: boolean;
  /** The conductor (default: present, off); `false` acts as an older server. */
  conductor?: MockConductorOptions | false;
  /** The to-do list (default: present, seeded); `false` acts as an older server. */
  tasks?: MockTasksOptions | false;
}

interface MockSession {
  info: SessionInfo;
  events: SessionEvent[];
  nextSeq: number;
  script: ScriptStep[];
  cursor: number;
  loop: boolean;
  pendingDecision: Decision | null;
  /** Text inputs waiting for the session to become free (CMD-04). */
  inbox: string[];
  baseCpu: number;
  baseMemoryMb: number;
  processes: number;
}

const encoder = new TextEncoder();

/** Small deterministic PRNG (mulberry32). */
function prng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const KEY_ECHO: Partial<Record<ControlKey, string>> = {
  enter: '\r\n',
  up: '\x1b[A',
  down: '\x1b[B',
  right: '\x1b[C',
  left: '\x1b[D',
  tab: '\t',
  backspace: '\b \b',
};

export class MockTransport implements Transport {
  private _state: ConnectionState = 'disconnected';
  private readonly messageListeners = new Set<(e: Envelope<ServerMsg>) => void>();
  private readonly stateListeners = new Set<(s: ConnectionState) => void>();
  private readonly sessions = new Map<string, MockSession>();
  private readonly subscriptions = new Set<string>();
  /** The card order the user chose (`null`: the default, oldest first). */
  private customOrder: string[] | null = null;
  /** This client opted in to result summaries (`set_voice_summary`). */
  private summariesOn = false;
  private queue: QueueEntry[];
  private devices: PairedDevice[];
  private readonly serverName: string;
  private readonly memoryTotal: number;
  private readonly now: () => UnixMillis;
  private readonly tickMs: number;
  private readonly latencyMs: number;
  private readonly bufferSize: number;
  private readonly autoTick: boolean;
  private readonly random: () => number;
  private timer: ReturnType<typeof setInterval> | null = null;
  /** Sessions whose card fields (preview) changed since the last push. */
  private readonly dirty = new Set<string>();
  /** Sessions with delivered text waiting for the agent to react. */
  private readonly awaitingAck = new Set<string>();
  private ticks = 0;
  /** This device's speech preferences (`null`: server default). */
  private speechEngine: string | null = null;
  private speechLlm: string | null = null;
  private launchCounter = 0;
  private jobCounter = 300;
  private readonly attachments: MockAttachments;
  private readonly shells: 'on' | 'disabled' | 'no_totp';
  private readonly totp: MockTotp;
  private readonly audit: MockAudit;
  private readonly restore: MockRestore;
  readonly arrange: MockArrange;
  private readonly folders: MockFolders | null;
  private readonly discovery: MockDiscovery | null;
  /** The mock conductor (`null`: an older server without one). */
  readonly conductor: MockConductor | null;
  /** The mock to-do list (`null`: an older server without one). */
  readonly tasks: MockTasks | null;
  /** The demo PC's pronunciation dictionary. */
  readonly pronunciations: MockPronunciations;
  /** The demo PC's commonly misheard words. */
  readonly personalTerms = new MockPersonalTerms();

  constructor(options: MockTransportOptions = {}) {
    const profile = options.profile ?? 'workstation';
    this.now = options.now ?? (() => Date.now());
    this.tickMs = options.tickMs ?? 1100;
    this.latencyMs = options.latencyMs ?? 60;
    this.bufferSize = options.bufferSize ?? 500;
    this.autoTick = options.autoTick ?? true;
    this.random = prng(profile === 'workstation' ? 7 : 11);
    this.shells = options.shells ?? 'on';
    this.totp = new MockTotp(this.now);
    this.attachments = new MockAttachments(
      (id) => this.sessions.get(id)?.info,
      this.now,
      (id, text, submit) => {
        const s = this.sessions.get(id);
        if (!s) return;
        if (submit) this.deliverText(s, text, this.now());
        else this.emit(s, { type: 'output', seq: 0, at: this.now(), data: encoder.encode(text) });
      },
    );

    const start = this.now();
    const scenario = createScenario(profile, start);
    this.serverName = scenario.serverName;
    this.memoryTotal = scenario.memoryTotalBytes;
    this.queue = scenario.queue;
    this.devices = scenario.devices;
    this.audit = new MockAudit(scenario.audit);
    this.restore = new MockRestore(profile, this.now, (body) => this.push(body));
    this.arrange = new MockArrange(profile, this.now);
    this.folders = (options.folderPicker ?? true) ? new MockFolders(profile) : null;
    this.pronunciations = new MockPronunciations(this.now, (body) => this.push(body));
    this.conductor =
      options.conductor === false
        ? null
        : new MockConductor(
            this.now,
            (body) => this.push(body),
            () => [...this.sessions.values()].map((s) => s.info),
            options.conductor,
            (code) =>
              code?.trim()
                ? this.checkTotp(code.trim())
                : this.error('totp_required', 'enter a code from your authenticator app'),
            (event, detail) => this.auditEntry(event, null, detail),
          );
    this.tasks =
      options.tasks === false
        ? null
        : new MockTasks(
            this.now,
            (body) => this.push(body),
            () => [...this.sessions.values()].map((s) => s.info),
            (id, text) => {
              const s = this.sessions.get(id);
              if (!s) return false;
              this.deliverText(s, text, this.now());
              return true;
            },
            options.tasks,
          );
    this.discovery =
      (options.sessionScan ?? true)
        ? new MockDiscovery({
            now: this.now,
            list: (info) => {
              if (this.sessions.has(info.id)) return;
              this.addSession(info, launchScript(info.display_name), 6, 300);
              this.push({ type: 'session_updated', session: info });
            },
            unlist: (id) => {
              if (!this.sessions.delete(id)) return;
              this.subscriptions.delete(id);
              this.push({ type: 'session_removed', session_id: id });
            },
            error: (code, message) => this.error(code, message),
          })
        : null;

    for (const seed of scenario.sessions) {
      const session: MockSession = {
        info: { ...seed.info },
        events: [],
        nextSeq: 1,
        script: seed.script,
        cursor: 0,
        loop: seed.loop,
        pendingDecision: null,
        inbox: [],
        baseCpu: seed.baseCpu,
        baseMemoryMb: seed.baseMemoryMb,
        processes: seed.processes,
      };
      this.sessions.set(seed.info.id, session);
      // Spread history timestamps between creation and last activity.
      const span = Math.max(1, seed.info.last_activity - seed.info.created_at);
      seed.history.forEach((step, i) => {
        const at = seed.info.created_at + Math.round((span * (i + 1)) / seed.history.length);
        this.applyStep(session, step, at, { silent: true });
      });
      // History must not alter the seeded card fields.
      session.info.state = seed.info.state;
      session.info.unread = seed.info.unread;
      session.info.last_activity = seed.info.last_activity;
    }
  }

  // ---------------------------------------------------------------- Transport

  get state(): ConnectionState {
    return this._state;
  }

  async connect(): Promise<void> {
    if (this._state === 'connected') return;
    this.setState('connecting');
    await this.delay(this.latencyMs * 2);
    this.setState('connected');
    this.startTimer();
  }

  disconnect(): void {
    this.stopTimer();
    this.subscriptions.clear();
    this.setState('disconnected');
  }

  async send(envelope: Envelope<ClientMsg>): Promise<ServerMsg> {
    if (this._state !== 'connected') throw new Error('not connected');
    await this.delay(this.latencyMs);
    if (this.state !== 'connected') throw new Error('connection lost');
    return this.handle(envelope.body);
  }

  onMessage(listener: (envelope: Envelope<ServerMsg>) => void): Unsubscribe {
    this.messageListeners.add(listener);
    return () => this.messageListeners.delete(listener);
  }

  onStateChange(listener: (state: ConnectionState) => void): Unsubscribe {
    this.stateListeners.add(listener);
    return () => this.stateListeners.delete(listener);
  }

  // ------------------------------------------------------- simulation helpers

  /**
   * Simulates a network drop: pushes stop, subscriptions are lost, the
   * sessions keep producing events. With `loseHistory`, the replay buffers
   * are trimmed so that resubscribing yields a `stream_gap`.
   */
  simulateDrop(durationMs = 3000, loseHistory = false): void {
    if (this._state !== 'connected') return;
    this.subscriptions.clear();
    this.setState('reconnecting');
    // Keep the world moving while the client is away.
    const extraTicks = Math.max(1, Math.round(durationMs / this.tickMs));
    for (let i = 0; i < extraTicks; i++) this.tick();
    if (loseHistory) {
      for (const s of this.sessions.values()) s.events = s.events.slice(-3);
    }
    setTimeout(() => {
      if (this._state === 'reconnecting') this.setState('connected');
    }, durationMs);
  }

  /** Makes a session ask for a decision right now (tests). */
  /** Adds or replaces a session card as the server would announce it (tests). */
  putSession(info: SessionInfo): void {
    const s = this.sessions.get(info.id);
    if (s) s.info = { ...info };
    else this.addSession({ ...info }, [], 0, 0);
    this.push({ type: 'session_updated', session: { ...info } });
  }

  simulateDecision(sessionId: string, decision: Decision): void {
    const s = this.sessions.get(sessionId);
    if (s) this.applyStep(s, { kind: 'decision', decision }, this.now());
    this.flushUpdates();
  }

  /** Advances the simulation by one step. */
  tick(): void {
    this.ticks += 1;
    const at = this.now();
    for (const session of this.sessions.values()) {
      const st = session.info.state.type;
      const shell = session.info.shell;
      if (shell && st !== 'exited') {
        if (at >= shell.hard_deadline) this.closeShell(session, 'lifetime');
        else if (at >= shell.idle_deadline) this.closeShell(session, 'idle');
        continue;
      }
      if (st === 'starting') {
        this.setSessionState(session, { type: 'running' }, at);
        continue;
      }
      if (st !== 'running') continue;
      if (session.inbox.length > 0) {
        const text = session.inbox.shift()!;
        this.deliverText(session, text, at);
        this.push({ type: 'input_ack', session_id: session.info.id, status: 'delivered' });
        continue;
      }
      const step = this.nextStep(session);
      if (step) {
        this.applyStep(session, step, at);
        if (this.awaitingAck.delete(session.info.id)) {
          this.push({ type: 'input_ack', session_id: session.info.id, status: 'acknowledged' });
        }
      }
    }
    this.flushUpdates();
    // Admit the head of the queue every few ticks and enqueue a new job.
    if (this.ticks % 9 === 0 && this.queue.length > 0) {
      this.queue = this.queue.slice(1).map((q, i) => ({ ...q, position: i }));
    }
    if (this.ticks % 13 === 0 && this.queue.length < 4) {
      this.jobCounter += 1;
      const running = [...this.sessions.values()].filter(
        (s) => s.info.origin.type === 'local' && s.info.state.type === 'running',
      );
      const owner = running[this.jobCounter % Math.max(1, running.length)];
      this.queue.push({
        job_id: `job-${this.jobCounter}`,
        session_id: owner?.info.id ?? null,
        command: this.jobCounter % 2 ? 'cargo build --release' : 'npm test',
        class: this.jobCounter % 2 ? 'heavy' : 'light',
        queued_at: at,
        position: this.queue.length,
      });
    }
    if (this.ticks % 2 === 0) this.push({ type: 'resources', snapshot: this.snapshot() });
    this.restore.tick();
  }

  // ----------------------------------------------------------------- requests

  private handle(msg: ClientMsg): ServerMsg {
    const response = this.handleRequest(msg);
    this.flushUpdates();
    return response;
  }

  /** Pushes `session_updated` for sessions whose preview changed. */
  private flushUpdates(): void {
    for (const id of this.dirty) {
      const s = this.sessions.get(id);
      if (s) this.push({ type: 'session_updated', session: s.info });
    }
    this.dirty.clear();
  }

  private handleRequest(msg: ClientMsg): ServerMsg {
    switch (msg.type) {
      case 'hello':
        if (msg.protocol_version !== PROTOCOL_VERSION) {
          return this.error('unsupported', `protocol ${msg.protocol_version} not supported`);
        }
        return {
          type: 'welcome',
          protocol_version: PROTOCOL_VERSION,
          server_version: '0.1.0-mock',
          server_name: this.serverName,
          features: [
            ...(this.folders ? ['project_folders'] : []),
            ...(this.discovery ? ['scan_sessions'] : []),
            ...(this.conductor ? ['conductor'] : []),
            ...(this.conductor?.commandCenter ? ['command_center'] : []),
            ...(this.tasks ? ['human_tasks'] : []),
            'pronunciations',
            'personal_terms',
            'session_order',
            'window_arrange',
            'dismiss_session',
            'dismiss_stop',
            'shell_join',
            'shell_confirm_lines',
          ],
        };
      case 'ping':
        return { type: 'pong', at: msg.at };
      case 'list_sessions':
        return {
          type: 'sessions',
          sessions: this.orderedIds().map((id) => this.sessions.get(id)!.info),
        };
      case 'get_session_order':
        return this.sessionOrder();
      case 'set_session_order': {
        const ids = msg.session_ids.filter(
          (id, i, all) => this.sessions.has(id) && all.indexOf(id) === i,
        );
        const rest = this.orderedIds().filter((id) => !ids.includes(id));
        // Gone cards keep their place in the stored order.
        const gone = (this.customOrder ?? []).filter((id) => !this.sessions.has(id));
        this.customOrder = [...ids, ...rest, ...gone];
        const order = this.sessionOrder();
        this.push(order);
        return order;
      }
      case 'reset_session_order': {
        this.customOrder = null;
        const order = this.sessionOrder();
        this.push(order);
        return order;
      }
      case 'subscribe':
        return this.subscribe(msg.session_id, msg.from_seq);
      case 'unsubscribe':
        this.subscriptions.delete(msg.session_id);
        return { type: 'ok' };
      case 'history':
        return this.history(msg.session_id, msg.before_seq, msg.limit);
      case 'send_input':
        return this.input(msg.session_id, msg.input);
      case 'control':
        return this.control(msg.session_id, msg.action);
      case 'launch': {
        const r = msg.request;
        this.launchCounter += 1;
        const name =
          r.display_name ?? r.project_dir.split(/[\\/]/).filter(Boolean).pop() ?? 'session';
        const info: SessionInfo = {
          id: `new-${this.launchCounter}`,
          origin: {
            type: 'local',
            agent: r.agent,
            project_dir: r.project_dir,
            pid: 60000 + this.launchCounter,
          },
          display_name: name,
          state: { type: 'starting' },
          control: 'managed_pty',
          profile: r.profile,
          created_at: this.now(),
          last_activity: this.now(),
          unread: 0,
          preview: null,
        };
        this.addSession(info, launchScript(name), 15, 400);
        return { type: 'launched', session: info };
      }
      case 'launch_cloud': {
        const r = msg.request;
        this.launchCounter += 1;
        const info: SessionInfo = {
          id: `cloud-${this.launchCounter}`,
          origin: {
            type: 'cloud',
            provider: r.provider,
            remote_id: `remote-${this.launchCounter}`,
            url: null,
          },
          display_name: r.display_name ?? r.repository,
          state: { type: 'starting' },
          control: 'cloud',
          profile: null,
          created_at: this.now(),
          last_activity: this.now(),
          unread: 0,
          preview: null,
        };
        this.addSession(
          info,
          [
            { kind: 'msg', role: 'user', text: r.prompt },
            { kind: 'msg', role: 'assistant', text: `Cloning ${r.repository}…` },
            { kind: 'tool', name: 'plan_step', summary: 'Analyze repository' },
            { kind: 'msg', role: 'assistant', text: 'Plan ready. Working on it.' },
            { kind: 'state', state: { type: 'idle' } },
          ],
          0,
          0,
        );
        return { type: 'launched', session: info };
      }
      case 'get_resources':
        return { type: 'resources', snapshot: this.snapshot() };
      case 'reorder_queue': {
        const idx = this.queue.findIndex((q) => q.job_id === msg.job_id);
        if (idx < 0) return this.error('not_found', `job ${msg.job_id} not queued`);
        const [entry] = this.queue.splice(idx, 1);
        const pos = Math.min(msg.position, this.queue.length);
        this.queue.splice(pos, 0, entry!);
        this.queue = this.queue.map((q, i) => ({ ...q, position: i }));
        this.push({ type: 'resources', snapshot: this.snapshot() });
        return { type: 'ok' };
      }
      case 'cancel_queued': {
        const before = this.queue.length;
        this.queue = this.queue
          .filter((q) => q.job_id !== msg.job_id)
          .map((q, i) => ({ ...q, position: i }));
        if (this.queue.length === before)
          return this.error('not_found', `job ${msg.job_id} not queued`);
        this.push({ type: 'resources', snapshot: this.snapshot() });
        return { type: 'ok' };
      }
      case 'mark_read': {
        const s = this.sessions.get(msg.session_id);
        if (!s) return this.error('not_found', `session ${msg.session_id} not found`);
        if (s.info.unread !== 0) {
          s.info = { ...s.info, unread: 0 };
          this.push({ type: 'session_updated', session: s.info });
        }
        return { type: 'ok' };
      }
      case 'dismiss_session': {
        const s = this.sessions.get(msg.session_id);
        if (!s) return this.error('not_found', `session ${msg.session_id} not found`);
        if (s.info.state.type !== 'exited') {
          if (!msg.stop) return this.error('conflict', 'only ended sessions can be removed');
          if (s.info.control !== 'managed_pty')
            return this.error('unsupported', 'sessions VCA did not start cannot be stopped');
        }
        this.sessions.delete(msg.session_id);
        this.subscriptions.delete(msg.session_id);
        this.push({ type: 'session_removed', session_id: msg.session_id });
        return { type: 'ok' };
      }
      case 'shell_keep_alive': {
        const s = this.sessions.get(msg.session_id);
        if (!s?.info.shell) return this.error('not_found', `no shell ${msg.session_id}`);
        return { type: 'ok' };
      }
      case 'rename_session': {
        const s = this.sessions.get(msg.session_id);
        if (!s) return this.error('not_found', `session ${msg.session_id} not found`);
        const { name, problem } = checkSessionName(msg.name ?? '');
        if (problem === 'control_chars')
          return this.error('invalid_request', 'the name must not contain control characters');
        if (problem === 'too_long')
          return this.error('invalid_request', 'the name is longer than 80 characters');
        const auto = s.info.auto_name ?? s.info.display_name;
        s.info = {
          ...s.info,
          auto_name: auto,
          display_name: name ?? auto,
          custom_name: name !== null,
        };
        this.push({ type: 'session_updated', session: s.info });
        return { type: 'ok' };
      }
      case 'list_devices':
        return { type: 'devices', devices: this.devices };
      case 'revoke_device': {
        const dev = this.devices.find((d) => d.id === msg.device_id);
        if (!dev) return this.error('not_found', 'unknown device');
        if (dev.current) return this.error('forbidden', 'cannot revoke the current device');
        this.devices = this.devices.filter((d) => d.id !== msg.device_id);
        return { type: 'devices', devices: this.devices };
      }
      // Device management mirrors the server: admin only, trimmed 1-64
      // character names, no change of the current device's own scope.
      case 'rename_device':
      case 'set_device_scope': {
        const self = this.devices.find((d) => d.current);
        if (self?.scope && self.scope !== 'admin')
          return this.error('forbidden', 'this device is not allowed to do that');
        const dev = this.devices.find((d) => d.id === msg.device_id);
        if (!dev) return this.error('not_found', `no active device ${msg.device_id}`);
        let patch: Partial<PairedDevice>;
        if (msg.type === 'rename_device') {
          const name = msg.name.trim();
          // eslint-disable-next-line no-control-regex
          if ([...name].length < 1 || [...name].length > 64 || /[\u0000-\u001f\u007f]/.test(name))
            return this.error('invalid_request', 'a device name has 1 to 64 characters');
          patch = { name };
        } else {
          if (dev.current)
            return this.error('forbidden', 'a device cannot change its own permission');
          patch = { scope: msg.scope };
        }
        this.devices = this.devices.map((d) => (d.id === dev.id ? { ...d, ...patch } : d));
        return { type: 'devices', devices: this.devices };
      }
      case 'kill_switch':
        // Respond first, then drop every connection.
        setTimeout(() => this.disconnect(), 0);
        return { type: 'ok' };
      case 'list_cloud_providers':
        return { type: 'cloud_providers', providers: [] };
      case 'list_speech_options':
        return this.speechOptions();
      case 'set_speech_preferences': {
        const engine = msg.engine ? MOCK_ENGINES.find((e) => e.id === msg.engine) : null;
        if (msg.engine && !engine) return this.error('not_found', `no engine ${msg.engine}`);
        const llm =
          msg.llm && msg.llm !== SPEECH_LLM_NONE ? MOCK_LLMS.find((l) => l.id === msg.llm) : null;
        if (msg.llm && msg.llm !== SPEECH_LLM_NONE && !llm) {
          return this.error('not_found', `no correction LLM ${msg.llm}`);
        }
        const unconfigured = [engine, llm].find((p) => p && !p.configured);
        if (unconfigured) {
          return this.error(
            'invalid_request',
            `speech profile "${unconfigured.id}" is not configured: add the key on the PC with \`vca speech login ${unconfigured.id}\``,
          );
        }
        this.speechEngine = msg.engine;
        this.speechLlm = msg.llm;
        return this.speechOptions();
      }
      case 'transcribe':
        return {
          type: 'transcript',
          text: 'Run the tests',
          raw_text: 'run the tests',
          language: msg.language,
          corrections: [],
          intent: { type: 'prompt' },
          engine: 'mock',
          // Like vcad with [speech.llm]: a topic once a final chunk was seen.
          topic: msg.final ? 'Testing' : null,
        };
      case 'attach':
      case 'attach_begin':
      case 'attach_chunk':
      case 'attach_end':
      case 'attach_cancel':
      case 'attach_folder_begin':
      case 'attach_folder_end':
      case 'attach_batch_end':
        return this.attachments.handle(msg);
      case 'open_shell':
        return this.openShell(msg.request);
      case 'shell_run':
        return this.shellRun(msg.session_id, msg.command, msg.totp, msg.confirm === true);
      case 'shell_join': {
        // One mock device: it opened every shell, so there is nothing to join.
        const denied = this.requireAdmin() ?? this.shellsAvailable();
        if (denied) return denied;
        return { type: 'ok' };
      }
      case 'classify_command':
        return { type: 'command_danger', ...classifyCommand(msg.command) };
      case 'list_audit': {
        const denied = this.requireAdmin();
        if (denied) return denied;
        return { type: 'audit', entries: this.audit.list(msg.limit) };
      }
      case 'restore_list':
        return this.restore.list();
      case 'restore_capture':
      case 'restore_edit':
      case 'restore_import': {
        const denied = this.requireAdmin();
        if (denied) return denied;
        const reply = this.restore.handle(msg);
        if (reply.type === 'restore_list') {
          this.auditEntry('restore_edit', null, msg.type);
        }
        return reply;
      }
      case 'restore_run': {
        const denied = this.requireAdmin();
        if (denied) return denied;
        if (!msg.request.dry_run && !this.totp.inStepUp()) {
          if (!msg.request.totp) return this.error('totp_required', 'a restore needs a TOTP code');
          const bad = this.checkTotp(msg.request.totp);
          if (bad) return bad;
        }
        const reply = this.restore.run(msg.request);
        if (reply.type === 'restore_progress' && !msg.request.dry_run) {
          this.auditEntry('restore_run', null, `profile=${reply.job.profile}`);
        }
        return reply;
      }
      case 'arrange_list':
        return this.arrange.list();
      case 'arrange_apply':
      case 'arrange_apply_layout':
      case 'arrange_undo':
      case 'arrange_edit': {
        const denied = this.requireAdmin();
        if (denied) return denied;
        const reply =
          msg.type === 'arrange_apply'
            ? this.arrange.apply(msg.monitors)
            : msg.type === 'arrange_apply_layout'
              ? this.arrange.applyLayout(msg.layout)
              : msg.type === 'arrange_undo'
                ? this.arrange.undo()
                : this.arrange.edit(msg.edit);
        if (reply.type !== 'error') this.auditEntry('window_arrange', null, msg.type);
        return reply;
      }
      // The demo PC lists one engine with two speakers; it makes no audio, so
      // spoken alerts end up on the device voice.
      case 'list_tts_engines':
        return {
          type: 'tts_engines',
          engines: [
            {
              name: 'demo-voice',
              languages: [],
              default: true,
              speakers: [
                { id: 'F1', label: 'Female 1', gender: 'female' },
                { id: 'M1', label: 'Male 1', gender: 'male' },
              ],
              disclosure: null,
            },
          ],
        };
      case 'synthesize':
        return this.error('unsupported', 'spoken alerts from the PC are off');
      case 'briefing':
        return mockBriefing(
          [...this.sessions.values()].map((s) => s.info),
          msg.order ?? [],
          msg.language,
        );
      case 'set_voice_summary':
        this.summariesOn = msg.enabled;
        return { type: 'ok' };
      case 'summarize':
        if (!this.summariesOn) {
          return this.error('forbidden', 'reading work results aloud is off on this device');
        }
        if (!this.sessions.has(msg.session_id)) {
          return this.error('not_found', `no session ${msg.session_id}`);
        }
        return mockSummary(msg.session_id, msg.number, msg.language);
      case 'focus_terminal':
        // There is no real window here: a known session "comes forward".
        return this.sessions.has(msg.session_id)
          ? { type: 'ok' }
          : this.error('not_found', 'that session is not running in a terminal on this computer');
      case 'project_folders':
      case 'list_folder':
      case 'folder_path':
        return this.folderRequest(msg);
      case 'scan_sessions':
      case 'hide_observed':
      case 'unhide_observed':
        // An older server does not know these requests.
        return this.discovery
          ? this.discovery.handle(msg)
          : this.error('invalid_request', `unknown message ${msg.type}`);
      case 'conductor_get':
      case 'conductor_configure':
      case 'conductor_start':
      case 'conductor_stop':
      case 'conductor_decide':
      case 'conductor_allow_session':
      case 'conductor_ask':
      case 'conductor_trust_folder':
      case 'cc_pause':
        return this.conductor
          ? this.conductor.handle(msg)
          : this.error('invalid_request', `unknown message ${msg.type}`);
      case 'task_list':
      case 'task_create':
      case 'task_update':
      case 'task_send_result':
      case 'task_open_admin_shell':
      case 'task_export':
      case 'task_digest':
      case 'task_configure':
        return this.tasks
          ? this.tasks.handle(msg)
          : this.error('invalid_request', `unknown message ${msg.type}`);
      case 'list_pronunciations':
      case 'set_pronunciation':
      case 'delete_pronunciation':
        return this.pronunciations.handle(msg);
      case 'list_personal_terms':
      case 'save_personal_terms':
        return this.personalTerms.handle(msg);
    }
  }

  // ------------------------------------------------------------ folder picker

  private folderRequest(
    msg: Extract<ClientMsg, { type: 'project_folders' | 'list_folder' | 'folder_path' }>,
  ): ServerMsg {
    // An older server does not know these requests.
    if (!this.folders) return this.error('invalid_request', `unknown message ${msg.type}`);
    try {
      switch (msg.type) {
        case 'project_folders':
          return { type: 'project_folders', ...this.folders.projectFolders(this.now()) };
        case 'list_folder':
          return { type: 'folder', ...this.folders.listFolder(msg.path, msg.show_hidden) };
        case 'folder_path':
          return { type: 'folder_path', ...this.folders.folderPath(msg.parent, msg.name) };
      }
    } catch (err) {
      if (err instanceof MockFolderError) return this.error(err.code, err.message);
      throw err;
    }
  }

  // ------------------------------------------------------------ remote shells

  /** The device this client is paired as. */
  private self(): PairedDevice | undefined {
    return this.devices.find((d) => d.current);
  }

  private requireAdmin(): ServerMsg | null {
    const scope = this.self()?.scope;
    return scope && scope !== 'admin'
      ? this.error('forbidden', 'remote shells need a device paired with admin permission')
      : null;
  }

  private shellsAvailable(): ServerMsg | null {
    if (this.shells === 'disabled') return this.error('unsupported', 'remote shells are disabled');
    if (this.shells === 'no_totp')
      return this.error('unsupported', 'TOTP is not enrolled on the server');
    return null;
  }

  /** Adds an audit entry as vcad would (tests and the demo: conductor actions). */
  recordAudit(event: string, sessionId: string | null, detail: string): void {
    this.auditEntry(event, sessionId, detail);
  }

  private auditEntry(event: string, sessionId: string | null, detail: string): void {
    const self = this.self();
    this.audit.add({
      ts: this.now(),
      device_id: self?.id ?? 'local',
      device_name: self?.name ?? 'local',
      event,
      session_id: sessionId,
      detail,
    });
  }

  /** Checks a TOTP code; `null` when it is good. */
  private checkTotp(code: string): ServerMsg | null {
    const verdict = this.totp.verify(code);
    if (verdict === 'ok') {
      this.auditEntry('step_up_ok', null, '');
      return null;
    }
    this.auditEntry('step_up_failed', null, verdict === 'locked' ? 'locked' : 'wrong code');
    return verdict === 'locked'
      ? this.error('forbidden', 'too many wrong codes; try again in 5 minutes')
      : this.error('totp_invalid', 'the code is wrong or was already used');
  }

  private openShell(r: ShellRequest): ServerMsg {
    const problem = this.requireAdmin() ?? this.shellsAvailable();
    if (problem) return problem;
    if (r.shell === 'pwsh') return this.error('unavailable', 'pwsh is not installed');
    const known = r.shell === 'default' || [...SHELLS.windows, ...SHELLS.unix].includes(r.shell);
    if (!known) return this.error('invalid_request', `unknown shell ${r.shell}`);
    if (!r.project_dir.trim()) return this.error('invalid_request', 'project_dir is empty');
    if (!r.totp.trim())
      return this.error('totp_required', 'enter a code from your authenticator app');
    const bad = this.checkTotp(r.totp);
    if (bad) return bad;
    this.launchCounter += 1;
    const name = r.shell === 'default' ? 'bash' : r.shell;
    const at = this.now();
    const folder = r.project_dir.split(/[\\/]/).filter(Boolean).pop() ?? r.project_dir;
    const info: SessionInfo = {
      id: `shell-${this.launchCounter}`,
      origin: {
        type: 'local',
        agent: { type: 'shell', name },
        project_dir: r.project_dir,
        pid: 61000 + this.launchCounter,
      },
      display_name: r.display_name ?? `${folder} · ${shellLabel(name)}`,
      state: { type: 'idle' },
      control: 'managed_pty',
      profile: null,
      created_at: at,
      last_activity: at,
      unread: 0,
      preview: null,
      shell: {
        idle_timeout_secs: SHELL_IDLE_MS / 1000,
        idle_deadline: at + SHELL_IDLE_MS,
        hard_deadline: at + SHELL_LIFETIME_MS,
        opened_by: this.self()?.name ?? 'local',
      },
    };
    this.addSession(info, [], 1, 30);
    this.emit(this.sessions.get(info.id)!, {
      type: 'output',
      seq: 0,
      at,
      data: encoder.encode(`${this.shellPrompt(info)}`),
    });
    this.auditEntry('shell_open', info.id, `shell=${name} dir=${r.project_dir}`);
    setTimeout(() => this.shellNotice(info.id, 'opened', null), 0);
    return { type: 'launched', session: info };
  }

  private shellPrompt(info: SessionInfo): string {
    const dir = info.origin.type === 'local' ? info.origin.project_dir : '';
    return `\x1b[32m${dir}\x1b[0m $ `;
  }

  private shellNotice(sessionId: string, event: 'opened' | 'closed', reason: string | null) {
    this.push({
      type: 'shell_notice',
      session_id: sessionId,
      event,
      device_name: this.self()?.name ?? 'local',
      reason,
    });
  }

  private isShell(s: MockSession): boolean {
    return s.info.origin.type === 'local' && s.info.origin.agent.type === 'shell';
  }

  /** Input moves the idle deadline (never past the hard deadline). */
  private touchShell(s: MockSession, at: UnixMillis): void {
    const shell = s.info.shell;
    if (!shell) return;
    s.info = {
      ...s.info,
      shell: { ...shell, idle_deadline: Math.min(at + SHELL_IDLE_MS, shell.hard_deadline) },
    };
    this.dirty.add(s.info.id);
  }

  private closeShell(s: MockSession, reason: string): void {
    if (s.info.state.type === 'exited') return;
    this.setSessionState(s, { type: 'exited', code: reason === 'user' ? 130 : 0 }, this.now());
    this.auditEntry('shell_close', s.info.id, `reason=${reason}`);
    this.shellNotice(s.info.id, 'closed', reason);
  }

  /** Types a line into a shell and prints a canned result. */
  private typeIntoShell(s: MockSession, command: string, at: UnixMillis): void {
    this.touchShell(s, at);
    const reply = /^\s*(ls|dir)\b/.test(command)
      ? 'README.md  package.json  src  tests\r\n'
      : /^\s*(pwd|cd)\s*$/.test(command)
        ? `${s.info.origin.type === 'local' ? s.info.origin.project_dir : ''}\r\n`
        : /^\s*git\s+status\b/.test(command)
          ? 'On branch main\r\nnothing to commit, working tree clean\r\n'
          : '';
    this.emit(s, {
      type: 'output',
      seq: 0,
      at,
      data: encoder.encode(`${command}\r\n${reply}${this.shellPrompt(s.info)}`),
    });
  }

  private shellRun(
    sessionId: string,
    command: string,
    totp: string | null,
    confirm = false,
  ): ServerMsg {
    const denied = this.requireAdmin() ?? this.shellsAvailable();
    if (denied) return denied;
    const s = this.sessions.get(sessionId);
    if (!s) return this.error('not_found', `session ${sessionId} not found`);
    if (!this.isShell(s)) return this.error('invalid_request', 'not a shell session');
    if (s.info.state.type === 'exited') return this.error('conflict', 'the shell was closed');
    const lines = submissionLines(command);
    const problem = commandProblem(lines.join(' '));
    if (problem) return this.error('invalid_request', 'a non-empty command, at most 8 KiB');
    if (lines.length > 1 && !confirm) {
      return { type: 'shell_confirm', session_id: sessionId, command, lines: lines.length };
    }
    // Every line is classified; the most dangerous one decides.
    const rank = { safe: 0, caution: 1, dangerous: 2 } as const;
    const level = lines
      .map((line) => classifyCommand(line).level)
      .reduce((a, b) => (rank[b] > rank[a] ? b : a), classifyCommand(command).level);
    if (level === 'dangerous' && !this.totp.inStepUp()) {
      if (!totp) return this.error('totp_required', 'a dangerous command needs a TOTP code');
      const bad = this.checkTotp(totp);
      if (bad) return bad;
    }
    const at = this.now();
    for (const line of lines) this.typeIntoShell(s, line, at);
    const detail = lines.length > 1 ? ` lines=${lines.length}` : '';
    this.auditEntry('shell_run', sessionId, `danger=${level}${detail}`);
    return { type: 'shell_ran', session_id: sessionId, danger: level };
  }

  private speechOptions(): ServerMsg {
    const llm = this.speechLlm === SPEECH_LLM_NONE ? null : this.speechLlm;
    return {
      type: 'speech_options',
      engines: MOCK_ENGINES,
      llms: MOCK_LLMS,
      current_engine: this.speechEngine ?? 'whisper-local',
      current_llm: llm,
    };
  }

  private subscribe(sessionId: string, fromSeq: number | null): ServerMsg {
    const s = this.sessions.get(sessionId);
    if (!s) return this.error('not_found', `session ${sessionId} not found`);
    this.subscriptions.add(sessionId);
    const firstAvailable = s.events[0]?.seq ?? s.nextSeq;
    // Like vcad: without `from_seq` only new events are streamed.
    let replay = fromSeq === null ? [] : s.events.filter((e) => e.seq > fromSeq);
    let gap = fromSeq !== null && fromSeq + 1 < firstAvailable;
    // Like vcad: only the newest page is replayed; earlier ones come by `history`.
    let gapAt = firstAvailable;
    if (replay.length > MOCK_PAGE_EVENTS) {
      replay = replay.slice(replay.length - MOCK_PAGE_EVENTS);
      gap = true;
      gapAt = replay[0]!.seq;
    }
    // Replay after the response so the client sees `ok` first.
    setTimeout(() => {
      if (!this.subscriptions.has(sessionId)) return;
      if (gap) {
        this.push({
          type: 'stream_gap',
          session_id: sessionId,
          first_available_seq: gapAt,
        });
      }
      for (const event of replay) this.push({ type: 'event', session_id: sessionId, event });
    }, 0);
    return { type: 'ok' };
  }

  private history(sessionId: string, before: number | null, limit: number): ServerMsg {
    const s = this.sessions.get(sessionId);
    if (!s) return this.error('not_found', `session ${sessionId} not found`);
    const older = before === null ? s.events : s.events.filter((e) => e.seq < before);
    const size = Math.max(1, Math.min(limit, 500));
    const events = older.slice(Math.max(0, older.length - size));
    return {
      type: 'history',
      session_id: sessionId,
      events,
      has_more: older.length > events.length,
    };
  }

  private input(sessionId: string, input: Input): ServerMsg {
    const s = this.sessions.get(sessionId);
    if (!s) return this.error('not_found', `session ${sessionId} not found`);
    const at = this.now();
    const st = s.info.state;
    if (s.info.control === 'observe_only')
      return this.error('unsupported', 'session is observe-only');
    if (st.type === 'exited') return this.error('conflict', 'session has exited');

    if (this.isShell(s)) {
      const denied = this.requireAdmin();
      if (denied) return denied;
      if (input.type === 'text') {
        const lines = submissionLines(input.text);
        if (lines.length > 1) {
          return {
            type: 'shell_confirm',
            session_id: sessionId,
            command: input.text,
            lines: lines.length,
          };
        }
        if (classifyCommand(input.text).level === 'dangerous' && !this.totp.inStepUp())
          return this.error('totp_required', 'dangerous input needs a TOTP code');
        if (input.submit) this.typeIntoShell(s, input.text, at);
        else {
          this.touchShell(s, at);
          this.emit(s, { type: 'output', seq: 0, at, data: encoder.encode(input.text) });
        }
        return { type: 'input_ack', session_id: sessionId, status: 'delivered' };
      }
      this.touchShell(s, at);
    }

    switch (input.type) {
      case 'text': {
        if (st.type === 'running' || st.type === 'starting' || st.type === 'paused') {
          s.inbox.push(input.text);
          return { type: 'input_ack', session_id: sessionId, status: 'queued' };
        }
        this.deliverText(s, input.text, at);
        return { type: 'input_ack', session_id: sessionId, status: 'delivered' };
      }
      case 'key': {
        if (s.info.origin.type === 'cloud')
          return this.error('unsupported', 'cloud sessions have no terminal');
        if (input.key === 'interrupt' || input.key === 'escape') {
          this.emit(s, {
            type: 'output',
            seq: 0,
            at,
            data: encoder.encode(input.key === 'interrupt' ? '^C\r\n' : ''),
          });
          if (input.key === 'interrupt' && st.type === 'running') {
            this.emit(s, {
              type: 'message',
              seq: 0,
              at,
              role: 'system',
              text: 'Interrupted by user.',
            });
            this.setSessionState(s, { type: 'awaiting_input' }, at);
          }
        } else {
          const echo = KEY_ECHO[input.key];
          if (echo) this.emit(s, { type: 'output', seq: 0, at, data: encoder.encode(echo) });
        }
        return { type: 'input_ack', session_id: sessionId, status: 'delivered' };
      }
      case 'choose': {
        const d = s.pendingDecision;
        if (!d || d.id !== input.decision_id)
          return this.error('not_found', 'no such pending decision');
        const option = d.options.find((o) => o.id === input.option_id);
        if (!option) return this.error('invalid_request', 'unknown option');
        s.pendingDecision = null;
        this.emit(s, { type: 'message', seq: 0, at, role: 'user', text: `Chose: ${option.label}` });
        this.setSessionState(
          s,
          option.id === 'no' ? { type: 'awaiting_input' } : { type: 'running' },
          at,
        );
        return { type: 'input_ack', session_id: sessionId, status: 'delivered' };
      }
      case 'answer': {
        // Same checks as crates/vca-server/src/questions.rs.
        const d = s.pendingDecision;
        if (!d || d.id !== input.decision_id || !d.questions?.length)
          return this.error('conflict', 'that question is no longer pending');
        const questions = d.questions;
        if (input.answers.length !== questions.length)
          return this.error('invalid_request', 'answer every question');
        const lines: string[] = [];
        for (const [i, q] of questions.entries()) {
          const a = input.answers[i]!;
          if (a.options.some((o) => o < 0 || o >= q.options.length))
            return this.error('invalid_request', `question ${i + 1} has no such option`);
          const picked = a.options.map((o) => q.options[o]!.label);
          if (a.text) picked.push(a.text);
          if (picked.length === 0 || (!q.multi_select && picked.length !== 1))
            return this.error('invalid_request', `question ${i + 1} needs one answer`);
          lines.push(`${q.header || q.question}: ${picked.join(', ')}`);
        }
        s.pendingDecision = null;
        this.emit(s, { type: 'message', seq: 0, at, role: 'user', text: lines.join('\n') });
        this.setSessionState(s, { type: 'running' }, at);
        return { type: 'input_ack', session_id: sessionId, status: 'delivered' };
      }
    }
  }

  private control(sessionId: string, action: SessionAction): ServerMsg {
    const s = this.sessions.get(sessionId);
    if (!s) return this.error('not_found', `session ${sessionId} not found`);
    // Same rules as SessionManager::control (crates/vca-server/src/sessions.rs).
    if (s.info.control === 'cloud' && action !== 'stop') {
      return this.error('unsupported', 'cloud sessions can only be stopped');
    }
    if (s.info.control !== 'cloud' && s.info.control !== 'managed_pty') {
      return this.error(
        'unsupported',
        action === 'restart'
          ? 'only managed sessions can be restarted'
          : 'sessions VCA did not start cannot be stopped from VCA',
      );
    }
    const at = this.now();
    const st = s.info.state.type;
    switch (action) {
      case 'pause':
        if (st === 'exited') return this.error('conflict', 'session has exited');
        this.setSessionState(s, { type: 'paused' }, at);
        break;
      case 'resume':
        if (st !== 'paused') return this.error('conflict', 'session is not paused');
        this.setSessionState(s, { type: 'running' }, at);
        break;
      case 'stop':
        s.pendingDecision = null;
        if (this.isShell(s)) this.closeShell(s, 'user');
        else this.setSessionState(s, { type: 'exited', code: 130 }, at);
        break;
      case 'restart':
        if (this.isShell(s)) return this.error('unsupported', 'shells cannot be restarted');
        s.pendingDecision = null;
        s.cursor = 0;
        this.setSessionState(s, { type: 'starting' }, at);
        break;
    }
    return { type: 'ok' };
  }

  // ---------------------------------------------------------------- internals

  private addSession(info: SessionInfo, script: ScriptStep[], cpu: number, memMb: number): void {
    this.sessions.set(info.id, {
      info,
      events: [],
      nextSeq: 1,
      script,
      cursor: 0,
      loop: false,
      pendingDecision: null,
      inbox: [],
      baseCpu: cpu,
      baseMemoryMb: memMb,
      processes: cpu > 0 ? 2 : 0,
    });
    this.emit(this.sessions.get(info.id)!, {
      type: 'state_changed',
      seq: 0,
      at: info.created_at,
      state: info.state,
    });
  }

  private nextStep(s: MockSession): ScriptStep | null {
    if (s.cursor >= s.script.length) {
      if (!s.loop || s.script.length === 0) {
        this.setSessionState(s, { type: 'idle' }, this.now());
        return null;
      }
      s.cursor = 0;
    }
    return s.script[s.cursor++] ?? null;
  }

  private deliverText(s: MockSession, text: string, at: UnixMillis): void {
    this.awaitingAck.add(s.info.id);
    if (s.info.origin.type === 'local') {
      this.emit(s, {
        type: 'output',
        seq: 0,
        at,
        data: encoder.encode(`\x1b[1m>\x1b[0m ${text}\r\n`),
      });
    }
    this.emit(s, { type: 'message', seq: 0, at, role: 'user', text });
    if (s.info.state.type !== 'running') {
      if (s.cursor >= s.script.length) s.cursor = 0;
      this.setSessionState(s, { type: 'running' }, at);
    }
  }

  private applyStep(
    s: MockSession,
    step: ScriptStep,
    at: UnixMillis,
    opts: { silent?: boolean } = {},
  ): void {
    switch (step.kind) {
      case 'out':
        this.emit(s, { type: 'output', seq: 0, at, data: encoder.encode(step.text) }, opts.silent);
        break;
      case 'msg':
        this.emit(
          s,
          { type: 'message', seq: 0, at, role: step.role, text: step.text },
          opts.silent,
        );
        if (step.role === 'assistant' && s.info.origin.type === 'local') {
          this.emit(
            s,
            {
              type: 'output',
              seq: 0,
              at,
              data: encoder.encode(`\x1b[35m⏺\x1b[0m ${step.text}\r\n`),
            },
            opts.silent,
          );
        }
        break;
      case 'tool':
        this.emit(
          s,
          { type: 'tool_call', seq: 0, at, name: step.name, summary: step.summary },
          opts.silent,
        );
        break;
      case 'notify':
        this.emit(
          s,
          { type: 'notification', seq: 0, at, title: step.title, body: step.body },
          opts.silent,
        );
        if (!opts.silent) this.bumpUnread(s);
        break;
      case 'decision':
        s.pendingDecision = step.decision;
        s.info = { ...s.info, pending_decision: { decision: step.decision, at } };
        this.emit(
          s,
          { type: 'decision_requested', seq: 0, at, decision: step.decision },
          opts.silent,
        );
        if (s.info.origin.type === 'local') {
          this.emit(
            s,
            {
              type: 'output',
              seq: 0,
              at,
              data: encoder.encode(
                `\x1b[33m?\x1b[0m ${step.decision.prompt.replace(/\n/g, '\r\n  ')}\r\n` +
                  step.decision.options.map((o, i) => `  ${i + 1}. ${o.label}`).join('\r\n') +
                  '\r\n',
              ),
            },
            opts.silent,
          );
        }
        // setSessionState bumps the unread counter for attention states.
        this.setSessionState(s, { type: 'awaiting_decision' }, at, opts.silent);
        break;
      case 'state':
        this.setSessionState(s, step.state, at, opts.silent);
        break;
    }
  }

  private bumpUnread(s: MockSession): void {
    s.info = { ...s.info, unread: s.info.unread + 1 };
    this.push({ type: 'session_updated', session: s.info });
  }

  private setSessionState(
    s: MockSession,
    state: SessionState,
    at: UnixMillis,
    silent = false,
  ): void {
    if (JSON.stringify(s.info.state) === JSON.stringify(state)) return;
    const unread = !silent && needsAttention(state) ? s.info.unread + 1 : s.info.unread;
    s.info = {
      ...s.info,
      state,
      last_activity: at,
      unread,
      pending_decision: state.type === 'awaiting_decision' ? s.info.pending_decision : null,
    };
    this.emit(s, { type: 'state_changed', seq: 0, at, state }, silent);
    if (!silent) {
      this.dirty.delete(s.info.id);
      this.push({ type: 'session_updated', session: s.info });
    }
  }

  /** Appends an event with the next seq, trims the buffer and pushes it to subscribers. */
  private emit(s: MockSession, event: SessionEvent, silent = false): void {
    const stamped = { ...event, seq: s.nextSeq++ } as SessionEvent;
    s.events.push(stamped);
    if (s.events.length > this.bufferSize) s.events.splice(0, s.events.length - this.bufferSize);
    const preview = eventPreview(stamped) ?? s.info.preview;
    s.info = { ...s.info, preview, last_activity: Math.max(s.info.last_activity, event.at) };
    if (!silent) this.dirty.add(s.info.id);
    if (!silent && this.subscriptions.has(s.info.id)) {
      this.push({ type: 'event', session_id: s.info.id, event: stamped });
    }
  }

  private snapshot(): ResourceSnapshot {
    const wave = (period: number, phase = 0) => Math.sin((this.ticks + phase) / period);
    const sessions = [...this.sessions.values()]
      .filter((s) => s.info.origin.type === 'local' && s.info.state.type !== 'exited')
      .map((s) => {
        const active = s.info.state.type === 'running' || s.info.state.type === 'starting';
        const cpu = active ? s.baseCpu * (0.8 + 0.4 * this.random()) : s.baseCpu * 0.05;
        return {
          session_id: s.info.id,
          cpu_percent: Math.round(cpu * 10) / 10,
          memory_bytes: Math.round(s.baseMemoryMb * (0.95 + 0.1 * this.random())) * 1024 * 1024,
          process_count: s.processes,
          monitor_only: s.info.control !== 'managed_pty',
        };
      });
    const sessionCpu = sessions.reduce((a, u) => a + u.cpu_percent, 0) / 4;
    const sessionMem = sessions.reduce((a, u) => a + u.memory_bytes, 0);
    return {
      at: this.now(),
      cpu_percent: Math.min(100, Math.round((8 + sessionCpu + 10 * wave(5)) * 10) / 10),
      memory_used_bytes: Math.min(
        this.memoryTotal,
        Math.round(this.memoryTotal * 0.35 + sessionMem + 1e9 * wave(7, 2)),
      ),
      memory_total_bytes: this.memoryTotal,
      disk_read_bytes_per_sec: Math.round(Math.abs(wave(3)) * 80e6 + 2e6),
      disk_write_bytes_per_sec: Math.round(Math.abs(wave(4, 1)) * 45e6 + 1e6),
      net_rx_bytes_per_sec: Math.round(Math.abs(wave(6, 3)) * 6e6 + 1e5),
      net_tx_bytes_per_sec: Math.round(Math.abs(wave(5, 4)) * 2e6 + 5e4),
      sessions,
      queue: this.queue,
    };
  }

  private error(code: ErrorCode, message: string): ServerMsg {
    return { type: 'error', code, message };
  }

  /** Session ids in the card order: the chosen order, then the rest oldest first. */
  private orderedIds(): string[] {
    const ids = [...this.sessions.keys()];
    const order = this.customOrder;
    if (!order) return ids;
    const rank = (id: string) => {
      const i = order.indexOf(id);
      return i < 0 ? Number.MAX_SAFE_INTEGER : i;
    };
    return ids
      .map((id, i) => ({ id, i }))
      .sort((a, b) => rank(a.id) - rank(b.id) || a.i - b.i)
      .map((x) => x.id);
  }

  private sessionOrder(): ServerMsg {
    return {
      type: 'session_order',
      session_ids: this.orderedIds(),
      custom: this.customOrder !== null,
    };
  }

  private push(body: ServerMsg): void {
    if (this._state !== 'connected') return;
    const envelope = { id: 0, body };
    for (const listener of this.messageListeners) listener(envelope);
  }

  private setState(state: ConnectionState): void {
    this._state = state;
    for (const listener of this.stateListeners) listener(state);
  }

  private startTimer(): void {
    if (!this.autoTick || this.timer) return;
    this.timer = setInterval(() => {
      if (this._state === 'connected') this.tick();
    }, this.tickMs);
  }

  private stopTimer(): void {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
  }

  private delay(ms: number): Promise<void> {
    return ms <= 0 ? Promise.resolve() : new Promise((r) => setTimeout(r, ms));
  }
}
