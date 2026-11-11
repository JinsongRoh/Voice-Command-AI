import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { App } from '../App';
import en from '../i18n/en.json';
import es from '../i18n/es.json';
import ja from '../i18n/ja.json';
import ko from '../i18n/ko.json';
import { mockRestoreLocal } from '../protocol/mockRestore';
import { MockTransport } from '../protocol/mockTransport';
import type { DeviceScope } from '../protocol/types';
import { noteComposer, resetInputGuard } from '../state/inputGuard';
import { defaultSettings, type ApprovalMode, type Settings } from '../state/settings';
import type { RestoreLocalServices } from '../state/store';

const speech = {
  transcribe: async () => ({ text: '', corrections: [] }),
  createRecorder: () => ({
    start: async () => {},
    stop: async () => new Blob(),
    cancel: () => {},
  }),
  fallbackWithoutMic: false,
};

function renderApp(
  options: {
    mode?: ApprovalMode;
    restoreLocal?: RestoreLocalServices;
    profile?: 'workstation' | 'build_server';
    settings?: Partial<Settings>;
  } = {},
) {
  const transport = new MockTransport({
    latencyMs: 0,
    autoTick: false,
    ...(options.profile ? { profile: options.profile } : {}),
  });
  render(
    <App
      machines={[
        {
          id: 'm1',
          name: 'workstation',
          connectivity: 'LAN',
          transport,
          simulate: {
            drop: () => {},
            ...(options.restoreLocal ? { restoreLocal: options.restoreLocal } : {}),
          },
        },
      ]}
      speech={speech}
      settings={{
        ...defaultSettings(),
        language: 'en',
        approvalMode: options.mode ?? 'ask',
        ...options.settings,
      }}
    />,
  );
  return transport;
}

const nav = () => within(screen.getByRole('navigation', { name: 'Main navigation' }));

async function openMenu(name: string | RegExp) {
  await screen.findByTestId('session-card-wk-claude-api');
  await userEvent.click(nav().getByRole('button', { name }));
}

/** A session that was not waiting asks for a decision. */
function ask(transport: MockTransport) {
  act(() =>
    transport.simulateDecision('wk-codex-web', {
      id: 'fresh-1',
      prompt: 'Allow Bash command?\nnpm test',
      options: [
        { id: 'yes', label: 'Yes' },
        { id: 'no', label: 'No' },
      ],
    }),
  );
}

afterEach(() => {
  resetInputGuard();
  vi.unstubAllGlobals();
});

describe('main navigation', () => {
  it('has the six menus in the agreed order', async () => {
    renderApp();
    await screen.findByTestId('session-card-wk-claude-api');
    const labels = nav()
      .getAllByRole('button')
      .map((b) => b.textContent);
    expect(labels[0]).toMatch(/^Sessions/);
    expect(labels[1]).toMatch(/^.*Command/);
    expect(labels[2]).toMatch(/^.*Alerts/);
    expect(labels[3]).toMatch(/^Resources/);
    expect(labels[4]).toMatch(/^Windows/);
    expect(labels[5]).toMatch(/^Settings/);
  });

  it('moves the count badge from Sessions to Alerts', async () => {
    renderApp();
    await screen.findByTestId('session-card-wk-claude-api');
    const buttons = nav().getAllByRole('button');
    expect(buttons[0]!.querySelector('.badge')).toBeNull();
    expect(buttons[2]!.querySelector('.badge')).not.toBeNull();
  });

  it.each([
    ['en', en],
    ['ko', ko],
    ['ja', ja],
    ['es', es],
  ])('labels fit six items at 360 px in %s', (_, messages) => {
    // 11 px labels below 420 px (App.module.css), 1 px padding per side.
    const budget = 360 / 6 - 2;
    const width = (text: string) =>
      [...text].reduce((px, ch) => px + (ch.charCodeAt(0) > 0x2e7f ? 11 : 6.4), 0);
    for (const key of [
      'nav.sessions',
      'nav.conductor',
      'nav.alerts',
      'nav.alertsTasks',
      'nav.resources',
      'nav.windows',
      'nav.settings',
    ] as const) {
      expect(width(messages[key])).toBeLessThanOrEqual(budget);
    }
  });
});

describe('alerts menu', () => {
  it('lists waiting sessions and jumps to the decision card', async () => {
    renderApp();
    await openMenu(/Alerts/);
    const item = await screen.findByRole('button', { name: 'Open payments-api at its request' });
    await userEvent.click(item);
    const sheet = await screen.findByRole('region', { name: 'Pending decision for payments-api' });
    await waitFor(() => expect(sheet).toHaveFocus());
    // On phones the jump shows the whole question.
    expect(sheet).toHaveAttribute('data-expanded', 'true');
  });

  it('answering removes it from the list and the count', async () => {
    renderApp();
    await openMenu(/Alerts/);
    const before = screen.getAllByRole('button', { name: /^Open .* at its request$/ }).length;
    await userEvent.click(screen.getByRole('button', { name: 'Open payments-api at its request' }));
    const sheet = await screen.findByRole('region', { name: 'Pending decision for payments-api' });
    await userEvent.click(within(sheet).getByRole('button', { name: 'Yes' }));
    // Phones: the session view has no bottom menu; Back returns to the list.
    await userEvent.click(screen.getByRole('button', { name: 'Back to sessions' }));
    await userEvent.click(nav().getByRole('button', { name: /Alerts/ }));
    expect(screen.queryByRole('button', { name: 'Open payments-api at its request' })).toBeNull();
    expect(screen.queryAllByRole('button', { name: /^Open .* at its request$/ })).toHaveLength(
      before - 1,
    );
  });

  it('lists an inferred decision, counts it and jumps to a card without answers', async () => {
    const transport = renderApp();
    await screen.findByTestId('session-card-wk-claude-api');
    const alertsButton = () => nav().getByRole('button', { name: /Alerts/ });
    const count = () => Number(alertsButton().querySelector('.badge')?.textContent ?? '0');
    const before = count();
    act(() =>
      transport.simulateDecision('wk-codex-web', {
        id: 'codex:abc:inferred-call_7',
        prompt: 'Allow shell: npm publish?',
        options: [],
        inferred: true,
      }),
    );
    await waitFor(() => expect(count()).toBe(before + 1));
    await userEvent.click(alertsButton());
    expect(
      await screen.findByText('Seems to need confirmation · Allow shell: npm publish?'),
    ).toBeInTheDocument();
    await userEvent.click(
      screen.getByRole('button', { name: 'Open web-dashboard at its request' }),
    );
    const sheet = await screen.findByRole('region', {
      name: 'Pending decision for web-dashboard',
    });
    expect(within(sheet).getByRole('heading')).toHaveTextContent('Seems to need confirmation');
    expect(sheet).toHaveTextContent('Choose in the PC window or answer with the key buttons.');
    expect(within(sheet).queryByRole('button', { name: 'Yes' })).toBeNull();
    expect(within(sheet).queryByRole('button', { name: 'No' })).toBeNull();
  });

  it('mark as handled clears an item without answering it, with Undo', async () => {
    renderApp();
    await openMenu(/Alerts/);
    await userEvent.click(screen.getByRole('button', { name: 'Mark payments-api as handled' }));
    expect(screen.queryByRole('button', { name: 'Open payments-api at its request' })).toBeNull();
    const region = within(screen.getByRole('region', { name: 'Notifications' }));
    expect(region.getByText('payments-api marked as handled.')).toBeInTheDocument();
    await userEvent.click(region.getByRole('button', { name: 'Undo' }));
    // Still waiting: it is back on the list.
    expect(
      await screen.findByRole('button', { name: 'Open payments-api at its request' }),
    ).toBeInTheDocument();
    expect(region.queryByText('payments-api marked as handled.')).toBeNull();
  });

  it('offers the three modes, asking by default', async () => {
    renderApp();
    await openMenu(/Alerts/);
    const group = screen.getByRole('group', { name: 'When approval is needed' });
    expect(within(group).getByRole('button', { name: 'Ask' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    await userEvent.click(within(group).getByRole('button', { name: 'Switch automatically' }));
    expect(screen.getByText(/Nothing is approved for you/)).toBeInTheDocument();
  });

  it('asks with Go and Later when a new decision comes', async () => {
    const transport = renderApp();
    await screen.findByTestId('session-card-wk-claude-api');
    ask(transport);
    expect(await screen.findByRole('button', { name: 'Go' })).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Later' }));
    expect(screen.queryByRole('button', { name: 'Go' })).toBeNull();
  });

  it("never switches in Don't switch mode but still shows a notice", async () => {
    const transport = renderApp({ mode: 'off' });
    await screen.findByTestId('session-card-wk-claude-api');
    ask(transport);
    const region = within(screen.getByRole('region', { name: 'Notifications' }));
    expect(await region.findByText(/needs a decision/)).toBeInTheDocument();
    // A plain notice: no Go / Later prompt, and the screen stays where it is.
    expect(region.getByRole('button', { name: 'Open' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Go' })).toBeNull();
    expect(screen.queryByTestId('decision-sheet')).toBeNull();
  });

  it("explains the Don't switch mode", async () => {
    renderApp();
    await openMenu(/Alerts/);
    const group = screen.getByRole('group', { name: 'When approval is needed' });
    await userEvent.click(within(group).getByRole('button', { name: "Don't switch" }));
    expect(screen.getByText(/never switches the screen/)).toBeInTheDocument();
  });

  it('switches automatically when the person is not typing', async () => {
    const transport = renderApp({ mode: 'auto' });
    await screen.findByTestId('session-card-wk-claude-api');
    ask(transport);
    expect(await screen.findByTestId('decision-sheet')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Go' })).toBeNull();
  });

  it('asks instead of switching while there is text in the composer', async () => {
    const transport = renderApp({ mode: 'auto' });
    await screen.findByTestId('session-card-wk-claude-api');
    noteComposer({ hasText: true, dictating: false });
    ask(transport);
    expect(await screen.findByRole('button', { name: 'Go' })).toBeInTheDocument();
    expect(screen.queryByTestId('decision-sheet')).toBeNull();
  });

  it('asks instead of switching during dictation', async () => {
    const transport = renderApp({ mode: 'auto' });
    await screen.findByTestId('session-card-wk-claude-api');
    noteComposer({ hasText: false, dictating: true });
    ask(transport);
    expect(await screen.findByRole('button', { name: 'Go' })).toBeInTheDocument();
    expect(screen.queryByTestId('decision-sheet')).toBeNull();
  });
});

describe('os notification', () => {
  it('names the session and opens it on click, only while the app is away', async () => {
    const shown: { title: string; onclick: (() => void) | null; close(): void }[] = [];
    class FakeNotification {
      static permission = 'granted';
      onclick: (() => void) | null = null;
      constructor(readonly title: string) {
        shown.push(this);
      }
      close() {}
    }
    vi.stubGlobal('Notification', FakeNotification);
    vi.spyOn(document, 'hasFocus').mockReturnValue(false);
    vi.spyOn(window, 'focus').mockImplementation(() => {});
    const transport = renderApp();
    await screen.findByTestId('session-card-wk-claude-api');
    ask(transport);
    expect(shown).toHaveLength(1);
    expect(shown[0]!.title).toContain('needs a decision');
    act(() => shown[0]!.onclick?.());
    expect(await screen.findByTestId('decision-sheet')).toBeInTheDocument();
    vi.restoreAllMocks();
  });

  it('respects the per-category switch', async () => {
    const shown: string[] = [];
    class FakeNotification {
      static permission = 'granted';
      onclick: (() => void) | null = null;
      constructor(readonly title: string) {
        shown.push(title);
      }
      close() {}
    }
    vi.stubGlobal('Notification', FakeNotification);
    vi.spyOn(document, 'hasFocus').mockReturnValue(false);
    const transport = renderApp({
      settings: { notifications: { ...defaultSettings().notifications, decision: false } },
    });
    await screen.findByTestId('session-card-wk-claude-api');
    ask(transport);
    expect(shown).toEqual([]);
    vi.restoreAllMocks();
  });
});

describe('windows menu', () => {
  const openWindows = async () => {
    const transport = renderApp();
    await openMenu(/Windows/);
    await screen.findByRole('heading', { level: 3, name: /Saved windows/ });
    return transport;
  };
  // The session order list above it names the same sessions.
  const saved = () => within(screen.getByRole('region', { name: /Saved windows/ }));

  it('shows the list with open and closed windows', async () => {
    await openWindows();
    expect(saved().getByText('payments-api')).toBeInTheDocument();
    expect(screen.getAllByText('Open').length).toBeGreaterThan(0);
    expect(screen.getAllByText('Closed').length).toBeGreaterThan(0);
    // Sessions VCA started have no window here: the tab says where they go.
    expect(screen.getByTestId('restore-vca-note')).toHaveTextContent(
      /Sessions started in VCA have no window on the PC.*Session order tab/,
    );
  });

  it('previews, asks for a code and reports progress per window', async () => {
    const transport = await openWindows();
    await userEvent.click(screen.getByRole('button', { name: 'Restore all' }));
    const dialog = await screen.findByRole('dialog', { name: 'Restore windows' });
    // The open one is skipped, nothing has started.
    expect(within(dialog).getAllByText('Skipped: already open').length).toBe(2);
    expect(within(dialog).getByText('claude --resume c44d10ee')).toBeInTheDocument();
    await userEvent.click(within(dialog).getByRole('button', { name: 'Open 2 windows' }));
    const totp = await screen.findByRole('dialog', { name: 'Confirm restore' });
    await userEvent.type(within(totp).getByRole('textbox'), '123456');
    await userEvent.click(within(totp).getByRole('button', { name: 'Restore' }));
    expect(await screen.findByRole('heading', { name: /Restoring Work/ })).toBeInTheDocument();
    for (let i = 0; i < 4; i++) act(() => transport.tick());
    expect(await screen.findByRole('heading', { name: /restore finished/ })).toBeInTheDocument();
    expect(screen.getAllByText('Placed')).toHaveLength(2);
  });

  it('removing an entry says the window stays open', async () => {
    await openWindows();
    await userEvent.click(
      screen.getByRole('button', { name: 'Remove payments-api from the list' }),
    );
    expect(screen.getByText(/Its window stays open/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Remove' }));
    await waitFor(() => expect(saved().queryByText('payments-api')).toBeNull());
  });

  it('reorders with the arrow buttons', async () => {
    await openWindows();
    await userEvent.click(saved().getByRole('button', { name: 'Move web-dashboard up' }));
    await waitFor(() => {
      const titles = screen.getAllByRole('checkbox', { name: /^Select / });
      expect(titles[0]).toHaveAccessibleName('Select web-dashboard');
    });
  });

  it('adds and switches profiles', async () => {
    await openWindows();
    await userEvent.click(screen.getByRole('button', { name: 'New profile' }));
    const dialog = await screen.findByRole('dialog', { name: 'New profile' });
    await userEvent.type(within(dialog).getByRole('textbox'), 'Home');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    const select = await screen.findByRole('combobox', { name: 'Profile' });
    await userEvent.selectOptions(select, within(select).getByRole('option', { name: 'Home' }));
    expect(await screen.findByText(/No windows yet/)).toBeInTheDocument();
  });

  it('clamps the wait between windows to 0-120 and commits it with Enter', async () => {
    await openWindows();
    const delay = await screen.findByRole('spinbutton', { name: 'Seconds between windows' });
    await userEvent.clear(delay);
    await userEvent.type(delay, '999{Enter}');
    await waitFor(() => expect(delay).toHaveValue(120));
  });

  it('keeps shortcut and sign-in off this view on other computers', async () => {
    await openWindows();
    expect(screen.queryByRole('heading', { name: 'On this computer' })).toBeNull();
  });

  it('offers shortcut and sign-in on the computer itself, off by default', async () => {
    renderApp({ restoreLocal: mockRestoreLocal() });
    await openMenu(/Windows/);
    expect(await screen.findByRole('heading', { name: 'On this computer' })).toBeInTheDocument();
    const signIn = await screen.findByRole('checkbox', { name: 'Restore at sign-in' });
    await waitFor(() => expect(signIn).toBeEnabled());
    expect(signIn).not.toBeChecked();
    expect(
      screen.getByRole('checkbox', { name: 'Send a message after restore' }),
    ).not.toBeChecked();
    await userEvent.click(signIn);
    await waitFor(() => expect(signIn).toBeChecked());
  });

  it('explains an unsupported computer', async () => {
    const transport = new MockTransport({ profile: 'build_server', latencyMs: 0, autoTick: false });
    render(
      <App
        machines={[{ id: 'm2', name: 'build-server', connectivity: 'LAN', transport }]}
        speech={speech}
        settings={{ ...defaultSettings(), language: 'en' }}
      />,
    );
    await userEvent.click(await screen.findByRole('button', { name: 'Windows' }));
    expect(await screen.findByText(/available on Windows only/)).toBeInTheDocument();
  });

  it('is view-only without admin permission', async () => {
    const transport = new MockTransport({ latencyMs: 0, autoTick: false });
    const scope: DeviceScope = 'view';
    const devices = (
      transport as unknown as { devices: { current: boolean; scope: DeviceScope }[] }
    ).devices;
    (transport as unknown as { devices: unknown[] }).devices = devices.map((d) => ({
      ...d,
      scope: d.current ? scope : d.scope,
    }));
    render(
      <App
        machines={[{ id: 'm1', name: 'workstation', connectivity: 'LAN', transport }]}
        speech={speech}
        settings={{ ...defaultSettings(), language: 'en' }}
      />,
    );
    await openMenu(/Windows/);
    expect(await screen.findByText(/Editing and restoring need admin/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Restore all' })).toBeDisabled();
    expect(screen.queryByRole('button', { name: 'New profile' })).toBeNull();
  });
});

describe('resources', () => {
  it('says that sessions started outside VCA are for monitoring only', async () => {
    const transport = renderApp();
    await openMenu('Resources');
    act(() => {
      transport.tick();
      transport.tick();
    });
    expect(
      await screen.findByText('Sessions started outside VCA are shown for monitoring only.'),
    ).toBeInTheDocument();
  });
});
