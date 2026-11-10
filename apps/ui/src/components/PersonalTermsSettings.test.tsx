import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { MAX_PERSONAL_TERMS, type PersonalTerm } from '../protocol/personalTerms';
import type { ClientMsg, PairedDevice, ServerMsg } from '../protocol/types';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { renderWithApp } from '../test/render';
import { PersonalTermsSettings } from './PersonalTermsSettings';

/** A PC with the list: answers and keeps saves like the server. */
function wordPc(
  initial: PersonalTerm[],
  reply?: (terms: PersonalTerm[]) => ServerMsg,
  devices: PairedDevice[] | null = null,
) {
  const saves: PersonalTerm[][] = [];
  let terms = initial;
  const transport = new FakeTransport((msg: ClientMsg): ServerMsg => {
    switch (msg.type) {
      case 'hello':
        return {
          type: 'welcome',
          protocol_version: 1,
          server_version: 'test',
          server_name: 'test',
          features: ['personal_terms'],
        };
      case 'list_devices':
        return devices ? { type: 'devices', devices } : defaultResponse(msg);
      case 'list_personal_terms':
        return { type: 'personal_terms', terms };
      case 'save_personal_terms': {
        saves.push(msg.terms);
        const answer = reply?.(msg.terms);
        if (answer) return answer;
        terms = msg.terms;
        return { type: 'personal_terms', terms };
      }
      default:
        return defaultResponse(msg);
    }
  });
  return { transport, saves };
}

const sky = { term: 'SkyHarbor', variants: ['sky harbour', '스카이 하버'] };

describe('PersonalTermsSettings', () => {
  it('lists each word with what it gets written as', async () => {
    renderWithApp(<PersonalTermsSettings />, { transport: wordPc([sky]).transport });
    expect(await screen.findByText('SkyHarbor')).toBeInTheDocument();
    expect(screen.getByText('sky harbour, 스카이 하버')).toBeInTheDocument();
  });

  it('says so when the list is empty', async () => {
    renderWithApp(<PersonalTermsSettings />, { transport: wordPc([]).transport });
    expect(await screen.findByText(/No words yet/)).toBeInTheDocument();
  });

  it('adds a word with forms split on commas and new lines', async () => {
    const { transport, saves } = wordPc([sky]);
    renderWithApp(<PersonalTermsSettings />, { transport });
    await screen.findByText('SkyHarbor');
    await userEvent.click(screen.getByRole('button', { name: 'Add word' }));
    await userEvent.type(screen.getByLabelText('Correct word'), ' Zentorix ');
    await userEvent.type(screen.getByLabelText('Written by mistake as'), 'zen torix, 젠 토릭스,, ');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(saves).toHaveLength(1));
    expect(saves[0]).toEqual([sky, { term: 'Zentorix', variants: ['zen torix', '젠 토릭스'] }]);
    expect(await screen.findByText('Zentorix')).toBeInTheDocument();
    expect(screen.queryByLabelText('Correct word')).toBeNull();
  });

  it('allows a word without forms', async () => {
    const { transport, saves } = wordPc([]);
    renderWithApp(<PersonalTermsSettings />, { transport });
    await screen.findByText(/No words yet/);
    await userEvent.click(screen.getByRole('button', { name: 'Add word' }));
    await userEvent.type(screen.getByLabelText('Correct word'), 'Zentorix');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(saves[0]).toEqual([{ term: 'Zentorix', variants: [] }]));
  });

  it('edits a word in place', async () => {
    const { transport, saves } = wordPc([sky]);
    renderWithApp(<PersonalTermsSettings />, { transport });
    await screen.findByText('SkyHarbor');
    await userEvent.click(screen.getByRole('button', { name: 'Edit SkyHarbor' }));
    const forms = screen.getByLabelText('Written by mistake as');
    expect(forms).toHaveValue('sky harbour, 스카이 하버');
    await userEvent.clear(forms);
    await userEvent.type(forms, 'sky arbor');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(saves[0]).toEqual([{ term: 'SkyHarbor', variants: ['sky arbor'] }]));
  });

  it('deletes a word', async () => {
    const { transport, saves } = wordPc([sky, { term: 'Zentorix', variants: [] }]);
    renderWithApp(<PersonalTermsSettings />, { transport });
    await screen.findByText('SkyHarbor');
    await userEvent.click(screen.getByRole('button', { name: 'Delete SkyHarbor' }));
    await waitFor(() => expect(saves[0]).toEqual([{ term: 'Zentorix', variants: [] }]));
    await waitFor(() => expect(screen.queryByText('SkyHarbor')).toBeNull());
  });

  it('explains a duplicate before asking the server', async () => {
    const { transport, saves } = wordPc([sky]);
    renderWithApp(<PersonalTermsSettings />, { transport });
    await screen.findByText('SkyHarbor');
    await userEvent.click(screen.getByRole('button', { name: 'Add word' }));
    await userEvent.type(screen.getByLabelText('Correct word'), 'skyharbor');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('already in the list');

    await userEvent.clear(screen.getByLabelText('Correct word'));
    await userEvent.type(screen.getByLabelText('Correct word'), 'Zentorix');
    await userEvent.type(screen.getByLabelText('Written by mistake as'), 'Sky Harbour');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('already used');
    expect(saves).toEqual([]);
  });

  it('refuses one-letter forms and the word itself', async () => {
    const { transport, saves } = wordPc([]);
    renderWithApp(<PersonalTermsSettings />, { transport });
    await screen.findByText(/No words yet/);
    await userEvent.click(screen.getByRole('button', { name: 'Add word' }));
    await userEvent.type(screen.getByLabelText('Correct word'), 'Zentorix');
    const forms = screen.getByLabelText('Written by mistake as');
    await userEvent.type(forms, 'z');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('too short');
    await userEvent.clear(forms);
    await userEvent.type(forms, 'Zentorix');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('same as the correct word');
    expect(saves).toEqual([]);
  });

  it('keeps the form open and shows the reason when saving fails', async () => {
    const { transport } = wordPc([], () => ({
      type: 'error',
      code: 'invalid_request',
      message: 'a word has control characters',
    }));
    renderWithApp(<PersonalTermsSettings />, { transport });
    await screen.findByText(/No words yet/);
    await userEvent.click(screen.getByRole('button', { name: 'Add word' }));
    await userEvent.type(screen.getByLabelText('Correct word'), 'Zentorix');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('control characters');
    expect(screen.getByLabelText('Correct word')).toHaveValue('Zentorix');
  });

  it('closes the add button when the list is full', async () => {
    const full = Array.from({ length: MAX_PERSONAL_TERMS }, (_, i) => ({
      term: `w${i}`,
      variants: [],
    }));
    renderWithApp(<PersonalTermsSettings />, { transport: wordPc(full).transport });
    await screen.findByText('w0');
    expect(screen.getByRole('button', { name: 'Add word' })).toBeDisabled();
    expect(screen.getByText(/at most 500 words/)).toBeInTheDocument();
  });

  it('is read-only without the admin scope', async () => {
    const { transport } = wordPc([sky], undefined, [
      {
        id: 'd1',
        name: 'phone',
        platform: 'ios',
        scope: 'command',
        current: true,
        last_seen: 1,
        paired_at: 1,
      },
    ]);
    renderWithApp(<PersonalTermsSettings />, { transport });
    expect(await screen.findByText(/admin access/)).toBeInTheDocument();
    const row = (await screen.findByText('SkyHarbor')).closest('li')!;
    expect(within(row).queryByRole('button')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Add word' })).toBeNull();
  });
});
