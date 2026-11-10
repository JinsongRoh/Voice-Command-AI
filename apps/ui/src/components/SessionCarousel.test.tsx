import { act, fireEvent, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { SessionInfo } from '../protocol/types';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { cloudSession, localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { SWIPE_TUTORIAL_KEY, SWIPE_TUTORIAL_MS } from './SessionCarousel';
import { SessionDeck } from './SessionDeck';

const THREE: SessionInfo[] = [
  localSession({ id: 's1', display_name: 'payments-api' }),
  localSession({ id: 's2', display_name: 'web-frontend', state: { type: 'awaiting_input' } }),
  cloudSession({ id: 'c1', display_name: 'docs-refresh', unread: 2 }),
];

function renderCarousel(sessions: SessionInfo[] = THREE, onOpen = vi.fn()) {
  const transport = new FakeTransport((msg) =>
    msg.type === 'list_sessions' ? { type: 'sessions', sessions } : defaultResponse(msg),
  );
  const result = renderWithApp(
    <SessionDeck variant="carousel" onOpen={onOpen} onNewSession={() => {}} />,
    { transport },
  );
  return { ...result, onOpen };
}

beforeEach(() => {
  localStorage.clear();
});
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  localStorage.clear();
});

describe('SessionCarousel (phone deck)', () => {
  it('shows the sub-agent count on compact cards', async () => {
    localStorage.setItem(SWIPE_TUTORIAL_KEY, '1');
    renderCarousel([
      localSession({ id: 's1', display_name: 'payments-api', subagents: 2 }),
      localSession({ id: 's2', display_name: 'web-frontend' }),
    ]);
    const list = await screen.findByRole('list', { name: 'Session cards' });
    const busy = within(list).getByRole('button', { name: /payments-api/ });
    expect(within(busy).getByRole('img', { name: 'Sub-agents working: 2' })).toHaveTextContent('2');
    const idle = within(list).getByRole('button', { name: /web-frontend/ });
    expect(within(idle).queryByTestId('subagent-badge')).not.toBeInTheDocument();
  });

  it("shows the selected machine's sessions as compact cards in a swipeable row", async () => {
    localStorage.setItem(SWIPE_TUTORIAL_KEY, '1');
    renderCarousel();
    const list = await screen.findByRole('list', { name: 'Session cards' });
    const cards = within(list).getAllByTestId(/^session-card-/);
    expect(cards.map((c) => c.dataset.testid)).toEqual([
      'session-card-s1',
      'session-card-s2',
      'session-card-c1',
    ]);
    const first = within(list).getByRole('button', { name: /payments-api/ });
    expect(first).toHaveAttribute('aria-pressed', 'false');
    expect(first).toHaveAccessibleDescription(/Running/);
    expect(within(first).getByRole('img', { name: 'Claude Code' })).toBeInTheDocument();
    expect(within(list).getByText('Waiting for input')).toBeInTheDocument();
    expect(within(list).getByRole('button', { name: /docs-refresh/ })).toHaveAccessibleDescription(
      /2 unread/,
    );
    // Filters and "New session" stay available in the row above.
    expect(screen.getByRole('group', { name: 'Filter sessions' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'New session' })).toBeInTheDocument();
  });

  it('selects a session on tap and highlights its card', async () => {
    localStorage.setItem(SWIPE_TUTORIAL_KEY, '1');
    const { onOpen } = renderCarousel();
    const second = await screen.findByTestId('session-card-s2');
    await userEvent.click(second);
    expect(second).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByTestId('session-card-s1')).toHaveAttribute('aria-pressed', 'false');
    expect(onOpen).toHaveBeenCalledWith('s2');
  });

  it('shows the position counter and updates it with the selection', async () => {
    localStorage.setItem(SWIPE_TUTORIAL_KEY, '1');
    renderCarousel();
    await screen.findByTestId('session-card-s1');
    const counter = screen.getByTestId('carousel-counter');
    expect(counter).toHaveTextContent('1 / 3');
    expect(counter).toHaveTextContent('Showing sessions 1 to 1 of 3');
    await userEvent.click(screen.getByTestId('session-card-c1'));
    expect(counter).toHaveTextContent('3 / 3');
  });

  it('shows no counter or tutorial for a single session', async () => {
    renderCarousel([localSession()]);
    await screen.findByTestId('session-card-s1');
    expect(screen.queryByTestId('carousel-counter')).not.toBeInTheDocument();
    expect(screen.queryByTestId('swipe-hint')).not.toBeInTheDocument();
    expect(localStorage.getItem(SWIPE_TUTORIAL_KEY)).toBeNull();
  });

  it('shows the swipe tutorial once, dismissed by a tap, and remembers it', async () => {
    const first = renderCarousel();
    await screen.findByTestId('session-card-s1');
    const hint = screen.getByTestId('swipe-hint');
    expect(hint).toHaveTextContent('Swipe to browse sessions');
    expect(hint).toHaveAttribute('role', 'status');
    // Touches go through the hint: tapping a card dismisses it and selects.
    await userEvent.click(screen.getByTestId('session-card-s2'));
    expect(screen.queryByTestId('swipe-hint')).not.toBeInTheDocument();
    expect(screen.getByTestId('session-card-s2')).toHaveAttribute('aria-pressed', 'true');
    expect(localStorage.getItem(SWIPE_TUTORIAL_KEY)).toBe('1');

    first.unmount();
    renderCarousel();
    await screen.findByTestId('session-card-s1');
    expect(screen.queryByTestId('swipe-hint')).not.toBeInTheDocument();
  });

  it('dismisses the tutorial on keyboard or assistive activation (click without pointer)', async () => {
    renderCarousel();
    await screen.findByTestId('session-card-s1');
    expect(screen.getByTestId('swipe-hint')).toBeInTheDocument();
    act(() => screen.getByTestId('session-card-s2').click());
    expect(screen.queryByTestId('swipe-hint')).not.toBeInTheDocument();
    expect(screen.getByTestId('session-card-s2')).toHaveAttribute('aria-pressed', 'true');
    expect(localStorage.getItem(SWIPE_TUTORIAL_KEY)).toBe('1');
  });

  it('dismisses the tutorial on the first scroll', async () => {
    renderCarousel();
    const list = await screen.findByRole('list', { name: 'Session cards' });
    expect(screen.getByTestId('swipe-hint')).toBeInTheDocument();
    fireEvent.scroll(list);
    expect(screen.queryByTestId('swipe-hint')).not.toBeInTheDocument();
    expect(localStorage.getItem(SWIPE_TUTORIAL_KEY)).toBe('1');
  });

  it('dismisses the tutorial by itself after a few seconds', async () => {
    vi.useFakeTimers();
    renderCarousel();
    // Let the session list arrive without reaching the hint's timeout.
    await act(() => vi.advanceTimersByTimeAsync(50));
    expect(screen.getByTestId('swipe-hint')).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(SWIPE_TUTORIAL_MS));
    expect(screen.queryByTestId('swipe-hint')).not.toBeInTheDocument();
    expect(localStorage.getItem(SWIPE_TUTORIAL_KEY)).toBe('1');
  });

  it('works without storage (the tutorial still shows and dismisses)', async () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('denied');
    });
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('denied');
    });
    renderCarousel();
    await screen.findByTestId('session-card-s1');
    expect(screen.getByTestId('swipe-hint')).toBeInTheDocument();
    await userEvent.click(screen.getByTestId('session-card-s1'));
    expect(screen.queryByTestId('swipe-hint')).not.toBeInTheDocument();
  });
});
