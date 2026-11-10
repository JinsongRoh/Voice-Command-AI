import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { Pronunciation } from '../protocol/pronunciations';
import type { ClientMsg, ServerMsg } from '../protocol/types';
import {
  getPronunciations,
  heardAsSpoken,
  latinPart,
  setPronunciations,
} from '../speech/pronunciation';
import { I18nProvider } from '../i18n/context';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { PronunciationDialog, PronunciationSettings } from './PronunciationSettings';
import { SessionCard } from './SessionCard';

class FakeUtterance {
  voice: SpeechSynthesisVoice | null = null;
  lang = '';
  rate = 1;
  constructor(public text: string) {}
}

const entry = (
  term: string,
  spoken: string,
  source: Pronunciation['source'] = 'user',
): Pronunciation => ({ term, spoken, language: 'ko', source, updated_at: 1 });

/** A PC with the dictionary: list, set and delete like the server. */
function dictionaryPc(initial: Pronunciation[], options: { features?: string[] } = {}) {
  let entries = [...initial];
  const transport = new FakeTransport((msg: ClientMsg): ServerMsg => {
    switch (msg.type) {
      case 'hello':
        return {
          type: 'welcome',
          protocol_version: 1,
          server_version: 'test',
          server_name: 'test',
          features: options.features ?? ['pronunciations'],
        };
      case 'list_pronunciations':
        return { type: 'pronunciations', entries };
      case 'set_pronunciation':
        if (msg.spoken === 'forbidden')
          return { type: 'error', code: 'forbidden', message: 'needs the command scope' };
        entries = [
          ...entries.filter((e) => e.term.toLowerCase() !== msg.term.toLowerCase()),
          entry(msg.term, msg.spoken),
        ];
        return { type: 'pronunciations', entries };
      case 'delete_pronunciation':
        entries = entries.filter((e) => e.term !== msg.term);
        return { type: 'pronunciations', entries };
      default:
        return defaultResponse(msg);
    }
  });
  return transport;
}

describe('PronunciationSettings', () => {
  const synth = {
    speak: vi.fn(),
    cancel: vi.fn(),
    getVoices: vi.fn(() => []),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  };

  beforeEach(() => {
    vi.clearAllMocks();
    vi.stubGlobal('SpeechSynthesisUtterance', FakeUtterance);
    Object.defineProperty(window, 'speechSynthesis', { value: synth, configurable: true });
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    Reflect.deleteProperty(window, 'speechSynthesis');
    setPronunciations([]);
  });

  const spokenTexts = () =>
    synth.speak.mock.calls.map((c) => (c[0] as unknown as FakeUtterance).text);

  async function open(initial: Pronunciation[]) {
    const transport = dictionaryPc(initial);
    const view = renderWithApp(<PronunciationSettings />, {
      transport,
      settings: { language: 'ko' },
    });
    await userEvent.click(screen.getByText('Pronunciation dictionary'));
    return { ...view, transport };
  }

  it('lists, searches, previews, edits and deletes entries', async () => {
    const user = userEvent.setup();
    const { transport } = await open([
      entry('SkyHarbor', '스카이하버', 'auto'),
      entry('Codex', '코덱스', 'default'),
    ]);
    expect(await screen.findByText('스카이하버')).toBeInTheDocument();
    expect(screen.getByText('Suggested')).toBeInTheDocument();
    expect(screen.getByText('Built-in')).toBeInTheDocument();

    // Search by term, by spoken form, or ignoring spaces and hyphens.
    const search = screen.getByRole('searchbox', { name: 'Search' });
    await user.type(search, 'sky harbor');
    expect(screen.queryByText('코덱스')).toBeNull();
    expect(screen.getByText('스카이하버')).toBeInTheDocument();
    await user.clear(search);
    await user.type(search, '코덱');
    expect(screen.queryByText('스카이하버')).toBeNull();
    await user.clear(search);
    await user.type(search, 'nothing');
    expect(screen.getByText('Nothing matches.')).toBeInTheDocument();
    await user.clear(search);

    // Preview speaks the spoken form with the alert voice (here the device voice).
    await user.click(screen.getByRole('button', { name: 'Preview SkyHarbor' }));
    expect(spokenTexts()).toContain('스카이하버');

    // Edit: the term stays, the spoken form changes.
    await user.click(screen.getByRole('button', { name: 'Edit SkyHarbor' }));
    const term = screen.getByLabelText('Written as');
    expect(term).toHaveValue('SkyHarbor');
    expect(term).toHaveAttribute('readonly');
    const spoken = screen.getByLabelText('Say it as');
    await user.clear(spoken);
    await user.type(spoken, '스카이 하버');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    expect(transport.bodies('set_pronunciation')).toEqual([
      { type: 'set_pronunciation', term: 'SkyHarbor', spoken: '스카이 하버', language: 'ko' },
    ]);
    expect(await screen.findByText('스카이 하버')).toBeInTheDocument();
    // Every spoken text uses the new entry at once.
    expect(getPronunciations().find((e) => e.term === 'SkyHarbor')?.spoken).toBe('스카이 하버');

    // Delete.
    await user.click(screen.getByRole('button', { name: 'Delete Codex' }));
    expect(transport.bodies('delete_pronunciation')).toEqual([
      { type: 'delete_pronunciation', term: 'Codex', language: 'ko' },
    ]);
    await waitFor(() => expect(screen.queryByText('코덱스')).toBeNull());
  });

  it('adds an entry by typing and refuses non-Latin terms', async () => {
    const user = userEvent.setup();
    const { transport } = await open([]);
    expect(await screen.findByText('No entries yet.')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Add' }));
    const save = screen.getByRole('button', { name: 'Save' });
    await user.type(screen.getByLabelText('Written as'), '젠토');
    await user.type(screen.getByLabelText('Say it as'), '젠토');
    expect(screen.getByText('Write the name in Latin letters and digits.')).toBeInTheDocument();
    expect(save).toBeDisabled();
    await user.clear(screen.getByLabelText('Written as'));
    await user.type(screen.getByLabelText('Written as'), 'Zento Labs');
    await user.click(screen.getByRole('button', { name: 'Preview' }));
    expect(spokenTexts()).toContain('젠토');
    await user.click(save);
    expect(transport.bodies('set_pronunciation')).toEqual([
      { type: 'set_pronunciation', term: 'Zento Labs', spoken: '젠토', language: 'ko' },
    ]);
    expect(await screen.findByText('Zento Labs')).toBeInTheDocument();
  });

  it('shows a refusal (a view-only device) without closing the form', async () => {
    const user = userEvent.setup();
    await open([]);
    await user.click(await screen.findByRole('button', { name: 'Add' }));
    await user.type(screen.getByLabelText('Written as'), 'Zento');
    await user.type(screen.getByLabelText('Say it as'), 'forbidden');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('needs the command scope');
    expect(screen.getByLabelText('Written as')).toHaveValue('Zento');
  });

  it('teaches by voice: the transcript is shown for review, then saved', async () => {
    const user = userEvent.setup();
    const transcribe = vi.fn(async () => ({
      // The vocabulary turned the Hangul back into Latin; the raw text is used.
      text: 'Zento.',
      rawText: '젠토.',
      corrections: [],
    }));
    const recorder = {
      start: vi.fn(async () => {}),
      stop: vi.fn(async () => new Blob()),
      cancel: vi.fn(),
    };
    const transport = dictionaryPc([]);
    renderWithApp(<PronunciationSettings />, {
      transport,
      settings: { language: 'ko' },
      speech: { transcribe, createRecorder: () => recorder },
    });
    await user.click(screen.getByText('Pronunciation dictionary'));
    await user.click(await screen.findByRole('button', { name: 'Add' }));
    await user.type(screen.getByLabelText('Written as'), 'Zento');
    await user.click(screen.getByRole('button', { name: 'Teach by voice' }));
    expect(recorder.start).toHaveBeenCalled();
    expect(screen.getByRole('status')).toHaveTextContent('Listening');
    await user.click(screen.getByRole('button', { name: 'Done speaking' }));
    expect(recorder.stop).toHaveBeenCalled();
    expect(transcribe).toHaveBeenCalledWith(
      expect.any(Blob),
      expect.objectContaining({ language: 'ko', sessionId: null, final: true }),
    );
    expect(screen.getByLabelText('Say it as')).toHaveValue('젠토');
    expect(screen.getByRole('status')).toHaveTextContent('Heard "젠토"');
    // Nothing is saved until the user confirms.
    expect(transport.bodies('set_pronunciation')).toEqual([]);
    await user.click(screen.getByRole('button', { name: 'Save' }));
    expect(transport.bodies('set_pronunciation')).toEqual([
      { type: 'set_pronunciation', term: 'Zento', spoken: '젠토', language: 'ko' },
    ]);
  });

  it('says so when the PC has no dictionary', async () => {
    const transport = dictionaryPc([], { features: [] });
    renderWithApp(<PronunciationSettings />, { transport, settings: { language: 'ko' } });
    await userEvent.click(screen.getByText('Pronunciation dictionary'));
    expect(await screen.findByText(/has no pronunciation dictionary yet/)).toBeInTheDocument();
    expect(transport.bodies('list_pronunciations')).toEqual([]);
  });

  it('follows pushes from the PC', async () => {
    const { transport } = await open([entry('SkyHarbor', '스카이하버')]);
    expect(await screen.findByText('스카이하버')).toBeInTheDocument();
    act(() =>
      transport.push({ type: 'pronunciations', entries: [entry('Zento', '젠토', 'auto')] }),
    );
    expect(await screen.findByText('젠토')).toBeInTheDocument();
    expect(screen.queryByText('스카이하버')).toBeNull();
  });
});

describe('PronunciationDialog (session card)', () => {
  afterEach(() => setPronunciations([]));

  it('starts from the Latin words of the session name', async () => {
    const user = userEvent.setup();
    const transport = dictionaryPc([]);
    const onClose = vi.fn();
    renderWithApp(<PronunciationDialog name="SkyHarbor Zento 정리" onClose={onClose} />, {
      transport,
      settings: { language: 'ko' },
    });
    const dialog = await screen.findByRole('dialog', { name: 'Fix pronunciation' });
    expect(within(dialog).getByLabelText('Written as')).toHaveValue('SkyHarbor Zento');
    expect(within(dialog).getByText(/Said now as: SkyHarbor Zento 정리/)).toBeInTheDocument();
    await user.type(within(dialog).getByLabelText('Say it as'), '스카이하버 젠토');
    await user.click(within(dialog).getByRole('button', { name: 'Save' }));
    expect(transport.bodies('set_pronunciation')).toEqual([
      {
        type: 'set_pronunciation',
        term: 'SkyHarbor Zento',
        spoken: '스카이하버 젠토',
        language: 'ko',
      },
    ]);
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });
});

describe('session card menu', () => {
  it('offers "Fix pronunciation" only when asked to', async () => {
    const user = userEvent.setup();
    const onFix = vi.fn();
    const session = localSession({ display_name: 'SkyHarbor 정리' });
    const { rerender } = render(
      <I18nProvider locale="en">
        <ul>
          <SessionCard
            session={session}
            preview=""
            selected={false}
            index={1}
            onSelect={() => {}}
            onRename={() => {}}
          />
        </ul>
      </I18nProvider>,
    );
    await user.click(screen.getByRole('button', { name: /More actions/ }));
    expect(screen.queryByRole('menuitem', { name: 'Fix pronunciation' })).toBeNull();
    rerender(
      <I18nProvider locale="en">
        <ul>
          <SessionCard
            session={session}
            preview=""
            selected={false}
            index={1}
            onSelect={() => {}}
            onRename={() => {}}
            onFixPronunciation={onFix}
          />
        </ul>
      </I18nProvider>,
    );
    await user.click(screen.getByRole('menuitem', { name: 'Fix pronunciation' }));
    expect(onFix).toHaveBeenCalledWith(session.id);
  });
});

describe('helpers', () => {
  it('picks the Latin words of a name', () => {
    expect(latinPart('SkyHarbor Zento 정리')).toBe('SkyHarbor Zento');
    expect(latinPart('(QRT) 점검, v2')).toBe('QRT v2');
    expect(latinPart('세션 3')).toBe('');
  });

  it('turns a transcript into a spoken form', () => {
    expect(heardAsSpoken('스카이하버.')).toBe('스카이하버');
    expect(heardAsSpoken(' 젠토 ,  하버! ')).toBe('젠토 하버');
    expect(heardAsSpoken('Zento', '젠토')).toBe('젠토');
    expect(heardAsSpoken('젠토', 'Zento')).toBe('젠토');
  });
});
