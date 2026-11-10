import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { App } from '../App';
import { I18nProvider } from '../i18n/context';
import { MockTransport } from '../protocol/mockTransport';
import type {
  ArrangeOverview,
  ArrangeWindow,
  ClientMsg,
  Envelope,
  ServerMsg,
} from '../protocol/types';
import { resetInputGuard } from '../state/inputGuard';
import { defaultSettings } from '../state/settings';
import { AppProvider } from '../state/store';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { ArrangePanel } from './ArrangePanel';

const speech = {
  transcribe: async () => ({ text: '', corrections: [] }),
  createRecorder: () => ({
    start: async () => {},
    stop: async () => new Blob(),
    cancel: () => {},
  }),
  fallbackWithoutMic: false,
};

function renderApp(transport = new MockTransport({ latencyMs: 0, autoTick: false })) {
  render(
    <App
      machines={[
        {
          id: 'm1',
          name: 'workstation',
          connectivity: 'LAN',
          transport,
          simulate: { drop: () => {} },
        },
      ]}
      speech={speech}
      settings={{ ...defaultSettings(), language: 'en', approvalMode: 'ask' }}
    />,
  );
  return transport;
}

const nav = () => within(screen.getByRole('navigation', { name: 'Main navigation' }));

async function openWindows() {
  await userEvent.click(await nav().findByRole('button', { name: /Windows/ }));
  await screen.findByRole('tablist', { name: 'Window management' });
}

async function openArrange() {
  await openWindows();
  await userEvent.click(screen.getByRole('tab', { name: 'Screen layout' }));
  await screen.findByRole('heading', { level: 3, name: 'Monitors' });
  await screen.findByRole('button', { name: 'Select web-dashboard' });
}

/** Spies on what reaches the server, answering `reply` for some messages. */
function spy(transport: MockTransport, reply?: (msg: ClientMsg) => ServerMsg | undefined) {
  const sent: ClientMsg[] = [];
  const send = transport.send.bind(transport);
  vi.spyOn(transport, 'send').mockImplementation(async (env: Envelope<ClientMsg>) => {
    if (env.body.type.startsWith('arrange_')) sent.push(env.body);
    return reply?.(env.body) ?? send(env);
  });
  return sent;
}

const cell = (row: number, col: number, rest = 'empty') =>
  screen.getByRole('button', { name: `Row ${row}, column ${col}: ${rest}` });

async function pickAndPlace(title: string, row: number, col: number) {
  await userEvent.click(screen.getByRole('button', { name: `Select ${title}` }));
  await userEvent.click(cell(row, col));
}

afterEach(() => {
  try {
    localStorage.removeItem('vca.windowsTab');
  } catch {
    // No storage in this environment.
  }
  resetInputGuard();
  vi.restoreAllMocks();
});

describe('the screen layout tab', () => {
  it('sits next to the other two tabs and is reached with the arrow keys', async () => {
    renderApp();
    await openWindows();
    const restore = screen.getByRole('tab', { name: 'Save & restore windows' });
    restore.focus();
    await userEvent.keyboard('{ArrowLeft}');
    const arrange = screen.getByRole('tab', { name: 'Screen layout' });
    expect(arrange).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('tabpanel')).toHaveAccessibleName('Screen layout');
    expect(screen.getByText(/does not open or close any window/)).toBeInTheDocument();
  });

  it('is not offered by a server without the feature', async () => {
    const transport = new MockTransport({ latencyMs: 0, autoTick: false });
    const send = transport.send.bind(transport);
    vi.spyOn(transport, 'send').mockImplementation(async (env: Envelope<ClientMsg>) => {
      const reply = await send(env);
      return reply.type === 'welcome'
        ? { ...reply, features: reply.features?.filter((f) => f !== 'window_arrange') ?? [] }
        : reply;
    });
    renderApp(transport);
    await openWindows();
    expect(screen.queryByRole('tab', { name: 'Screen layout' })).toBeNull();
  });

  it('draws the monitors to scale and lays each out separately', async () => {
    renderApp();
    await openArrange();
    const first = screen.getByRole('button', {
      name: /DISPLAY1, 1920×1080, 0 of 4 cells filled/,
    });
    const left = screen.getByRole('button', { name: /DISPLAY3/ });
    // DISPLAY3 is left of DISPLAY1, DISPLAY4 above: 5760 × 2160 around them.
    expect(first.style.left).toBe(`${(1920 / 5760) * 100}%`);
    expect(first.style.top).toBe('50%');
    expect(left.style.left).toBe('0%');
    expect(first).toHaveAttribute('aria-pressed', 'true');

    await pickAndPlace('web-dashboard', 1, 2);
    await userEvent.click(left);
    expect(left).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByRole('heading', { name: /DISPLAY3/ })).toBeInTheDocument();
    expect(cell(1, 2)).toBeInTheDocument();
    expect(
      screen.getByRole('button', { name: /DISPLAY1, 1920×1080, 1 of 4 cells filled/ }),
    ).toBeInTheDocument();
  });

  it('offers the presets and a bounded custom grid', async () => {
    renderApp();
    await openArrange();
    const presets = within(screen.getByRole('group', { name: 'Number of cells' }));
    expect(presets.getAllByRole('button').map((b) => b.textContent)).toEqual([
      '1',
      '2',
      '4',
      '6',
      '8',
      '10',
    ]);
    await userEvent.click(presets.getByRole('button', { name: '2 by 5 grid' }));
    const cells = within(screen.getByRole('group', { name: 'Grid cells' }));
    expect(cells.getAllByRole('button')).toHaveLength(10);
    expect(screen.queryByText(/small, so windows may overlap/)).toBeNull();

    const rows = screen.getByRole('spinbutton', { name: 'Rows' });
    const cols = screen.getByRole('spinbutton', { name: 'Columns' });
    await userEvent.clear(rows);
    await userEvent.type(rows, '99');
    await userEvent.clear(cols);
    await userEvent.type(cols, '3');
    await userEvent.click(screen.getByRole('button', { name: 'Set grid' }));
    expect(cells.getAllByRole('button')).toHaveLength(18);
    expect(screen.getByRole('spinbutton', { name: 'Rows' })).toHaveValue(6);
    expect(screen.getByText(/small, so windows may overlap/)).toBeInTheDocument();
  });

  it('places by selecting a window and a cell, from the keyboard too', async () => {
    const transport = renderApp();
    const sent = spy(transport);
    await openArrange();
    expect(screen.getByRole('button', { name: 'Arrange windows' })).toBeDisabled();

    screen.getByRole('button', { name: 'Select web-dashboard' }).focus();
    await userEvent.keyboard('{Enter}');
    expect(screen.getByText('web-dashboard selected. Now choose a cell.')).toBeInTheDocument();
    cell(1, 2).focus();
    await userEvent.keyboard('{Enter}');
    expect(cell(1, 2, 'web-dashboard')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Select web-dashboard' })).toBeNull();

    // Picking it up again and moving it; then taking it out.
    await userEvent.click(cell(1, 2, 'web-dashboard'));
    await userEvent.click(cell(2, 1));
    expect(cell(2, 1, 'web-dashboard')).toBeInTheDocument();
    await userEvent.click(
      screen.getByRole('button', { name: 'Take web-dashboard out of the cell' }),
    );
    expect(screen.getByRole('button', { name: 'Select web-dashboard' })).toBeInTheDocument();
    expect(sent.filter((m) => m.type !== 'arrange_list')).toEqual([]);
  });

  it('places by drag and drop', async () => {
    renderApp();
    await openArrange();
    const dataTransfer = { setData: vi.fn(), effectAllowed: '' };
    fireEvent.dragStart(screen.getByRole('button', { name: 'Select docs site' }), { dataTransfer });
    const target = cell(2, 2).parentElement!;
    fireEvent.dragOver(target, { dataTransfer });
    fireEvent.drop(target, { dataTransfer });
    expect(cell(2, 2, 'docs site')).toBeInTheDocument();

    // A cell drags too: two windows swap.
    await pickAndPlace('web-dashboard', 1, 1);
    fireEvent.dragStart(cell(1, 1, 'web-dashboard'), { dataTransfer });
    fireEvent.drop(cell(2, 2, 'docs site').parentElement!, { dataTransfer });
    expect(cell(2, 2, 'web-dashboard')).toBeInTheDocument();
    expect(cell(1, 1, 'docs site')).toBeInTheDocument();
  });

  it('ignores a drop that did not start in the tab', async () => {
    renderApp();
    await openArrange();
    fireEvent.drop(cell(1, 1).parentElement!, { dataTransfer: { getData: () => '999' } });
    expect(cell(1, 1)).toBeInTheDocument();
  });

  it('fills the cells in session order, and by group when asked', async () => {
    renderApp();
    await openArrange();
    await userEvent.click(screen.getByRole('button', { name: 'Auto fill' }));
    const grid = within(screen.getByRole('group', { name: 'Grid cells' }));
    // The mock's session order; the plain terminal and the plain shell are
    // not offered, so a cell stays empty.
    expect(grid.getAllByRole('button', { name: /^Row/ }).map((b) => b.ariaLabel)).toEqual([
      'Row 1, column 1: payments-api',
      'Row 1, column 2: web-dashboard',
      'Row 2, column 1: docs site',
      'Row 2, column 2: empty',
    ]);
    await userEvent.selectOptions(screen.getByRole('combobox', { name: 'Fill order' }), 'By agent');
    await userEvent.click(screen.getByRole('button', { name: 'Auto fill' }));
    expect(screen.getByRole('button', { name: 'Clear cells' })).toBeEnabled();
    await userEvent.click(screen.getByRole('button', { name: 'Clear cells' }));
    expect(cell(1, 1)).toBeInTheDocument();
  });

  it('leaves plain terminals and plain shells out of the list', async () => {
    renderApp();
    await openArrange();
    expect(screen.getByRole('heading', { name: /Open windows \(3\)/ })).toBeInTheDocument();
    expect(screen.queryByText('Windows PowerShell')).toBeNull();
    expect(screen.queryByText('remote pwsh')).toBeNull();
  });

  it('loads a saved layout into the grid without moving anything', async () => {
    const transport = renderApp();
    const sent = spy(transport);
    await openArrange();
    await pickAndPlace('web-dashboard', 1, 1);
    await pickAndPlace('docs site', 2, 2);
    await userEvent.click(screen.getByRole('button', { name: 'Save layout' }));
    const save = await screen.findByRole('dialog', { name: 'Save layout' });
    await userEvent.type(within(save).getByRole('textbox', { name: 'Layout name' }), 'Morning');
    await userEvent.click(within(save).getByRole('button', { name: 'Save' }));
    const name = await screen.findByRole('button', { name: 'Morning' });
    expect(name).toHaveAttribute('aria-pressed', 'false');

    // Saving is an edit: the only layout does not load over the draft.
    await userEvent.click(screen.getByRole('button', { name: 'Clear cells' }));
    await userEvent.click(screen.getByRole('button', { name: '2 by 3 grid' }));
    expect(cell(1, 1)).toBeInTheDocument();

    await userEvent.click(screen.getByRole('button', { name: 'Load Morning into the grid' }));
    expect(cell(1, 1, 'web-dashboard')).toBeInTheDocument();
    expect(cell(2, 2, 'docs site')).toBeInTheDocument();
    expect(
      within(screen.getByRole('group', { name: 'Grid cells' })).getAllByRole('button', {
        name: /^Row/,
      }),
    ).toHaveLength(4);
    expect(screen.getByText(/‘Morning’ loaded\. Nothing moves/)).toBeInTheDocument();
    expect(name).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByRole('button', { name: 'Arrange windows' })).toBeEnabled();
    expect(sent.filter((m) => m.type !== 'arrange_list' && m.type !== 'arrange_edit')).toEqual([]);

    // The name loads it too.
    await userEvent.click(screen.getByRole('button', { name: 'Clear cells' }));
    await userEvent.click(name);
    expect(cell(1, 1, 'web-dashboard')).toBeInTheDocument();
  });

  it('shows the remaining layout when the others are deleted, even after changes', async () => {
    renderApp();
    await openArrange();
    const saveAs = async (title: string) => {
      await userEvent.click(screen.getByRole('button', { name: 'Save layout' }));
      const save = await screen.findByRole('dialog', { name: 'Save layout' });
      await userEvent.type(within(save).getByRole('textbox', { name: 'Layout name' }), title);
      await userEvent.click(within(save).getByRole('button', { name: 'Save' }));
      await screen.findByRole('button', { name: title });
    };
    await pickAndPlace('web-dashboard', 1, 1);
    await saveAs('Old');
    await userEvent.click(screen.getByRole('button', { name: 'Clear cells' }));
    await pickAndPlace('docs site', 1, 2);
    await saveAs('New');
    await userEvent.click(screen.getByRole('button', { name: 'Clear cells' }));
    expect(cell(1, 2)).toBeInTheDocument();

    await userEvent.click(screen.getByRole('button', { name: 'Delete Old' }));
    const confirm = await screen.findByRole('dialog', { name: 'Delete layout' });
    await userEvent.click(within(confirm).getByRole('button', { name: 'Delete' }));
    expect(await screen.findByText(/‘New’ loaded/)).toBeInTheDocument();
    expect(cell(1, 2, 'docs site')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'New' })).toHaveAttribute('aria-pressed', 'true');
  });

  it('shows a window with several tabs as one item with a hint', async () => {
    renderApp();
    await openArrange();
    const item = screen.getByRole('button', { name: 'Select payments-api' });
    expect(within(item).getByText('2 sessions')).toBeInTheDocument();
    expect(screen.getByText(/They cannot be split/)).toBeInTheDocument();
    await userEvent.click(item);
    await userEvent.click(cell(1, 1));
    expect(within(cell(1, 1, 'payments-api')).getByText('2 sessions')).toBeInTheDocument();
  });

  it('sends the plan and reports moved and skipped windows with reasons', async () => {
    const transport = renderApp();
    const sent = spy(transport);
    await openArrange();
    await pickAndPlace('web-dashboard', 1, 1);
    await pickAndPlace('docs site', 2, 2);
    // A window closes before the button is pressed.
    transport.arrange.closeWindow('103');
    await userEvent.click(screen.getByRole('button', { name: 'Arrange windows' }));
    expect(await screen.findByText('Moved 1, skipped 1.')).toBeInTheDocument();
    expect(screen.getByText('Skipped: not open')).toBeInTheDocument();
    expect(sent.find((m) => m.type === 'arrange_apply')).toEqual({
      type: 'arrange_apply',
      monitors: [
        {
          monitor: '\\\\.\\DISPLAY1',
          rows: 2,
          cols: 2,
          cells: [
            { cell: 0, window: '102' },
            { cell: 3, window: '103' },
          ],
        },
      ],
    });
    // Rectangles are the server's business.
    expect(JSON.stringify(sent)).not.toMatch(/"(x|y|w|h)":/);
  });

  it('undoes the last arrangement with one action', async () => {
    const transport = renderApp();
    const sent = spy(transport);
    await openArrange();
    const undo = screen.getByRole('button', { name: 'Undo' });
    expect(undo).toBeDisabled();
    await pickAndPlace('web-dashboard', 1, 1);
    await userEvent.click(screen.getByRole('button', { name: 'Arrange windows' }));
    await screen.findByText('Moved 1, skipped 0.');
    await waitFor(() => expect(undo).toBeEnabled());
    await userEvent.click(undo);
    await waitFor(() => expect(undo).toBeDisabled());
    expect(sent.some((m) => m.type === 'arrange_undo')).toBe(true);
  });

  it('shows the error of a refused arrangement', async () => {
    const transport = renderApp();
    spy(transport, (msg) =>
      msg.type === 'arrange_apply'
        ? { type: 'error', code: 'conflict', message: 'a restore is running' }
        : undefined,
    );
    await openArrange();
    await pickAndPlace('web-dashboard', 1, 1);
    await userEvent.click(screen.getByRole('button', { name: 'Arrange windows' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('a restore is running');
    expect(screen.getByRole('button', { name: 'Arrange windows' })).toBeEnabled();
  });

  it('asks a paired device for a code and sends the arrangement again with it', async () => {
    const transport = renderApp();
    // Like vcad for a paired device: no code, no move; a wrong code is refused.
    const sent = spy(transport, (msg) => {
      if (msg.type !== 'arrange_apply' && msg.type !== 'arrange_undo') return undefined;
      if (!msg.totp) return { type: 'error', code: 'totp_required', message: 'enter a code' };
      if (msg.totp !== '123456') return { type: 'error', code: 'totp_invalid', message: 'wrong' };
      return undefined;
    });
    await openArrange();
    await pickAndPlace('web-dashboard', 1, 1);
    await userEvent.click(screen.getByRole('button', { name: 'Arrange windows' }));
    const prompt = await screen.findByRole('dialog', { name: 'Confirm moving windows' });
    expect(prompt).toHaveTextContent('Moves the terminal windows on the computer.');
    // Nothing moved yet.
    expect(screen.queryByText(/Moved \d/)).toBeNull();
    await userEvent.type(within(prompt).getByRole('textbox'), '000000');
    await userEvent.click(within(prompt).getByRole('button', { name: 'Move windows' }));
    expect(await within(prompt).findByRole('alert')).toBeInTheDocument();
    await userEvent.type(within(prompt).getByRole('textbox'), '123456');
    await userEvent.click(within(prompt).getByRole('button', { name: 'Move windows' }));
    expect(await screen.findByText('Moved 1, skipped 0.')).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.queryByRole('dialog', { name: 'Confirm moving windows' })).toBeNull(),
    );
    const applies = sent.filter((m) => m.type === 'arrange_apply');
    expect(applies.map((m) => (m.type === 'arrange_apply' ? m.totp : null))).toEqual([
      undefined,
      '000000',
      '123456',
    ]);
    expect(applies[2]).toMatchObject({ monitors: (applies[0] as { monitors: unknown }).monitors });

    // Undo asks too; cancelling the prompt moves nothing and shows no error.
    const undo = screen.getByRole('button', { name: 'Undo' });
    await waitFor(() => expect(undo).toBeEnabled());
    await userEvent.click(undo);
    const again = await screen.findByRole('dialog', { name: 'Confirm moving windows' });
    await userEvent.click(within(again).getByRole('button', { name: 'Cancel' }));
    await waitFor(() =>
      expect(screen.queryByRole('dialog', { name: 'Confirm moving windows' })).toBeNull(),
    );
    expect(sent.filter((m) => m.type === 'arrange_undo')).toEqual([{ type: 'arrange_undo' }]);
    expect(screen.queryByRole('alert')).toBeNull();
    expect(undo).toBeEnabled();
  });

  it('sends no code and asks for none when the server needs none (this computer)', async () => {
    const transport = renderApp();
    const sent = spy(transport);
    await openArrange();
    await pickAndPlace('web-dashboard', 1, 1);
    await userEvent.click(screen.getByRole('button', { name: 'Arrange windows' }));
    await screen.findByText('Moved 1, skipped 0.');
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(sent.find((m) => m.type === 'arrange_apply')).not.toHaveProperty('totp');
  });

  it('saves, applies, renames and deletes named layouts', async () => {
    const transport = renderApp();
    const sent = spy(transport);
    await openArrange();
    expect(screen.getByText('No saved layouts yet.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save layout' })).toBeDisabled();

    await pickAndPlace('web-dashboard', 1, 1);
    await pickAndPlace('docs site', 1, 2);
    await userEvent.click(screen.getByRole('button', { name: 'Save layout' }));
    const save = await screen.findByRole('dialog', { name: 'Save layout' });
    await userEvent.type(within(save).getByRole('textbox', { name: 'Layout name' }), 'Morning');
    await userEvent.click(within(save).getByRole('button', { name: 'Save' }));
    expect(await screen.findByText('Morning')).toBeInTheDocument();
    expect(screen.getByText('2 windows')).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Saved layouts (1)' })).toBeInTheDocument();

    // A closed window is skipped, not an error.
    transport.arrange.closeWindow('103');
    await userEvent.click(screen.getByRole('button', { name: 'Apply Morning' }));
    expect(await screen.findByText('Moved 1, skipped 1.')).toBeInTheDocument();
    expect(screen.getByText('docs site')).toBeInTheDocument();
    const applied = sent.find((m) => m.type === 'arrange_apply_layout');
    expect(applied).toEqual({ type: 'arrange_apply_layout', layout: 'layout-1' });

    await userEvent.click(screen.getByRole('button', { name: 'Rename Morning' }));
    const rename = await screen.findByRole('dialog', { name: 'Rename layout' });
    const name = within(rename).getByRole('textbox', { name: 'Layout name' });
    await userEvent.clear(name);
    await userEvent.type(name, 'Evening');
    await userEvent.click(within(rename).getByRole('button', { name: 'Save' }));
    expect(await screen.findByText('Evening')).toBeInTheDocument();

    await userEvent.click(screen.getByRole('button', { name: 'Delete Evening' }));
    const confirm = await screen.findByRole('dialog', { name: 'Delete layout' });
    expect(within(confirm).getByText(/No window moves or closes/)).toBeInTheDocument();
    await userEvent.click(within(confirm).getByRole('button', { name: 'Delete' }));
    expect(await screen.findByText('No saved layouts yet.')).toBeInTheDocument();
  });

  it('shows why a name was refused', async () => {
    const transport = renderApp();
    spy(transport, (msg) =>
      msg.type === 'arrange_edit'
        ? { type: 'error', code: 'conflict', message: 'another layout is already named x' }
        : undefined,
    );
    await openArrange();
    await pickAndPlace('web-dashboard', 1, 1);
    await userEvent.click(screen.getByRole('button', { name: 'Save layout' }));
    const save = await screen.findByRole('dialog', { name: 'Save layout' });
    await userEvent.type(within(save).getByRole('textbox', { name: 'Layout name' }), 'x');
    await userEvent.click(within(save).getByRole('button', { name: 'Save' }));
    expect(await within(save).findByRole('alert')).toHaveTextContent('already named');
  });

  it("shows the backend's message instead of the controls where windows cannot move", async () => {
    renderApp(new MockTransport({ latencyMs: 0, autoTick: false, profile: 'build_server' }));
    await openWindows();
    await userEvent.click(screen.getByRole('tab', { name: 'Screen layout' }));
    const notice = await screen.findByText(/available on Windows only/);
    expect(screen.getAllByText(/available on Windows only/)).toEqual([notice]);
    expect(screen.queryByRole('button', { name: 'Arrange windows' })).toBeNull();
    expect(screen.queryByRole('heading', { name: 'Monitors' })).toBeNull();
  });
});

describe('a device that only views', () => {
  const overview: ArrangeOverview = {
    supported: true,
    message: null,
    monitors: [
      {
        id: '\\\\.\\DISPLAY1',
        rect: { x: 0, y: 0, w: 1920, h: 1080 },
        work: { x: 0, y: 0, w: 1920, h: 1040 },
        dpi: 96,
      },
    ],
    windows: [
      {
        id: '7',
        title: 'api',
        monitor: null,
        maximized: true,
        minimized: false,
        sessions: [{ id: 's7', title: 'api', agent: { type: 'claude_code' }, folder: 'E:\\api' }],
      },
    ],
    layouts: [{ id: 'l1', name: 'Morning', saved_at: 0, monitors: [] }],
    undo_windows: 3,
  };

  function renderView(scope: 'view' | 'admin') {
    const transport = new FakeTransport((msg) => {
      if (msg.type === 'arrange_list') return { type: 'arrange_list', overview };
      if (msg.type === 'list_devices') {
        return {
          type: 'devices',
          devices: [
            {
              id: 'd',
              name: 'phone',
              platform: 'ios',
              paired_at: 0,
              last_seen: 0,
              current: true,
              scope,
              online: true,
              connections: 1,
              last_ip: null,
              app_version: null,
            },
          ],
        };
      }
      return defaultResponse(msg);
    });
    render(
      <AppProvider
        machines={[{ id: 'm', name: 'pc', connectivity: 'LAN', transport }]}
        settings={defaultSettings()}
        persistSettings={false}
      >
        <I18nProvider locale="en">
          <ArrangePanel />
        </I18nProvider>
      </AppProvider>,
    );
    return transport;
  }

  it('sees the monitors, windows and layouts and no control that changes anything', async () => {
    const transport = renderView('view');
    expect(await screen.findByText(/can only view/)).toBeInTheDocument();
    expect(screen.getByText('api')).toBeInTheDocument();
    expect(screen.getByText('Morning')).toBeInTheDocument();
    expect(screen.getByText('DISPLAY1')).toBeInTheDocument();
    expect(screen.queryAllByRole('button')).toEqual([]);
    expect(screen.queryByText(/loaded/)).toBeNull();
    expect(transport.bodies('arrange_apply')).toEqual([]);
  });

  it('shows the controls to an admin device', async () => {
    renderView('admin');
    expect(await screen.findByRole('button', { name: 'Arrange windows' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Undo' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Apply Morning' })).toBeInTheDocument();
  });
});

describe('loading saved layouts', () => {
  const DISPLAY1 = '\\\\.\\DISPLAY1';
  const DISPLAY2 = '\\\\.\\DISPLAY2';

  function win(id: string, title: string, agent: string | null): ArrangeWindow {
    return {
      id,
      title,
      monitor: DISPLAY1,
      maximized: false,
      minimized: false,
      sessions: agent
        ? [{ id: `s${id}`, title, agent: { type: agent, name: 'pwsh' }, folder: 'E:\\a' }]
        : [],
    } as ArrangeWindow;
  }

  const monitor = (id: string, x: number) => ({
    id,
    rect: { x, y: 0, w: 1920, h: 1080 },
    work: { x, y: 0, w: 1920, h: 1040 },
    dpi: 96,
  });

  const saved = (id: string, name: string, cells: number) => ({
    id,
    name,
    saved_at: 0,
    monitors: [
      {
        monitor: DISPLAY2,
        rows: 1,
        cols: 3,
        cells: Array.from({ length: cells }, (_, cell) => ({
          cell,
          title: `w${cell}`,
          sessions: [],
        })),
      },
    ],
  });

  function overviewWith(over: Partial<ArrangeOverview>): ArrangeOverview {
    return {
      supported: true,
      message: null,
      monitors: [monitor(DISPLAY1, 0), monitor(DISPLAY2, 1920)],
      windows: [win('7', 'api', 'claude_code'), win('8', 'web', 'codex')],
      layouts: [saved('l1', 'Morning', 3)],
      undo_windows: 0,
      layout_plans: {
        l1: [
          {
            monitor: DISPLAY2,
            rows: 1,
            cols: 3,
            cells: [
              { cell: 0, window: '7' },
              { cell: 2, window: '8' },
            ],
          },
        ],
      },
      ...over,
    };
  }

  function renderPanel(first: ArrangeOverview) {
    const state = { overview: first };
    const transport = new FakeTransport((msg) =>
      msg.type === 'arrange_list'
        ? { type: 'arrange_list', overview: state.overview }
        : defaultResponse(msg),
    );
    render(
      <AppProvider
        machines={[{ id: 'm', name: 'pc', connectivity: 'LAN', transport }]}
        settings={defaultSettings()}
        persistSettings={false}
      >
        <I18nProvider locale="en">
          <ArrangePanel />
        </I18nProvider>
      </AppProvider>,
    );
    return { transport, state };
  }

  const gridLabels = () =>
    within(screen.getByRole('group', { name: 'Grid cells' }))
      .getAllByRole('button', { name: /^Row/ })
      .map((b) => b.ariaLabel ?? '');

  afterEach(() => {
    vi.useRealTimers();
  });

  it('loads the only saved layout when the tab opens, and says what is missing', async () => {
    const { transport } = renderPanel(overviewWith({}));
    expect(await screen.findByRole('heading', { name: /DISPLAY2/ })).toBeInTheDocument();
    expect(gridLabels()).toEqual([
      'Row 1, column 1: api',
      'Row 1, column 2: empty',
      'Row 1, column 3: web',
    ]);
    expect(
      screen.getByText('‘Morning’ loaded: 2 of 3 windows found. The others are not open now.'),
    ).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Morning' })).toHaveAttribute('aria-pressed', 'true');
    // Loading moves nothing.
    expect(transport.bodies('arrange_apply')).toEqual([]);
    expect(transport.bodies('arrange_apply_layout')).toEqual([]);
  });

  it('loads nothing by itself with two layouts or none', async () => {
    renderPanel(
      overviewWith({
        layouts: [saved('l1', 'Morning', 3), saved('l2', 'Evening', 1)],
        layout_plans: {
          l1: overviewWith({}).layout_plans!.l1!,
          l2: [{ monitor: DISPLAY2, rows: 1, cols: 3, cells: [{ cell: 0, window: '7' }] }],
        },
      }),
    );
    expect(await screen.findByRole('heading', { name: /DISPLAY1/ })).toBeInTheDocument();
    expect(screen.queryByText(/loaded/)).toBeNull();
    expect(screen.getByRole('button', { name: 'Select api' })).toBeInTheDocument();

    await userEvent.click(screen.getByRole('button', { name: 'Load Evening into the grid' }));
    expect(screen.getByRole('heading', { name: /DISPLAY2/ })).toBeInTheDocument();
    expect(gridLabels()[0]).toBe('Row 1, column 1: api');
    expect(
      screen.getByText('‘Evening’ loaded. Nothing moves until you arrange the windows.'),
    ).toBeInTheDocument();
  });

  it('fills in windows that open later, until the draft is changed', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const plans = (cells: { cell: number; window: string }[]) => ({
      l1: [{ monitor: DISPLAY2, rows: 1, cols: 3, cells }],
    });
    const { state } = renderPanel(
      overviewWith({
        windows: [win('7', 'api', 'claude_code')],
        layout_plans: plans([{ cell: 0, window: '7' }]),
      }),
    );
    expect(await screen.findByRole('heading', { name: /DISPLAY2/ })).toBeInTheDocument();
    expect(gridLabels()[2]).toBe('Row 1, column 3: empty');

    // A refresh before any change fills in the window that opened since.
    state.overview = overviewWith({});
    await act(() => vi.advanceTimersByTimeAsync(15000));
    expect(gridLabels()[2]).toBe('Row 1, column 3: web');

    // A change by the person stays through the next refresh.
    fireEvent.click(screen.getByRole('button', { name: 'Take api out of the cell' }));
    state.overview = overviewWith({
      windows: [...overviewWith({}).windows, win('9', 'docs', 'gemini_cli')],
    });
    await act(() => vi.advanceTimersByTimeAsync(15000));
    expect(gridLabels()).toEqual([
      'Row 1, column 1: empty',
      'Row 1, column 2: empty',
      'Row 1, column 3: web',
    ]);
  });

  it('hides plain terminals and plain shells from the list and from auto fill', async () => {
    renderPanel(
      overviewWith({
        windows: [
          win('7', 'api', 'claude_code'),
          win('8', 'plain', null),
          win('9', 'remote', 'shell'),
          {
            ...win('10', 'mixed', 'shell'),
            sessions: [
              ...win('10', 'mixed', 'shell').sessions,
              ...win('10', 'x', 'codex').sessions,
            ],
          },
        ],
        layouts: [],
        layout_plans: {},
      }),
    );
    expect(await screen.findByRole('button', { name: 'Select api' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Select mixed' })).toBeInTheDocument();
    expect(screen.queryByText('plain')).toBeNull();
    expect(screen.queryByText('remote')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Auto fill' }));
    expect(gridLabels().filter((label) => !label.endsWith('empty'))).toHaveLength(2);
    expect(gridLabels().join()).not.toMatch(/plain|remote/);
  });

  it('offers no Load button when the server sends no grids', async () => {
    const old = overviewWith({});
    delete old.layout_plans;
    renderPanel(old);
    expect(await screen.findByRole('button', { name: 'Apply Morning' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^Load/ })).toBeNull();
    expect(screen.queryByText(/loaded/)).toBeNull();
  });
});
