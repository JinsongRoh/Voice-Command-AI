import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { Toast } from '../state/reducer';
import { useActions } from '../state/store';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { Toasts } from './Toasts';

function Show({ toast }: { toast: Toast }) {
  const actions = useActions();
  return (
    <button type="button" onClick={() => actions.showToast(toast)}>
      show
    </button>
  );
}

const notice: Toast = {
  id: 'n1',
  category: 'completed',
  sessionId: null,
  title: 'Build finished',
  body: '',
};

function renderToast(toast: Toast) {
  renderWithApp(
    <>
      <Show toast={toast} />
      <Toasts onOpen={() => {}} />
    </>,
  );
  fireEvent.click(screen.getByRole('button', { name: 'show' }));
  return screen.getByText(toast.title).closest('li')!;
}

describe('Toasts', () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'Date'] });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('closes a plain notice after a few seconds', () => {
    renderToast(notice);
    act(() => vi.advanceTimersByTime(7000));
    expect(screen.queryByText('Build finished')).toBeNull();
  });

  it('pauses while hovered or focused and resumes with the time that was left', () => {
    const item = renderToast(notice);
    act(() => vi.advanceTimersByTime(5000));
    fireEvent.mouseEnter(item);
    act(() => vi.advanceTimersByTime(30_000));
    expect(screen.getByText('Build finished')).toBeInTheDocument();
    fireEvent.mouseLeave(item);
    // Focus inside pauses it too.
    fireEvent.focus(screen.getByRole('button', { name: 'Dismiss' }));
    act(() => vi.advanceTimersByTime(30_000));
    expect(screen.getByText('Build finished')).toBeInTheDocument();
    fireEvent.blur(screen.getByRole('button', { name: 'Dismiss' }));
    act(() => vi.advanceTimersByTime(1500));
    expect(screen.getByText('Build finished')).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(600));
    expect(screen.queryByText('Build finished')).toBeNull();
  });

  it('closes an input alert by itself, still offering Go and Later', () => {
    renderToast({
      ...notice,
      id: 'alert:i1',
      category: 'input',
      sessionId: 's1',
      title: 'payments-api is waiting for input',
      alertKey: 'i1',
    });
    expect(screen.getByRole('button', { name: 'Go' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Later' })).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(7000));
    expect(screen.queryByText('payments-api is waiting for input')).toBeNull();
  });

  it('keeps an approval alert until it is answered', () => {
    renderToast({
      ...notice,
      id: 'alert:a1',
      category: 'decision',
      sessionId: 's1',
      title: 'payments-api needs a decision',
      alertKey: 'a1',
    });
    act(() => vi.advanceTimersByTime(60_000));
    expect(screen.getByText('payments-api needs a decision')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Later' }));
    expect(screen.queryByText('payments-api needs a decision')).toBeNull();
  });
});

describe('task completed toasts', () => {
  function server() {
    return new FakeTransport((msg) =>
      msg.type === 'list_sessions'
        ? {
            type: 'sessions',
            sessions: [localSession(), localSession({ id: 's2', display_name: 'web' })],
          }
        : defaultResponse(msg),
    );
  }
  function Select({ id }: { id: string }) {
    const actions = useActions();
    return (
      <button type="button" onClick={() => actions.select(id)}>
        select
      </button>
    );
  }
  const finish = (transport: FakeTransport) =>
    act(() =>
      transport.push({
        type: 'session_updated',
        session: localSession({ id: 's2', display_name: 'web', state: { type: 'idle' } }),
      }),
    );

  it('are skipped for the session on screen', async () => {
    const transport = server();
    renderWithApp(
      <>
        <Select id="s2" />
        <Toasts onOpen={() => {}} />
      </>,
      { transport },
    );
    vi.spyOn(document, 'hasFocus').mockReturnValue(true);
    await waitFor(() => expect(transport.bodies('list_sessions')).toHaveLength(1));
    fireEvent.click(screen.getByRole('button', { name: 'select' }));
    finish(transport);
    expect(screen.queryByText('web finished')).toBeNull();
    vi.restoreAllMocks();
  });

  it('are shown for the selected session while the window is in the background', async () => {
    const transport = server();
    renderWithApp(
      <>
        <Select id="s2" />
        <Toasts onOpen={() => {}} />
      </>,
      { transport },
    );
    vi.spyOn(document, 'hasFocus').mockReturnValue(false);
    await waitFor(() => expect(transport.bodies('list_sessions')).toHaveLength(1));
    fireEvent.click(screen.getByRole('button', { name: 'select' }));
    finish(transport);
    expect(await screen.findByText('web finished')).toBeInTheDocument();
    vi.restoreAllMocks();
  });

  it('are shown for another session', async () => {
    const transport = server();
    renderWithApp(
      <>
        <Select id="s1" />
        <Toasts onOpen={() => {}} />
      </>,
      { transport },
    );
    await waitFor(() => expect(transport.bodies('list_sessions')).toHaveLength(1));
    fireEvent.click(screen.getByRole('button', { name: 'select' }));
    finish(transport);
    expect(await screen.findByText('web finished')).toBeInTheDocument();
  });
});
