/**
 * Simulated session discovery for the mock server: AI sessions "found" on
 * the PC in every status, with hiding and listing again.
 */
import type { ClientMsg, FoundAgent, ServerMsg, SessionInfo, UnixMillis } from './types';

/** What the mock server does when a found session joins or leaves the list. */
export interface MockDiscoveryHost {
  now(): UnixMillis;
  /** Lists a session card (no-op when already listed). */
  list(info: SessionInfo): void;
  /** Takes a session card away. */
  unlist(id: string): void;
  error(code: 'not_found' | 'invalid_request', message: string): ServerMsg;
}

type ScanMsg = Extract<ClientMsg, { type: 'scan_sessions' | 'hide_observed' | 'unhide_observed' }>;

interface Row {
  found: FoundAgent;
  /** The status while listed (`imported` or `no_transcript_yet`). */
  listedAs: 'imported' | 'no_transcript_yet' | null;
}

/** Demo rows: one per status. */
function demoRows(): Row[] {
  const row = (found: Omit<FoundAgent, 'session_id'>, listedAs: Row['listedAs']): Row => ({
    found: { ...found, session_id: null },
    listedAs,
  });
  return [
    row(
      {
        pid: 51200,
        agent: { type: 'claude_code' },
        project_dir: 'D:\\work\\billing',
        terminal: 'Windows Terminal',
        title: 'Fix invoice rounding',
        status: 'imported',
      },
      'imported',
    ),
    row(
      {
        pid: 51340,
        agent: { type: 'codex' },
        project_dir: 'D:\\work\\billing-api',
        terminal: 'Windows Terminal',
        title: null,
        status: 'no_transcript_yet',
      },
      'no_transcript_yet',
    ),
    row(
      {
        pid: 51410,
        agent: { type: 'gemini_cli' },
        project_dir: 'D:\\work\\notes',
        terminal: 'VS Code terminal',
        title: null,
        status: 'hidden_by_user',
      },
      'imported',
    ),
    row(
      {
        pid: 51522,
        agent: { type: 'codex' },
        project_dir: null,
        terminal: 'ChatGPT',
        title: null,
        status: 'not_in_a_terminal',
      },
      null,
    ),
    row(
      {
        pid: 51630,
        agent: { type: 'antigravity' },
        project_dir: 'D:\\work\\site',
        terminal: null,
        title: null,
        status: 'unsupported_agent',
      },
      null,
    ),
  ];
}

function folderOf(path: string | null | undefined): string {
  return (path ?? '').split(/[\\/]/).filter(Boolean).pop() ?? '';
}

export class MockDiscovery {
  private readonly rows = demoRows();

  constructor(private readonly host: MockDiscoveryHost) {}

  handle(msg: ScanMsg): ServerMsg {
    switch (msg.type) {
      case 'scan_sessions':
        break;
      case 'hide_observed': {
        const row = this.find(msg.session_id ?? null, msg.pid ?? null);
        if (!row) return this.host.error('not_found', 'no AI session runs as that process');
        if (row.found.status !== 'unsupported_agent') row.found.status = 'hidden_by_user';
        break;
      }
      case 'unhide_observed': {
        const rows = msg.all
          ? this.rows.filter((r) => r.found.status === 'hidden_by_user')
          : [this.find(msg.session_id ?? null, msg.pid ?? null)].filter(
              (r): r is Row => r?.found.status === 'hidden_by_user',
            );
        if (rows.length === 0 && !msg.all) {
          return this.host.error('not_found', 'that session is not hidden');
        }
        for (const r of rows) r.found.status = r.listedAs ?? 'imported';
        break;
      }
    }
    this.sync();
    return {
      type: 'scan_report',
      found: this.rows.map((r) => ({ ...r.found })),
      scanned_at: this.host.now(),
    };
  }

  private find(sessionId: string | null, pid: number | null): Row | undefined {
    return this.rows.find(
      (r) =>
        (pid !== null && r.found.pid === pid) ||
        (sessionId !== null && r.found.session_id === sessionId),
    );
  }

  /** Lists the rows that should be on the list and removes the others. */
  private sync(): void {
    for (const r of this.rows) {
      const listed = r.found.status === 'imported' || r.found.status === 'no_transcript_yet';
      const id = `found-${r.found.pid}`;
      if (listed) {
        r.found.session_id = id;
        const name = r.found.title ?? `${folderOf(r.found.project_dir)} · ${agentName(r)}`;
        r.found.title = name;
        const now = this.host.now();
        this.host.list({
          id,
          origin: {
            type: 'local',
            agent: r.found.agent,
            project_dir: r.found.project_dir ?? '',
            pid: r.found.pid,
          },
          display_name: name,
          state: { type: 'running' },
          control: 'terminal_api',
          profile: null,
          created_at: now,
          last_activity: now,
          unread: 0,
          preview: null,
        });
      } else if (r.found.session_id) {
        this.host.unlist(r.found.session_id);
        r.found.session_id = null;
      }
    }
  }
}

function agentName(r: Row): string {
  switch (r.found.agent.type) {
    case 'claude_code':
      return 'Claude Code';
    case 'codex':
      return 'Codex';
    case 'gemini_cli':
      return 'Gemini CLI';
    default:
      return r.found.agent.type;
  }
}
