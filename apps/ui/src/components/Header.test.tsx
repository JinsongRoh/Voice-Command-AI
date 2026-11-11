import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { renderWithApp } from '../test/render';
import { Header } from './Header';

/** Reports `matches` for the "tight" (< 340 px) query only. */
function width(tight: boolean) {
  vi.stubGlobal(
    'matchMedia',
    (query: string) =>
      ({
        matches: tight && query.includes('339px'),
        media: query,
        addEventListener: () => {},
        removeEventListener: () => {},
      }) as unknown as MediaQueryList,
  );
  window.matchMedia = globalThis.matchMedia;
}

afterEach(() => vi.unstubAllGlobals());

const ALERTS = 'Voice announcements on/off';
const STATUS = 'Listen to status';

describe('Header actions', () => {
  it('shows both speaker actions with different icons and labels', async () => {
    renderWithApp(<Header />);
    const status = await screen.findByRole('button', { name: STATUS });
    const alerts = screen.getByRole('button', { name: ALERTS });
    expect(alerts).toBeVisible();
    expect(status).toBeVisible();
    expect(alerts).toHaveAttribute('title');
    // The title explains what the summary is; the name stays short.
    expect(status.getAttribute('title')).toMatch(/^Listen to status:/);
    // Two clearly different pictures (speaker vs the spoken-summary list).
    expect(alerts.querySelector('path')?.getAttribute('d')).not.toEqual(
      status.querySelector('path')?.getAttribute('d'),
    );
    expect(screen.queryByRole('button', { name: 'More actions' })).toBeNull();
  });

  it('keeps both actions reachable in the overflow menu on narrow phones', async () => {
    width(true);
    renderWithApp(<Header />);
    const more = await screen.findByRole('button', { name: 'More actions' });
    expect(more).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByRole('button', { name: ALERTS })).toBeNull();
    await userEvent.click(more);
    expect(more).toHaveAttribute('aria-expanded', 'true');
    expect(await screen.findByRole('button', { name: STATUS })).toBeVisible();
    expect(screen.getByRole('button', { name: ALERTS })).toBeVisible();
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('button', { name: ALERTS })).toBeNull();
  });

  it('lets the machine select shrink so the actions never leave the screen', () => {
    renderWithApp(<Header />);
    const select = screen.getByRole('combobox', { name: 'Machine' });
    // The CSS module carries the truncation rules (jsdom has no layout);
    // the class must be applied to the select.
    expect(select.className).toMatch(/machine/);
  });
});
