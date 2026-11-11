import { act, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { renderWithApp } from '../test/render';
import settingsSource from './SettingsView.tsx?raw';

/** The stylesheet as written (tests do not process CSS). */
async function settingsCss(): Promise<string> {
  // Node's fs, without pulling Node types into the app's type check.
  const fs = (await import(/* @vite-ignore */ 'node:' + 'fs')) as {
    readFileSync(path: string, encoding: 'utf8'): string;
  };
  return fs.readFileSync('src/components/SettingsView.module.css', 'utf8'); // vitest runs in apps/ui;
}
import { SettingsView, USER_GUIDE_URL } from './SettingsView';
import {
  SETTINGS_SECTIONS,
  SETTINGS_TABS,
  openSettingsAt,
  tabOf,
  type SettingsTab,
} from './settingsNav';

/**
 * Every Settings section, by tab. A new section must be added here and to
 * `SETTINGS_SECTIONS`, so no section can end up outside every tab.
 */
const EXPECTED: Record<SettingsTab, string[]> = {
  general: ['set-appearance', 'set-startup'],
  voice: ['set-speech', 'set-misheard', 'set-spoken', 'set-engines'],
  alerts: ['set-notify', 'set-push', 'set-agent-hooks'],
  connection: [
    'set-connect-phone',
    'set-conn',
    'set-dnspilot',
    'set-machines',
    'set-devices',
    'set-audit',
  ],
  security: ['set-app-lock', 'set-sec-status', 'set-sec-mail', 'set-sec-recovery', 'set-security'],
  conductor: ['set-conductor'],
  about: ['set-about', 'set-oss', 'set-support', 'set-dev'],
};

/** Sections that need the desktop shell, a phone or the mock server. */
const CONDITIONAL = [
  'set-startup',
  'set-misheard',
  'set-engines',
  'set-push',
  'set-dnspilot',
  'set-dev',
  'set-sec-mail',
  'set-sec-recovery',
  'set-app-lock',
];

const TAB_NAMES: Record<SettingsTab, string> = {
  general: 'General',
  voice: 'Voice',
  alerts: 'Alerts',
  connection: 'Connection & security',
  security: 'Security',
  conductor: 'Command Center',
  about: 'About',
};

function renderSettings() {
  return renderWithApp(<SettingsView onPair={() => {}} onConnectPhone={() => {}} />);
}

function shownSectionIds(): string[] {
  const panel = screen.getByRole('tabpanel');
  return Array.from(panel.querySelectorAll('h3[id^="set-"]')).map((h) => h.id);
}

const scroll = vi.fn();
beforeEach(() => {
  localStorage.clear();
  scroll.mockClear();
  Element.prototype.scrollIntoView = scroll;
});
afterEach(() => {
  vi.restoreAllMocks();
});

describe('Settings tabs', () => {
  it('render as a tab list and show one group at a time', async () => {
    renderSettings();
    const tabs = within(screen.getByRole('tablist', { name: 'Settings sections' })).getAllByRole(
      'tab',
    );
    expect(tabs.map((tab) => tab.textContent)).toEqual(SETTINGS_TABS.map((x) => TAB_NAMES[x]));
    expect(screen.getByRole('tab', { name: 'General' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('heading', { name: 'Appearance' })).toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: 'Speech recognition' })).toBeNull();

    await userEvent.click(screen.getByRole('tab', { name: 'Voice' }));
    expect(screen.getByRole('tab', { name: 'Voice' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('heading', { name: 'Speech recognition' })).toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: 'Appearance' })).toBeNull();
    expect(screen.getByRole('tabpanel')).toHaveAttribute(
      'aria-labelledby',
      screen.getByRole('tab', { name: 'Voice' }).id,
    );
  });

  it('lists every section in exactly one tab', () => {
    const registry: Record<string, string[]> = {};
    for (const [id, tab] of Object.entries(SETTINGS_SECTIONS)) (registry[tab] ??= []).push(id);
    expect(registry).toEqual(EXPECTED);
    // Every section heading in the source is registered (conditional ones too).
    const ids = [...settingsSource.matchAll(/<h3 id="(set-[a-z-]+)"/g)].map((m) => m[1]);
    const external = ['set-machines', 'set-devices', 'set-audit']; // their own components
    expect(new Set([...ids, ...external])).toEqual(new Set(Object.keys(SETTINGS_SECTIONS)));
  });

  it('reaches every section through its tab', async () => {
    renderSettings();
    const seen: string[] = [];
    for (const tab of SETTINGS_TABS) {
      await userEvent.click(screen.getByRole('tab', { name: TAB_NAMES[tab] }));
      const ids = shownSectionIds();
      for (const id of ids)
        expect([id, tabOf(id as keyof typeof SETTINGS_SECTIONS)]).toEqual([id, tab]);
      seen.push(...ids);
    }
    const always = Object.keys(SETTINGS_SECTIONS).filter((id) => !CONDITIONAL.includes(id));
    expect(new Set(seen)).toEqual(new Set(always));
  });

  it('moves with the arrow, Home and End keys', async () => {
    renderSettings();
    const general = screen.getByRole('tab', { name: 'General' });
    expect(general).toHaveAttribute('tabindex', '0');
    expect(screen.getByRole('tab', { name: 'Voice' })).toHaveAttribute('tabindex', '-1');
    general.focus();
    await userEvent.keyboard('{ArrowRight}');
    expect(screen.getByRole('tab', { name: 'Voice' })).toHaveFocus();
    expect(screen.getByRole('tab', { name: 'Voice' })).toHaveAttribute('aria-selected', 'true');
    await userEvent.keyboard('{End}');
    expect(screen.getByRole('tab', { name: 'About' })).toHaveFocus();
    await userEvent.keyboard('{ArrowRight}');
    expect(screen.getByRole('tab', { name: 'General' })).toHaveFocus();
    await userEvent.keyboard('{ArrowLeft}');
    expect(screen.getByRole('tab', { name: 'About' })).toHaveAttribute('aria-selected', 'true');
    await userEvent.keyboard('{Home}');
    expect(screen.getByRole('tab', { name: 'General' })).toHaveAttribute('aria-selected', 'true');
  });

  it('remembers the last tab', async () => {
    const { unmount } = renderSettings();
    await userEvent.click(screen.getByRole('tab', { name: 'Alerts' }));
    expect(localStorage.getItem('vca.settings.tab')).toBe('alerts');
    unmount();
    renderSettings();
    expect(screen.getByRole('tab', { name: 'Alerts' })).toHaveAttribute('aria-selected', 'true');
  });

  it('works when storage is unavailable', async () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('denied');
    });
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('denied');
    });
    renderSettings();
    expect(screen.getByRole('tab', { name: 'General' })).toHaveAttribute('aria-selected', 'true');
    await userEvent.click(screen.getByRole('tab', { name: 'About' }));
    expect(screen.getByRole('tab', { name: 'About' })).toHaveAttribute('aria-selected', 'true');
  });

  it('keeps the tab row on one scrolling line (no page overflow on phones)', async () => {
    const css = await settingsCss();
    renderSettings();
    expect(screen.getByRole('tablist')).toHaveClass('tabs');
    const rule = (name: string) => new RegExp(`\\.${name} \\{([^}]*)\\}`).exec(css)?.[1] ?? '';
    expect(rule('tabs')).toMatch(/overflow-x: auto/);
    expect(rule('tabs')).toMatch(/position: sticky/);
    expect(rule('tabs')).toMatch(/max-width: 100%/);
    expect(rule('tabs')).toMatch(/min-width: 0/);
    expect(rule('tab')).toMatch(/white-space: nowrap/);
    expect(rule('tab')).toMatch(/flex: none/);
    expect(rule('tabs')).not.toMatch(/flex-wrap: wrap/);
  });
});

describe('Settings deep links', () => {
  it('open the tab of the requested section and scroll to it', () => {
    localStorage.setItem('vca.settings.tab', 'about');
    openSettingsAt('set-conductor');
    renderSettings();
    expect(screen.getByRole('tab', { name: 'Command Center' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    const section = screen.getByRole('region', { name: 'Command Center' });
    expect(scroll).toHaveBeenCalled();
    expect(scroll.mock.contexts).toContain(section);
  });

  it('switch an open Settings view', () => {
    renderSettings();
    act(() => openSettingsAt('set-security'));
    expect(screen.getByRole('tab', { name: 'Security' })).toHaveAttribute('aria-selected', 'true');
    expect(scroll.mock.contexts).toContain(screen.getByRole('region', { name: 'Kill switch' }));
    act(() => openSettingsAt('set-connect-phone'));
    expect(scroll.mock.contexts).toContain(screen.getByRole('region', { name: 'Connect a phone' }));
  });

  it('map anchors inside a section to its tab', () => {
    expect(tabOf('set-updates')).toBe('about');
    expect(tabOf('set-spoken')).toBe('voice');
    expect(tabOf('set-agent-hooks')).toBe('alerts');
  });

  it('expire when Settings does not open', () => {
    vi.useFakeTimers();
    try {
      openSettingsAt('set-conductor');
      vi.advanceTimersByTime(5000);
      renderSettings();
      expect(screen.getByRole('tab', { name: 'General' })).toHaveAttribute('aria-selected', 'true');
    } finally {
      vi.useRealTimers();
    }
  });
});

describe('Settings search', () => {
  it('finds sections across tabs and opens them', async () => {
    renderSettings();
    await userEvent.type(screen.getByRole('searchbox', { name: 'Search settings' }), 'kill');
    const results = screen.getByTestId('settings-search-results');
    expect(screen.queryByRole('tabpanel')).toBeNull();
    const hit = within(results).getByRole('button', { name: /Kill switch/ });
    expect(hit).toHaveTextContent('Security');
    await userEvent.click(hit);
    expect(screen.getByRole('tab', { name: 'Security' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('searchbox', { name: 'Search settings' })).toHaveValue('');
    expect(screen.getByRole('heading', { name: 'Kill switch' })).toBeInTheDocument();
  });

  it('finds sections by untranslated technical words', async () => {
    renderSettings();
    const search = screen.getByRole('searchbox', { name: 'Search settings' });
    await userEvent.type(search, 'TOTP');
    let results = screen.getByTestId('settings-search-results');
    expect(within(results).getByRole('button', { name: /Security status/ })).toBeInTheDocument();
    await userEvent.clear(search);
    await userEvent.type(search, 'claude');
    results = screen.getByTestId('settings-search-results');
    expect(within(results).getByRole('button', { name: /Agent/ })).toBeInTheDocument();
    await userEvent.clear(search);
    await userEvent.type(search, 'whisper');
    results = screen.getByTestId('settings-search-results');
    expect(within(results).getByRole('button', { name: /Speech recognition/ })).toBeInTheDocument();
  });

  it('says when nothing matches', async () => {
    renderSettings();
    await userEvent.type(screen.getByRole('searchbox', { name: 'Search settings' }), 'zzzz');
    expect(screen.getByText('No matching settings.')).toBeInTheDocument();
  });
});

describe('User manual', () => {
  it('opens the online guide from the top of Settings and from About', async () => {
    renderSettings();
    const top = screen.getByTestId('settings-manual');
    expect(top).toHaveAccessibleName('User manual');
    expect(top).toHaveAttribute('href', USER_GUIDE_URL);
    expect(top).toHaveAttribute('target', '_blank');
    expect(USER_GUIDE_URL).toBe(
      'https://github.com/JinsongRoh/Voice-Command-AI/blob/main/docs/user-guide.md',
    );
    await userEvent.click(screen.getByRole('tab', { name: 'About' }));
    const about = screen.getByRole('region', { name: 'About' });
    expect(within(about).getByRole('link', { name: 'User manual' })).toHaveAttribute(
      'href',
      USER_GUIDE_URL,
    );
  });
});
