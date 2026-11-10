import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import en from '../i18n/en.json';
import es from '../i18n/es.json';
import ja from '../i18n/ja.json';
import ko from '../i18n/ko.json';
import { I18nProvider } from '../i18n/context';
import { NOTICE_GROUPS, loadNotices } from '../licenses/notices';
import OpenSourceLicenses from './OpenSourceLicenses';

function renderScreen(onClose = vi.fn()) {
  render(
    <I18nProvider locale="en">
      <OpenSourceLicenses onClose={onClose} />
    </I18nProvider>,
  );
  return onClose;
}

describe('OpenSourceLicenses', () => {
  it('shows the generated counts by license', async () => {
    const notices = await loadNotices();
    renderScreen();
    expect(
      await screen.findByText(`Components by license · ${notices.packages.length} components`),
    ).toBeInTheDocument();
    const counts = screen.getByRole('list', { name: /^Components by license/ });
    const [license, count] = notices.counts[0]!;
    expect(within(counts).getByText(license)).toBeInTheDocument();
    expect(within(counts).getByText(String(count))).toBeInTheDocument();
    // Every component group present in the data has a heading.
    for (const group of NOTICE_GROUPS) {
      if (notices.packages.some((p) => p.g.includes(group))) {
        expect(screen.getByText(en[`oss.group.${group}`])).toBeInTheDocument();
      }
    }
    expect(screen.getByRole('link', { name: en['oss.fullNotices'] })).toHaveAttribute(
      'href',
      'https://github.com/JinsongRoh/Voice-Command-AI/blob/main/THIRD_PARTY_NOTICES.md',
    );
    expect(screen.getByText(en['oss.notBundledBody'])).toBeInTheDocument();
    expect(screen.getByText(en['oss.mplTitle'])).toBeInTheDocument();
  });

  it('filters by search and opens an item with its license text', async () => {
    const user = userEvent.setup();
    renderScreen();
    const search = await screen.findByLabelText(en['oss.search']);
    await user.type(search, 'tokio-tungstenite');
    const items = screen.getAllByRole('button', { name: /^tokio-tungstenite/ });
    expect(items.length).toBeGreaterThan(0);
    expect(screen.queryByRole('button', { name: /^react-dom/ })).not.toBeInTheDocument();

    await user.click(items[0]!);
    expect(screen.getByRole('heading', { name: /^tokio-tungstenite/ })).toBeInTheDocument();
    expect(screen.getByText(/Permission is hereby granted, free of charge/)).toBeInTheDocument();
    expect(
      screen.getByRole('link', { name: /github\.com\/snapview\/tokio-tungstenite/ }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: en['oss.back'] }));
    expect(screen.getByLabelText(en['oss.search'])).toHaveValue('tokio-tungstenite');
  });

  it('says so when nothing matches', async () => {
    const user = userEvent.setup();
    renderScreen();
    await user.type(await screen.findByLabelText(en['oss.search']), 'no-such-component-xyz');
    expect(screen.getByText(en['oss.noResults'])).toBeInTheDocument();
  });

  it('has every open-source licenses string in every locale', () => {
    const keys = Object.keys(en).filter((k) => k.startsWith('oss.'));
    expect(keys.length).toBeGreaterThan(10);
    for (const dict of [ko, ja, es] as Record<string, string>[]) {
      for (const key of keys) expect(dict[key], key).toBeTruthy();
    }
  });
});
