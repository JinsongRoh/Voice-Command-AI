/**
 * Remote shell pieces of the simulated server: a small command classifier
 * (same reason codes and UTF-8 byte spans as vcad's), TOTP checks with
 * replay protection, the step-up window and the audit log.
 */
import type { AuditEntry, DangerLevel, UnixMillis } from './types';

interface Rule {
  reason: string;
  level: DangerLevel;
  pattern: RegExp;
}

/** Ordered: more specific rules first (a recursive delete is not also a plain delete). */
const RULES: Rule[] = [
  {
    reason: 'recursive_delete',
    level: 'dangerous',
    pattern:
      /\brm\s+-[a-z]*r[a-z]*\b|\brm\s+-[a-z]*f[a-z]*r\b|\bRemove-Item\b[^|;&]*-Recurse\b|\b(del|erase)\s+\/[sq]\b|\b(rd|rmdir)\s+\/s\b/gi,
  },
  {
    reason: 'disk_format',
    level: 'dangerous',
    pattern: /\b(format|diskpart|mkfs(\.\w+)?)\b|\bdd\s+[^|;&]*\bof=/gi,
  },
  {
    reason: 'permissions',
    level: 'dangerous',
    pattern: /\b(chmod|chown)\s+-R\b|\b(icacls|takeown|Set-Acl)\b/gi,
  },
  {
    reason: 'registry_write',
    level: 'dangerous',
    pattern: /\breg\s+(add|delete)\b|\bSet-ItemProperty\b[^|;&]*HKLM:/gi,
  },
  {
    reason: 'service_or_process_kill',
    level: 'dangerous',
    pattern: /\bStop-Service\b|\bsc(\.exe)?\s+delete\b|\btaskkill\b[^|;&]*\/f\b|\bkill\s+-9\b/gi,
  },
  {
    reason: 'shutdown',
    level: 'dangerous',
    pattern: /\b(shutdown|reboot|halt|poweroff|Restart-Computer|Stop-Computer)\b/gi,
  },
  {
    reason: 'remote_code',
    level: 'dangerous',
    pattern:
      /\b(curl|wget|iwr|Invoke-WebRequest)\b[^|]*\|\s*(sh|bash|zsh|iex|Invoke-Expression)\b|\bInvoke-Expression\b/gi,
  },
  {
    reason: 'encoded_command',
    level: 'dangerous',
    pattern: /\s-(EncodedCommand|enc|e)\s+[A-Za-z0-9+/=]{16,}/gi,
  },
  {
    reason: 'secret_read',
    level: 'dangerous',
    pattern:
      /(~|\$HOME|%USERPROFILE%)?[\\/]?\.(ssh|aws)\b[^\s]*|\bid_rsa\b|[\w-]+\.p8\b|(^|[\s\\/])\.env\b/gi,
  },
  {
    reason: 'git_destructive',
    level: 'dangerous',
    pattern: /\bgit\s+push\b[^|;&]*(--force\b|-f\b)|\bgit\s+reset\s+--hard\b/gi,
  },
  { reason: 'elevation', level: 'dangerous', pattern: /\b(sudo|runas|doas)\b/gi },
  {
    reason: 'delete',
    level: 'caution',
    pattern: /\b(rm|del|erase|rd|rmdir|Remove-Item|unlink)\b/gi,
  },
];

const encoder = new TextEncoder();
const bytes = (text: string) => encoder.encode(text).length;

/** Classifies a command line like vcad (a subset of its rules). */
export function classifyCommand(command: string): {
  level: DangerLevel;
  reasons: string[];
  spans: [number, number][];
} {
  const reasons: string[] = [];
  const spans: [number, number][] = [];
  let level: DangerLevel = 'safe';
  const covered: [number, number][] = [];
  for (const rule of RULES) {
    rule.pattern.lastIndex = 0;
    let found = false;
    for (const m of command.matchAll(rule.pattern)) {
      const start = m.index;
      const end = start + m[0].length;
      // The plain-delete rule does not repeat what a stronger rule flagged.
      if (covered.some(([s, e]) => start < e && end > s)) continue;
      found = true;
      covered.push([start, end]);
      const lead = m[0].length - m[0].trimStart().length;
      spans.push([bytes(command.slice(0, start + lead)), bytes(command.slice(0, end))]);
    }
    if (!found) continue;
    reasons.push(rule.reason);
    if (rule.level === 'dangerous') level = 'dangerous';
    else if (level === 'safe') level = rule.level;
  }
  spans.sort((a, b) => a[0] - b[0]);
  return { level, reasons, spans };
}

export const STEP_UP_WINDOW_MS = 5 * 60_000;
const LOCKOUT_FAILURES = 5;
const LOCKOUT_WINDOW_MS = 5 * 60_000;

/**
 * The demo authenticator: any six digits except `000000`, or any backup
 * code (16 base32 characters, dashes and spaces ignored); each code is
 * usable once.
 */
export class MockTotp {
  private readonly used = new Set<string>();
  private failures: UnixMillis[] = [];
  private stepUpUntil = 0;

  constructor(private readonly now: () => UnixMillis) {}

  locked(): boolean {
    const now = this.now();
    this.failures = this.failures.filter((t) => now - t < LOCKOUT_WINDOW_MS);
    return this.failures.length >= LOCKOUT_FAILURES;
  }

  /** Checks a code; a good one starts the step-up window. */
  verify(code: string): 'ok' | 'invalid' | 'locked' {
    if (this.locked()) return 'locked';
    const backup = code.replace(/[\s-]/g, '').toUpperCase();
    const isBackup = /^[A-Z2-7]{16}$/.test(backup);
    const key = isBackup ? `backup:${backup}` : code;
    const shaped = isBackup || /^\d{6}$/.test(code);
    if (!shaped || code === '000000' || this.used.has(key)) {
      this.failures.push(this.now());
      return this.locked() ? 'locked' : 'invalid';
    }
    this.used.add(key);
    this.stepUpUntil = this.now() + STEP_UP_WINDOW_MS;
    return 'ok';
  }

  inStepUp(): boolean {
    return this.now() < this.stepUpUntil;
  }
}

/** Append-only audit log, newest first when listed. */
export class MockAudit {
  private readonly entries: AuditEntry[] = [];

  constructor(seed: AuditEntry[] = []) {
    this.entries.push(...seed);
  }

  add(entry: AuditEntry): void {
    this.entries.push(entry);
  }

  list(limit: number): AuditEntry[] {
    const n = limit <= 0 ? 100 : Math.min(limit, 1000);
    return [...this.entries].sort((a, b) => b.ts - a.ts).slice(0, n);
  }
}
