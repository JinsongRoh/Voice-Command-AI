import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { describe, expect, it, vi } from 'vitest';

import { ErrorBoundary } from './ErrorBoundary';

function Boom({ fail }: { fail: boolean }) {
  if (fail) throw new Error('kaput');
  return <p>fine</p>;
}

function Harness() {
  const [fail, setFail] = useState(true);
  const [key, setKey] = useState(0);
  return (
    <>
      <button onClick={() => setFail(false)}>fix</button>
      <button onClick={() => setKey((k) => k + 1)}>switch</button>
      <ErrorBoundary
        resetKey={key}
        fallback={(err, retry) => (
          <div role="alert">
            {err.message}
            <button onClick={retry}>retry</button>
          </div>
        )}
      >
        <Boom fail={fail} />
      </ErrorBoundary>
      <p>still here</p>
    </>
  );
}

describe('ErrorBoundary', () => {
  it('replaces only the crashed subtree and recovers on retry or key change', async () => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    const user = userEvent.setup();
    render(<Harness />);
    expect(screen.getByRole('alert')).toHaveTextContent('kaput');
    expect(screen.getByText('still here')).toBeInTheDocument();
    await user.click(screen.getByText('switch'));
    expect(screen.getByRole('alert')).toBeInTheDocument();
    await user.click(screen.getByText('fix'));
    await user.click(screen.getByText('retry'));
    expect(screen.getByText('fine')).toBeInTheDocument();
  });
});
