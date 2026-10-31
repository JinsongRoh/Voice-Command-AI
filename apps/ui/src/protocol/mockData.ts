/**
 * Scenario data for `MockTransport`: sessions, their history and the scripts
 * they "play" while running. Everything is deterministic.
 */
import type {
  Decision,
  MessageRole,
  PairedDevice,
  QueueEntry,
  SessionInfo,
  SessionOrigin,
  SessionState,
  UnixMillis,
} from './types';

export type ScriptStep =
  | { kind: 'out'; text: string }
  | { kind: 'msg'; role: MessageRole; text: string }
  | { kind: 'tool'; name: string; summary: string }
  | { kind: 'state'; state: SessionState }
  | { kind: 'decision'; decision: Decision }
  | { kind: 'notify'; title: string; body: string };

export interface MockSessionSeed {
  info: SessionInfo;
  /** Replayed into the event log at start-up (timestamps spread backwards). */
  history: ScriptStep[];
  /** Played one step per tick while the session is running. */
  script: ScriptStep[];
  /** Restart the script from the top when it ends. */
  loop: boolean;
  /** Approximate process tree size for the resource snapshot. */
  baseCpu: number;
  baseMemoryMb: number;
  processes: number;
}

export interface MockScenario {
  serverName: string;
  sessions: MockSessionSeed[];
  queue: QueueEntry[];
  devices: PairedDevice[];
  memoryTotalBytes: number;
}

export type MockProfile = 'workstation' | 'build_server';

const ESC = '\x1b';
const green = (s: string) => `${ESC}[32m${s}${ESC}[0m`;
const red = (s: string) => `${ESC}[31m${s}${ESC}[0m`;
const dim = (s: string) => `${ESC}[2m${s}${ESC}[0m`;
const bold = (s: string) => `${ESC}[1m${s}${ESC}[0m`;
const cyan = (s: string) => `${ESC}[36m${s}${ESC}[0m`;

const out = (text: string): ScriptStep => ({ kind: 'out', text: `${text}\r\n` });
const msg = (role: MessageRole, text: string): ScriptStep => ({ kind: 'msg', role, text });
const tool = (name: string, summary: string): ScriptStep => ({ kind: 'tool', name, summary });
const state = (s: SessionState): ScriptStep => ({ kind: 'state', state: s });

const MIN = 60_000;

function local(
  id: string,
  agent: Extract<SessionOrigin, { type: 'local' }>,
  display_name: string,
  s: SessionState,
  now: UnixMillis,
  ageMin: number,
  idleMin: number,
  extra: Partial<SessionInfo> = {},
): SessionInfo {
  return {
    id,
    origin: agent,
    display_name,
    state: s,
    control: 'managed_pty',
    profile: 'personal',
    created_at: now - ageMin * MIN,
    last_activity: now - idleMin * MIN,
    unread: 0,
    preview: null,
    ...extra,
  };
}

const permissionDecision = (id: string, command: string): Decision => ({
  id,
  prompt: `Allow Bash command?\n${command}`,
  options: [
    { id: 'yes', label: 'Yes' },
    { id: 'always', label: "Yes, and don't ask again for this command" },
    { id: 'no', label: 'No, tell the agent what to do instead' },
  ],
});

function workstation(now: UnixMillis): MockScenario {
  const sessions: MockSessionSeed[] = [
    {
      info: local(
        'wk-claude-api',
        {
          type: 'local',
          agent: { type: 'claude_code' },
          project_dir: '~/code/payments-api',
          pid: 48211,
        },
        'payments-api',
        { type: 'awaiting_decision' },
        now,
        95,
        1,
        { unread: 1 },
      ),
      history: [
        msg('user', 'Fix the flaky refund test and make sure the whole suite passes.'),
        msg('assistant', "I'll look at the refund tests first."),
        tool('Read', 'src/refunds/refund.service.ts'),
        tool('Grep', '"setTimeout" in test/refunds'),
        out(`${cyan('●')} Reading ${bold('test/refunds/refund.spec.ts')}`),
        msg(
          'assistant',
          'The test relies on real timers. I will switch it to fake timers and rebuild before running the suite.',
        ),
        tool('Edit', 'test/refunds/refund.spec.ts (+6 −2)'),
        out(`${green('✓')} Updated test/refunds/refund.spec.ts`),
        {
          kind: 'decision',
          decision: permissionDecision('dec-api-1', 'rm -rf dist && pnpm build'),
        },
      ],
      script: [
        tool('Bash', 'rm -rf dist && pnpm build'),
        out(dim('$ pnpm build')),
        out('vite v6.2.0 building for production...'),
        out(`${green('✓')} 412 modules transformed.`),
        tool('Bash', 'pnpm test'),
        out(dim('$ pnpm test')),
        out(` ${green('✓')} test/refunds/refund.spec.ts ${dim('(12 tests) 840ms')}`),
        out(` ${green('✓')} test/payments/charge.spec.ts ${dim('(31 tests) 1.2s')}`),
        out(` ${green('✓')} test/payments/webhook.spec.ts ${dim('(9 tests) 410ms')}`),
        out(` ${bold('Test Files')}  ${green('3 passed')} (3)`),
        msg('assistant', 'All 52 tests pass. The refund test is now deterministic.'),
        { kind: 'notify', title: 'Tests passed', body: 'payments-api: 52 tests passed' },
        {
          kind: 'decision',
          decision: permissionDecision('dec-api-2', 'git push origin fix/refund-test'),
        },
      ],
      loop: true,
      baseCpu: 38,
      baseMemoryMb: 1450,
      processes: 9,
    },
    {
      info: local(
        'wk-codex-web',
        {
          type: 'local',
          agent: { type: 'codex' },
          project_dir: '~/code/web-dashboard',
          pid: 51077,
        },
        'web-dashboard',
        { type: 'running' },
        now,
        42,
        0,
        { profile: 'work' },
      ),
      history: [
        msg('user', 'Add a dark mode toggle to the settings page.'),
        tool('shell', 'rg -n "theme" src/'),
        out(dim('src/settings/Settings.tsx:14:  const theme = useTheme();')),
        msg('assistant', 'Adding a ThemeToggle component and wiring it to the store.'),
      ],
      script: [
        tool('apply_patch', 'src/settings/ThemeToggle.tsx (new file)'),
        out(`${green('+')} src/settings/ThemeToggle.tsx`),
        tool('shell', 'npm run lint'),
        out(dim('> web-dashboard@2.4.0 lint')),
        out(dim('> eslint .')),
        out(`${green('✔')} No problems found`),
        tool('shell', 'npm test -- settings'),
        out(` PASS  src/settings/Settings.test.tsx`),
        out(` PASS  src/settings/ThemeToggle.test.tsx`),
        msg('assistant', 'Toggle added with tests. Checking contrast in both themes next.'),
        out(`${cyan('…')} thinking`),
      ],
      loop: true,
      baseCpu: 22,
      baseMemoryMb: 880,
      processes: 6,
    },
    {
      info: local(
        'wk-gemini-docs',
        {
          type: 'local',
          agent: { type: 'gemini_cli' },
          project_dir: '~/code/docs-site',
          pid: 50112,
        },
        'docs-site',
        { type: 'awaiting_input' },
        now,
        180,
        12,
        { unread: 2, control: 'native_channel' },
      ),
      history: [
        msg('user', 'Translate the getting-started guide into Spanish.'),
        tool('ReadFile', 'docs/getting-started.md'),
        tool('WriteFile', 'docs/es/getting-started.md'),
        out(`${green('✓')} Wrote docs/es/getting-started.md (214 lines)`),
        msg('assistant', 'Done. Should I also translate the API reference, or open a PR now?'),
        {
          kind: 'notify',
          title: 'Waiting for input',
          body: 'docs-site asks: translate API reference or open a PR?',
        },
        state({ type: 'awaiting_input' }),
      ],
      script: [
        tool('Shell', 'git checkout -b docs/es-getting-started'),
        tool('Shell', 'git commit -am "docs: add Spanish getting-started guide"'),
        out(`[docs/es-getting-started 3f2a1c9] docs: add Spanish getting-started guide`),
        msg('assistant', 'Committed. Anything else?'),
        state({ type: 'awaiting_input' }),
      ],
      loop: false,
      baseCpu: 2,
      baseMemoryMb: 310,
      processes: 2,
    },
    {
      info: local(
        'wk-claude-infra',
        { type: 'local', agent: { type: 'claude_code' }, project_dir: '~/code/infra', pid: 39001 },
        'infra',
        { type: 'idle' },
        now,
        600,
        48,
        { profile: 'work' },
      ),
      history: [
        msg('user', 'Bump the Postgres chart to 16.4 and run a dry-run plan.'),
        tool('Edit', 'charts/postgres/values.yaml'),
        tool('Bash', 'terraform plan -out plan.tfout'),
        out(`Plan: ${green('1 to add')}, ${cyan('2 to change')}, ${red('0 to destroy')}.`),
        msg('assistant', 'The plan looks safe: one new parameter group, two in-place changes.'),
        state({ type: 'idle' }),
      ],
      script: [
        tool('Bash', 'terraform validate'),
        out(`${green('Success!')} The configuration is valid.`),
        msg('assistant', 'Validated. Ready for the next instruction.'),
        state({ type: 'idle' }),
      ],
      loop: false,
      baseCpu: 0.4,
      baseMemoryMb: 220,
      processes: 1,
    },
    {
      info: {
        id: 'wk-jules-mobile',
        origin: {
          type: 'cloud',
          provider: 'jules',
          remote_id: 'sessions/8812043',
          url: 'https://jules.google.com/session/8812043',
        },
        display_name: 'mobile-app: upgrade RN',
        state: { type: 'running' },
        control: 'cloud',
        profile: null,
        created_at: now - 35 * MIN,
        last_activity: now - 2 * MIN,
        unread: 0,
        preview: null,
      },
      history: [
        msg('user', 'Upgrade React Native to the latest stable and fix the build.'),
        msg(
          'assistant',
          'Plan: 1) bump dependencies 2) migrate the Gradle config 3) run the iOS and Android builds.',
        ),
        tool('plan_step', 'Bump react-native and peer dependencies'),
      ],
      script: [
        tool('plan_step', 'Migrate android/build.gradle'),
        msg('assistant', 'Gradle config migrated; namespace moved to the module build file.'),
        tool('plan_step', 'Run Android build'),
        msg('assistant', 'Android build succeeded. Starting the iOS build.'),
        tool('plan_step', 'Run iOS build'),
        msg('assistant', 'iOS build succeeded. Opening a pull request.'),
        { kind: 'notify', title: 'Pull request ready', body: 'Jules opened PR #214 in mobile-app' },
        state({ type: 'idle' }),
      ],
      loop: false,
      baseCpu: 0,
      baseMemoryMb: 0,
      processes: 0,
    },
    {
      info: {
        id: 'wk-cursor-search',
        origin: {
          type: 'cloud',
          provider: 'cursor_cloud',
          remote_id: 'bc-4f1a9e',
          url: 'https://cursor.com/agents?id=bc-4f1a9e',
        },
        display_name: 'search-service: reindex job',
        state: { type: 'error', message: 'Repository access denied (token expired)' },
        control: 'cloud',
        profile: null,
        created_at: now - 300 * MIN,
        last_activity: now - 25 * MIN,
        unread: 1,
        preview: null,
      },
      history: [
        msg('user', 'Make the reindex job resumable after a crash.'),
        msg('assistant', 'Cloning the repository…'),
        msg(
          'system',
          'Repository access denied (token expired). Reconnect GitHub in Cursor settings.',
        ),
        state({ type: 'error', message: 'Repository access denied (token expired)' }),
      ],
      script: [
        msg('assistant', 'Retrying with refreshed credentials…'),
        state({ type: 'running' }),
      ],
      loop: false,
      baseCpu: 0,
      baseMemoryMb: 0,
      processes: 0,
    },
  ];

  return {
    serverName: 'workstation',
    sessions,
    memoryTotalBytes: 32 * 1024 ** 3,
    queue: [
      {
        job_id: 'job-101',
        session_id: 'wk-codex-web',
        command: 'npm run build',
        class: 'heavy',
        queued_at: now - 40_000,
        position: 0,
      },
      {
        job_id: 'job-102',
        session_id: 'wk-claude-infra',
        command: 'docker compose up -d postgres',
        class: 'heavy',
        queued_at: now - 25_000,
        position: 1,
      },
      {
        job_id: 'job-103',
        session_id: 'wk-claude-api',
        command: 'pnpm test --coverage',
        class: 'light',
        queued_at: now - 9_000,
        position: 2,
      },
    ],
    devices: [
      {
        id: 'dev-pixel',
        name: 'Pixel 9',
        platform: 'Android 16',
        paired_at: now - 40 * 24 * 60 * MIN,
        last_seen: now,
        current: true,
      },
      {
        id: 'dev-ipad',
        name: 'iPad Air',
        platform: 'iPadOS 19',
        paired_at: now - 12 * 24 * 60 * MIN,
        last_seen: now - 3 * 24 * 60 * MIN,
        current: false,
      },
      {
        id: 'dev-laptop',
        name: 'Travel laptop',
        platform: 'macOS 16',
        paired_at: now - 90 * 24 * 60 * MIN,
        last_seen: now - 20 * 24 * 60 * MIN,
        current: false,
      },
    ],
  };
}

function buildServer(now: UnixMillis): MockScenario {
  return {
    serverName: 'build-server',
    memoryTotalBytes: 128 * 1024 ** 3,
    sessions: [
      {
        info: local(
          'bs-codex-mono',
          { type: 'local', agent: { type: 'codex' }, project_dir: '/srv/work/monorepo', pid: 7712 },
          'monorepo: migrate to pnpm',
          { type: 'running' },
          now,
          240,
          0,
          { profile: 'api' },
        ),
        history: [msg('user', 'Migrate the monorepo from yarn to pnpm workspaces.')],
        script: [
          tool('shell', 'pnpm import'),
          out('Lockfile imported: 1 842 packages'),
          tool('shell', 'pnpm -r build'),
          out(`${green('✓')} packages/core built`),
          out(`${green('✓')} packages/api built`),
          out(`${red('✗')} packages/legacy-ui failed: Cannot find module 'left-pad'`),
          msg('assistant', 'legacy-ui relies on a hoisted dependency. Adding it explicitly.'),
          tool('apply_patch', 'packages/legacy-ui/package.json'),
        ],
        loop: true,
        baseCpu: 61,
        baseMemoryMb: 5200,
        processes: 23,
      },
      {
        info: local(
          'bs-aider-legacy',
          {
            type: 'local',
            agent: { type: 'aider' },
            project_dir: '/srv/work/legacy-billing',
            pid: 7790,
          },
          'legacy-billing',
          { type: 'paused' },
          now,
          400,
          60,
        ),
        history: [
          msg('user', 'Port the invoice PDF generator to Python 3.13.'),
          out('Paused by the resource broker (memory above 85%).'),
          state({ type: 'paused' }),
        ],
        script: [
          out('Resumed.'),
          tool('edit', 'billing/pdf.py'),
          msg('assistant', 'Ported. Running tests.'),
        ],
        loop: true,
        baseCpu: 0,
        baseMemoryMb: 640,
        processes: 3,
      },
      {
        info: local(
          'bs-grok-perf',
          { type: 'local', agent: { type: 'grok' }, project_dir: '/srv/work/perf-lab', pid: 7801 },
          'perf-lab',
          { type: 'exited', code: 0 },
          now,
          900,
          300,
          { control: 'terminal_api' },
        ),
        history: [
          msg('user', 'Profile the hot path in the matcher and summarize.'),
          msg(
            'assistant',
            'Summary written to PERF.md. 38% of time is spent in regex compilation.',
          ),
          state({ type: 'exited', code: 0 }),
        ],
        script: [],
        loop: false,
        baseCpu: 0,
        baseMemoryMb: 0,
        processes: 0,
      },
    ],
    queue: [
      {
        job_id: 'job-201',
        session_id: 'bs-codex-mono',
        command: 'pnpm -r test',
        class: 'heavy',
        queued_at: now - 70_000,
        position: 0,
      },
    ],
    devices: [
      {
        id: 'dev-pixel',
        name: 'Pixel 9',
        platform: 'Android 16',
        paired_at: now - 5 * 24 * 60 * MIN,
        last_seen: now,
        current: true,
      },
    ],
  };
}

export function createScenario(profile: MockProfile, now: UnixMillis): MockScenario {
  return profile === 'workstation' ? workstation(now) : buildServer(now);
}

/** Canned folder list for the "existing project" picker of the mock server. */
export const MOCK_PROJECT_DIRS: Record<MockProfile, string[]> = {
  workstation: [
    '~/code/payments-api',
    '~/code/web-dashboard',
    '~/code/docs-site',
    '~/code/infra',
    '~/code/mobile-app',
  ],
  build_server: ['/srv/work/monorepo', '/srv/work/legacy-billing', '/srv/work/perf-lab'],
};

/** Generic script for newly launched sessions. */
export function launchScript(name: string): ScriptStep[] {
  return [
    out(dim(`Starting ${name}…`)),
    msg('system', `Session ${name} started.`),
    msg('assistant', 'Ready. What would you like me to work on?'),
    state({ type: 'awaiting_input' }),
  ];
}
