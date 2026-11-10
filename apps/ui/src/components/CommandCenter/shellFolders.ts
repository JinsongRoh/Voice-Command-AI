import { isShellSession } from '../../protocol/helpers';
import { shellChoices, type MachineOs } from '../../protocol/shell';
import type { SessionInfo } from '../../protocol/types';
import { sameFolder } from '../ShellTab';

/** The Command Center's shell: PowerShell on Windows, the machine's default shell elsewhere. */
export function commandShellName(os: MachineOs | null): string {
  return os === 'windows' && shellChoices(os).includes('powershell') ? 'powershell' : 'default';
}

/**
 * Folders the Command Center's shell never opens in: the conductor's own working folder
 * (vcad's data folder, with the conductor's token and MCP config).
 */
export function conductorFolders(
  sessions: Record<string, SessionInfo>,
  conductorSessionId: string | null | undefined,
): string[] {
  const out: string[] = [];
  for (const s of Object.values(sessions)) {
    if (s.origin.type !== 'local') continue;
    if (s.conductor || (conductorSessionId && s.id === conductorSessionId))
      out.push(s.origin.project_dir);
  }
  return out;
}

/** Whether `folder` is one of `excluded` (whatever the slashes or letter case). */
export function isExcludedFolder(folder: string, excluded: readonly string[]): boolean {
  return excluded.some((x) => sameFolder(x, folder));
}

/**
 * The folder the Command Center's shell starts in: the server's most recent project
 * folder, else the project folder of the most recently active local
 * session. Never one of `excluded`; `null` when nothing fits.
 */
export function defaultShellFolder(
  recent: readonly string[],
  sessions: Record<string, SessionInfo>,
  excluded: readonly string[],
): string | null {
  const ok = (p: string) => !!p && !isExcludedFolder(p, excluded);
  const fromServer = recent.find(ok);
  if (fromServer) return fromServer;
  let best: { dir: string; at: number; shell: boolean } | null = null;
  for (const s of Object.values(sessions)) {
    if (s.origin.type !== 'local' || s.conductor || !ok(s.origin.project_dir)) continue;
    // Agent sessions first: a shell's folder only when no agent session fits.
    const shell = isShellSession(s);
    if (!best || (best.shell && !shell) || (best.shell === shell && s.last_activity > best.at))
      best = { dir: s.origin.project_dir, at: s.last_activity, shell };
  }
  return best?.dir ?? null;
}
