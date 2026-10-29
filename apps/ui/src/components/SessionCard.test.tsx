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
