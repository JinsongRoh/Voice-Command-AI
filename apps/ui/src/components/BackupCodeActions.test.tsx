import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { I18nProvider } from '../i18n/context';
import { resetSaveTarget } from '../native/saveFile';
import { tauri } from '../test/tauriMock';
import { BackupCodeActions } from './SecurityTotpSetup';

vi.mock('@tauri-apps/api/core', () => import('../test/tauriMock'));

function renderActions(locale: 'en' | 'ko' = 'en') {
  return render(
    <I18nProvider locale={locale}>
      <BackupCodeActions codes={['AAAA-BBBB', 'CCCC-DDDD']} />
    </I18nProvider>,
  );
}

describe('BackupCodeActions "Save as file"', () => {
  beforeEach(() => {
    tauri.reset();
    resetSaveTarget();
    tauri.handlers.set('vca_platform', () => 'windows');
  });

  it('saves through the desktop dialog and says where', async () => {
    tauri.handlers.set('vca_save_text_file', () => ({
      status: 'saved',
      fileName: 'my-codes.txt',
    }));
    renderActions();
    await userEvent.click(screen.getByRole('button', { name: 'Save as file' }));
    expect(await screen.findByRole('status')).toHaveTextContent('Saved to my-codes.txt.');
    expect(tauri.callsOf('vca_save_text_file')[0]?.args).toEqual({
      suggestedName: 'vca-backup-codes.txt',
      text: 'AAAA-BBBB\nCCCC-DDDD\n',
    });
  });

  it('says when the dialog was cancelled', async () => {
    tauri.handlers.set('vca_save_text_file', () => ({ status: 'cancelled' }));
    renderActions();
    await userEvent.click(screen.getByRole('button', { name: 'Save as file' }));
    expect(await screen.findByRole('status')).toHaveTextContent('Cancelled.');
  });

  it('shows the error when the file cannot be written', async () => {
    tauri.handlers.set('vca_save_text_file', () => {
      throw { code: 'save_failed', message: 'could not write the file: access denied' };
    });
    renderActions('ko');
    await userEvent.click(screen.getByRole('button', { name: '파일로 저장' }));
    expect(await screen.findByRole('status')).toHaveTextContent(
      '파일을 저장하지 못했습니다: could not write the file: access denied',
    );
  });
});
