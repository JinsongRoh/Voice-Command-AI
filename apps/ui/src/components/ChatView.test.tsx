import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { I18nProvider } from '../i18n/context';
import type { SessionEvent } from '../protocol/types';
import type { SentPrompt } from '../state/reducer';
import { ChatView, type ChatViewProps } from './ChatView';

const enc = new TextEncoder();

function renderChat(props: Partial<ChatViewProps> = {}) {
  return render(
    <I18nProvider locale="en">
      <ChatView events={[]} gapFrom={null} {...props} />
    </I18nProvider>,
  );
}

const sent = (overrides: Partial<SentPrompt> = {}): SentPrompt => ({
  id: 'p1',
  text: 'say hello',
  at: Date.now(),
  afterSeq: null,
  status: 'sending',
  ...overrides,
});

describe('ChatView', () => {
  it('shows sent prompts as outgoing bubbles with their delivery status', () => {
    const { rerender } = renderChat({ sent: [sent()] });
    const log = screen.getByRole('log');
    expect(within(log).getByText('say hello')).toBeInTheDocument();
    expect(within(log).getByText('Sending…')).toBeInTheDocument();
    rerender(
      <I18nProvider locale="en">
        <ChatView events={[]} gapFrom={null} sent={[sent({ status: 'failed', error: 'x' })]} />
      </I18nProvider>,
    );
    expect(within(log).getByText('Not delivered')).toHaveAttribute('title', 'x');
  });

  it('shows agent output and a terminal hint for agents without a transcript', async () => {
    const onShowTerminal = vi.fn();
    const events: SessionEvent[] = [
      { type: 'state_changed', seq: 1, at: 1, state: { type: 'running' } },
      {
        type: 'output',
        seq: 2,
        at: 2,
        data: enc.encode('say hello\r\n\x1b[1mHello there\x1b[0m\r\n'),
      },
    ];
    renderChat({
      events,
      sent: [sent({ afterSeq: 1, status: 'delivered' })],
      outputFallback: true,
      onShowTerminal,
    });
    expect(screen.getByRole('note')).toHaveTextContent(/no chat transcript/);
    expect(screen.getByText('Agent output')).toBeInTheDocument();
    expect(screen.getByText('Hello there')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Open Terminal' }));
    expect(onShowTerminal).toHaveBeenCalled();
  });

  it('hides output and the hint for transcript agents', () => {
    renderChat({
      events: [{ type: 'output', seq: 1, at: 1, data: enc.encode('raw tui') }],
    });
    expect(screen.queryByRole('note')).not.toBeInTheDocument();
    expect(screen.queryByText('raw tui')).not.toBeInTheDocument();
    expect(screen.getByText('No activity yet.')).toBeInTheDocument();
  });
});
