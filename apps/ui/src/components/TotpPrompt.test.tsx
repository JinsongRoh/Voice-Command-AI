import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { I18nProvider } from '../i18n/context';
import type { Locale } from '../i18n/i18n';
import { RequestError } from '../state/core';
import { TotpPrompt, type TotpPromptProps } from './TotpPrompt';

function renderPrompt(over: Partial<TotpPromptProps> = {}, locale: Locale = 'ko') {
  const props: TotpPromptProps = {
    title: '원격 셸 열기',
    description: 'PC의 /work에서 원격 셸을 엽니다.',
    submitLabel: '셸 열기',
    onSubmit: vi.fn(async () => {}),
    onCancel: vi.fn(),
    onFail: vi.fn(),
    ...over,
  };
  render(
    <I18nProvider locale={locale}>
      <TotpPrompt {...props} />
    </I18nProvider>,
  );
  return { props, dialog: screen.getByRole('dialog', { name: props.title }) };
}

describe('TotpPrompt', () => {
  it('names the authenticator entry, the backup code option and how to set it up again', () => {
    const { dialog } = renderPrompt();
    expect(dialog).toHaveTextContent(
      "PC에서 설정한 인증 앱(Google Authenticator, Microsoft Authenticator, iPhone 암호 앱 등)의 'Voice Command AI' 항목에 표시되는 6자리 숫자를 입력하세요.",
    );
    expect(within(dialog).getByRole('button', { name: '백업 코드 사용' })).toBeInTheDocument();
    expect(dialog).toHaveTextContent(
      '인증 앱을 쓸 수 없으면 백업 코드를 같은 칸에 입력해도 됩니다.',
    );
    expect(within(dialog).getByText('인증 앱이 없나요?').tagName).toBe('SUMMARY');
    expect(dialog).toHaveTextContent(
      'PC의 VCA 앱 › 설정 › 보안 › 보안 상태 › 2단계 인증에서 다시 설정할 수 있습니다.',
    );
    const input = within(dialog).getByLabelText('인증 코드');
    expect(input).toHaveAttribute('inputmode', 'numeric');
    expect(input).toHaveAttribute('autocomplete', 'one-time-code');
  });

  it('takes a backup code in the same field and sends it normalized', async () => {
    const { dialog, props } = renderPrompt();
    await userEvent.click(within(dialog).getByRole('button', { name: '백업 코드 사용' }));
    expect(dialog).toHaveTextContent(
      'PC에서 2단계 인증을 설정할 때 저장한 백업 코드(XXXX-XXXX-XXXX-XXXX) 중 하나를 입력하세요. 각 코드는 한 번만 쓸 수 있습니다.',
    );
    const input = within(dialog).getByLabelText('백업 코드');
    expect(input).toHaveAttribute('inputmode', 'text');
    const submit = within(dialog).getByRole('button', { name: '셸 열기' });
    await userEvent.type(input, 'abcd-efgh');
    expect(submit).toBeDisabled();
    await userEvent.type(input, '-ijkl-mnop');
    expect(input).toHaveValue('ABCD-EFGH-IJKL-MNOP');
    await userEvent.click(submit);
    await waitFor(() => expect(props.onSubmit).toHaveBeenCalledWith('ABCDEFGHIJKLMNOP'));
    // Back to the authenticator code.
    await userEvent.click(within(dialog).getByRole('button', { name: '인증 앱 코드 사용' }));
    expect(within(dialog).getByLabelText('인증 코드')).toHaveValue('');
  });

  it('keeps the prompt open for a wrong backup code', async () => {
    const onSubmit = vi.fn(async () => {
      throw new RequestError('totp_invalid', 'the code is not valid');
    });
    const { dialog, props } = renderPrompt({ onSubmit, submitLabel: 'Open shell' }, 'en');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Use a backup code' }));
    await userEvent.type(within(dialog).getByLabelText('Backup code'), 'AAAABBBBCCCCDDDD');
    await userEvent.click(within(dialog).getByRole('button', { name: 'Open shell' }));
    expect(await within(dialog).findByRole('alert')).toHaveTextContent(/wrong or was already used/);
    expect(props.onFail).not.toHaveBeenCalled();
  });

  it('explains a missing enrollment instead of asking for a code', () => {
    const { dialog } = renderPrompt({ notEnrolled: true });
    expect(dialog).toHaveTextContent(
      '이 PC에는 아직 2단계 인증(인증 앱)이 설정되지 않아 입력할 코드가 없습니다. 설정하기 전에는 원격 셸을 열 수 없습니다.',
    );
    expect(dialog).toHaveTextContent(
      'PC의 VCA 앱 › 설정 › 보안 › 보안 상태 › 2단계 인증에서 설정하세요.',
    );
    expect(within(dialog).queryByRole('textbox')).not.toBeInTheDocument();
    expect(within(dialog).queryByRole('button', { name: '셸 열기' })).not.toBeInTheDocument();
  });

  it('switches to the explanation when the server says nothing is enrolled', async () => {
    const onSubmit = vi.fn(async () => {
      throw new RequestError('unsupported', 'TOTP is not set up on this computer');
    });
    const { dialog, props } = renderPrompt({ onSubmit });
    await userEvent.type(within(dialog).getByLabelText('인증 코드'), '123456');
    await userEvent.click(within(dialog).getByRole('button', { name: '셸 열기' }));
    expect(await within(dialog).findByRole('alert')).toHaveTextContent(
      '2단계 인증(인증 앱)이 설정되지 않아',
    );
    expect(props.onFail).not.toHaveBeenCalled();
  });
});
