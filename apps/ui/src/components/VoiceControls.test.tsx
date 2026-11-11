import { act, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { voiceQueue } from '../speech/voiceQueue';
import { defaultSettings, parseSettings } from '../state/settings';
import { useSettings } from '../state/store';
import { renderWithApp } from '../test/render';
import { SpeakingIndicator, VoiceToggle } from './VoiceControls';

/** Mirrors the Settings checkbox (the same setting). */
function SettingsProbe() {
  const { settings } = useSettings();
  return <output data-testid="spoken">{String(settings.spokenAlerts)}</output>;
}

function phone(matches: boolean) {
  vi.stubGlobal(
    'matchMedia',
    (query: string) =>
      ({
        matches,
        media: query,
        addEventListener: () => {},
        removeEventListener: () => {},
      }) as unknown as MediaQueryList,
  );
  window.matchMedia = globalThis.matchMedia;
}

beforeEach(() => voiceQueue.stopAll());
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('voice quick toggle', () => {
  it('switches spoken alerts in sync with Settings, and off stops the voice', async () => {
    const stopAll = vi.spyOn(voiceQueue, 'stopAll');
    renderWithApp(
      <>
        <VoiceToggle compact />
        <VoiceToggle />
        <SettingsProbe />
      </>,
    );
    const [header, alerts] = screen.getAllByRole('button', { name: 'Voice announcements on/off' });
    expect(header).toHaveAttribute('aria-pressed', 'false');
    expect(screen.getByTestId('spoken')).toHaveTextContent('false');
    await userEvent.click(header!);
    expect(header).toHaveAttribute('aria-pressed', 'true');
    expect(alerts).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByTestId('spoken')).toHaveTextContent('true');
    expect(stopAll).not.toHaveBeenCalled();
    await userEvent.click(alerts!);
    expect(header).toHaveAttribute('aria-pressed', 'false');
    await waitFor(() => expect(stopAll).toHaveBeenCalledTimes(1));
  });

  it('explains once on a phone what will be read aloud', async () => {
    phone(true);
    renderWithApp(<VoiceToggle />);
    const button = screen.getByRole('button', { name: 'Voice announcements on/off' });
    await userEvent.click(button);
    expect(screen.getByRole('status')).toHaveTextContent(/Alert titles will be read aloud/);
    expect(screen.getByRole('status')).toHaveTextContent(/only if you turn them on/);
    await userEvent.click(screen.getByRole('button', { name: 'OK' }));
    // Off and on again: not shown a second time.
    await userEvent.click(button);
    await userEvent.click(button);
    expect(screen.queryByRole('status')).toBeNull();
  });

  it('persists the note flag with the other settings', () => {
    const defaults = defaultSettings();
    expect(defaults.spokenAlertsNoteSeen).toBe(false);
    const saved = JSON.stringify({ ...defaults, spokenAlerts: true, spokenAlertsNoteSeen: true });
    const loaded = parseSettings(saved, defaults);
    expect(loaded.spokenAlerts).toBe(true);
    expect(loaded.spokenAlertsNoteSeen).toBe(true);
    expect(parseSettings('{"spokenAlertsNoteSeen":"yes"}', defaults).spokenAlertsNoteSeen).toBe(
      false,
    );
  });
});

describe('speaking indicator', () => {
  it('appears while the queue speaks; a tap or Esc stops everything', async () => {
    renderWithApp(<SpeakingIndicator />);
    expect(screen.queryByRole('button')).toBeNull();
    const stop = vi.fn();
    const item = () => ({
      priority: 'completion' as const,
      play: () => new Promise<void>(() => {}),
      stop,
    });
    act(() => {
      voiceQueue.enqueue({ ...item(), key: 'a' });
    });
    const button = await screen.findByRole('button', { name: 'Speaking. Tap to stop all' });
    await userEvent.click(button);
    expect(stop).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole('button')).toBeNull();
    act(() => {
      voiceQueue.enqueue({ ...item(), key: 'b' });
    });
    await screen.findByRole('button', { name: 'Speaking. Tap to stop all' });
    await userEvent.keyboard('{Escape}');
    expect(stop).toHaveBeenCalledTimes(2);
    await waitFor(() => expect(screen.queryByRole('button')).toBeNull());
  });
});
