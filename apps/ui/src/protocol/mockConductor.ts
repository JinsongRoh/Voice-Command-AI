/**
 * The conductor of the mock server: the same rules as vcad (setup only on
 * the PC, confirmations for sends, kill switch) with canned replies.
 */
import type {
  ClientMsg,
  ConductorEscalation,
  ConductorRequest,
  ConductorState,
  ErrorCode,
  ServerMsg,
  SessionInfo,
  UnixMillis,
} from './types';

export type ConductorClientMsg = Extract<ClientMsg, { type: `conductor_${string}` | 'cc_pause' }>;

export interface MockConductorOptions {
  /** This client is the desktop app of the PC (setup allowed). Default true. */
  local?: boolean;
  /** Initial state on top of the defaults. */
  seed?: Partial<ConductorState>;
  /**
   * Announce `command_center` (`cc_pause`, the kill switch behind a code on
   * paired devices). Default true; false acts as an older server.
   */
  commandCenter?: boolean;
}

/**
 * Checks the authenticator code of a paired device's kill switch (the
 * mock server's authenticator); `null` when it is good.
 */
export type KillCodeCheck = (code: string | undefined) => ServerMsg | null;

export function defaultConductorState(): ConductorState {
  return {
    settings: {
      enabled: false,
      agent: 'claude_code',
      builtin_llm: null,
      wake_events: false,
      max_actions_per_hour: 20,
      max_wakes_per_hour: 6,
      confirm_timeout_secs: 120,
      builtin_max_calls_per_hour: 30,
      builtin_max_tokens: 1024,
    },
    status: 'off',
    session_id: null,
    model: null,
    kill_switch: false,
    pending: [],
    allowlist: [],
    chat: [],
    actions_last_hour: 0,
    wakes_last_hour: 0,
    agents: [
      { agent: 'claude_code', available: true, experimental: false },
      { agent: 'codex', available: true, experimental: true },
      { agent: 'gemini_cli', available: false, experimental: true },
      { agent: 'builtin', available: true, experimental: false },
    ],
    llms: [{ id: 'local-llm', label: 'Local LLM', model: 'qwen2.5-7b-instruct' }],
    escalations: [],
  };
}

export class MockConductor {
  state: ConductorState;
  private readonly local: boolean;
  readonly commandCenter: boolean;
  private nextRequest = 1;
  private nextEscalation = 1;

  constructor(
    private readonly now: () => UnixMillis,
    private readonly push: (msg: ServerMsg) => void,
    private readonly sessions: () => SessionInfo[],
    options: MockConductorOptions = {},
    private readonly checkKillCode: KillCodeCheck = () => null,
    private readonly audit: (event: string, detail: string) => void = () => {},
  ) {
    this.local = options.local ?? true;
    this.commandCenter = options.commandCenter ?? true;
    this.state = { ...defaultConductorState(), ...options.seed };
  }

  private error(code: ErrorCode, message: string): ServerMsg {
    return { type: 'error', code, message };
  }

  private snapshot(): ServerMsg {
    // As vcad: an escalation lives only as long as its decision.
    const sessions = this.sessions();
    this.state.escalations = (this.state.escalations ?? []).filter((e) => {
      const session = sessions.find((x) => x.id === e.session_id);
      return !session || session.pending_decision?.decision.id === e.decision_id;
    });
    return { type: 'conductor', state: structuredClone(this.state) };
  }

  /**
   * Raises a session's pending decision to the inbox, as the conductor's
   * `escalate_decision` (or, without a summary, the fallback) does.
   */
  escalate(
    sessionId: string,
    summary = '',
    recommendation: string | null = null,
  ): ConductorEscalation | null {
    const session = this.sessions().find((x) => x.id === sessionId);
    const pending = session?.pending_decision;
    if (!session || !pending) return null;
    const at = this.now();
    const escalation: ConductorEscalation = {
      id: `ce-${this.nextEscalation++}`,
      session_id: session.id,
      session_name: session.display_name,
      decision_id: pending.decision.id,
      asked_at: pending.at,
      source: summary ? 'conductor' : 'fallback',
      summary,
      recommendation,
      request: pending.decision.prompt,
      command: pending.decision.command ?? null,
      reason: summary ? null : 'timeout',
      created_at: at,
    };
    this.state.escalations = [
      ...(this.state.escalations ?? []).filter((e) => e.session_id !== session.id),
      escalation,
    ];
    this.changed();
    return escalation;
  }

  private changed(): void {
    this.push(this.snapshot());
  }

  private reply(text: string): void {
    const at = this.now();
    this.state.chat = [...this.state.chat, { role: 'assistant', text, at }].slice(-40);
    this.push({ type: 'conductor_reply', text, at });
    this.changed();
  }

  /** A send the mock conductor wants to make (tests and the demo). */
  requestSend(sessionName: string, text: string): ConductorRequest {
    const target =
      this.sessions().find((s) => s.display_name === sessionName) ?? this.sessions()[0];
    const at = this.now();
    const request: ConductorRequest = {
      id: `cr-${this.nextRequest++}`,
      session_id: target?.id ?? 'unknown',
      session_name: target?.display_name ?? sessionName,
      text,
      created_at: at,
      expires_at: at + this.state.settings.confirm_timeout_secs * 1000,
    };
    this.state.pending = [...this.state.pending, request];
    this.state.status = 'waiting_for_you';
    this.changed();
    return request;
  }

  handle(msg: ConductorClientMsg): ServerMsg {
    const s = this.state;
    switch (msg.type) {
      case 'conductor_get':
        return this.snapshot();
      case 'conductor_configure':
        if (!this.local)
          return this.error('forbidden', 'the conductor can only be set up on the PC');
        s.settings = { ...msg.settings };
        if (!s.settings.enabled) {
          s.status = 'off';
          s.pending = [];
          s.escalations = [];
        }
        s.model =
          s.settings.agent === 'builtin'
            ? (s.llms.find((l) => l.id === s.settings.builtin_llm)?.model ?? null)
            : null;
        break;
      case 'conductor_start':
        if (!this.local)
          return this.error('forbidden', 'the conductor can only be set up on the PC');
        if (!s.settings.enabled)
          return this.error('invalid_request', 'turn the conductor on in Settings first');
        s.status = 'ready';
        s.session_id = s.settings.agent === 'builtin' ? null : 'mock-conductor';
        break;
      case 'conductor_stop': {
        // The kill switch from a paired device needs the owner's code.
        if (msg.kill && !this.local && this.commandCenter) {
          const refused = this.checkKillCode(msg.code);
          if (refused) {
            this.audit('conductor_refused', 'kill switch without a valid code');
            return refused;
          }
        }
        s.status = msg.kill ? 'revoked' : 'off';
        s.pending = [];
        if (msg.kill) s.allowlist = [];
        s.session_id = null;
        break;
      }
      case 'cc_pause':
        if (!this.commandCenter) return this.error('invalid_request', 'unknown message cc_pause');
        if (s.settings.paused === msg.paused) break;
        s.settings = { ...s.settings, paused: msg.paused };
        this.audit('cc_pause', `paused=${msg.paused}`);
        if (s.status !== 'off' && s.status !== 'revoked' && s.pending.length === 0)
          s.status = msg.paused ? 'paused' : 'ready';
        break;
      case 'conductor_decide': {
        const request = s.pending.find((r) => r.id === msg.request_id);
        if (!request) return this.error('not_found', 'that conductor request is no longer pending');
        s.pending = s.pending.filter((r) => r.id !== msg.request_id);
        if (s.pending.length === 0 && s.status === 'waiting_for_you')
          s.status = s.settings.paused ? 'paused' : 'ready';
        setTimeout(
          () => this.reply(msg.allow ? `Sent to ${request.session_name}.` : 'Not sent.'),
          50,
        );
        break;
      }
      case 'conductor_allow_session':
        if (!this.local)
          return this.error('forbidden', 'the conductor can only be set up on the PC');
        s.allowlist = s.allowlist.filter((a) => a.session_id !== msg.session_id);
        if (msg.minutes > 0)
          s.allowlist.push({
            session_id: msg.session_id,
            expires_at: this.now() + msg.minutes * 60_000,
          });
        break;
      case 'conductor_trust_folder': {
        const session = this.sessions().find((x) => x.id === s.session_id);
        if (session?.terminal_prompt !== 'trust_folder')
          return this.error(
            'conflict',
            "the conductor's terminal is not asking to trust its folder",
          );
        break;
      }
      case 'conductor_ask': {
        if (!s.settings.enabled || s.status === 'off' || s.status === 'revoked')
          return this.error('conflict', 'the conductor is not running; start it first');
        const text = msg.text.trim();
        s.chat = [...s.chat, { role: 'user', text, at: this.now() }].slice(-40);
        if (/send|보내/i.test(text)) {
          setTimeout(() => this.requestSend('', 'npm test'), 50);
        } else {
          const n = this.sessions().length;
          setTimeout(() => this.reply(`${n} sessions on this PC; nothing needs you.`), 50);
        }
        break;
      }
    }
    return this.snapshot();
  }
}
