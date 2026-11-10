import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import {
  DIAG_DEFAULT_ON,
  clearDiag,
  diag,
  diagEntries,
  flushDiag,
  setDiagEnabled,
} from '../diag/log';
import { defaultSettings, parseSettings } from '../state/settings';
import { useSettings } from '../state/store';
import { renderWithApp } from '../test/render';
import { DiagLog } from './DiagLog';

afterEach(() => {
  setDiagEnabled(true);
  clearDiag();
});

describe('DiagLog', () => {
  it('is off until the person turns it on, and records nothing meanwhile', async () => {
    setDiagEnabled(false);
    renderWithApp(<DiagLog />);
    const toggle = screen.getByRole('checkbox', { name: 'Keep a diagnostic log on this device' });
    expect(toggle).not.toBeChecked();
    expect(screen.queryByRole('button', { name: /Diagnostic log/ })).toBeNull();
    diag('mic.start.ok');
    expect(diagEntries()).toEqual([]);

    await userEvent.click(toggle);
    expect(toggle).toBeChecked();
    diag('mic.start.ok');
    expect(diagEntries()).toHaveLength(1);
  });

  it('shows what the log holds and that it stays on the device', () => {
    renderWithApp(<DiagLog />, { settings: { diagnostics: true } });
    const toggle = screen.getByRole('checkbox', { name: 'Keep a diagnostic log on this device' });
    expect(toggle).toBeChecked();
    expect(toggle).toHaveAccessibleDescription(/stays on this device and is never uploaded/);
    expect(toggle).toHaveAccessibleDescription(/does not hold audio, transcripts/);
    expect(toggle).toHaveAccessibleDescription(/only if you copy it/);
  });

  it('stops recording and clears the stored log when turned off', async () => {
    diag('mic.start.ok');
    flushDiag();
    renderWithApp(<DiagLog />, { settings: { diagnostics: true } });
    await userEvent.click(screen.getByRole('button', { name: /Diagnostic log/ }));
    expect(screen.getByRole('button', { name: 'Copy' })).toBeInTheDocument();

    await userEvent.click(screen.getByRole('checkbox'));
    expect(diagEntries()).toEqual([]);
    expect(localStorage.getItem('vca.diag.v1')).toBeNull();
    expect(screen.queryByRole('button', { name: /Diagnostic log/ })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Copy' })).toBeNull();
    diag('mic.start.ok');
    expect(diagEntries()).toEqual([]);

    await userEvent.click(screen.getByRole('checkbox'));
    diag('mic.start.ok');
    expect(diagEntries()).toHaveLength(1);
  });

  it('starts off when the setting is off', () => {
    renderWithApp(<DiagLog />, { settings: { diagnostics: false } });
    expect(screen.getByRole('checkbox')).not.toBeChecked();
    expect(screen.queryByRole('button', { name: /Diagnostic log/ })).toBeNull();
  });
});

describe('diagnostics setting', () => {
  it('defaults to off (opt-in) and keeps a choice across a restart', () => {
    const defaults = defaultSettings();
    expect(DIAG_DEFAULT_ON).toBe(false);
    expect(defaults.diagnostics).toBe(false);
    expect(defaults.diagnosticsChosen).toBe(false);
    expect(parseSettings('{}', defaults).diagnostics).toBe(false);
    const chosen = (diagnostics: unknown) =>
      parseSettings(JSON.stringify({ diagnostics, diagnosticsChosen: true }), defaults);
    expect(chosen(true)).toMatchObject({ diagnostics: true, diagnosticsChosen: true });
    expect(chosen(false)).toMatchObject({ diagnostics: false, diagnosticsChosen: true });
    expect(chosen('no')).toMatchObject({ diagnostics: false, diagnosticsChosen: false });
  });

  it('turns off a log that an older build kept on only by default', () => {
    // Older builds saved the whole settings object, the default `true` included.
    const defaults = defaultSettings();
    const old = JSON.stringify({ theme: 'dark', diagnostics: true });
    expect(parseSettings(old, defaults)).toMatchObject({
      theme: 'dark',
      diagnostics: false,
      diagnosticsChosen: false,
    });
    expect(parseSettings(JSON.stringify({ diagnostics: false }), defaults).diagnostics).toBe(false);
  });

  it('records the choice when the switch is used', async () => {
    let saved: { diagnostics: boolean; diagnosticsChosen: boolean } | undefined;
    function Probe() {
      const { settings } = useSettings();
      saved = settings;
      return null;
    }
    renderWithApp(
      <>
        <DiagLog />
        <Probe />
      </>,
    );
    await userEvent.click(screen.getByRole('checkbox'));
    expect(saved).toMatchObject({ diagnostics: true, diagnosticsChosen: true });
  });
});
