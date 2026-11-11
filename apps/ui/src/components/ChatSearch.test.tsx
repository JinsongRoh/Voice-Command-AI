import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { describe, expect, it, vi } from 'vitest';

import type { SessionEvent } from '../protocol/types';
import { renderWithApp } from '../test/render';
import { ChatSearch } from './ChatSearch';
import { countMatches, indexMatches, splitMatches } from './chatSearchIndex';
import { buildTimeline } from './chatTimeline';
import { ChatView } from './ChatView';

const msg = (seq: number, role: 'user' | 'assistant' | 'tool', text: string): SessionEvent => ({
  type: 'message',
  seq,
  at: seq,
  role,
  text,
});

const EVENTS: SessionEvent[] = [
  msg(1, 'user', '테스트를 실행해 줘'),
  msg(2, 'assistant', 'Running the 테스트 suite now. TEST output follows.'),
  { type: 'tool_call', seq: 3, at: 3, name: 'Bash', summary: 'pnpm test' },
  msg(4, 'tool', 'ran 52 files\n1 failing: 결제 테스트 timeout'),
  msg(5, 'assistant', 'One 테스트 failed; rerun the tests.'),
];

function Harness({ events = EVENTS }: { events?: SessionEvent[] }) {
  const [query, setQuery] = useState('');
  const [index, setIndex] = useState(0);
  const [count, setCount] = useState(0);
  return (
    <>
      <ChatSearch
        count={count}
        index={index}
        onQuery={(q) => {
          setQuery(q);
          setIndex(0);
        }}
        onIndex={setIndex}
      />
      <ChatView
        events={events}
        gapFrom={null}
        searchQuery={query}
        searchIndex={index}
        onSearchCount={setCount}
      />
    </>
  );
}

const current = () => document.querySelector('mark[data-current]');

describe('chat search helpers', () => {
  it('matches case-insensitively in any script and escapes the query', () => {
    expect(countMatches('Test test TEST', 'test')).toBe(3);
    expect(countMatches('결제 테스트, 테스트', '테스트')).toBe(2);
    expect(countMatches('a.b axb', 'a.b')).toBe(1);
    expect(splitMatches('xTeSty', 'test')).toEqual([
      { text: 'x', match: null },
      { text: 'TeSt', match: 0 },
      { text: 'y', match: null },
    ]);
    const items = buildTimeline(EVENTS, [], { includeOutput: false });
    const found = indexMatches(items, '테스트');
    expect(found.total).toBe(4);
    // The tool result's hit is in its collapsible body.
    expect([...found.byKey.values()].some((m) => m.body === 1 && m.head === 0)).toBe(true);
  });
});

describe('ChatSearch', () => {
  it('counts matches, highlights them and steps with wrap-around', async () => {
    const scroll = vi.fn();
    Element.prototype.scrollIntoView = scroll;
    renderWithApp(<Harness />);
    await userEvent.click(screen.getByRole('button', { name: 'Search this chat' }));
    const input = screen.getByRole('searchbox', { name: 'Search messages' });
    expect(input).toHaveFocus();
    await userEvent.type(input, 'TEST');
    await waitFor(() => expect(screen.getByTestId('search-count')).toHaveTextContent('1 / 3'));
    expect(document.querySelectorAll('mark[data-search-index]')).toHaveLength(3);
    expect(current()).toHaveTextContent('TEST');
    expect(current()).toHaveAttribute('data-search-index', '0');
    expect(scroll).toHaveBeenCalledWith({ block: 'center' });
    await userEvent.click(screen.getByRole('button', { name: 'Next match' }));
    expect(screen.getByTestId('search-count')).toHaveTextContent('2 / 3');
    await userEvent.click(input);
    await userEvent.keyboard('{Enter}');
    expect(screen.getByTestId('search-count')).toHaveTextContent('3 / 3');
    await userEvent.keyboard('{Enter}');
    expect(screen.getByTestId('search-count')).toHaveTextContent('1 / 3');
    await userEvent.keyboard('{Shift>}{Enter}{/Shift}');
    expect(screen.getByTestId('search-count')).toHaveTextContent('3 / 3');
    await userEvent.click(screen.getByRole('button', { name: 'Previous match' }));
    expect(screen.getByTestId('search-count')).toHaveTextContent('2 / 3');
    expect(current()).toHaveAttribute('data-search-index', '1');
  });

  it('ignores the Enter that confirms an IME composition', async () => {
    Element.prototype.scrollIntoView = vi.fn();
    renderWithApp(<Harness />);
    await userEvent.click(screen.getByRole('button', { name: 'Search this chat' }));
    const input = screen.getByRole('searchbox', { name: 'Search messages' });
    await userEvent.type(input, 'TEST');
    await waitFor(() => expect(screen.getByTestId('search-count')).toHaveTextContent('1 / 3'));
    fireEvent.keyDown(input, { key: 'Enter', isComposing: true });
    fireEvent.keyDown(input, { key: 'Enter', keyCode: 229 });
    expect(screen.getByTestId('search-count')).toHaveTextContent('1 / 3');
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(screen.getByTestId('search-count')).toHaveTextContent('2 / 3');
  });

  it('opens a collapsed row when the current match is inside it', async () => {
    Element.prototype.scrollIntoView = vi.fn();
    renderWithApp(<Harness />);
    const row = screen.getByRole('button', { name: /ran 52 files/ });
    expect(row).toHaveAttribute('aria-expanded', 'false');
    await userEvent.click(screen.getByRole('button', { name: 'Search this chat' }));
    await userEvent.type(screen.getByRole('searchbox'), '테스트');
    await waitFor(() => expect(screen.getByTestId('search-count')).toHaveTextContent('1 / 4'));
    expect(row).toHaveAttribute('aria-expanded', 'false');
    // Matches: user prompt, assistant reply, the tool result body, last reply.
    await userEvent.keyboard('{Enter}{Enter}');
    expect(screen.getByTestId('search-count')).toHaveTextContent('3 / 4');
    expect(row).toHaveAttribute('aria-expanded', 'true');
    expect(current()).toHaveTextContent('테스트');
    expect(current()?.closest('li')).toHaveTextContent('결제 테스트 timeout');
  });

  it('shows no results, and Escape clears and closes the search', async () => {
    Element.prototype.scrollIntoView = vi.fn();
    renderWithApp(<Harness />);
    await userEvent.click(screen.getByRole('button', { name: 'Search this chat' }));
    await userEvent.type(screen.getByRole('searchbox'), 'zzz');
    await waitFor(() => expect(screen.getByTestId('search-count')).toHaveTextContent('No results'));
    expect(screen.getByRole('button', { name: 'Next match' })).toBeDisabled();
    await userEvent.clear(screen.getByRole('searchbox'));
    await userEvent.type(screen.getByRole('searchbox'), 'one');
    await waitFor(() => expect(document.querySelectorAll('mark')).toHaveLength(1));
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('searchbox')).not.toBeInTheDocument();
    await act(async () => {
      await new Promise((r) => setTimeout(r, 200));
    });
    expect(document.querySelectorAll('mark')).toHaveLength(0);
    expect(screen.getByRole('button', { name: 'Search this chat' })).toHaveFocus();
  });
});

describe('ChatSearch in paged history', () => {
  it('offers to search earlier messages only while there are some, and says it works on loaded ones', async () => {
    const onSearchOlder = vi.fn();
    const { rerender } = renderWithApp(
      <ChatSearch
        count={0}
        index={0}
        onQuery={() => {}}
        onIndex={() => {}}
        canSearchOlder
        onSearchOlder={onSearchOlder}
      />,
    );
    await userEvent.click(screen.getByRole('button', { name: /search/i }));
    // Nothing typed yet: no extra button.
    expect(screen.queryByRole('button', { name: 'Search earlier messages' })).toBeNull();
    await userEvent.type(screen.getByRole('searchbox'), 'zzz');
    const older = screen.getByRole('button', { name: 'Search earlier messages' });
    expect(older).toHaveAttribute('title', 'Only loaded messages are searched');
    await userEvent.click(older);
    expect(onSearchOlder).toHaveBeenCalledTimes(1);
    rerender(
      <ChatSearch
        count={0}
        index={0}
        onQuery={() => {}}
        onIndex={() => {}}
        canSearchOlder={false}
      />,
    );
    expect(screen.queryByRole('button', { name: 'Search earlier messages' })).toBeNull();
  });

  it('stops a running search of earlier messages when it closes', async () => {
    const onStopSearchOlder = vi.fn();
    renderWithApp(
      <ChatSearch
        count={0}
        index={0}
        onQuery={() => {}}
        onIndex={() => {}}
        canSearchOlder
        searchingOlder
        onSearchOlder={() => {}}
        onStopSearchOlder={onStopSearchOlder}
        initialOpen
      />,
    );
    await userEvent.type(screen.getByRole('searchbox'), 'zzz');
    // Possibly within the debounce, before the parent ever saw the query.
    await userEvent.click(screen.getByRole('button', { name: 'Close search' }));
    expect(onStopSearchOlder).toHaveBeenCalledTimes(1);
  });
});
