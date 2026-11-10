import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from '../App';
import { translate, type MessageKey, type Vars } from '../i18n/i18n';
import { NativeError } from '../native/bridge';
import { STEP_UP_CANCELED } from '../native/security';
import { defaultConductorState, type MockConductorOptions } from '../protocol/mockConductor';
import { MockTransport } from '../protocol/mockTransport';
import type { ConductorState, ErrorCode, SessionInfo } from '../protocol/types';
import { isDictationActive, micBorrowed } from '../speech/handsFree';
import { RequestError } from '../state/core';
import { deferredItems } from '../state/inbox';
import { defaultSettings, type Settings } from '../state/settings';
import { killErrorText } from './CommandCenter/killErrors';
import type { StepUpApi } from './StepUpPrompt';

/**
 * The step-up (local verification on the PC), recorded: gated calls run
 * as if verified, so tests see which action each call asked for.
 */
const stepUpApi = vi.hoisted(() => ({
  request: async () => true,
  run: vi.fn(async (_action: string, call: () => Promise<unknown>) => call()),
}));
vi.mock('./StepUpPrompt', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./StepUpPrompt')>()),
  useStepUp: () => stepUpApi as unknown as StepUpApi,
}));

beforeEach(() => {
  stepUpApi.run.mockClear();
});

const speech = {
  transcribe: async () => ({ text: 'which sessions need me', corrections: [] }),
  createRecorder: () => ({
    start: async () => {},
    stop: async () => new Blob(['x']),
    cancel: () => {},
  }),
  fallbackWithoutMic: false,
};

const NOW = Date.now();

afterEach(() => {
  deferredItems.set(new Set());
  vi.unstubAllGlobals();
});

function running(extra: Partial<ConductorState> = {}): Partial<ConductorState> {
  return {
    settings: { ...defaultConductorState().settings, enabled: true },
    status: 'ready',
    session_id: 'mock-conductor',
    ...extra,
  };
}

/** The built-in conductor: no session, its chat in `ConductorState.chat`. */
function builtin(extra: Partial<ConductorState> = {}): Partial<ConductorState> {
  return running({
    settings: { ...defaultConductorState().settings, enabled: true, agent: 'builtin' },
    session_id: null,
    ...extra,
  });
}

const PENDING = {
  id: 'cr-1',
  session_id: 'wk-claude-api',
  session_name: 'api',
  text: 'npm test',
  created_at: NOW,
  expires_at: NOW + 120_000,
};

/** Wide screens: the session list instead of the phone carousel. */
function wideScreen() {
  vi.stubGlobal('matchMedia', (query: string) => ({
    matches: /min-width: (900|640)px/.test(query),
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  }));
}

function renderApp(options: { machineId?: string; conductor?: MockConductorOptions | false } = {}) {
  const transport = new MockTransport({
    latencyMs: 0,
    autoTick: false,
    conductor: options.conductor,
  });
  render(
    <App
      machines={[
        {
          id: options.machineId ?? 'local',
          name: 'workstation',
          connectivity: 'LAN',
          transport,
          simulate: { drop: () => {} },
        },
      ]}
      speech={speech}
      settings={{ ...defaultSettings(), language: 'en' }}
    />,
  );
  return transport;
}

const nav = () => within(screen.getByRole('navigation', { name: 'Main navigation' }));

async function openCommand() {
  await screen.findByTestId('session-card-wk-claude-api');
  await userEvent.click(nav().getByRole('button', { name: /Command/ }));
  return screen.findByTestId('conductor-view');
}

async function openTab(view: HTMLElement, name: RegExp | string) {
  await userEvent.click(within(view).getByRole('tab', { name }));
  return within(view).getByRole('tabpanel');
}

/** The conductor's own session, as vcad announces it. */
function conductorSession(patch: Partial<SessionInfo> = {}): SessionInfo {
  return {
    id: 'mock-conductor',
    origin: {
      type: 'local',
      agent: { type: 'claude_code' },
      project_dir: 'C:/Users/me/AppData/Roaming/vca/data/conductor',
      pid: 7,
    },
    display_name: 'Conductor',
    state: { type: 'awaiting_input' },
    control: 'managed_pty',
    profile: null,
    created_at: NOW - 60_000,
    last_activity: NOW,
    unread: 0,
    preview: null,
    conductor: true,
    ...patch,
  };
}

describe('Command Center (지휘소) page', () => {
  it('sits second in the menu with a badge for pending requests', async () => {
    renderApp({ conductor: { seed: running({ status: 'waiting_for_you', pending: [PENDING] }) } });
    await screen.findByTestId('session-card-wk-claude-api');
    const buttons = nav().getAllByRole('button');
    expect(buttons[1]).toHaveTextContent('Command');
    const badge = await within(buttons[1]!).findByTestId('conductor-badge');
    expect(badge).toHaveTextContent('1');
    expect(buttons[1]).toHaveTextContent('1 conductor requests');
  });

  it('explains the off state and links to Settings › Command Center on the PC', async () => {
    renderApp();
    const view = await openCommand();
    const off = await within(view).findByTestId('conductor-off');
    expect(off).toHaveTextContent('Turn it on in Settings › Command Center');
    // Whatever tab Settings showed last, the link opens the conductor tab.
    localStorage.setItem('vca.settings.tab', 'about');
    await userEvent.click(within(off).getByRole('button', { name: 'Open Settings' }));
    expect(await screen.findByTestId('conductor-settings')).toBeInTheDocument();
    expect(screen.getByRole('tab', { name: 'Command Center' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
  });

  it('tells a phone to enable it on the PC', async () => {
    renderApp({ machineId: 'm1', conductor: { local: false } });
    const view = await openCommand();
    const off = await within(view).findByTestId('conductor-off');
    expect(off).toHaveTextContent('Turn it on in the desktop app on the PC');
    expect(within(off).queryByRole('button')).toBeNull();
  });

  it('says when the PC has no conductor', async () => {
    renderApp({ conductor: false });
    const view = await openCommand();
    expect(await within(view).findByText(/has no conductor yet/)).toBeInTheDocument();
  });

  it('opens on the conversation and switches tabs by click and arrow keys', async () => {
    renderApp({ conductor: { seed: builtin() } });
    const view = await openCommand();
    const tablist = await within(view).findByRole('tablist', { name: 'Command Center sections' });
    const tabs = within(tablist).getAllByRole('tab');
    expect(tabs.map((t) => t.textContent?.replace(/\d|\(|\)/g, ''))).toEqual([
      'Conversation',
      'Approvals',
      'Log',
    ]);
    expect(tabs[0]).toHaveAttribute('aria-selected', 'true');
    expect(within(view).getByTestId('cc-conversation')).toBeInTheDocument();
    // Missions/Org and Reports wait for their server features: not shown.
    expect(within(tablist).queryByRole('tab', { name: /Missions|Reports/ })).toBeNull();
    // Settings are not on this page any more.
    expect(within(view).queryByTestId('conductor-settings')).toBeNull();
    expect(within(view).queryByRole('button', { name: 'Allow without asking' })).toBeNull();
    tabs[0]!.focus();
    await userEvent.keyboard('{ArrowRight}');
    expect(within(tablist).getByRole('tab', { name: /Approvals/ })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    expect(within(tablist).getByRole('tab', { name: /Approvals/ })).toHaveFocus();
    expect(within(view).getByRole('tabpanel')).toHaveAttribute('id', 'cc-panel-approvals');
    await userEvent.keyboard('{End}');
    expect(within(tablist).getByRole('tab', { name: 'Log' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    await userEvent.keyboard('{ArrowRight}');
    expect(tabs[0]).toHaveAttribute('aria-selected', 'true');
    await userEvent.keyboard('{ArrowLeft}');
    expect(within(tablist).getByRole('tab', { name: 'Log' })).toHaveFocus();
  });

  it('puts the built-in conductor’s shell next to its chat, not among the tabs', async () => {
    renderApp({ conductor: { seed: builtin() } });
    const view = await openCommand();
    const conversation = await within(view).findByTestId('cc-conversation');
    const modes = within(conversation).getByRole('group', { name: 'View mode' });
    const chat = within(modes).getByRole('button', { name: /Chat/ });
    const shell = within(modes).getByRole('button', { name: 'Shell' });
    expect(chat).toHaveAttribute('aria-pressed', 'true');
    await userEvent.click(shell);
    expect(shell).toHaveAttribute('aria-pressed', 'true');
    expect(await within(view).findByTestId('cc-shell')).toBeInTheDocument();
    await userEvent.click(chat);
    expect(
      within(view).getByRole('textbox', { name: 'Instruction for the conductor' }),
    ).toBeInTheDocument();
  });

  it('chats with the built-in conductor in the chat view', async () => {
    renderApp({ conductor: { seed: builtin() } });
    const view = await openCommand();
    expect(await within(view).findByTestId('conductor-status')).toHaveTextContent('Ready');
    const box = within(view).getByRole('textbox', { name: 'Instruction for the conductor' });
    await userEvent.type(box, 'which sessions need me?');
    await userEvent.click(within(view).getByRole('button', { name: 'Send' }));
    expect(await within(view).findByText('which sessions need me?')).toBeInTheDocument();
    expect(
      await within(view).findByText(/sessions on this PC; nothing needs you/),
    ).toBeInTheDocument();
  });

  it('dictates into the box for review', async () => {
    renderApp({ conductor: { seed: builtin() } });
    const view = await openCommand();
    const mic = await within(view).findByRole('button', { name: 'Dictate' });
    await userEvent.click(mic);
    await userEvent.click(within(view).getByRole('button', { name: 'Stop dictation' }));
    const box = within(view).getByRole('textbox', { name: 'Instruction for the conductor' });
    await waitFor(() => expect(box).toHaveValue('which sessions need me'));
  });

  it('shows how to fix unreadable replies when the server reports missing hooks', async () => {
    const transport = renderApp({ conductor: { seed: running() } });
    const view = await openCommand();
    await within(view).findByTestId('cc-conversation');
    expect(within(view).queryByTestId('cc-hooks-missing')).toBeNull();
    act(() => {
      transport.conductor!.state.problem = 'hooks_missing';
      void transport.conductor!.requestSend('api', 'x');
    });
    const banner = await within(view).findByTestId('cc-hooks-missing');
    expect(banner).toHaveTextContent("The conductor's replies can't be read.");
    expect(banner).toHaveTextContent('Settings › Alerts › Agent connection');
    await userEvent.click(within(banner).getByRole('button', { name: 'Open agent connection' }));
    expect(await screen.findByRole('tab', { name: 'Alerts' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
  });

  it('says when the last turn has not finished for 15 minutes, with Stop and Restart', async () => {
    const transport = renderApp({ conductor: { seed: running() } });
    const view = await openCommand();
    await within(view).findByTestId('cc-conversation');
    expect(within(view).queryByTestId('cc-turn-stalled')).toBeNull();
    act(() => {
      transport.conductor!.state.problem = 'turn_stalled';
      void transport.conductor!.requestSend('api', 'x');
    });
    const banner = await within(view).findByTestId('cc-turn-stalled');
    expect(banner).toHaveTextContent("The conductor's last turn hasn't finished for 15 minutes.");
    expect(banner).toHaveTextContent(/Stop conductor.*start it again here/);
    expect(within(view).queryByTestId('cc-hooks-missing')).toBeNull();
    expect(within(banner).getByRole('button', { name: 'Show terminal' })).toBeInTheDocument();
    // Missing hooks win: the server reports one problem.
    act(() => {
      transport.conductor!.state.problem = 'hooks_missing';
      void transport.conductor!.requestSend('api', 'y');
    });
    expect(await within(view).findByTestId('cc-hooks-missing')).toBeInTheDocument();
    expect(within(view).queryByTestId('cc-turn-stalled')).toBeNull();
  });

  it('tells a phone to restart a stalled conductor on the PC', async () => {
    renderApp({
      machineId: 'm1',
      conductor: { local: false, seed: running({ problem: 'turn_stalled' }) },
    });
    const view = await openCommand();
    const banner = await within(view).findByTestId('cc-turn-stalled');
    expect(banner).toHaveTextContent(/start it again in the desktop app on the PC/);
  });

  it('says the conductor is stopped (not paused) when it is off', async () => {
    renderApp({ conductor: { seed: running({ status: 'off', session_id: null }) } });
    const view = await openCommand();
    const stopped = await within(view).findByTestId('cc-stopped');
    expect(stopped).toHaveTextContent('The conductor is stopped.');
    expect(stopped).not.toHaveTextContent(/paused/i);
  });

  it('counts conductor requests on "more" inside a phone session (no bottom menu there)', async () => {
    renderApp({ conductor: { seed: running({ status: 'waiting_for_you', pending: [PENDING] }) } });
    await screen.findByTestId('session-card-wk-claude-api');
    await nav().findByTestId('conductor-badge');
    const alertsButton = nav().getByRole('button', { name: /^Alerts/ });
    const alerts = Number(/(\d+) unhandled alert/.exec(alertsButton.textContent ?? '')?.[1] ?? 0);
    await userEvent.click(screen.getByTestId('session-card-wk-claude-api'));
    const more = await screen.findByTestId('topbar-more');
    expect(more).toHaveAccessibleName(
      alerts + 1 === 1 ? 'More actions, 1 alert' : `More actions, ${alerts + 1} alerts`,
    );
  });

  it('opens from the session top bar on phones (one conductor control)', async () => {
    renderApp({ conductor: { seed: running() } });
    await userEvent.click(await screen.findByTestId('session-card-wk-claude-api'));
    // Phones: the conductor control is in the top bar's "more" menu only;
    // the composer's tools drawer no longer repeats "Conductor mode".
    await userEvent.click(await screen.findByRole('button', { name: 'More input tools' }));
    expect(screen.queryByRole('button', { name: 'Conductor mode' })).toBeNull();
    await userEvent.click(screen.getByTestId('topbar-more'));
    await userEvent.click(await screen.findByTestId('cc-control-open'));
    expect(await screen.findByTestId('conductor-view')).toBeInTheDocument();
  });
});

describe('Command Center › Approvals', () => {
  it('lists a decision once the conductor escalates it and answers it with the decision card', async () => {
    const transport = renderApp({ conductor: { seed: builtin() } });
    const view = await openCommand();
    const panel = await openTab(view, /Approvals/);
    // Another session's raw decision stays under Alerts, not in Approvals.
    expect(within(panel).queryAllByTestId(/^inbox-item-d:wk-claude-api:/)).toEqual([]);
    expect(nav().queryByTestId('conductor-badge')).toBeNull();
    act(
      () =>
        void transport.conductor!.escalate(
          'wk-claude-api',
          'payments-api wants to delete dist and rebuild.',
          'Allow it: dist is generated.',
        ),
    );
    const item = await within(panel).findByTestId('inbox-item-esc:ce-1');
    expect(item).toHaveTextContent('payments-api');
    expect(item).toHaveTextContent('Decision raised to you');
    expect(item).toHaveTextContent('Command Center summary');
    expect(within(item).getByTestId('escalation-summary')).toHaveTextContent(
      'payments-api wants to delete dist and rebuild.',
    );
    expect(within(item).getByTestId('escalation-recommendation')).toHaveTextContent(
      'Recommendation: Allow it: dist is generated.',
    );
    expect(within(item).getByTestId('escalation-request')).toHaveTextContent('Allow Bash command?');
    // The 지휘소 menu counts it.
    expect(nav().getByTestId('conductor-badge')).toHaveTextContent('1');
    // The same answers as on the session screen.
    await userEvent.click(within(item).getByRole('button', { name: 'Answer' }));
    const sheet = within(item).getByTestId('decision-sheet');
    const option = within(sheet).getAllByRole('button', { name: /^Yes/ })[0]!;
    await userEvent.click(option);
    await waitFor(() => expect(within(panel).queryByTestId('inbox-item-esc:ce-1')).toBeNull());
    expect(nav().queryByTestId('conductor-badge')).toBeNull();
  });

  it('shows a fallback escalation with why it came straight to the person', async () => {
    const transport = renderApp({ conductor: { seed: builtin() } });
    const view = await openCommand();
    const panel = await openTab(view, /Approvals/);
    act(() => void transport.conductor!.escalate('wk-claude-api'));
    const item = await within(panel).findByTestId('inbox-item-esc:ce-1');
    expect(item).toHaveTextContent('Raised automatically');
    expect(within(item).getByTestId('escalation-summary')).toHaveTextContent(
      'did not handle this in time',
    );
    expect(within(item).queryByTestId('escalation-recommendation')).toBeNull();
    expect(within(item).getByRole('button', { name: 'Open session' })).toBeInTheDocument();
  });

  it('answers conductor requests and puts items off for later', async () => {
    const transport = renderApp({
      conductor: { seed: builtin({ status: 'waiting_for_you', pending: [PENDING] }) },
    });
    const view = await openCommand();
    const panel = await openTab(view, /Approvals/);
    const request = await within(panel).findByTestId('inbox-item-cr:cr-1');
    expect(request).toHaveTextContent('The conductor wants to send this to api:');
    expect(request).toHaveTextContent('npm test');
    // Later: moves down and leaves the count.
    const tab = within(view).getByRole('tab', { name: /Approvals/ });
    const before = Number(tab.querySelector('.badge')?.textContent);
    await userEvent.click(within(request).getByRole('button', { name: 'Later' }));
    const later = within(panel).getByTestId('cc-inbox-later');
    expect(within(later).getByTestId('inbox-item-cr:cr-1')).toBeInTheDocument();
    expect(Number(tab.querySelector('.badge')?.textContent ?? 0)).toBe(before - 1);
    await userEvent.click(
      within(within(later).getByTestId('inbox-item-cr:cr-1')).getByRole('button', {
        name: 'Back to the list',
      }),
    );
    await userEvent.click(
      within(within(panel).getByTestId('inbox-item-cr:cr-1')).getByRole('button', {
        name: 'Allow',
      }),
    );
    await waitFor(() => expect(within(panel).queryByTestId('inbox-item-cr:cr-1')).toBeNull());
    expect(transport.conductor!.state.pending).toEqual([]);
    expect(nav().queryByTestId('conductor-badge')).toBeNull();
    // A new request arrives as a push.
    act(() => void transport.conductor!.requestSend('api', 'git status'));
    expect(await within(panel).findByText('git status')).toBeInTheDocument();
    await userEvent.click(within(panel).getByRole('button', { name: 'Deny' }));
    await waitFor(() => expect(within(panel).queryByText('git status')).toBeNull());
  });

  it('still shows conductor requests under Alerts', async () => {
    renderApp({ conductor: { seed: running({ status: 'waiting_for_you', pending: [PENDING] }) } });
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(nav().getByRole('button', { name: /Alerts/ }));
    expect(await screen.findByTestId('conductor-request-cr-1')).toHaveTextContent(
      'The conductor wants to send this to api:',
    );
  });

  it('says why a request waits for the person', async () => {
    const pending = [{ ...PENDING, reason: 'not_named' }];
    renderApp({ conductor: { seed: running({ status: 'waiting_for_you', pending }) } });
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(nav().getByRole('button', { name: /Alerts/ }));
    expect(await screen.findByTestId('conductor-request-cr-1')).toHaveTextContent(
      'Your instruction did not name this session.',
    );
  });
});

describe('Command Center › Log', () => {
  it('reads audit entries as sentences', async () => {
    const transport = renderApp({ conductor: { seed: builtin() } });
    transport.recordAudit(
      'conductor_send',
      'wk-claude-api',
      'outcome=sent confirmed_by=allowlist text=keep going with the refund tests',
    );
    transport.recordAudit('conductor_kill', null, '');
    transport.recordAudit('shell_open', null, 'shell=pwsh');
    const view = await openCommand();
    const panel = await openTab(view, 'Log');
    const log = await within(panel).findByTestId('cc-log');
    expect(log).toHaveTextContent(
      'Sent “keep going with the refund tests” to payments-api (auto-allowed).',
    );
    expect(log).toHaveTextContent('The kill switch stopped the conductor.');
    expect(log).not.toHaveTextContent('outcome=');
    // Only the conductor's entries.
    expect(log).not.toHaveTextContent('pwsh');
  });
});

describe('Header control', () => {
  it('is hidden while the conductor is off', async () => {
    renderApp();
    await screen.findByTestId('session-card-wk-claude-api');
    expect(screen.queryByTestId('cc-control-open')).toBeNull();
  });

  it('pauses, starts again and trips the kill switch from any page', async () => {
    const transport = renderApp({ conductor: { seed: builtin() } });
    await screen.findByTestId('session-card-wk-claude-api');
    const open = await screen.findByTestId('cc-control-open');
    expect(open).toHaveAccessibleName(/Command Center: Ready/);
    const trigger = screen.getByRole('button', { name: 'Pause or kill switch' });
    expect(trigger).toHaveAttribute('aria-haspopup', 'menu');
    // Keyboard: the menu opens on the trigger, arrows move, Escape closes.
    trigger.focus();
    await userEvent.keyboard('{Enter}');
    const menu = screen.getByRole('menu', { name: 'Pause or kill switch' });
    expect(trigger).toHaveAttribute('aria-expanded', 'true');
    // Phones: hands-free sits in the menu too (a checkbox item), first.
    const items = [...menu.querySelectorAll<HTMLElement>('[role^="menuitem"]')];
    expect(within(menu).getByRole('menuitemcheckbox', { name: /^Hands-free mode/ })).toBe(items[0]);
    expect(items[0]).toHaveFocus();
    await userEvent.keyboard('{ArrowUp}');
    expect(items[items.length - 1]).toHaveFocus();
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('menu')).toBeNull();
    expect(trigger).toHaveFocus();
    // Pause: `cc_pause` (nothing wakes it; its session keeps running).
    await userEvent.click(trigger);
    expect(screen.getByRole('menuitem', { name: /Pause conductor/ })).toHaveTextContent(
      /until you resume it/,
    );
    await userEvent.click(screen.getByRole('menuitem', { name: /Pause conductor/ }));
    await waitFor(() => expect(transport.conductor!.state.status).toBe('paused'));
    expect(transport.conductor!.state.settings.paused).toBe(true);
    expect(screen.getByTestId('cc-control-open')).toHaveAccessibleName(/Paused/);
    // Resume: `cc_pause` again, no verification.
    await userEvent.click(trigger);
    expect(screen.queryByRole('menuitem', { name: /Pause conductor/ })).toBeNull();
    await userEvent.click(screen.getByRole('menuitem', { name: /Resume conductor/ }));
    await waitFor(() => expect(transport.conductor!.state.status).toBe('ready'));
    expect(transport.conductor!.state.settings.paused).toBe(false);
    expect(stepUpApi.run).not.toHaveBeenCalled();
    // Stop ends it; start again (on the PC, behind the step-up).
    await userEvent.click(trigger);
    await userEvent.click(screen.getByRole('menuitem', { name: /Stop conductor/ }));
    await waitFor(() => expect(transport.conductor!.state.status).toBe('off'));
    expect(screen.getByTestId('cc-control-open')).toHaveAccessibleName(/Off/);
    await userEvent.click(trigger);
    await userEvent.click(screen.getByRole('menuitem', { name: /Start conductor again/ }));
    await waitFor(() => expect(transport.conductor!.state.status).toBe('ready'));
    expect(stepUpApi.run).toHaveBeenLastCalledWith('conductor_setup', expect.any(Function));
    // The kill switch asks first, then verifies on the PC (step-up).
    await userEvent.click(trigger);
    await userEvent.click(screen.getByRole('menuitem', { name: /Kill switch/ }));
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveTextContent('Kill switch');
    expect(transport.conductor!.state.status).toBe('ready');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Kill switch' }));
    await waitFor(() => expect(transport.conductor!.state.status).toBe('revoked'));
    expect(stepUpApi.run).toHaveBeenLastCalledWith('conductor_kill', expect.any(Function));
    // Revoked: no second kill switch in the menu.
    await userEvent.click(trigger);
    expect(screen.queryByRole('menuitem', { name: /Kill switch/ })).toBeNull();
  });

  it('keeps the kill switch undone when the step-up on the PC is declined', async () => {
    stepUpApi.run.mockImplementationOnce(async (action: string) => {
      if (action === 'conductor_kill') throw new NativeError(STEP_UP_CANCELED, 'canceled');
      throw new Error(`unexpected ${action}`);
    });
    const transport = renderApp({ conductor: { seed: builtin() } });
    await screen.findByTestId('cc-control-open');
    await userEvent.click(screen.getByRole('button', { name: 'Pause or kill switch' }));
    await userEvent.click(screen.getByRole('menuitem', { name: /Kill switch/ }));
    const dialog = await screen.findByRole('dialog');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Kill switch' }));
    await waitFor(() =>
      expect(stepUpApi.run).toHaveBeenCalledWith('conductor_kill', expect.any(Function)),
    );
    expect(transport.conductor!.state.status).toBe('ready');
    // Declining is no error.
    expect(screen.queryByText(/Request failed/)).toBeNull();
  });

  it('pauses by stopping on an older server without command_center', async () => {
    const transport = renderApp({ conductor: { seed: builtin(), commandCenter: false } });
    await screen.findByTestId('cc-control-open');
    const trigger = screen.getByRole('button', { name: 'Pause or kill switch' });
    await userEvent.click(trigger);
    expect(screen.queryByRole('menuitem', { name: /Stop conductor/ })).toBeNull();
    await userEvent.click(screen.getByRole('menuitem', { name: /Pause conductor/ }));
    await waitFor(() => expect(transport.conductor!.state.status).toBe('off'));
    expect(transport.conductor!.state.settings.paused).toBeUndefined();
  });

  it('lets a phone pause and resume', async () => {
    const transport = renderApp({
      machineId: 'm1',
      conductor: { local: false, seed: builtin() },
    });
    await screen.findByTestId('cc-control-open');
    const trigger = screen.getByRole('button', { name: 'Pause or kill switch' });
    await userEvent.click(trigger);
    await userEvent.click(screen.getByRole('menuitem', { name: /Pause conductor/ }));
    await waitFor(() => expect(transport.conductor!.state.status).toBe('paused'));
    // The Command Center says so, with its own resume.
    const view = await openCommand();
    const note = await within(view).findByTestId('cc-paused');
    expect(note).toHaveTextContent('nothing wakes the conductor');
    await userEvent.click(within(note).getByRole('button', { name: 'Resume conductor' }));
    await waitFor(() => expect(transport.conductor!.state.status).toBe('ready'));
    expect(within(view).queryByTestId('cc-paused')).toBeNull();
  });

  it('asks a phone for the authenticator code before the kill switch', async () => {
    const transport = renderApp({
      machineId: 'm1',
      conductor: { local: false, seed: builtin() },
    });
    await screen.findByTestId('cc-control-open');
    await userEvent.click(screen.getByRole('button', { name: 'Pause or kill switch' }));
    await userEvent.click(screen.getByRole('menuitem', { name: /Kill switch/ }));
    const dialog = await screen.findByRole('dialog', { name: 'Kill switch' });
    expect(dialog).toHaveTextContent('needs the code from your authenticator app');
    const field = within(dialog).getByLabelText('Authenticator code');
    // A wrong code: refused (and audited), the prompt stays with a message.
    await userEvent.type(field, '000000');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Kill switch' }));
    expect(
      await within(dialog).findByText(/That code is wrong or was already used/),
    ).toBeInTheDocument();
    expect(field).toHaveValue('');
    expect(transport.conductor!.state.status).toBe('ready');
    // The right code trips it.
    await userEvent.type(field, '135790');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Kill switch' }));
    await waitFor(() => expect(transport.conductor!.state.status).toBe('revoked'));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(stepUpApi.run).not.toHaveBeenCalled();
    // The refusal is in the Log.
    const view = await openCommand();
    const log = await within(await openTab(view, 'Log')).findByTestId('cc-log');
    expect(log).toHaveTextContent('A kill switch request was refused');
  });

  it('explains each refusal of a phone kill switch', () => {
    const en = (key: MessageKey, vars?: Vars) => translate('en', key, vars);
    const text = (code: ErrorCode) => killErrorText(en, new RequestError(code, 'x'));
    expect(text('totp_required')).toMatch(/Enter the 6-digit code/);
    expect(text('totp_invalid')).toMatch(/wrong or was already used/);
    expect(text('forbidden')).toMatch(/Too many wrong codes/);
    expect(text('unsupported')).toMatch(/No authenticator app is set up on the PC/);
    expect(text('unavailable')).toMatch(/security service is off/);
    expect(text('internal')).toBeNull();
    expect(killErrorText(en, new Error('x'))).toBeNull();
  });

  it('opens the Command Center on Approvals when something waits', async () => {
    renderApp({ conductor: { seed: builtin({ status: 'waiting_for_you', pending: [PENDING] }) } });
    await screen.findByTestId('session-card-wk-claude-api');
    const open = await screen.findByTestId('cc-control-open');
    await waitFor(() => expect(open).toHaveAccessibleName(/[1-9]\d* waiting/));
    await userEvent.click(open);
    const view = await screen.findByTestId('conductor-view');
    expect(within(view).getByRole('tab', { name: /Approvals/ })).toHaveAttribute(
      'aria-selected',
      'true',
    );
  });

  it('puts what you say into the conductor box, never sending it', async () => {
    const transport = renderApp({ conductor: { seed: builtin() } });
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(await screen.findByRole('button', { name: 'Talk to the conductor' }));
    await userEvent.click(screen.getByRole('button', { name: 'Stop and review' }));
    const view = await screen.findByTestId('conductor-view');
    const box = await within(view).findByRole('textbox', { name: 'Instruction for the conductor' });
    await waitFor(() => expect(box).toHaveValue('which sessions need me'));
    expect(transport.conductor!.state.chat).toEqual([]);
  });
});

describe('Settings › Command Center', () => {
  async function openSettings() {
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(nav().getByRole('button', { name: /Settings/ }));
    await userEvent.click(await screen.findByRole('tab', { name: 'Command Center' }));
  }

  it('turns the conductor on and chooses the agent on the PC', async () => {
    const transport = renderApp();
    await openSettings();
    const panel = await screen.findByTestId('conductor-settings');
    expect(panel).toHaveTextContent('Off by default');
    const enable = within(panel).getByRole('checkbox', { name: 'Use a conductor on this PC' });
    expect(enable).not.toBeChecked();
    await userEvent.click(enable);
    await waitFor(() => expect(transport.conductor!.state.settings.enabled).toBe(true));
    expect(within(panel).getByRole('radio', { name: /Claude Code/ })).toBeChecked();
    expect(within(panel).getByRole('radio', { name: /Codex CLI/ })).not.toBeChecked();
    expect(panel).toHaveTextContent('experimental');
    expect(panel).toHaveTextContent('not found on this PC');
    // The built-in conductor needs an LLM profile.
    await userEvent.click(within(panel).getByRole('radio', { name: /Built-in/ }));
    await userEvent.selectOptions(
      within(panel).getByRole('combobox', { name: 'LLM profile' }),
      'local-llm',
    );
    await userEvent.click(
      within(panel).getByRole('checkbox', { name: 'Pass other events to the conductor' }),
    );
    expect(panel).toHaveTextContent('at most once every 2 minutes and 6 times an hour');
    await userEvent.click(within(panel).getByRole('button', { name: 'Save' }));
    await waitFor(() => {
      const s = transport.conductor!.state.settings;
      expect(s.agent).toBe('builtin');
      expect(s.builtin_llm).toBe('local-llm');
      expect(s.wake_events).toBe(true);
    });
    expect(await within(panel).findByText('Saved.')).toBeInTheDocument();
    expect(transport.conductor!.state.model).toBe('qwen2.5-7b-instruct');
  });

  it('holds auto-allow (moved from the page) on the PC', async () => {
    renderApp({ conductor: { seed: running() } });
    await openSettings();
    const panel = await screen.findByTestId('conductor-settings');
    expect(
      await within(panel).findByText('Every send needs your confirmation.'),
    ).toBeInTheDocument();
    await userEvent.click(within(panel).getByRole('button', { name: 'Allow without asking' }));
    expect(await within(panel).findByText(/ · until /)).toBeInTheDocument();
    await userEvent.click(within(panel).getByRole('button', { name: 'Remove' }));
    expect(
      await within(panel).findByText('Every send needs your confirmation.'),
    ).toBeInTheDocument();
  });

  it('is not offered on a paired phone', async () => {
    renderApp({ machineId: 'm1', conductor: { local: false } });
    await openSettings();
    expect(
      await screen.findByText('The conductor is set up in the desktop app on the PC.'),
    ).toBeInTheDocument();
    expect(screen.queryByTestId('conductor-settings')).toBeNull();
  });
});

describe("The conductor's own session", () => {
  it('has no card in the phone carousel, and is the conversation in Command Center', async () => {
    const transport = renderApp({ conductor: { seed: running() } });
    await screen.findByTestId('session-card-wk-claude-api');
    act(() => transport.putSession(conductorSession()));
    await act(async () => {});
    expect(screen.queryByTestId('session-card-mock-conductor')).toBeNull();
    const view = await openCommand();
    const conversation = await within(view).findByTestId('cc-conversation');
    // The normal session view: its title, chat and composer.
    expect(
      await within(conversation).findByRole('heading', { name: 'Conductor', level: 2 }),
    ).toBeInTheDocument();
    expect(within(conversation).getByRole('button', { name: /Chat/ })).toBeInTheDocument();
    expect(within(conversation).queryByRole('button', { name: 'Conductor mode' })).toBeNull();
    // Back on the session screen, the person's own selection is shown.
    await userEvent.click(nav().getByRole('button', { name: /Sessions/ }));
    expect(screen.queryByRole('heading', { name: 'Conductor', level: 2 })).toBeNull();
  });

  it('is left out of the list and its numbers', async () => {
    wideScreen();
    const transport = renderApp({ conductor: { seed: running() } });
    const card = () => screen.getByTestId('session-card-wk-claude-api');
    await screen.findByTestId('session-card-wk-claude-api');
    expect(card().textContent).toContain('1payments-api');
    act(() => transport.putSession(conductorSession()));
    // First in the order, it still has no card and takes no number.
    await act(async () => {
      await transport.send({
        id: 9001,
        body: { type: 'set_session_order', session_ids: ['mock-conductor'] },
      } as never);
    });
    expect(screen.queryByTestId('session-card-mock-conductor')).toBeNull();
    expect(card().textContent).toContain('1payments-api');
  });

  it('asks to trust its folder on first start, in Command Center', async () => {
    const transport = renderApp({ conductor: { seed: running() } });
    await screen.findByTestId('session-card-wk-claude-api');
    act(() => transport.putSession(conductorSession({ terminal_prompt: 'trust_folder' })));
    // The Command badge counts it.
    expect(await nav().findByTestId('conductor-badge')).toHaveTextContent('1');
    const view = await openCommand();
    const trust = await within(view).findByTestId('conductor-trust');
    expect(trust).toHaveTextContent('First start: Claude Code asks whether to trust this folder');
    await userEvent.click(within(trust).getByRole('button', { name: 'Trust and start' }));
    expect(within(trust).queryByRole('alert')).toBeNull();
  });

  it('shows its terminal inside Command Center, not an empty session screen', async () => {
    const transport = renderApp({ conductor: { seed: running() } });
    await screen.findByTestId('session-card-wk-claude-api');
    act(() => transport.putSession(conductorSession({ terminal_prompt: 'trust_folder' })));
    const view = await openCommand();
    const trust = await within(view).findByTestId('conductor-trust');
    await waitFor(() =>
      expect(view.querySelector('[data-mode]')).toHaveAttribute('data-mode', 'chat'),
    );
    await userEvent.click(within(trust).getByRole('button', { name: 'Show terminal' }));
    // Still in Command Center, on the conversation, its session view now on the terminal.
    expect(screen.getByTestId('conductor-view')).toBe(view);
    expect(within(view).getByRole('tab', { name: 'Conversation' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    await waitFor(() =>
      expect(view.querySelector('[data-mode]')).toHaveAttribute('data-mode', 'terminal'),
    );
  });

  it('keeps its decisions in Alerts, labelled, and opens them in Command Center', async () => {
    const transport = renderApp({ conductor: { seed: running() } });
    await screen.findByTestId('session-card-wk-claude-api');
    // A finished turn: no alert.
    act(() => transport.putSession(conductorSession({ state: { type: 'running' } })));
    act(() => transport.putSession(conductorSession({ state: { type: 'idle' } })));
    await userEvent.click(nav().getByRole('button', { name: /Alerts/ }));
    expect(screen.queryByTestId('alert-conductor')).toBeNull();
    // A permission decision: listed as "Conductor session".
    act(() =>
      transport.putSession(
        conductorSession({
          state: { type: 'awaiting_decision' },
          pending_decision: {
            at: NOW,
            decision: {
              id: 'd-bash',
              prompt: 'Allow Bash: rm -rf build?',
              options: [
                { id: '1', label: 'Yes' },
                { id: '2', label: 'No' },
              ],
            },
          },
        }),
      ),
    );
    expect(await screen.findByTestId('alert-conductor')).toHaveTextContent('Conductor session');
    // The Approvals inbox lists it too, labelled.
    await userEvent.click(nav().getByRole('button', { name: /Command/ }));
    const view = await screen.findByTestId('conductor-view');
    const panel = await openTab(view, /Approvals/);
    const item = await within(panel).findByTestId('inbox-item-d:mock-conductor:d-bash');
    expect(item).toHaveTextContent('Conductor session');
    expect(item).toHaveTextContent('Allow Bash: rm -rf build?');
    // "Open session" stays in Command Center, on the conversation with the card.
    await userEvent.click(within(item).getByRole('button', { name: 'Open session' }));
    expect(within(view).getByRole('tab', { name: 'Conversation' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    expect(await within(view).findByTestId('decision-sheet')).toBeInTheDocument();
  });
});

describe('Header "talk to the conductor"', () => {
  function renderTalk(settings: Partial<Settings> = {}, text = 'which sessions need me') {
    const transport = new MockTransport({
      latencyMs: 0,
      autoTick: false,
      conductor: { seed: builtin() },
    });
    const rec = {
      start: vi.fn(async () => {}),
      stop: vi.fn(async () => new Blob(['x'])),
      cancel: vi.fn(),
    };
    const transcribe = vi.fn(async () => ({ text, corrections: [] }));
    render(
      <App
        machines={[
          {
            id: 'local',
            name: 'workstation',
            connectivity: 'LAN',
            transport,
            simulate: { drop: () => {} },
          },
        ]}
        speech={{ ...speech, transcribe, createRecorder: () => rec }}
        settings={{ ...defaultSettings(), language: 'en', ...settings }}
      />,
    );
    return { rec, transcribe };
  }
  const talk = () => screen.findByTestId('cc-talk');

  it('takes the microphone from Voco while it records; Cancel and Escape discard it', async () => {
    const { rec, transcribe } = renderTalk();
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(await talk());
    await waitFor(() =>
      expect(screen.getByTestId('cc-talk')).toHaveAttribute('aria-pressed', 'true'),
    );
    // Voco is paused and spoken decisions wait, as for a composer dictation.
    expect(micBorrowed()).toBe(true);
    expect(isDictationActive()).toBe(true);
    await userEvent.click(screen.getByTestId('cc-talk-cancel'));
    expect(rec.cancel).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId('cc-talk')).toHaveAttribute('aria-pressed', 'false');
    expect(isDictationActive()).toBe(false);
    expect(micBorrowed()).toBe(false);
    // Escape discards too, wherever the focus is.
    await userEvent.click(screen.getByTestId('cc-talk'));
    await waitFor(() =>
      expect(screen.getByTestId('cc-talk')).toHaveAttribute('aria-pressed', 'true'),
    );
    await userEvent.keyboard('{Escape}');
    expect(rec.cancel).toHaveBeenCalledTimes(2);
    expect(screen.queryByTestId('cc-talk-cancel')).toBeNull();
    expect(transcribe).not.toHaveBeenCalled();
    expect(screen.queryByTestId('conductor-view')).toBeNull();
  });

  it('records while held with Hold to talk, and opens the transcript for review on release', async () => {
    const { rec, transcribe } = renderTalk({ micMode: 'hold' });
    await screen.findByTestId('session-card-wk-claude-api');
    const button = await talk();
    fireEvent.pointerDown(button, { pointerId: 1, button: 0 });
    await waitFor(() => expect(button).toHaveAttribute('aria-pressed', 'true'));
    expect(rec.start).toHaveBeenCalledTimes(1);
    fireEvent.pointerUp(button, { pointerId: 1, button: 0 });
    await waitFor(() => expect(transcribe).toHaveBeenCalledTimes(1));
    expect(rec.stop).toHaveBeenCalledTimes(1);
    expect(await screen.findByTestId('conductor-view')).toBeInTheDocument();
    expect(isDictationActive()).toBe(false);
  });

  it('stays put when nothing was heard', async () => {
    const { transcribe } = renderTalk({}, '  ');
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(await talk());
    await waitFor(() =>
      expect(screen.getByTestId('cc-talk')).toHaveAttribute('aria-pressed', 'true'),
    );
    await userEvent.click(screen.getByTestId('cc-talk'));
    await waitFor(() => expect(transcribe).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(screen.getByTestId('cc-talk')).toHaveAttribute('aria-pressed', 'false'),
    );
    expect(screen.queryByTestId('conductor-view')).toBeNull();
  });

  it('stops a tap recording when the app goes to the background', async () => {
    const { rec, transcribe } = renderTalk();
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(await talk());
    await waitFor(() =>
      expect(screen.getByTestId('cc-talk')).toHaveAttribute('aria-pressed', 'true'),
    );
    Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true });
    try {
      await act(async () => {
        document.dispatchEvent(new Event('visibilitychange'));
      });
      await waitFor(() => expect(rec.stop).toHaveBeenCalledTimes(1));
      await waitFor(() => expect(transcribe).toHaveBeenCalledTimes(1));
    } finally {
      delete (document as unknown as Record<string, unknown>).visibilityState;
    }
  });
});
