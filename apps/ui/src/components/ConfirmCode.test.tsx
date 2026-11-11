import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { ConfirmCode } from './ConfirmCode';

describe('ConfirmCode', () => {
  it('shows the 19-character code whole, breaking only after its dashes', () => {
    render(<ConfirmCode code="ABCD-EFGH-JKMN-PQRS" />);
    const code = screen.getByTestId('confirm-code');
    expect(code.textContent).toBe('ABCD-EFGH-JKMN-PQRS');
    expect(code.textContent).toHaveLength(19);
    // Four unbreakable groups, a break opportunity after each dash.
    expect([...code.querySelectorAll('span')].map((s) => s.textContent)).toEqual([
      'ABCD',
      'EFGH',
      'JKMN',
      'PQRS',
    ]);
    expect(code.querySelectorAll('wbr')).toHaveLength(3);
  });

  it('still shows the shorter code of an older server', () => {
    render(<ConfirmCode code="ABCD-EFGH" />);
    expect(screen.getByTestId('confirm-code').textContent).toBe('ABCD-EFGH');
  });
});
