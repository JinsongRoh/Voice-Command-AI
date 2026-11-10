import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { I18nProvider } from '../i18n/context';
import { MockTransport } from '../protocol/mockTransport';
import type { ServerMsg } from '../protocol/types';
import { SpeechProvider, type SpeechServices } from '../speech/context';
import { initialState, reducer } from '../state/reducer';
import { moveBy, moveNextTo, moveTo } from '../state/sessionOrder';
import { defaultSettings } from '../state/settings';
import { AppProvider } from '../state/store';
import { localSession } from '../test/fixtures';
import { SessionDeck } from './SessionDeck';
import { LONG_PRESS_MS, SessionOrderPanel } from './SessionOrderPanel';

const speech: SpeechServices = {
  transcribe: async () => ({ text: '', corrections: [] }),
  createRecorder: () => ({
    start: async () => {},
    stop: async () => new Blob(),
    cancel: () => {},
  }),
  fallbackWithoutMic: false,
};

function renderWith(
  ui: React.ReactElement,
  transport = new MockTransport({ latencyMs: 0, autoTick: false }),
) {
  render(
    <AppProvider
      machines={[{ id: 'm', name: 'workstation', connectivity: 'LAN', transport }]}
      settings={{ ...defaultSettings(), language: 'en' }}
      persistSettings={false}
    >
      <I18nProvider locale="en">
        <SpeechProvider value={speech}>{ui}</SpeechProvider>
      </I18nProvider>
    </AppProvider>,
  );
  return transport;
}

/** Session ids of the cards, top to bottom. */
const cardOrder = () =>
  screen
    .getAllByTestId(/^session-card-/)
    .map((el) => el.getAttribute('data-testid')!.replace('session-card-', ''));
const rowOrder = () =>
  screen
    .getAllByTestId(/^order-row-/)
    .map((el) => el.getAttribute('data-testid')!.replace('order-row-', ''));

/** The order a second client of the same PC gets. */
async function serverOrder(transport: MockTransport): Promise<string[]> {
  const res = await transport.send({ id: 9001, body: { type: 'get_session_order' } });
  if (res.type !== 'session_order') throw new Error(res.type);
  return res.session_ids;
}

function dataTransfer() {
  const data = new Map<string, string>();
  return {
    effectAllowed: 'all',
    dropEffect: 'none',
    setData: (type: string, value: string) => data.set(type, value),
    getData: (type: string) => data.get(type) ?? '',
    setDragImage: vi.fn(),
  };
}

afterEach(() => {
  vi.useRealTimers();
});

describe('session order helpers', () => {
  const order = ['a', 'b', 'c', 'd'];
  it('moves next to another card', () => {
    expect(moveNextTo(order, 'd', 'a', false)).toEqual(['d', 'a', 'b', 'c']);
    expect(moveNextTo(order, 'a', 'c', true)).toEqual(['b', 'c', 'a', 'd']);
    expect(moveNextTo(order, 'a', 'a', true)).toBe(order);
  });
  it('moves among the visible cards only', () => {
    // A filter shows b and d: moving d up passes b, c keeps its place.
    expect(moveBy(order, 'd', -1, ['b', 'd'])).toEqual(['a', 'd', 'b', 'c']);
    expect(moveBy(order, 'a', -1)).toBe(order);
    expect(moveTo(order, 'a', 3)).toEqual(['b', 'c', 'd', 'a']);
    expect(moveTo(order, 'c', 0)).toEqual(['c', 'a', 'b', 'd']);
  });
});

describe('session order in the state', () => {
  const sessions: ServerMsg = {
    type: 'sessions',
    sessions: [localSession({ id: 'a' }), localSession({ id: 'b' }), localSession({ id: 'c' })],
  };
  const server = (s: typeof initialState, msg: ServerMsg) => reducer(s, { type: 'server', msg });

  it('follows the order the server pushes, also for cards that arrive later', () => {
    let s = server(initialState, sessions);
    s = server(s, { type: 'session_order', session_ids: ['c', 'd', 'a', 'b'], custom: true });
    expect(s.order).toEqual(['c', 'a', 'b']);
    expect(s.customOrder).toBe(true);
    // d was placed by the server before it arrived: it goes to its place.
    s = server(s, { type: 'session_updated', session: localSession({ id: 'd' }) });
    expect(s.order).toEqual(['c', 'd', 'a', 'b']);
    // A card the server did not place goes last.
    s = server(s, { type: 'session_updated', session: localSession({ id: 'e' }) });
    expect(s.order).toEqual(['c', 'd', 'a', 'b', 'e']);
  });

  it('reorders at once on a local move', () => {
    const s = reducer(server(initialState, sessions), { type: 'reorder', order: ['b', 'a'] });
    expect(s.order).toEqual(['b', 'a', 'c']);
  });
});

describe('desktop session list', () => {
  it('reorders by dragging a card and numbers the cards in the new order', async () => {
    const transport = renderWith(<SessionDeck onOpen={() => {}} onNewSession={() => {}} />);
    await screen.findByTestId('session-handle-wk-claude-api');
    const before = cardOrder();
    const third = before[2]!;
    const first = before[0]!;

    const dt = dataTransfer();
    fireEvent.dragStart(screen.getByTestId(`session-handle-${third}`), { dataTransfer: dt });
    const target = screen.getByTestId(`session-item-${first}`);
    fireEvent.dragOver(target, { dataTransfer: dt, clientY: 1 });
    fireEvent.drop(target, { dataTransfer: dt, clientY: 1 });

    await waitFor(() => expect(cardOrder()[0]).toBe(third));
    expect(cardOrder()).toEqual([third, first, before[1], ...before.slice(3)]);
    // The number on the card (used by voice) follows the order.
    expect(within(screen.getByTestId(`session-card-${third}`)).getByText('1')).toBeInTheDocument();
    // The PC keeps it for every client.
    await waitFor(async () => expect((await serverOrder(transport))[0]).toBe(third));
  });

  it('reorders by dragging the handle with the mouse (pointer events)', async () => {
    const transport = renderWith(<SessionDeck onOpen={() => {}} onNewSession={() => {}} />);
    await screen.findByTestId('session-handle-wk-claude-api');
    const before = cardOrder();
    const third = before[2]!;
    const first = before[0]!;
    const target = screen.getByTestId(`session-item-${first}`);
    const original = document.elementFromPoint;
    document.elementFromPoint = () => target;
    try {
      const handle = screen.getByTestId(`session-handle-${third}`);
      const mouse = { pointerType: 'mouse', pointerId: 1, button: 0, clientX: 5 };
      fireEvent.pointerDown(handle, { ...mouse, clientY: 200 });
      fireEvent.pointerMove(handle, { ...mouse, clientY: 150 });
      fireEvent.pointerMove(handle, { ...mouse, clientY: 10 });
      fireEvent.pointerUp(handle, { ...mouse, clientY: 10 });
    } finally {
      document.elementFromPoint = original;
    }
    await waitFor(() => expect(cardOrder()[0]).toBe(third));
    expect(cardOrder()).toEqual([third, first, before[1], ...before.slice(3)]);
    await waitFor(async () => expect((await serverOrder(transport))[0]).toBe(third));
  });

  it('ignores a mouse click on the handle without movement', async () => {
    renderWith(<SessionDeck onOpen={() => {}} onNewSession={() => {}} />);
    await screen.findByTestId('session-handle-wk-claude-api');
    const before = cardOrder();
    const handle = screen.getByTestId(`session-handle-${before[2]!}`);
    const mouse = { pointerType: 'mouse', pointerId: 1, button: 0, clientX: 5, clientY: 50 };
    fireEvent.pointerDown(handle, mouse);
    fireEvent.pointerUp(handle, mouse);
    expect(cardOrder()).toEqual(before);
  });

  it('moves the focused card with Alt+Down', async () => {
    renderWith(<SessionDeck onOpen={() => {}} onNewSession={() => {}} />);
    await screen.findByTestId('session-handle-wk-claude-api');
    const before = cardOrder();
    const card = screen.getByTestId(`session-card-${before[0]}`);
    card.focus();
    fireEvent.keyDown(card, { key: 'ArrowDown', altKey: true });
    await waitFor(() => expect(cardOrder().slice(0, 2)).toEqual([before[1], before[0]]));
  });

  it('shows an order pushed by another client', async () => {
    const transport = renderWith(<SessionDeck onOpen={() => {}} onNewSession={() => {}} />);
    await screen.findByTestId('session-handle-wk-claude-api');
    const before = cardOrder();
    const reversed = [...before].reverse();
    // Another device of the PC changes the order.
    await act(async () => {
      await transport.send({
        id: 9002,
        body: { type: 'set_session_order', session_ids: reversed },
      });
    });
    await waitFor(() => expect(cardOrder()).toEqual(reversed));
  });
});

describe('phone order list (Windows menu)', () => {
  it('moves with the arrow buttons and resets to the default order', async () => {
    const transport = renderWith(<SessionOrderPanel />);
    await screen.findByTestId('order-row-wk-claude-api');
    const before = rowOrder();
    const reset = screen.getByTestId('order-reset');
    expect(reset).toBeDisabled();

    const firstRow = screen.getByTestId(`order-row-${before[0]}`);
    await userEvent.click(
      within(firstRow).getByRole('button', { name: /down in the session order$/ }),
    );
    await waitFor(() => expect(rowOrder().slice(0, 2)).toEqual([before[1], before[0]]));
    expect(within(screen.getByTestId(`order-row-${before[0]}`)).getByText('2')).toBeInTheDocument();
    expect(await serverOrder(transport)).toEqual(rowOrder());
    // The first row cannot go up.
    const top = screen.getByTestId(`order-row-${before[1]}`);
    expect(within(top).getByRole('button', { name: /up in the session order$/ })).toBeDisabled();

    await waitFor(() => expect(reset).toBeEnabled());
    await userEvent.click(reset);
    await waitFor(() => expect(rowOrder()).toEqual(before));
    await waitFor(() => expect(reset).toBeDisabled());
  });

  it('drags a row after a long press', async () => {
    renderWith(<SessionOrderPanel />);
    await screen.findByTestId('order-row-wk-claude-api');
    const before = rowOrder();
    // Rows are 60 px tall, in list order.
    for (const row of screen.getAllByTestId(/^order-row-/)) {
      row.getBoundingClientRect = () => {
        const i = [...row.parentElement!.children].indexOf(row);
        return {
          top: i * 60,
          bottom: i * 60 + 60,
          height: 60,
          left: 0,
          right: 300,
          width: 300,
          x: 0,
          y: i * 60,
          toJSON: () => ({}),
        };
      };
    }
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const handle = screen.getByTestId(`order-handle-${before[0]}`);
    const touch = { pointerId: 7, pointerType: 'touch', button: 0, clientX: 10 };
    fireEvent.pointerDown(handle, { ...touch, clientY: 30 });
    // Moving before the long press is a scroll: nothing moves.
    act(() => vi.advanceTimersByTime(LONG_PRESS_MS / 2));
    expect(rowOrder()).toEqual(before);
    act(() => vi.advanceTimersByTime(LONG_PRESS_MS));
    // Picked up: the row follows the finger to the third place.
    fireEvent.pointerMove(handle, { ...touch, clientY: 160 });
    expect(rowOrder().slice(0, 3)).toEqual([before[1], before[2], before[0]]);
    fireEvent.pointerUp(handle, { ...touch, clientY: 160 });
    vi.useRealTimers();
    await waitFor(() => expect(rowOrder().slice(0, 3)).toEqual([before[1], before[2], before[0]]));
    expect(within(screen.getByTestId(`order-row-${before[0]}`)).getByText('3')).toBeInTheDocument();
  });
});
