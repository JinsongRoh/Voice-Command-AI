import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { I18nProvider } from '../i18n/context';
import { defaultSettings, loadSettings } from '../state/settings';
import { AppProvider } from '../state/store';
import { FakeTransport } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { Composer } from './Composer';
import { MicAnimationSettings } from './MicAnimationSettings';
import { SettingsView } from './SettingsView';
import { REDUCED_MOTION_QUERY } from './VoiceWave';

function mockReducedMotion(reduce: boolean) {
  vi.stubGlobal(
    'matchMedia',
    vi.fn((query: string) => ({
      matches: query === REDUCED_MOTION_QUERY ? reduce : false,
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    })),
  );
}

const LIGHT = [
  'Drop from above',
  'Light particles',
  'From all sides',
  'Rise from below',
  'Sideways whoosh',
  'Spiral',
  'Burst and reassemble',
  'Scan line',
];
const STROKE = [
  'Amplitude (classic)',
  'Voice-driven',
  'Travelling wave',
  'Center out',
  'Light flow',
  'Glow breath',
  'Stroke writing',
  'Logo echo',
  'Spread apart',
  'Ring of bars',
];

/** The picker and a composer sharing one settings store. */
function renderBoth(settings = {}) {
  return renderWithApp(
    <>
      <MicAnimationSettings defaultExpanded />
      <Composer session={localSession()} />
    </>,
    { settings },
  );
}

const micButton = () => screen.getByRole('button', { name: 'Tap to dictate' });
const micSymbol = (mic: HTMLElement) => within(mic).getByTestId('mic-symbol');

beforeEach(() => {
  localStorage.clear();
  mockReducedMotion(false);
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe('Dictation button animation setting', () => {
  it('lists the 18 styles in two groups, light assembly (E first, G second) then strokes', () => {
    renderBoth();
    const picker = screen.getByRole('group', { name: 'Dictation button animation' });
    const light = within(picker).getByRole('group', { name: 'Light assembly' });
    const stroke = within(picker).getByRole('group', { name: 'Stroke motion' });
    const lightRadios = within(light).getAllByRole('radio');
    const strokeRadios = within(stroke).getAllByRole('radio');
    expect(lightRadios).toHaveLength(8);
    expect(strokeRadios).toHaveLength(10);
    lightRadios.forEach((radio, i) => expect(radio).toHaveAccessibleName(LIGHT[i]));
    strokeRadios.forEach((radio, i) => expect(radio).toHaveAccessibleName(STROKE[i]));
    expect(within(picker).getAllByRole('radio')).toHaveLength(18);
    // Each style says what it does.
    expect(lightRadios[0]).toHaveAccessibleDescription(/fall from above/);
  });

  it('is one row by default, and all 18 styles are one click away', async () => {
    renderWithApp(<MicAnimationSettings />);
    const current = screen.getByTestId('mic-animation-current');
    expect(current).toHaveTextContent('Drop from above');
    expect(screen.queryAllByRole('radio')).toHaveLength(0);
    const toggle = within(current).getByRole('button', { name: 'Choose another style' });
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    await userEvent.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    expect(screen.getAllByRole('radio')).toHaveLength(18);
    await userEvent.click(screen.getByRole('radio', { name: 'Light particles' }));
    expect(current).toHaveTextContent('Light particles');
  });

  it('selects E by default, for the picker and the composer mic button', () => {
    renderBoth();
    expect(screen.getByRole('radio', { name: 'Drop from above' })).toBeChecked();
    const mic = micButton();
    expect(mic).toHaveAttribute('data-animation', 'light-drop');
    expect(micSymbol(mic)).toHaveAttribute('data-anim', 'light-drop');
  });

  it('selecting a style changes the composer mic button', async () => {
    renderBoth();
    await userEvent.click(screen.getByRole('radio', { name: 'Travelling wave' }));
    expect(screen.getByRole('radio', { name: 'Travelling wave' })).toBeChecked();
    expect(screen.getByRole('radio', { name: 'Drop from above' })).not.toBeChecked();
    const mic = micButton();
    expect(mic).toHaveAttribute('data-animation', 'stroke-wave');
    expect(micSymbol(mic)).toHaveAttribute('data-kind', 'stroke');
  });

  it('persists the choice in the settings store', async () => {
    render(
      <AppProvider
        machines={[{ id: 'm', name: 'test', connectivity: 'LAN', transport: new FakeTransport() }]}
        settings={defaultSettings()}
      >
        <I18nProvider locale="en">
          <MicAnimationSettings defaultExpanded />
        </I18nProvider>
      </AppProvider>,
    );
    await userEvent.click(screen.getByRole('radio', { name: 'Light particles' }));
    await waitFor(() => expect(loadSettings().micAnimation).toBe('light-particles'));
    expect(JSON.parse(localStorage.getItem('vca.settings.v1')!)).toMatchObject({
      micAnimation: 'light-particles',
    });
  });

  it('animates only the selected preview and the one under the pointer or focus', async () => {
    renderBoth();
    const picker = screen.getByTestId('mic-animation-picker');
    const moving = () =>
      within(picker)
        .getAllByTestId('mic-symbol')
        .filter((s) => s.getAttribute('data-phase') === 'recording')
        .map((s) => s.getAttribute('data-anim'));
    expect(within(picker).getAllByTestId('mic-symbol')).toHaveLength(18);
    expect(moving()).toEqual(['light-drop']);
    const spiral = screen.getByRole('radio', { name: 'Spiral' });
    fireEvent.mouseEnter(spiral.closest('label')!);
    expect(moving()).toEqual(['light-drop', 'light-spiral']);
    // Pointing at a light style plays its assembly.
    const spiralPreview = within(spiral.closest('label')!).getByTestId('mic-symbol');
    expect(spiralPreview).toHaveAttribute('data-assembling', 'true');
    fireEvent.mouseLeave(spiral.closest('label')!);
    expect(moving()).toEqual(['light-drop']);
    act(() => screen.getByRole('radio', { name: 'Logo echo' }).focus());
    expect(moving()).toEqual(['light-drop', 'stroke-echo']);
  });

  it('choosing the selected light style again replays it', () => {
    renderBoth();
    const drop = screen.getByRole('radio', { name: 'Drop from above' });
    const preview = within(drop.closest('label')!).getByTestId('mic-symbol');
    expect(preview).not.toHaveAttribute('data-assembling');
    fireEvent.click(drop);
    expect(preview).toHaveAttribute('data-assembling', 'true');
  });

  it('is found by the Settings search in several languages', async () => {
    renderWithApp(<SettingsView onPair={() => {}} onConnectPhone={() => {}} />);
    const search = screen.getByRole('searchbox', { name: 'Search settings' });
    for (const word of ['animation', 'mic', '받아쓰기', '애니메이션', '마이크']) {
      await userEvent.clear(search);
      await userEvent.type(search, word);
      const results = screen.getByTestId('settings-search-results');
      expect(
        within(results).getByRole('button', { name: /Speech recognition/ }),
      ).toBeInTheDocument();
    }
  });
});

describe('Composer mic button animation', () => {
  it('plays the light assembly when recording starts and at the send', async () => {
    renderBoth();
    const mic = micButton();
    expect(micSymbol(mic)).not.toHaveAttribute('data-assembling');
    await act(async () => {
      fireEvent.click(mic);
    });
    expect(mic).toHaveAttribute('data-phase', 'recording');
    expect(micSymbol(mic)).toHaveAttribute('data-assembling', 'true');
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Cancel recording' }));
    });

    await waitFor(() => expect(micSymbol(mic)).not.toHaveAttribute('data-assembling'), {
      timeout: 3000,
    });
    await userEvent.type(screen.getByRole('textbox', { name: 'Command for payments-api' }), 'go');
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(mic).toHaveAttribute('data-sent', 'true'));
    expect(micSymbol(mic)).toHaveAttribute('data-assembling', 'true');
    expect(micSymbol(mic)).not.toHaveAttribute('data-sent');
  });

  it('stroke styles move while recording and keep the converge motion on send', async () => {
    renderBoth({ micAnimation: 'stroke-amplitude' });
    const mic = micButton();
    await act(async () => {
      fireEvent.click(mic);
    });
    expect(micSymbol(mic)).toHaveAttribute('data-phase', 'recording');
    expect(micSymbol(mic)).not.toHaveAttribute('data-assembling');
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Cancel recording' }));
    });
    await userEvent.type(screen.getByRole('textbox', { name: 'Command for payments-api' }), 'go');
    await userEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(micSymbol(mic)).toHaveAttribute('data-sent', 'true'));
    expect(micSymbol(mic)).not.toHaveAttribute('data-assembling');
  });

  it('shows the still symbol with reduced motion', async () => {
    mockReducedMotion(true);
    renderBoth();
    expect(screen.getByText(/Reduce motion is on/)).toBeInTheDocument();
    const picker = screen.getByTestId('mic-animation-picker');
    expect(
      within(picker)
        .getAllByTestId('mic-symbol')
        .filter((s) => s.getAttribute('data-phase') === 'recording'),
    ).toHaveLength(0);
    const mic = micButton();
    await act(async () => {
      fireEvent.click(mic);
    });
    expect(mic).toHaveAttribute('data-phase', 'recording');
    expect(micSymbol(mic)).toHaveAttribute('data-motion', 'reduced');
    expect(micSymbol(mic)).not.toHaveAttribute('data-assembling');
  });
});
