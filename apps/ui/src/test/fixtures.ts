import type { SessionInfo } from '../protocol/types';

export function localSession(overrides: Partial<SessionInfo> = {}): SessionInfo {
  return {
    id: 's1',
    origin: {
      type: 'local',
      agent: { type: 'claude_code' },
      project_dir: '/home/dev/payments-api',
      pid: 42,
    },
    display_name: 'payments-api',
    state: { type: 'running' },
    control: 'managed_pty',
    profile: 'work',
    created_at: Date.now() - 60_000,
    last_activity: Date.now() - 30_000,
    unread: 0,
    preview: null,
    ...overrides,
  };
}

export function cloudSession(overrides: Partial<SessionInfo> = {}): SessionInfo {
  return {
    id: 'c1',
    origin: {
      type: 'cloud',
      provider: 'jules',
      remote_id: 'sessions/1',
      url: 'https://example.com/s/1',
    },
    display_name: 'mobile-app',
    state: { type: 'idle' },
    control: 'cloud',
    profile: null,
    created_at: Date.now() - 60_000,
    last_activity: Date.now() - 30_000,
    unread: 0,
    preview: null,
    ...overrides,
  };
}
