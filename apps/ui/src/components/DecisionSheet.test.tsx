import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { localSession } from '../test/fixtures';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { renderWithApp } from '../test/render';
import { DecisionSheet } from './DecisionSheet';

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

  it('shows a waiting state before the decision details arrive', () => {
    renderWithApp(<DecisionSheet session={localSession()} decision={undefined} />);
    expect(screen.getByText('Waiting for the decision details…')).toBeInTheDocument();
  });
});
