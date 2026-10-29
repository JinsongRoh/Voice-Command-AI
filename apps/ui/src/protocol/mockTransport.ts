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
  UnixMillis,
} from './types';
import { createScenario, launchScript, type MockProfile, type ScriptStep } from './mockData';
import type { ConnectionState, Transport, Unsubscribe } from './transport';
import { PROTOCOL_VERSION } from './types';
import { eventPreview, needsAttention } from './helpers';

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
  private launchCounter = 0;
  private jobCounter = 300;

  constructor(options: MockTransportOptions = {}) {
    const profile = options.profile ?? 'workstation';
    this.now = options.now ?? (() => Date.now());
    this.tickMs = options.tickMs ?? 1100;
    this.latencyMs = options.latencyMs ?? 60;
    this.bufferSize = options.bufferSize ?? 500;
    this.autoTick = options.autoTick ?? true;
    this.random = prng(profile === 'workstation' ? 7 : 11);

    const start = this.now();
    const scenario = createScenario(profile, start);
    this.serverName = scenario.serverName;
    this.memoryTotal = scenario.memoryTotalBytes;
    this.queue = scenario.queue;
    this.devices = scenario.devices;

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

  /** Advances the simulation by one step. */
  tick(): void {
    this.ticks += 1;
    const at = this.now();
    for (const session of this.sessions.values()) {
      const st = session.info.state.type;
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
        };
      case 'ping':
        return { type: 'pong', at: msg.at };
      case 'list_sessions':
        return { type: 'sessions', sessions: [...this.sessions.values()].map((s) => s.info) };
      case 'subscribe':
        return this.subscribe(msg.session_id, msg.from_seq);
      case 'unsubscribe':
        this.subscriptions.delete(msg.session_id);
        return { type: 'ok' };
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
      case 'list_devices':
        return { type: 'devices', devices: this.devices };
      case 'revoke_device': {
        const dev = this.devices.find((d) => d.id === msg.device_id);
        if (!dev) return this.error('not_found', 'unknown device');
        if (dev.current) return this.error('forbidden', 'cannot revoke the current device');
        this.devices = this.devices.filter((d) => d.id !== msg.device_id);
        return { type: 'devices', devices: this.devices };
      }
      case 'kill_switch':
        // Respond first, then drop every connection.
        setTimeout(() => this.disconnect(), 0);
        return { type: 'ok' };
      case 'list_cloud_providers':
        return { type: 'cloud_providers', providers: [] };
      case 'transcribe':
        return {
          type: 'transcript',
          text: 'Run the tests',
          raw_text: 'run the tests',
          language: msg.language,
          corrections: [],
          intent: { type: 'prompt' },
          engine: 'mock',
        };
    }
  }

  private subscribe(sessionId: string, fromSeq: number | null): ServerMsg {
    const s = this.sessions.get(sessionId);
    if (!s) return this.error('not_found', `session ${sessionId} not found`);
    this.subscriptions.add(sessionId);
    const firstAvailable = s.events[0]?.seq ?? s.nextSeq;
    const replay = s.events.filter((e) => fromSeq === null || e.seq > fromSeq);
    const gap = fromSeq !== null && fromSeq + 1 < firstAvailable;
    // Replay after the response so the client sees `ok` first.
    setTimeout(() => {
      if (!this.subscriptions.has(sessionId)) return;
      if (gap) {
        this.push({
          type: 'stream_gap',
          session_id: sessionId,
          first_available_seq: firstAvailable,
        });
      }
      for (const event of replay) this.push({ type: 'event', session_id: sessionId, event });
    }, 0);
    return { type: 'ok' };
  }

  private input(sessionId: string, input: Input): ServerMsg {
    const s = this.sessions.get(sessionId);
    if (!s) return this.error('not_found', `session ${sessionId} not found`);
    const at = this.now();
    const st = s.info.state;
    if (s.info.control === 'observe_only')
      return this.error('unsupported', 'session is observe-only');
    if (st.type === 'exited') return this.error('conflict', 'session has exited');

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
    }
  }

  private control(sessionId: string, action: SessionAction): ServerMsg {
    const s = this.sessions.get(sessionId);
    if (!s) return this.error('not_found', `session ${sessionId} not found`);
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
        this.setSessionState(s, { type: 'exited', code: 130 }, at);
        break;
      case 'restart':
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
    s.info = { ...s.info, state, last_activity: at, unread };
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
