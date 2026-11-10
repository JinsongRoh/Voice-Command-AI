import { act, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { NativeProvider, type NativeServices } from '../native/context';
import {
  emptyForm,
  parseSpeakers,
  speakersToText,
  type EngineEntry,
  type EngineForm,
  type EngineServices,
  type EngineSetup as Setup,
  type EngineTestResult,
  type EngineTestTarget,
  type EngineType,
  type GatewayChange,
} from '../native/engines';
import type { HostServices } from '../native/host';
import { renderWithApp } from '../test/render';
import { ENGINE_PRESETS, presetForm } from './enginePresets';
import { EngineSetup, EngineSetupHint } from './EngineSetup';
import { SettingsView } from './SettingsView';

const KEY = 'sk-typed-once-7c1d';

function entry(profile: Partial<EngineForm> & { id: string }, extra: Partial<EngineEntry> = {}) {
  return {
    profile: { ...emptyForm(), ...profile },
    key: 'none',
    ready: true,
    default: false,
    read_only: false,
    ...extra,
  } satisfies EngineEntry;
}

function initial(): Setup {
  return {
    config_path: 'C:/Users/me/AppData/Roaming/vca/config.toml',
    stt: [
      entry(
        { id: 'whisper', label: 'Whisper', base_url: 'http://127.0.0.1:8000/v1', model: 'small' },
        { key: 'stored', default: true },
      ),
    ],
    llm: [],
    tts: [
      entry(
        {
          id: 'melo',
          base_url: 'http://127.0.0.1:8880/v1',
          speakers: [{ id: 'EN-US', label: 'American', gender: 'female' }],
        },
        { default: true },
      ),
    ],
    restart_required: false,
  };
}

/** A fake server that applies saves like vcad (and never echoes keys). */
function fakeServices(test?: EngineTestResult) {
  let setup = initial();
  const services = {
    get: vi.fn(async () => setup),
    save: vi.fn(
      async (
        kind: EngineType,
        originalId: string | null,
        profile: EngineForm,
        secret: string | null,
        makeDefault: boolean,
        gateway?: GatewayChange,
      ) => {
        const list = setup[kind].filter((e) => e.profile.id !== (originalId ?? profile.id));
        const old = setup[kind].find((e) => e.profile.id === originalId);
        const saved = entry(profile, {
          key: secret ? 'stored' : (old?.key ?? 'none'),
          gateway: gateway?.token ? 'stored' : gateway?.clear ? 'none' : (old?.gateway ?? 'none'),
          default: makeDefault,
        });
        setup = {
          ...setup,
          [kind]: [...list.map((e) => (makeDefault ? { ...e, default: false } : e)), saved],
          restart_required: true,
        };
        return setup;
      },
    ),
    remove: vi.fn(async (kind: EngineType, id: string) => {
      setup = { ...setup, [kind]: setup[kind].filter((e) => e.profile.id !== id) };
      return setup;
    }),
    setDefault: vi.fn(async (kind: EngineType, id: string | null) => {
      setup = {
        ...setup,
        [kind]: setup[kind].map((e) => ({ ...e, default: e.profile.id === id })),
      };
      return setup;
    }),
    test: vi.fn(
      async (
        _kind: EngineType,
        _target: EngineTestTarget,
        _secret: string | null,
        _gatewayToken?: string | null,
      ): Promise<EngineTestResult> =>
        test ?? { ok: true, latency_ms: 42, message: 'the LLM answered', chars: 2 },
    ),
  } satisfies EngineServices;
  return services;
}

async function renderSetup(services = fakeServices(), host?: HostServices) {
  renderWithApp(<EngineSetup services={services} host={host} />);
  await screen.findByTestId('engine-setup');
  return services;
}

describe('EngineSetup', () => {
  it('lists the engines with their key state, default and settings file', async () => {
    await renderSetup();
    const whisper = screen.getByTestId('engine-stt-whisper');
    expect(within(whisper).getByText('Whisper')).toBeInTheDocument();
    expect(within(whisper).getByText('default')).toBeInTheDocument();
    expect(within(whisper).getByText('Key stored on this PC')).toBeInTheDocument();
    expect(screen.getByTestId('engine-tts-melo')).toHaveTextContent('No key (not needed)');
    expect(screen.getByText('None yet.')).toBeInTheDocument();
    expect(screen.getByText(/config\.toml$/)).toBeInTheDocument();
    expect(screen.getByRole('link', { name: /Step-by-step guide/ })).toHaveAttribute(
      'href',
      expect.stringContaining('docs/speech-engines.md'),
    );
  });

  it('shows an engine kind it has no label for by its plain name, never a raw key', async () => {
    const services = fakeServices();
    const setup = initial();
    setup.stt.push(
      entry({ id: 'future', kind: 'future_cloud' as EngineForm['kind'], model: 'm1' }),
    );
    services.get.mockResolvedValue(setup);
    await renderSetup(services);
    const row = screen.getByTestId('engine-stt-future');
    expect(row).toHaveTextContent('future_cloud · m1');
    expect(row).not.toHaveTextContent('engines.kind');
    expect(screen.getByTestId('engine-stt-whisper')).toHaveTextContent('OpenAI-compatible server');
  });

  it('offers plain http only for a private address, and sends the choice', async () => {
    const services = await renderSetup();
    await userEvent.click(screen.getByRole('button', { name: 'Add: Transcript correction (LLM)' }));
    const form = screen.getByRole('form', { name: 'Add: Transcript correction (LLM)' });
    const box = () =>
      within(form).queryByRole('checkbox', { name: /Allow plain http on my private network/ });
    expect(box()).toBeNull();
    const url = within(form).getByLabelText('Base URL');
    await userEvent.clear(url);
    await userEvent.type(url, 'http://203.0.113.9:8000/v1');
    expect(box()).toBeNull();
    await userEvent.clear(url);
    await userEvent.type(url, 'http://192.168.0.10:8000/v1');
    await userEvent.click(box()!);
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(services.save).toHaveBeenCalled());
    expect(vi.mocked(services.save).mock.calls[0]![2]).toMatchObject({
      base_url: 'http://192.168.0.10:8000/v1',
      allow_plain_http: true,
    });
  });

  it('adds an engine from a preset; the key is sent once and never shown again', async () => {
    const services = await renderSetup();
    await userEvent.click(screen.getByRole('button', { name: 'Add: Transcript correction (LLM)' }));
    const form = screen.getByRole('form', { name: 'Add: Transcript correction (LLM)' });
    // The local preset fills in the fields.
    expect(within(form).getByLabelText('Base URL')).toHaveValue('http://127.0.0.1:11434/v1');
    expect(within(form).getByLabelText('Model')).toHaveValue('qwen2.5:1.5b');
    expect(within(form).getByLabelText('Name')).toHaveValue('local-llm');
    const keyInput = within(form).getByLabelText('API key');
    expect(keyInput).toHaveAttribute('type', 'password');
    expect(keyInput).toHaveValue('');
    await userEvent.type(keyInput, KEY);
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }));

    await waitFor(() => expect(services.save).toHaveBeenCalledTimes(1));
    const [kind, originalId, profile, secret, makeDefault] = services.save.mock.calls[0]!;
    expect([kind, originalId, secret, makeDefault]).toEqual(['llm', null, KEY, true]);
    expect(profile).toMatchObject({
      id: 'local-llm',
      kind: 'openai_compatible',
      base_url: 'http://127.0.0.1:11434/v1',
    });
    expect(JSON.stringify(profile)).not.toContain(KEY);

    // The editor is gone, and the key is nowhere on the page.
    expect(screen.queryByRole('form')).not.toBeInTheDocument();
    expect(screen.queryByDisplayValue(KEY)).not.toBeInTheDocument();
    expect(document.body.innerHTML).not.toContain(KEY);
    expect(screen.getByTestId('engine-llm-local-llm')).toHaveTextContent('Key stored on this PC');
    expect(screen.getByText(/Restart the VCA server on this PC/)).toBeInTheDocument();
  });

  it('edits an engine without re-displaying its stored key', async () => {
    const services = await renderSetup();
    await userEvent.click(screen.getByRole('button', { name: 'Edit Whisper' }));
    const form = screen.getByRole('form', { name: 'Edit whisper' });
    const keyInput = within(form).getByLabelText('API key');
    expect(keyInput).toHaveValue('');
    expect(keyInput).toHaveAttribute('placeholder', 'Stored (hidden)');
    expect(within(form).getByText(/A key is stored/)).toBeInTheDocument();
    // No preset picker when editing.
    expect(within(form).queryByLabelText('Start from')).not.toBeInTheDocument();
    const model = within(form).getByLabelText('Model');
    await userEvent.clear(model);
    await userEvent.type(model, 'large-v3');
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(services.save).toHaveBeenCalled());
    const [kind, originalId, profile, secret, makeDefault] = services.save.mock.calls[0]!;
    expect([kind, originalId, secret, makeDefault]).toEqual(['stt', 'whisper', null, true]);
    expect(profile.model).toBe('large-v3');
    // The stored key stays stored.
    expect(screen.getByTestId('engine-stt-whisper')).toHaveTextContent('Key stored on this PC');
  });

  it('sets a key header and a gateway token under advanced options, never shown again', async () => {
    const services = await renderSetup();
    await userEvent.click(screen.getByRole('button', { name: 'Edit Whisper' }));
    const form = screen.getByRole('form', { name: 'Edit whisper' });
    await userEvent.click(within(form).getByText('Advanced'));
    await userEvent.type(within(form).getByLabelText('Key header (gateway)'), 'X-VCA-Key');
    const token = within(form).getByLabelText('Gateway token');
    expect(token).toHaveAttribute('type', 'password');
    await userEvent.type(token, 'gw-typed-once-4b2');
    expect(within(form).queryByLabelText('Remove the stored gateway token')).toBeNull();
    await userEvent.click(within(form).getByRole('button', { name: 'Test' }));
    await waitFor(() => expect(services.test).toHaveBeenCalled());
    expect(services.test.mock.calls[0]![3]).toBe('gw-typed-once-4b2');
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(services.save).toHaveBeenCalled());
    const [, , profile, secret, , gateway] = services.save.mock.calls[0]!;
    expect(profile.api_key_header).toBe('X-VCA-Key');
    expect(secret).toBeNull();
    expect(gateway).toEqual({ token: 'gw-typed-once-4b2', clear: false });
    const row = screen.getByTestId('engine-stt-whisper');
    expect(row).toHaveTextContent('Gateway token stored on this PC');
    expect(document.body).not.toHaveTextContent('gw-typed-once-4b2');

    // Editing again: the token is not filled in, and can be removed.
    await userEvent.click(screen.getByRole('button', { name: 'Edit Whisper' }));
    const again = screen.getByRole('form', { name: 'Edit whisper' });
    await userEvent.click(within(again).getByText('Advanced'));
    expect(within(again).getByLabelText('Gateway token')).toHaveValue('');
    await userEvent.click(within(again).getByLabelText('Remove the stored gateway token'));
    await userEvent.click(within(again).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(services.save).toHaveBeenCalledTimes(2));
    expect(services.save.mock.calls[1]![5]).toEqual({ token: null, clear: true });
    expect(screen.getByTestId('engine-stt-whisper')).not.toHaveTextContent('Gateway token');
  });

  it('offers no gateway fields for APIs that do not take them', async () => {
    await renderSetup();
    await userEvent.click(
      screen.getByRole('button', { name: 'Add: Speech recognition (speech-to-text)' }),
    );
    const form = screen.getByRole('form', { name: 'Add: Speech recognition (speech-to-text)' });
    await userEvent.click(within(form).getByText('Advanced'));
    expect(within(form).getByLabelText('Gateway token')).toBeInTheDocument();
    await userEvent.selectOptions(within(form).getByLabelText('Start from'), 'google');
    expect(within(form).queryByLabelText('Gateway token')).toBeNull();
    expect(within(form).queryByLabelText('Key header (gateway)')).toBeNull();
  });

  it('tests a saved engine and a draft, and shows the outcome', async () => {
    const services = await renderSetup();
    await userEvent.click(screen.getByRole('button', { name: 'Test Whisper' }));
    expect(services.test).toHaveBeenCalledWith('stt', { profileId: 'whisper' }, null);
    expect(await screen.findByText('Works (42 ms)')).toBeInTheDocument();

    services.test.mockResolvedValueOnce({
      ok: false,
      latency_ms: 3,
      code: 'unavailable',
      message: 'http error: connection refused',
    });
    await userEvent.click(
      screen.getByRole('button', { name: 'Add: Spoken alerts (text-to-speech)' }),
    );
    const form = screen.getByRole('form', { name: 'Add: Spoken alerts (text-to-speech)' });
    await userEvent.type(within(form).getByLabelText('API key'), KEY);
    await userEvent.click(within(form).getByRole('button', { name: 'Test' }));
    await waitFor(() => expect(services.test).toHaveBeenCalledTimes(2));
    const [kind, target, secret] = services.test.mock.calls[1]!;
    expect(kind).toBe('tts');
    expect(target).toMatchObject({
      profile: { id: 'local-tts', base_url: 'http://127.0.0.1:8880/v1' },
    });
    expect(secret).toBe(KEY);
    expect(
      await within(form).findByText('Failed: http error: connection refused'),
    ).toBeInTheDocument();
    expect(services.save).not.toHaveBeenCalled();
  });

  it('removes an engine after confirming, with its key', async () => {
    const services = await renderSetup();
    await userEvent.click(screen.getByRole('button', { name: 'Remove melo' }));
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveTextContent('Remove melo?');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Remove' }));
    await waitFor(() => expect(services.remove).toHaveBeenCalledWith('tts', 'melo', true));
    expect(screen.queryByTestId('engine-tts-melo')).not.toBeInTheDocument();
  });

  it('switches presets and sets defaults', async () => {
    const services = await renderSetup();
    await userEvent.click(
      screen.getByRole('button', { name: 'Add: Speech recognition (speech-to-text)' }),
    );
    const form = screen.getByRole('form', { name: 'Add: Speech recognition (speech-to-text)' });
    await userEvent.selectOptions(within(form).getByLabelText('Start from'), 'openai');
    expect(within(form).getByLabelText('API')).toHaveValue('openai');
    expect(within(form).getByLabelText('Base URL')).toHaveValue('https://api.openai.com/v1');
    await userEvent.selectOptions(within(form).getByLabelText('Start from'), 'google');
    expect(within(form).getByLabelText('Google Cloud project id')).toHaveValue('');
    await userEvent.click(within(form).getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('form')).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole('button', { name: 'Remove Whisper' }));
    await userEvent.click(
      within(await screen.findByRole('dialog')).getByRole('button', { name: 'Remove' }),
    );
    await waitFor(() => expect(services.remove).toHaveBeenCalled());
    expect(screen.queryByRole('button', { name: 'Make default melo' })).not.toBeInTheDocument();
  });

  it('refuses an invalid name before sending anything', async () => {
    const services = await renderSetup();
    await userEvent.click(
      screen.getByRole('button', { name: 'Add: Spoken alerts (text-to-speech)' }),
    );
    const form = screen.getByRole('form', { name: 'Add: Spoken alerts (text-to-speech)' });
    const name = within(form).getByLabelText('Name');
    await userEvent.clear(name);
    await userEvent.type(name, 'My Voice');
    await userEvent.click(within(form).getByRole('button', { name: 'Save' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('a-z, 0-9, - and _');
    expect(services.save).not.toHaveBeenCalled();
  });

  it('shows server errors and offers a restart of the server it runs', async () => {
    const services = fakeServices();
    services.save.mockRejectedValueOnce({
      code: 'invalid_request',
      message: 'the base URL must start with http:// or https://',
    });
    const host = {
      stop: vi.fn(async () => ({}) as never),
      start: vi.fn(async () => ({}) as never),
    } as unknown as HostServices;
    await renderSetup(services, host);
    await userEvent.click(screen.getByRole('button', { name: 'Edit Whisper' }));
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('must start with http://');
    // The second save works and asks for a restart.
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Restart the server' }));
    const dialog = await screen.findByRole('dialog');
    await act(async () => {
      await userEvent.click(within(dialog).getByRole('button', { name: 'Restart the server' }));
    });
    await waitFor(() => expect(host.start).toHaveBeenCalled());
    expect(host.stop).toHaveBeenCalled();
    expect(
      await screen.findByText('The server restarted with the new engines.'),
    ).toBeInTheDocument();
  });

  it('reports a server that cannot be reached', async () => {
    const services = fakeServices();
    services.get.mockRejectedValueOnce({
      code: 'not_running',
      message: 'the VCA server on this PC is not running',
    });
    renderWithApp(<EngineSetup services={services} />);
    expect(await screen.findByRole('alert')).toHaveTextContent('not running');
    await userEvent.click(screen.getByRole('button', { name: 'Try again' }));
    expect(await screen.findByTestId('engine-setup')).toBeInTheDocument();
  });
});

describe('engine presets', () => {
  it('fill in fields only and never contain keys', () => {
    for (const kind of ['stt', 'llm', 'tts'] as const) {
      expect(ENGINE_PRESETS[kind][0]?.id).toBe('local');
      for (const preset of ENGINE_PRESETS[kind]) {
        const form = preset.form as unknown as Record<string, unknown>;
        expect(Object.keys(form).sort()).toEqual(Object.keys(emptyForm()).sort());
        expect(JSON.stringify(form)).not.toMatch(/sk-|secret|api_key"\s*:\s*"/i);
        expect(form.credential).toBeNull();
        expect(form.api_key_env).toBeNull();
        const base = form.base_url as string | null;
        if (base) expect(base).toMatch(/^https?:\/\//);
      }
    }
    // Local presets point at this PC.
    expect(presetForm('stt', 'local')?.base_url).toMatch(/^http:\/\/127\.0\.0\.1:/);
    expect(presetForm('llm', 'local')?.base_url).toMatch(/^http:\/\/127\.0\.0\.1:/);
    expect(presetForm('tts', 'local')?.base_url).toMatch(/^http:\/\/127\.0\.0\.1:/);
    // A copy: editing it never changes the preset.
    const copy = presetForm('tts', 'openai')!;
    copy.voice = 'changed';
    expect(presetForm('tts', 'openai')?.voice).toBe('alloy');
    expect(presetForm('tts', 'nope')).toBeNull();
  });

  it('round-trips speakers as text', () => {
    const speakers = parseSpeakers('EN-US | American | female\n\nKR|Korean|MALE\nx');
    expect(speakers).toEqual([
      { id: 'EN-US', label: 'American', gender: 'female' },
      { id: 'KR', label: 'Korean', gender: 'male' },
      { id: 'x', label: 'x', gender: 'other' },
    ]);
    expect(parseSpeakers(speakersToText(speakers))).toEqual(speakers);
  });
});

describe('phones and other machines', () => {
  it('show a read-only hint instead of the setup', async () => {
    renderWithApp(<EngineSetupHint />);
    expect(screen.getByTestId('engine-setup-hint')).toHaveTextContent(
      'Speech engines are set up on the PC',
    );
  });

  it('settings on a paired machine never offer engine setup, even on desktop', async () => {
    const services = fakeServices();
    const native = {
      pair: vi.fn(),
      forget: vi.fn(),
      platform: async () => 'windows',
      host: { engines: services } as unknown as HostServices,
    } as unknown as NativeServices;
    renderWithApp(
      <NativeProvider value={native}>
        <SettingsView onPair={() => {}} />
      </NativeProvider>,
    );
    await userEvent.click(screen.getByRole('tab', { name: 'Voice' }));
    expect(await screen.findByTestId('engine-setup-hint')).toBeInTheDocument();
    expect(screen.queryByTestId('engine-setup')).not.toBeInTheDocument();
    expect(
      screen.queryByRole('heading', { name: 'Speech engines on this PC' }),
    ).not.toBeInTheDocument();
    expect(services.get).not.toHaveBeenCalled();
  });

  it('settings without the desktop shell (phone, browser) show the hint', async () => {
    renderWithApp(<SettingsView onPair={() => {}} />);
    await userEvent.click(screen.getByRole('tab', { name: 'Voice' }));
    expect(await screen.findByTestId('engine-setup-hint')).toBeInTheDocument();
    expect(screen.queryByTestId('engine-setup')).not.toBeInTheDocument();
  });
});
