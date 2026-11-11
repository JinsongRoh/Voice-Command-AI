import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect } from 'react';
import { describe, expect, it, vi } from 'vitest';

import { App } from '../App';
import { MockTransport } from '../protocol/mockTransport';
import type { SessionInfo } from '../protocol/types';
import { defaultSettings, type Settings } from '../state/settings';
import { useActions, useAppState } from '../state/store';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { Composer } from './Composer';
import { LiveView, type PhoneChrome } from './LiveView';
import { BAR_SCROLL_START, barOnScroll, isEmptyChatSpace, swipeDirection } from './phoneChrome';

// xterm needs a real canvas; the terminal tab is not under test here.
vi.mock('./TerminalView', () => ({ default: () => <div data-testid="terminal" /> }));

/**
 * The phone layout of the session screen. The test DOM has no matchMedia,
 * so the App renders its narrow (phone) layout.
 */
function renderApp(settings: Partial<Settings> = {}) {
  const transport = new MockTransport({ latencyMs: 0, autoTick: false });
  render(
    <App
      machines={[{ id: 'm1', name: 'workstation', connectivity: 'LAN', transport }]}
      speech={{
        transcribe: async () => ({ text: '', corrections: [] }),
        createRecorder: () => ({
          start: async () => {},
          stop: async () => new Blob(),
          cancel: () => {},
        }),
        fallbackWithoutMic: false,
      }}
      settings={{ ...defaultSettings(), ...settings }}
    />,
  );
  return transport;
}

async function openSession(id = 'wk-claude-api') {
  await userEvent.click(await screen.findByTestId(`session-card-${id}`));
  return screen.findByTestId('session-topbar');
}

describe('phone session top bar', () => {
  it('holds back, the title, the view menu, reading mode, the status dot and "more"', async () => {
    renderApp();
    const bar = await openSession();
    expect(within(bar).getByRole('button', { name: 'Back to sessions' })).toBeInTheDocument();
    expect(
      within(bar).getByRole('heading', { level: 2, name: 'payments-api' }),
    ).toBeInTheDocument();
    expect(within(bar).getByRole('button', { name: 'View: Chat' })).toBeInTheDocument();
    expect(within(bar).getByRole('button', { name: 'Reading mode' })).toBeInTheDocument();
    // The connection dot (the strip's cards have their own status chips).
    expect(
      within(bar)
        .getAllByRole('status')
        .some((el) => /Connect/.test(el.textContent ?? '')),
    ).toBe(true);
    // Pending alerts: their count is on "more" (the bottom menu is hidden here).
    const more = within(bar).getByTestId('topbar-more');
    expect(more).toHaveAccessibleName(/^More actions, \d+ alerts?$/);
    expect(more.querySelector('.badge')).not.toBeNull();
    // The old header and toolbar are gone.
    expect(screen.queryByRole('banner')).toBeNull();
    expect(screen.queryByRole('group', { name: 'View mode' })).toBeNull();
  });

  it('shows the title as a plain session name, with the strip always under the bar', async () => {
    renderApp();
    const bar = await openSession();
    // No session selector in the title: no button, chevron or popup.
    const title = within(bar).getByTestId('live-title');
    expect(title).toHaveTextContent('payments-api');
    expect(title.querySelector('button, svg')).toBeNull();
    expect(title).not.toHaveAttribute('aria-haspopup');
    expect(within(bar).queryByTestId('live-title-switch')).toBeNull();
    expect(within(within(bar).getByTestId('live-title-area')).queryByRole('button')).toBeNull();
    // The strip is shown without a tap.
    const strip = within(bar).getByTestId('session-strip');
    expect(within(strip).getByRole('list', { name: 'Session cards' })).toBeInTheDocument();
    // A tap on the title opens nothing.
    await userEvent.click(title);
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(within(bar).getByTestId('session-strip')).toBeInTheDocument();
  });

  it('switches Chat and Terminal from the view menu', async () => {
    renderApp();
    const bar = await openSession();
    await userEvent.click(within(bar).getByRole('button', { name: 'View: Chat' }));
    const menu = within(bar).getByRole('group', { name: 'View mode' });
    await userEvent.click(within(menu).getByRole('button', { name: /Terminal/ }));
    expect(within(bar).getByRole('button', { name: 'View: Terminal' })).toHaveAttribute(
      'aria-expanded',
      'false',
    );
    expect(await screen.findByTestId('terminal')).toBeInTheDocument();
  });

  it('puts the header controls, session actions and alerts in "more"', async () => {
    renderApp();
    const bar = await openSession();
    await userEvent.click(within(bar).getByTestId('topbar-more'));
    const menu = within(bar).getByRole('group', { name: /^More actions/ });
    expect(within(menu).getByRole('combobox', { name: 'Machine' })).toHaveValue('m1');
    expect(within(menu).getByRole('button', { name: 'Restart' })).toBeInTheDocument();
    expect(within(menu).getByRole('button', { name: /Search/ })).toBeInTheDocument();
    await userEvent.keyboard('{Escape}');
    expect(within(bar).getByTestId('topbar-more')).toHaveFocus();
    expect(within(bar).queryByRole('group', { name: /^More actions/ })).toBeNull();
    // Alerts stay reachable without the bottom menu.
    await userEvent.click(within(bar).getByTestId('topbar-more'));
    await userEvent.click(screen.getByRole('button', { name: /^Alerts, \d+ unhandled alerts?$/ }));
    expect(
      await screen.findAllByRole('button', { name: /^Open .* at its request$/ }),
    ).not.toHaveLength(0);
    expect(screen.getByRole('navigation', { name: 'Main navigation' })).toBeInTheDocument();
  });

  it('opens the full switcher sheet from the strip, with the deck filters inside', async () => {
    renderApp();
    const bar = await openSession();
    await userEvent.click(within(bar).getByRole('button', { name: 'Search sessions' }));
    const dialog = await screen.findByRole('dialog', { name: 'Switch session' });
    expect(within(dialog).getByRole('searchbox')).toHaveFocus();
    const filters = within(dialog).getByRole('group', { name: 'Filter sessions' });
    expect(within(filters).getByRole('button', { name: 'All' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    expect(within(dialog).getByTestId('pick-wk-jules-mobile')).toBeInTheDocument();
    await userEvent.click(within(filters).getByRole('button', { name: 'Local' }));
    expect(within(filters).getByRole('button', { name: 'Local' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    // A cloud session is filtered out of the sheet.
    expect(within(dialog).queryByTestId('pick-wk-jules-mobile')).toBeNull();
    expect(within(dialog).getByTestId('pick-wk-claude-api')).toBeInTheDocument();
  });

  it('has no session strip option in Settings', async () => {
    renderApp();
    await screen.findByTestId('session-card-wk-claude-api');
    await userEvent.click(
      within(screen.getByRole('navigation', { name: 'Main navigation' })).getByRole('button', {
        name: 'Settings',
      }),
    );
    expect(await screen.findByRole('combobox', { name: 'Language' })).toBeInTheDocument();
    expect(screen.queryByRole('combobox', { name: /Session strip/ })).toBeNull();
  });

  it('hides the bottom menu in the session and shows it again in the list', async () => {
    renderApp();
    await screen.findByTestId('session-card-wk-claude-api');
    expect(screen.getByRole('navigation', { name: 'Main navigation' })).toBeInTheDocument();
    const bar = await openSession();
    expect(screen.queryByRole('navigation', { name: 'Main navigation' })).toBeNull();
    await userEvent.click(within(bar).getByRole('button', { name: 'Back to sessions' }));
    expect(await screen.findByRole('navigation', { name: 'Main navigation' })).toBeInTheDocument();
  });

  it('keeps the decision card in the session and in reading mode', async () => {
    renderApp();
    const bar = await openSession();
    expect(
      screen.getByRole('region', { name: 'Pending decision for payments-api' }),
    ).toBeInTheDocument();
    // A decision pins the bar (it never slides away while one waits).
    expect(bar).not.toHaveAttribute('data-hidden');
    await userEvent.click(within(bar).getByRole('button', { name: 'Reading mode' }));
    const sheet = screen.getByRole('region', { name: 'Pending decision for payments-api' });
    expect(within(sheet).getByRole('button', { name: 'Yes' })).toBeEnabled();
    // The full composer stays below the card (no floating button over it).
    expect(screen.getByRole('region', { name: /^Command for/ })).not.toHaveAttribute(
      'data-reading',
    );
  });
});

/** The strip's cards, in order (their session ids). */
function stripOrder(bar: HTMLElement): string[] {
  const list = within(bar).getByRole('list', { name: 'Session cards' });
  return within(list)
    .getAllByRole('listitem')
    .map((li) => li.getAttribute('data-session-id') ?? '');
}

/** A sideways drag on the title area (`dx` px; negative: toward the left). */
function swipeTitle(bar: HTMLElement, dx: number, dy = 0) {
  const area = within(bar).getByTestId('live-title-area');
  const title = within(bar).getByTestId('live-title');
  fireEvent.pointerDown(title, { pointerId: 7, clientX: 200, clientY: 20 });
  fireEvent.pointerUp(title, { pointerId: 7, clientX: 200 + dx, clientY: 20 + dy });
  fireEvent.click(title);
  return area;
}

describe('phone session strip (the one switch control)', () => {
  it('opens a session picked in the strip, and its title takes the focus', async () => {
    renderApp();
    let bar = await openSession();
    const other = stripOrder(bar).find((id) => id !== 'wk-claude-api')!;
    const card = within(bar).getByTestId(`session-card-${other}`);
    const name = card
      .querySelector('[id]')!
      .textContent!.replace(/^\d+Session \d+,/, '')
      .trim();
    await userEvent.click(card);
    bar = await screen.findByTestId('session-topbar');
    await waitFor(() => expect(within(bar).getByTestId('live-title')).toHaveTextContent(name));
    // The strip stays under the bar.
    expect(within(bar).getByTestId('session-strip')).toBeInTheDocument();
    expect(within(bar).getByTestId('live-title')).toHaveFocus();
    expect(within(bar).getByTestId(`session-card-${other}`)).toHaveAttribute(
      'aria-pressed',
      'true',
    );
  });

  it('steps to the next and previous session with a swipe on the title', async () => {
    renderApp();
    let bar = await openSession();
    const order = stripOrder(bar);
    const at = order.indexOf('wk-claude-api');
    expect(at).toBeGreaterThanOrEqual(0);
    const nextId = order[at + 1] ?? order[at - 1]!;
    const dx = order[at + 1] ? -80 : 80;
    swipeTitle(bar, dx);
    bar = await screen.findByTestId('session-topbar');
    const note = await screen.findByTestId('switch-note');
    expect(note).toHaveAttribute('role', 'status');
    expect(note.textContent).toMatch(/^Switched to /);
    // The swipe opens nothing.
    expect(screen.queryByRole('dialog')).toBeNull();
    await waitFor(() =>
      expect(within(bar).getByTestId(`session-card-${nextId}`)).toHaveAttribute(
        'aria-pressed',
        'true',
      ),
    );
    // And back the other way.
    swipeTitle(bar, -dx);
    bar = await screen.findByTestId('session-topbar');
    await waitFor(() =>
      expect(within(bar).getByTestId('session-card-wk-claude-api')).toHaveAttribute(
        'aria-pressed',
        'true',
      ),
    );
  });

  it('still steps when the open session is outside the category filter', async () => {
    renderApp();
    let bar = await openSession();
    // A filter the open (local) session is not in.
    await userEvent.click(within(bar).getByRole('button', { name: 'Search sessions' }));
    const dialog = await screen.findByRole('dialog', { name: 'Switch session' });
    await userEvent.click(within(dialog).getByRole('button', { name: 'Cloud' }));
    await userEvent.keyboard('{Escape}');
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    bar = await screen.findByTestId('session-topbar');
    swipeTitle(bar, -80);
    const note = await screen.findByTestId('switch-note');
    expect(note.textContent).toMatch(/^Switched to /);
    bar = await screen.findByTestId('session-topbar');
    expect(within(bar).getByTestId('live-title')).not.toHaveTextContent('payments-api');
  });

  it('ignores short or vertical moves on the title, and moves outside it', async () => {
    renderApp();
    const bar = await openSession();
    swipeTitle(bar, -20); // too short
    swipeTitle(bar, -60, 90); // mostly vertical
    expect(within(bar).getByTestId('live-title')).toHaveTextContent('payments-api');
    expect(screen.queryByRole('dialog')).toBeNull();
    // The same move on the chat changes nothing.
    const chat = screen.getByTestId('chat-scroller');
    fireEvent.pointerDown(chat, { pointerId: 3, clientX: 200, clientY: 300 });
    fireEvent.pointerUp(chat, { pointerId: 3, clientX: 60, clientY: 300 });
    expect(within(bar).getByTestId('live-title')).toHaveTextContent('payments-api');
    expect(screen.queryByTestId('switch-note')).toBeNull();
  });

  it('decides a swipe from distance and angle (pure helper)', () => {
    expect(swipeDirection(-60, 5)).toBe(1);
    expect(swipeDirection(60, -5)).toBe(-1);
    expect(swipeDirection(-30, 0)).toBe(0);
    expect(swipeDirection(-60, 50)).toBe(0);
  });

  it('shows one switch control and no duplicate controls at rest', async () => {
    renderApp();
    const bar = await openSession();
    // At rest: the slim bar with the strip, the chat and the one-row composer.
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(screen.queryByTestId('composer-tray')).toBeNull();
    expect(screen.queryByTestId('composer-voice-line')).toBeNull();
    expect(screen.queryByRole('group', { name: /^More actions/ })).toBeNull();
    expect(screen.queryByRole('group', { name: 'View mode' })).toBeNull();
    // One "more" menu (the app header's is merged into the bar's).
    expect(screen.getAllByRole('button', { name: /^More actions/ })).toHaveLength(1);
    expect(screen.queryByRole('banner')).toBeNull();
    // One session list, one search button for the sheet.
    expect(screen.getAllByRole('list', { name: 'Session cards' })).toHaveLength(1);
    expect(screen.getAllByRole('button', { name: 'Search sessions' })).toHaveLength(1);
    const strip = within(bar).getByTestId('session-strip');
    // No filters, rename or "new session" in the strip (sheet, "more", list).
    expect(within(strip).queryByRole('group', { name: 'Filter sessions' })).toBeNull();
    expect(within(strip).queryByRole('button', { name: 'Rename session' })).toBeNull();
    expect(within(strip).queryByRole('button', { name: 'New session' })).toBeNull();
    // Rename lives only in "more".
    await userEvent.click(within(bar).getByTestId('topbar-more'));
    const menu = within(bar).getByRole('group', { name: /^More actions/ });
    expect(within(menu).getAllByRole('button', { name: /Rename/ })).toHaveLength(1);
    // Hands-free is in the composer tray only, not in "more".
    expect(within(menu).queryByRole('button', { name: 'Hands-free mode' })).toBeNull();
    expect(within(menu).queryByRole('menuitemcheckbox', { name: /Hands-free/ })).toBeNull();
  });

  it('closes "more" after an app control acts, like the session actions', async () => {
    renderApp();
    const bar = await openSession();
    await userEvent.click(within(bar).getByTestId('topbar-more'));
    const menu = within(bar).getByRole('group', { name: /^More actions/ });
    await userEvent.click(within(menu).getByRole('button', { name: 'Voice announcements on/off' }));
    await waitFor(() =>
      expect(within(bar).queryByRole('group', { name: /^More actions/ })).toBeNull(),
    );
    expect(within(bar).getByTestId('topbar-more')).toHaveAttribute('aria-expanded', 'false');
  });
});

const idle = localSession({ id: 's1', display_name: 'demo', state: { type: 'idle' } });

function Selected({ session, chrome }: { session: SessionInfo; chrome: PhoneChrome }) {
  const actions = useActions();
  const selected = useAppState((s) => s.selectedId);
  const known = useAppState((s) => !!s.sessions[session.id]);
  useEffect(() => {
    if (known) actions.select(session.id);
  }, [actions, known, session.id]);
  return selected === session.id ? <LiveView session={session} chrome={chrome} /> : null;
}

function renderLive(chrome: Partial<PhoneChrome> = {}) {
  const transport = new FakeTransport((msg) =>
    msg.type === 'list_sessions' ? { type: 'sessions', sessions: [idle] } : defaultResponse(msg),
  );
  const onBack = vi.fn();
  renderWithApp(
    <Selected
      session={idle}
      chrome={{
        onBack,
        alerts: 0,
        onOpenAlerts: () => {},
        headerItems: <span>header items</span>,
        ...chrome,
      }}
    />,
    { transport },
  );
  return { onBack };
}

/** Scrolls the chat to `top` (jsdom has no layout: the sizes are set by hand). */
function scrollChat(el: HTMLElement, top: number, height = 2000, client = 500) {
  Object.defineProperty(el, 'scrollHeight', { configurable: true, value: height });
  Object.defineProperty(el, 'clientHeight', { configurable: true, value: client });
  el.scrollTop = top;
  fireEvent.scroll(el);
}

describe('phone top bar auto-hide', () => {
  it('slides away toward older messages and comes back toward the newest', async () => {
    renderLive();
    const bar = await screen.findByTestId('session-topbar');
    const chat = await screen.findByTestId('chat-scroller');
    scrollChat(chat, 1500); // the bottom: newest message
    expect(bar).not.toHaveAttribute('data-hidden');
    scrollChat(chat, 1400);
    scrollChat(chat, 1300);
    expect(bar).toHaveAttribute('data-hidden');
    // Back down a little: shown again.
    scrollChat(chat, 1340);
    expect(bar).not.toHaveAttribute('data-hidden');
    scrollChat(chat, 1200);
    expect(bar).toHaveAttribute('data-hidden');
    // Reaching the newest message shows it.
    scrollChat(chat, 1500);
    expect(bar).not.toHaveAttribute('data-hidden');
  });

  it('never hides while an alert needs attention', async () => {
    renderLive({ alerts: 2 });
    const bar = await screen.findByTestId('session-topbar');
    const chat = await screen.findByTestId('chat-scroller');
    scrollChat(chat, 1500);
    scrollChat(chat, 1000);
    expect(bar).not.toHaveAttribute('data-hidden');
    expect(screen.getByTestId('topbar-more')).toHaveAccessibleName('More actions, 2 alerts');
  });

  it('decides from scroll distance, ignoring jumps (pure helper)', () => {
    let s = barOnScroll(BAR_SCROLL_START, 1000, false, 500);
    s = barOnScroll(s, 990, false, 500);
    expect(s.hidden).toBe(false); // under the hide distance
    s = barOnScroll(s, 960, false, 500);
    expect(s.hidden).toBe(true);
    // Older pages loading above jump the position: not the person scrolling.
    s = barOnScroll(s, 3000, false, 500);
    expect(s.hidden).toBe(true);
    s = barOnScroll(s, 3020, false, 500);
    expect(s.hidden).toBe(false);
    expect(barOnScroll({ ...s, hidden: true }, 5000, true).hidden).toBe(false);
  });
});

describe('phone reading mode', () => {
  it('keeps the pending alert count on the exit button while the bar is hidden', async () => {
    renderLive({ alerts: 3 });
    const bar = await screen.findByTestId('session-topbar');
    await userEvent.click(within(bar).getByRole('button', { name: 'Reading mode' }));
    const exit = screen.getByTestId('reading-exit');
    expect(exit).toHaveAccessibleName('Exit reading mode, 3 unhandled alerts');
    expect(exit.querySelector('.badge')).toHaveTextContent('3');
  });

  it('hides the bar and composer chrome from the bar button, and comes back', async () => {
    renderLive();
    const bar = await screen.findByTestId('session-topbar');
    const live = bar.closest('section')!;
    await userEvent.click(within(bar).getByRole('button', { name: 'Reading mode' }));
    expect(live).toHaveAttribute('data-reading', 'true');
    const exit = screen.getByRole('button', { name: 'Exit reading mode' });
    expect(exit).toHaveFocus();
    // Only a small dictate button floats; it still dictates.
    const composer = screen.getByRole('region', { name: 'Command for demo' });
    expect(composer).toHaveAttribute('data-reading', 'true');
    expect(within(composer).getByRole('button', { name: /dictat|Tap to/i })).toBeEnabled();
    await userEvent.click(exit);
    expect(live).not.toHaveAttribute('data-reading');
    expect(within(bar).getByRole('button', { name: 'Reading mode' })).toHaveFocus();
  });

  it('switches on a double tap on empty chat space, not on text or buttons', async () => {
    renderLive();
    const bar = await screen.findByTestId('session-topbar');
    const live = bar.closest('section')!;
    const chat = await screen.findByTestId('chat-scroller');
    const log = within(chat).getByRole('log');
    const tap = (el: Element) => {
      fireEvent.pointerUp(el);
      fireEvent.pointerUp(el);
    };
    tap(bar.querySelector('button')!);
    expect(live).not.toHaveAttribute('data-reading');
    tap(log);
    expect(live).toHaveAttribute('data-reading', 'true');
    tap(log);
    expect(live).not.toHaveAttribute('data-reading');
  });

  it('tells empty space from text, links and code (pure helper)', () => {
    const box = document.createElement('div');
    box.innerHTML = '<ol><li><p>hello</p><a href="#">link</a><pre><span>x</span></pre></li></ol>';
    document.body.append(box);
    expect(isEmptyChatSpace(box.querySelector('ol'))).toBe(true);
    expect(isEmptyChatSpace(box.querySelector('li'))).toBe(true);
    expect(isEmptyChatSpace(box.querySelector('p'))).toBe(false);
    expect(isEmptyChatSpace(box.querySelector('a'))).toBe(false);
    expect(isEmptyChatSpace(box.querySelector('pre span'))).toBe(false);
    box.remove();
  });
});

describe('phone composer row and tools drawer', () => {
  it('keeps the text box on top and dictate, tray and send in the bottom row', () => {
    renderWithApp(<Composer session={localSession()} layout="phone" />);
    // The drawer is closed: no quick prompts or key bar on screen.
    expect(screen.queryByRole('group', { name: 'Control keys' })).toBeNull();
    expect(
      screen.queryByRole('button', { name: 'Run the tests and fix any failures.' }),
    ).toBeNull();
    const tray = screen.getByRole('button', { name: 'More input tools' });
    const textRow = screen.getByTestId('composer-text-row');
    const row = screen.getByTestId('composer-bottom-row');
    expect(
      within(textRow).getByRole('textbox', { name: 'Command for payments-api' }),
    ).toBeVisible();
    expect(within(row).queryByRole('textbox')).toBeNull();
    expect(within(row).getByRole('button', { name: 'More input tools' })).toBe(tray);
    expect(within(row).getByRole('button', { name: 'Send' })).toBeInTheDocument();
    expect(within(row).getByRole('button', { name: 'Tap to dictate' })).toBeInTheDocument();
    expect(tray).toHaveAttribute('aria-expanded', 'false');
  });

  it('orders the rows: text box first, then attach, tray, dictate, send', () => {
    renderWithApp(<Composer session={localSession()} layout="phone" />);
    const textRow = screen.getByTestId('composer-text-row');
    const row = screen.getByTestId('composer-bottom-row');
    // The text box row comes before the bottom row.
    expect(textRow.compareDocumentPosition(row) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    const names = within(row)
      .getAllByRole('button')
      .map((b) => b.getAttribute('aria-label') ?? b.textContent ?? '');
    const at = (re: RegExp) => names.findIndex((n) => re.test(n));
    const attach = at(/attach/i);
    const tray = at(/More input tools/);
    const mic = at(/Tap to dictate/);
    const send = at(/^Send$/);
    expect(attach).toBeGreaterThanOrEqual(0);
    expect(attach).toBeLessThan(tray);
    expect(tray).toBeLessThan(mic);
    expect(mic).toBeLessThan(send);
  });

  it('opens the drawer upward; a key keeps it open, a prompt closes it', async () => {
    const { transport } = renderWithApp(<Composer session={localSession()} layout="phone" />);
    const trayButton = screen.getByRole('button', { name: 'More input tools' });
    await userEvent.click(trayButton);
    expect(trayButton).toHaveAttribute('aria-expanded', 'true');
    const drawer = screen.getByTestId('composer-tray');
    const keys = within(drawer).getByRole('group', { name: 'Control keys' });
    expect(within(drawer).getByRole('button', { name: 'Hands-free mode' })).toBeInTheDocument();
    expect(within(drawer).getByRole('button', { name: 'Expand text box' })).toBeInTheDocument();
    // A key (Esc, Enter, arrows…): pressed again and again, the drawer stays.
    const key = within(keys).getAllByRole('button')[1]!;
    await userEvent.click(key);
    await userEvent.click(key);
    await waitFor(() =>
      expect(
        transport
          .bodies('send_input')
          .filter((b) => (b as { input?: { type?: string } }).input?.type === 'key'),
      ).toHaveLength(2),
    );
    expect(screen.getByTestId('composer-tray')).toBeInTheDocument();
    // Snippets open inside the drawer; it stays.
    await userEvent.click(within(drawer).getByRole('button', { name: 'Saved snippets' }));
    expect(screen.getByTestId('composer-tray')).toBeInTheDocument();
    // A quick prompt fills the box and closes the drawer.
    await userEvent.click(
      within(drawer).getByRole('button', { name: 'Run the tests and fix any failures.' }),
    );
    expect(screen.queryByTestId('composer-tray')).toBeNull();
    expect(trayButton).toHaveFocus();
    expect(screen.getByRole('textbox', { name: 'Command for payments-api' })).toHaveValue(
      'Run the tests and fix any failures.',
    );
  });

  it('closes the drawer with Escape and with a press outside', async () => {
    renderWithApp(<Composer session={localSession()} layout="phone" />);
    const trayButton = screen.getByRole('button', { name: 'More input tools' });
    await userEvent.click(trayButton);
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByTestId('composer-tray')).toBeNull();
    expect(trayButton).toHaveFocus();
    await userEvent.click(trayButton);
    expect(screen.getByTestId('composer-tray')).toBeInTheDocument();
    act(() => {
      document.body.dispatchEvent(new Event('pointerdown', { bubbles: true }));
    });
    expect(screen.queryByTestId('composer-tray')).toBeNull();
  });
});
