import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { I18nProvider } from '../i18n/context';
import { cloudSession, localSession } from '../test/fixtures';
import { SessionCard } from './SessionCard';

function renderCard(
  props: Partial<Parameters<typeof SessionCard>[0]> = {},
  locale: 'en' | 'es' = 'en',
) {
  const onSelect = vi.fn();
  render(
    <I18nProvider locale={locale}>
      <ul>
        <SessionCard
          session={localSession()}
          preview="✓ 52 tests passed"
          selected={false}
          index={1}
          onSelect={onSelect}
          {...props}
        />
      </ul>
    </I18nProvider>,
  );
  return { onSelect };
}

describe('SessionCard', () => {
  it('menu: arrows, Home / End, Tab closes, focus back on the menu button', async () => {
    const onBringToFront = vi.fn();
    renderCard({ onRename: vi.fn(), onFixPronunciation: vi.fn(), onBringToFront });
    const button = screen.getByRole('button', { name: 'More actions for payments-api' });
    await userEvent.click(button);
    const items = screen.getAllByRole('menuitem');
    expect(items).toHaveLength(3);
    expect(items[0]).toHaveFocus();
    await userEvent.keyboard('{ArrowDown}');
    expect(items[1]).toHaveFocus();
    await userEvent.keyboard('{End}');
    expect(items[2]).toHaveFocus();
    await userEvent.keyboard('{ArrowDown}');
    expect(items[0]).toHaveFocus();
    await userEvent.keyboard('{ArrowUp}');
    expect(items[2]).toHaveFocus();
    await userEvent.keyboard('{Home}');
    expect(items[0]).toHaveFocus();
    await userEvent.keyboard('{Tab}');
    expect(screen.queryByRole('menu')).toBeNull();
    expect(button).toHaveFocus();
    // An action that opens no dialog still leaves the focus on the button.
    await userEvent.click(button);
    await userEvent.click(screen.getAllByRole('menuitem')[2]!);
    expect(onBringToFront).toHaveBeenCalledWith('s1');
    expect(screen.queryByRole('menu')).toBeNull();
    expect(button).toHaveFocus();
  });

  it('refers to the unread count only when there is one', () => {
    renderCard();
    const card = screen.getByRole('button', { pressed: false, name: /payments-api/ });
    const ids = card.getAttribute('aria-describedby')!.split(' ');
    for (const id of ids) expect(document.getElementById(id)).not.toBeNull();
  });

  it('marks the conductor session with a small badge, without unread or highlight', () => {
    renderCard({
      session: localSession({
        conductor: true,
        unread: 2,
        state: { type: 'awaiting_input' },
        display_name: 'Conductor',
      }),
    });
    expect(screen.getByTestId('session-conductor-badge')).toHaveTextContent('Conductor');
    expect(screen.queryByText('2 unread')).not.toBeInTheDocument();
    const card = screen.getByTestId('session-card-s1');
    expect(card.className).not.toMatch(/attention/);
  });

  it('highlights the conductor session for a decision', () => {
    renderCard({
      session: localSession({ conductor: true, state: { type: 'awaiting_decision' } }),
    });
    expect(screen.getByTestId('session-card-s1').className).toMatch(/attention/);
  });

  it('says a menu waits in the terminal instead of a bare input wait', () => {
    renderCard({
      session: localSession({ state: { type: 'awaiting_input' }, terminal_prompt: 'menu' }),
    });
    expect(screen.getByText('Choose in the terminal')).toBeInTheDocument();
    expect(screen.queryByText('Waiting for input')).not.toBeInTheDocument();
  });

  it('renders agent, folder, state, preview and relative time', () => {
    renderCard();
    const card = screen.getByRole('button', { name: /payments-api/ });
    expect(card).toHaveAttribute('aria-pressed', 'false');
    expect(screen.getByRole('img', { name: 'Claude Code' })).toBeInTheDocument();
    expect(screen.getByText('Claude Code')).toBeInTheDocument();
    expect(screen.getByText('Running')).toBeInTheDocument();
    expect(screen.getByText('✓ 52 tests passed')).toBeInTheDocument();
    expect(screen.getByText(/Last activity 30 seconds ago/)).toBeInTheDocument();
    expect(screen.queryByText('Cloud')).not.toBeInTheDocument();
    expect(card).toHaveAccessibleDescription(/Running/);
  });

  it('shows the cloud badge, provider and unread count', () => {
    renderCard({ session: cloudSession({ unread: 3, state: { type: 'awaiting_input' } }) });
    expect(screen.getByText('Cloud')).toBeInTheDocument();
    expect(screen.getAllByText('Jules').length).toBeGreaterThan(0);
    expect(screen.getByText('3 unread')).toBeInTheDocument();
    expect(screen.getByText('Waiting for input')).toBeInTheDocument();
  });

  it('highlights the selected card and reports clicks', async () => {
    const { onSelect } = renderCard({ selected: true });
    const card = screen.getByRole('button', { name: /payments-api/ });
    expect(card).toHaveAttribute('aria-pressed', 'true');
    await userEvent.click(card);
    expect(onSelect).toHaveBeenCalledWith('s1');
  });

  it('shows exit codes and falls back to a placeholder preview', () => {
    renderCard({ session: localSession({ state: { type: 'exited', code: 1 } }), preview: '' });
    expect(screen.getByText('Exited (1)')).toBeInTheDocument();
    expect(screen.getByText('No output yet')).toBeInTheDocument();
  });

  it('shows how many sub-agents work, hidden at 0', () => {
    renderCard({ session: localSession({ subagents: 3 }) });
    const badge = screen.getByTestId('subagent-badge');
    expect(badge).toHaveTextContent('3');
    expect(screen.getByRole('img', { name: 'Sub-agents working: 3' })).toBe(badge);
    expect(badge).toHaveAttribute('title', 'Sub-agents working: 3');
  });

  it('has no sub-agent badge without sub-agents', () => {
    renderCard({ session: localSession({ subagents: 0 }) });
    expect(screen.queryByTestId('subagent-badge')).not.toBeInTheDocument();
    renderCard();
    expect(screen.queryByTestId('subagent-badge')).not.toBeInTheDocument();
  });

  it('labels the sub-agent badge in Korean', () => {
    render(
      <I18nProvider locale="ko">
        <ul>
          <SessionCard
            session={localSession({ subagents: 3 })}
            preview=""
            selected={false}
            index={1}
            onSelect={() => {}}
          />
        </ul>
      </I18nProvider>,
    );
    const badge = screen.getByRole('img', { name: '서브 에이전트 3개 작업 중' });
    expect(badge).toHaveAttribute('title', '서브 에이전트 3개 작업 중');
  });

  it('is localized', () => {
    renderCard({ session: localSession({ unread: 2 }) }, 'es');
    expect(screen.getByText('En ejecución')).toBeInTheDocument();
    expect(screen.getByText('2 sin leer')).toBeInTheDocument();
  });

  it('shows the executable name of a generic agent launched by path', () => {
    const path = '/Users/x/dev/voice-command-ai/target/debug/vca-fake-agent';
    renderCard({
      session: localSession({
        origin: {
          type: 'local',
          agent: { type: 'generic', name: path },
          project_dir: '/Users/x/dev/demo',
          pid: 1,
        },
      }),
    });
    expect(screen.getByText('vca-fake-agent')).toHaveAttribute('title', path);
    expect(screen.getByRole('img', { name: 'vca-fake-agent' })).toHaveTextContent('VF');
    expect(screen.queryByText(path)).not.toBeInTheDocument();
  });
});
