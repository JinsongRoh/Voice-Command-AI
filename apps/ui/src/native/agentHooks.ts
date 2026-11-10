/**
 * Agent hooks for exact approval alerts (desktop shell only): Claude Code
 * hooks and the Codex `notify` program, installed in the agents' user
 * settings so that they run the `vca` CLI bundled with this app. Mirrors
 * `apps/shell/src/host/hooks.rs`. Only the PC app has these commands; the
 * phone app answers `forbidden`.
 */
import { call } from './bridge';

export type HookAgent = 'claude' | 'codex';

export const HOOK_AGENTS: readonly HookAgent[] = ['claude', 'codex'];

/**
 * `installed`: every hook runs the bundled `vca`. `missing`: no VCA hook.
 * `outdated`: some hooks are missing or changed. `other_path`: the hooks
 * run another `vca` (an older or manual install). `error`: the settings
 * file could not be read.
 */
export type AgentHookState = 'installed' | 'missing' | 'outdated' | 'other_path' | 'error';

export interface AgentHooks {
  agent: HookAgent;
  state: AgentHookState;
  /** The agent's settings file. */
  config_path: string;
  /** Programs the VCA hooks in that file run. */
  programs: string[];
  /** Backup written by the last install or uninstall, when it changed the file. */
  backup_path: string | null;
  error: string | null;
}

export interface AgentHooksStatus {
  /** The bundled `vca` the hooks run; null when this build has none. */
  cli_path: string | null;
  agents: AgentHooks[];
}

export interface AgentHooksServices {
  status(): Promise<AgentHooksStatus>;
  /** Installs, or repairs, the agent's hooks (user scope). */
  install(agent: HookAgent): Promise<AgentHooks>;
  uninstall(agent: HookAgent): Promise<AgentHooks>;
}

export const agentHooksBridge: AgentHooksServices = {
  status: () => call<AgentHooksStatus>('vca_host_hooks_status'),
  install: (agent) => call<AgentHooks>('vca_host_hooks_install', { agent }),
  uninstall: (agent) => call<AgentHooks>('vca_host_hooks_uninstall', { agent }),
};
