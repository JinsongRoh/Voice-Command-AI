import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { localSession } from '../test/fixtures';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { renderWithApp } from '../test/render';
import { DecisionSheet } from './DecisionSheet';
import { onSettingsViewRequest, peekSettingsTarget } from './settingsNav';

const decision = {
  id: 'dec-1',
  prompt: 'Allow Bash command?\nrm -rf dist',
  options: [
    { id: 'yes', label: 'Yes' },
    { id: 'no', label: 'No' },
  ],
};

describe('DecisionSheet', () => {
  it('shows the prompt and answers with Input::Choose', async () => {
    const { transport } = renderWithApp(
      <DecisionSheet
        session={localSession({ state: { type: 'awaiting_decision' } })}
        decision={decision}
      />,
    );
    const region = screen.getByRole('region', { name: 'Pending decision for payments-api' });
    expect(region).toHaveTextContent('Allow Bash command?');
    expect(region).toHaveTextContent('rm -rf dist');
    await userEvent.click(screen.getByRole('button', { name: 'No' }));
    await waitFor(() =>
      expect(transport.bodies('send_input')).toEqual([
        {
          type: 'send_input',
          session_id: 's1',
          input: { type: 'choose', decision_id: 'dec-1', option_id: 'no' },
        },
      ]),
    );
  });

  it('shows server errors', async () => {
    const transport = new FakeTransport((msg) =>
      msg.type === 'send_input'
        ? { type: 'error', code: 'not_found', message: 'no such pending decision' }
        : defaultResponse(msg),
    );
    renderWithApp(<DecisionSheet session={localSession()} decision={decision} />, { transport });
    await userEvent.click(screen.getByRole('button', { name: 'Yes' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('no such pending decision');
  });

  it('says an inferred decision seems to need confirmation and offers no answers', () => {
    renderWithApp(
      <DecisionSheet
        session={localSession({ state: { type: 'awaiting_decision' } })}
        decision={{
          id: 'claude:abc:inferred-toolu_1',
          prompt: 'Allow Bash: rm -rf dist?',
          options: [],
          inferred: true,
        }}
      />,
    );
    const region = screen.getByRole('region', { name: 'Pending decision for payments-api' });
    expect(region).toHaveTextContent('Seems to need confirmation');
    expect(region).toHaveTextContent('Allow Bash: rm -rf dist?');
    expect(screen.getByTestId('decision-inferred-hint')).toHaveTextContent(
      'Choose in the PC window or answer with the key buttons.',
    );
    expect(screen.queryByRole('button', { name: 'Yes' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Run in shell' })).toBeNull();
  });

  it('links an inferred decision to the agent hooks settings', async () => {
    const opened = vi.fn();
    const stop = onSettingsViewRequest(opened);
    renderWithApp(
      <DecisionSheet
        session={localSession({ state: { type: 'awaiting_decision' } })}
        decision={{ id: 'inf', prompt: 'Allow Bash: ls?', options: [], inferred: true }}
      />,
    );
    await userEvent.click(screen.getByRole('button', { name: 'Set up exact answer buttons' }));
    expect(opened).toHaveBeenCalled();
    expect(peekSettingsTarget()).toBe('set-agent-hooks');
    stop();
  });

  it('collapsed, says how many answers are left out and shows them on request', async () => {
    renderWithApp(
      <DecisionSheet
        session={localSession({ state: { type: 'awaiting_decision' } })}
        decision={{
          id: 'many',
          prompt: 'Edit src/app.ts?',
          options: [
            { id: 'yes', label: 'Yes' },
            { id: 'session', label: 'Yes, allow all edits this session' },
            { id: 'tell', label: 'Tell the agent something else' },
            { id: 'no', label: 'No' },
          ],
        }}
      />,
    );
    await userEvent.click(screen.getByRole('button', { name: 'Collapse the decision card' }));
    expect(screen.queryByRole('button', { name: 'Yes, allow all edits this session' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: '+2 more' }));
    expect(
      screen.getByRole('button', { name: 'Yes, allow all edits this session' }),
    ).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /more$/ })).toBeNull();
  });

  it('shows a waiting state before the decision details arrive', () => {
    renderWithApp(<DecisionSheet session={localSession()} decision={undefined} />);
    expect(screen.getByText('Waiting for the decision details…')).toBeInTheDocument();
  });
});
