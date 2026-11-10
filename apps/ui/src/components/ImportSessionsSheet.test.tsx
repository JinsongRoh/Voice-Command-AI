import { act, fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import type { ClientMsg, FoundAgent, FoundStatus, ServerMsg } from '../protocol/types';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { renderWithApp } from '../test/render';
import { PULL_THRESHOLD } from './PullToRefresh';
import { SessionDeck } from './SessionDeck';
import { Toasts } from './Toasts';

function row(pid: number, status: FoundStatus, patch: Partial<FoundAgent> = {}): FoundAgent {
  return {
    pid,
    agent: { type: 'claude_code' },
    project_dir: `/work/p${pid}`,
    terminal: 'Windows Terminal',
    title: `Session ${pid}`,
    status,
    session_id: status === 'imported' || status === 'no_transcript_yet' ? `s${pid}` : null,
    ...patch,
  };
}

/** A server that answers the discovery requests (or an older one without them). */
function scanServer(withScan = true) {
  let rows: FoundAgent[] = [
    row(1, 'imported'),
    row(2, 'no_transcript_yet', { agent: { type: 'codex' } }),
    row(3, 'hidden_by_user', { agent: { type: 'gemini_cli' } }),
    row(4, 'not_in_a_terminal', {
      agent: { type: 'codex' },
      terminal: 'ChatGPT',
      title: null,
      project_dir: null,
    }),
    row(5, 'unsupported_agent', { agent: { type: 'antigravity' }, terminal: null }),
  ];
  const report = (): ServerMsg => ({ type: 'scan_report', found: rows, scanned_at: 1 });
  const respond = (msg: ClientMsg): ServerMsg => {
    switch (msg.type) {
      case 'hello':
        return {
          type: 'welcome',
          protocol_version: 1,
          server_version: 'test',
          server_name: 'Desk PC',
          features: withScan ? ['scan_sessions'] : [],
        };
      case 'scan_sessions':
        return report();
      case 'hide_observed':
        rows = rows.map((r) =>
          r.pid === msg.pid ? { ...r, status: 'hidden_by_user', session_id: null } : r,
        );
        return report();
      case 'unhide_observed':
        rows = rows.map((r) =>
          r.status === 'hidden_by_user' && (msg.all || r.pid === msg.pid)
            ? { ...r, status: 'imported', session_id: `s${r.pid}` }
            : r,
        );
        return report();
      default:
        return defaultResponse(msg);
    }
  };
  return new FakeTransport(respond);
}

async function renderDeck(variant: 'list' | 'carousel', withScan = true) {
  const transport = scanServer(withScan);
  renderWithApp(
    <>
      <SessionDeck onOpen={() => {}} onNewSession={() => {}} variant={variant} />
      <Toasts onOpen={() => {}} />
    </>,
    { transport },
  );
  await act(async () => {});
  await waitFor(() => expect(transport.bodies('list_sessions').length).toBeGreaterThan(0));
  return transport;
}

describe('Bring in AI sessions', () => {
  it('opens a sheet with every status from the Sessions header', async () => {
    const transport = await renderDeck('list');
    await userEvent.click(await screen.findByTestId('scan-open'));
    const dialog = await screen.findByRole('dialog', { name: 'Bring in AI sessions' });
    await within(dialog).findByTestId('found-1');
    expect(transport.bodies('scan_sessions')).toHaveLength(1);
    expect(
      within(dialog).getByText('AI sessions running in terminal windows on Desk PC.'),
    ).toBeTruthy();
    const badge = (pid: number) =>
      within(dialog).getByTestId(`found-${pid}`).querySelector('span[data-status]')?.textContent;
    expect(badge(1)).toBe('Listed');
    expect(badge(2)).toBe('No conversation yet');
    expect(badge(3)).toBe('Hidden');
    expect(badge(4)).toBe('No terminal window');
    expect(badge(5)).toBe('Not supported');
    // Rows that cannot be listed say why and have no switch.
    const chatgpt = within(dialog).getByTestId('found-4');
    expect(within(chatgpt).getByText(/inside an app without a terminal window/)).toBeTruthy();
    expect(within(chatgpt).queryByRole('switch')).toBeNull();
    expect(within(within(dialog).getByTestId('found-5')).queryByRole('switch')).toBeNull();
    // The notes on plain terminals and on Codex in the ChatGPT app.
    expect(within(dialog).getByText(/Plain terminal windows without an AI agent/)).toBeTruthy();
    expect(within(dialog).getByText(/Codex inside the ChatGPT app/)).toBeTruthy();
  });

  it('switches hide and show through the API', async () => {
    const transport = await renderDeck('list');
    await userEvent.click(await screen.findByTestId('scan-open'));
    const dialog = await screen.findByRole('dialog');
    const listed = await within(dialog).findByRole('switch', { name: /Session 1/ });
    expect((listed as HTMLInputElement).checked).toBe(true);
    await userEvent.click(listed);
    await waitFor(() => expect(transport.bodies('hide_observed')).toHaveLength(1));
    expect(transport.bodies('hide_observed')[0]).toMatchObject({ pid: 1 });
    await waitFor(() =>
      expect(within(dialog).getByTestId('found-1').dataset.status).toBe('hidden_by_user'),
    );

    const hidden = within(dialog).getByRole('switch', { name: /Session 3/ });
    expect((hidden as HTMLInputElement).checked).toBe(false);
    await userEvent.click(hidden);
    await waitFor(() => expect(transport.bodies('unhide_observed')).toHaveLength(1));
    expect(transport.bodies('unhide_observed')[0]).toMatchObject({ pid: 3, all: false });
  });

  it('brings in all hidden sessions', async () => {
    const transport = await renderDeck('list');
    await userEvent.click(await screen.findByTestId('scan-open'));
    const dialog = await screen.findByRole('dialog');
    await within(dialog).findByTestId('found-3');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Bring in all' }));
    await waitFor(() => expect(transport.bodies('unhide_observed')).toHaveLength(1));
    expect(transport.bodies('unhide_observed')[0]).toMatchObject({ all: true });
    await waitFor(() =>
      expect(within(dialog).getByTestId('found-3').dataset.status).toBe('imported'),
    );
    // "Look again" scans once more.
    await userEvent.click(within(dialog).getByTestId('scan-again'));
    await waitFor(() => expect(transport.bodies('scan_sessions')).toHaveLength(2));
  });

  it('has the button on phones too, and pull to refresh scans', async () => {
    const transport = await renderDeck('carousel');
    expect(await screen.findByTestId('scan-open')).toBeTruthy();
    const track = screen.getAllByRole('list')[0]!;
    fireEvent.touchStart(track, { touches: [{ clientX: 100, clientY: 10 }] });
    fireEvent.touchMove(track, { touches: [{ clientX: 104, clientY: 10 + PULL_THRESHOLD + 20 }] });
    expect(screen.getByTestId('pull-indicator').textContent).toMatch(/Release/);
    fireEvent.touchEnd(track);
    await waitFor(() => expect(transport.bodies('scan_sessions')).toHaveLength(1));
    expect(await screen.findByText('Sessions checked')).toBeTruthy();
    // A sideways swipe is not a pull.
    fireEvent.touchStart(track, { touches: [{ clientX: 200, clientY: 10 }] });
    fireEvent.touchMove(track, { touches: [{ clientX: 40, clientY: 90 }] });
    fireEvent.touchEnd(track);
    await act(async () => {});
    expect(transport.bodies('scan_sessions')).toHaveLength(1);
  });

  it('hides the button on servers without the scan', async () => {
    const transport = await renderDeck('list', false);
    await waitFor(() => expect(transport.bodies('hello')).toHaveLength(1));
    await act(async () => {});
    expect(screen.queryByTestId('scan-open')).toBeNull();
    expect(transport.bodies('scan_sessions')).toHaveLength(0);
  });
});
