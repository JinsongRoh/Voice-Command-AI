import { screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { renderWithApp } from '../test/render';
import { NewSessionDialog } from './NewSessionDialog';

describe('new session account profile', () => {
  it('hides the account profile picker on servers that cannot use it', async () => {
    renderWithApp(
      <NewSessionDialog
        projectDirs={['/home/dev/payments-api']}
        onClose={() => {}}
        onLaunched={() => {}}
      />,
      { transport: new FakeTransport(defaultResponse) },
    );
    expect(await screen.findByRole('dialog')).toBeInTheDocument();
    // A chosen profile would make the launch fail ("account profiles are not available yet").
    expect(screen.queryByLabelText(/profile/i)).toBeNull();
  });
});
