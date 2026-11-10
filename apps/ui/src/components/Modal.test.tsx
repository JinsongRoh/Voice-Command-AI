import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { I18nProvider } from '../i18n/context';
import { Modal } from './Modal';

function renderModal(props: { dismissOnBackdrop?: boolean } = {}) {
  const onClose = vi.fn();
  render(
    <I18nProvider locale="en">
      <Modal title="Rename" onClose={onClose} {...props}>
        <input aria-label="Name" />
      </Modal>
    </I18nProvider>,
  );
  const backdrop = screen.getByRole('dialog').parentElement!;
  return { onClose, backdrop };
}

describe('Modal', () => {
  it('closes on a backdrop press by default', () => {
    const { onClose, backdrop } = renderModal();
    fireEvent.mouseDown(backdrop);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('keeps a form open on a backdrop press, but Escape and the close button still close it', () => {
    const { onClose, backdrop } = renderModal({ dismissOnBackdrop: false });
    fireEvent.mouseDown(backdrop);
    // A press inside the dialog never closes it either way.
    fireEvent.mouseDown(screen.getByRole('dialog'));
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(onClose).toHaveBeenCalledTimes(2);
  });
});
