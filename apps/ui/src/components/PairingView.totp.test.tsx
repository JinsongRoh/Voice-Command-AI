import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { I18nProvider } from '../i18n/context';
import { translate } from '../i18n/i18n';
import { NativeError } from '../native/bridge';
import { NativeProvider, type NativeServices } from '../native/context';
import { describePairingError, isTotpPrompt } from '../native/errors';
import { classifyLink, linkRequiresTotp } from '../native/pairLinks';
import type { QrScanner } from '../native/qrScanner';
import { PairingView } from './PairingView';

vi.mock('@tauri-apps/plugin-deep-link', () => ({
  getCurrent: async () => null,
  onOpenUrl: async () => () => {},
}));
vi.mock('@tauri-apps/plugin-barcode-scanner', () => ({ Format: { QRCode: 'QR_CODE' } }));

const PAIR = 'vca://pair?v=1&k=abc&s=secret';
const PAIR_TOTP = `${PAIR}&tf=1`;
const CODE_LABEL = 'VCA 2-step verification code (6 digits)';
const PROMPT = /2-step verification is on in VCA on your PC.*“Voice Command AI”/;

const MACHINE = {
  id: 'm1',
  name: 'Studio',
  endpoints: [],
  deviceId: 'd',
  scope: 'command' as const,
  pairedAt: 0,
};

function fixture() {
  const pair = vi.fn<NativeServices['pair']>();
  const native: NativeServices = {
    pair,
    pairingCode: async () => 'ABCD-EFGH-JKMN-PQRS',
    forget: async () => {},
    platform: async () => 'android',
  };
  return { native, pair };
}

function renderView(native: NativeServices, onPaired = vi.fn()) {
  render(
    <I18nProvider locale="en">
      <NativeProvider value={native}>
        <PairingView onPaired={onPaired} />
      </NativeProvider>
    </I18nProvider>,
  );
  return onPaired;
}

/** Pastes a link: typing ~100 characters can outlast the test timeout under load. */
async function paste(user: ReturnType<typeof userEvent.setup>, field: HTMLElement, text: string) {
  await user.click(field);
  await user.paste(text);
  // The confirm code of a pairing link is checked in every test but its own.
  await act(async () => {});
  const box = screen.queryByRole('checkbox');
  if (box) await user.click(box);
}

const t = (key: Parameters<typeof translate>[1], vars?: Record<string, string | number>) =>
  translate('en', key, vars);

describe('pairing error messages', () => {
  it('map server codes to plain words and keep the raw code', () => {
    const used = describePairingError(
      t,
      new NativeError('invalid_pairing', 'authentication failed: invalid pairing secret'),
    );
    expect(used.message).toBe(
      'This pairing link was already used or has expired. Create a new pairing QR code in the PC app and scan it again.',
    );
    expect(used.message).not.toMatch(/authentication failed/);
    expect(used.code).toBe('invalid_pairing');

    expect(describePairingError(t, new NativeError('totp_required', 'x')).message).toMatch(PROMPT);
    expect(describePairingError(t, new NativeError('invalid_totp', 'x')).message).toMatch(
      /not accepted.*“Voice Command AI”/,
    );
    expect(
      describePairingError(t, new NativeError('pairing_attempts_exhausted', 'x')).message,
    ).toMatch(/Too many wrong codes, so this pairing link was revoked/);
    expect(describePairingError(t, new NativeError('rate_limited', 'x')).message).toMatch(
      /Too many failed attempts/,
    );
    // Unknown codes still say what happened.
    const other = describePairingError(t, new NativeError('weird', 'boom'));
    expect(other.message).toBe('Something went wrong: boom (weird)');
    expect(other.code).toBe('weird');
  });

  it('are worded in every locale', () => {
    for (const locale of ['en', 'ko', 'ja', 'es'] as const) {
      for (const key of [
        'native.error.invalid_pairing',
        'native.error.invalid_totp',
        'native.error.pairing_attempts_exhausted',
        'native.error.rate_limited',
        'pairing.totpPrompt',
      ] as const) {
        expect(translate(locale, key)).not.toBe(key);
      }
    }
    expect(translate('ko', 'pairing.totpPrompt')).toBe(
      'PC의 VCA에 2단계 인증이 켜져 있습니다. 인증 앱(Google Authenticator 등)에서 ‘Voice Command AI’ 항목의 6자리 코드를 입력하세요.',
    );
    expect(translate('ko', 'native.error.invalid_pairing')).toBe(
      '이 페어링 링크는 이미 사용됐거나 만료됐습니다. PC 앱에서 새 페어링 QR을 만들어 다시 찍으세요.',
    );
  });

  it('know which codes ask for a 2-step verification code', () => {
    expect(isTotpPrompt('totp_required')).toBe(true);
    expect(isTotpPrompt('invalid_totp')).toBe(true);
    expect(isTotpPrompt('invalid_pairing')).toBe(false);
    expect(isTotpPrompt(null)).toBe(false);
  });
});

describe('the tf=1 hint in pairing links', () => {
  it('is read from vca://pair links only and is not an invite', () => {
    expect(linkRequiresTotp(PAIR_TOTP)).toBe(true);
    expect(linkRequiresTotp(PAIR)).toBe(false);
    expect(linkRequiresTotp(`${PAIR}&tf=0`)).toBe(false);
    expect(linkRequiresTotp('https://example.com/?tf=1')).toBe(false);
    expect(classifyLink(PAIR_TOTP)).toBe('pair');
  });
});

describe('pairing with 2-step verification', () => {
  it('shows the steps, and the code step only when the PC asks for a code', async () => {
    const user = userEvent.setup();
    const { native } = fixture();
    renderView(native);
    const steps = screen.getByRole('list', { name: 'Pairing steps' });
    // Scan, then connect: nothing else is known yet.
    expect(steps.querySelectorAll(':scope > li')).toHaveLength(2);
    expect(screen.getByTestId('step-scan')).toHaveAttribute('data-state', 'active');
    expect(screen.getByTestId('step-scan')).toHaveTextContent('Connect a phone');
    expect(screen.getByTestId('step-connect')).toHaveAttribute('data-state', 'pending');
    expect(screen.queryByLabelText(CODE_LABEL)).toBeNull();

    await paste(user, screen.getByLabelText('Pairing link'), PAIR_TOTP);
    expect(steps.querySelectorAll(':scope > li')).toHaveLength(3);
    const field = screen.getByLabelText(CODE_LABEL);
    expect(field).toHaveAttribute('inputmode', 'numeric');
    expect(field).toBeRequired();
    const help = document.getElementById('pair-totp-help');
    expect(help).toHaveTextContent('“Voice Command AI” entry in your authenticator app');
    expect(help).toHaveTextContent('It is not a DNSPilot code.');
    expect(screen.getByTestId('step-scan')).toHaveAttribute('data-state', 'done');
    expect(screen.getByTestId('step-code')).toHaveAttribute('data-state', 'active');
  });

  it('keeps the form on totp_required, focuses the code and sends it at the sixth digit', async () => {
    const user = userEvent.setup();
    const { native, pair } = fixture();
    pair
      .mockRejectedValueOnce(new NativeError('totp_required', 'authentication failed'))
      .mockResolvedValue(MACHINE);
    const onPaired = renderView(native);
    await paste(user, screen.getByLabelText('Pairing link'), PAIR);
    await user.click(screen.getByRole('button', { name: 'Pair' }));

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(PROMPT);
    expect(alert).toHaveTextContent('Error code: totp_required');
    expect(alert).not.toHaveTextContent('authentication failed');
    expect(within(screen.getByTestId('step-code')).getByRole('alert')).toBe(alert);
    const field = screen.getByLabelText(CODE_LABEL);
    await waitFor(() => expect(field).toHaveFocus());
    expect(field).toBeRequired();
    expect(screen.getByLabelText('Pairing link')).toHaveValue(PAIR);

    await user.type(field, '123456');
    // A pasted link never goes by itself: it may be someone else's.
    expect(pair).toHaveBeenCalledOnce();
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    await waitFor(() => expect(onPaired).toHaveBeenCalledWith('m1'));
    expect(pair).toHaveBeenCalledTimes(2);
    expect(pair.mock.calls[1]?.[0]).toBe(PAIR);
    expect(pair.mock.calls[1]?.[2]).toBe('123456');
  });

  it('asks again after a wrong code with the same link', async () => {
    const user = userEvent.setup();
    const { native, pair } = fixture();
    pair.mockRejectedValueOnce(new NativeError('invalid_totp', 'x')).mockResolvedValue(MACHINE);
    const onPaired = renderView(native);
    await paste(user, screen.getByLabelText('Pairing link'), PAIR_TOTP);
    await user.type(screen.getByLabelText(CODE_LABEL), '111111');
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(/That code was not accepted/);
    const field = screen.getByLabelText(CODE_LABEL);
    expect(field).toHaveValue('');
    await user.type(field, '222 222');
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    await waitFor(() => expect(onPaired).toHaveBeenCalledWith('m1'));
    expect(pair.mock.calls.map((c) => c[2])).toEqual(['111111', '222222']);
  });

  it('marks the code as required when the link says so, without calling the PC', async () => {
    const user = userEvent.setup();
    const { native, pair } = fixture();
    renderView(native);
    await paste(user, screen.getByLabelText('Pairing link'), PAIR_TOTP);
    const field = screen.getByLabelText(CODE_LABEL);
    expect(field).toBeRequired();
    expect(document.getElementById('pair-totp-help')).toHaveTextContent(
      'This PC has 2-step verification on: the code is required.',
    );
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(PROMPT);
    expect(pair).not.toHaveBeenCalled();
    await waitFor(() => expect(field).toHaveFocus());
  });

  it('a scanned link that needs a code focuses the code; confirm, six digits, paired', async () => {
    const user = userEvent.setup();
    const { native, pair } = fixture();
    const scanner: QrScanner = {
      scan: async () => ({ kind: 'link', uri: PAIR_TOTP }),
      cancel: async () => {},
      openSettings: async () => {},
    };
    native.qrScanner = scanner;
    pair.mockResolvedValue(MACHINE);
    const onPaired = renderView(native);
    await user.click(await screen.findByRole('button', { name: /Scan QR code/ }));
    const field = await screen.findByLabelText(CODE_LABEL);
    await waitFor(() => expect(field).toHaveFocus());
    // Nothing is wrong: no alert, the confirm code is the step-1 action.
    expect(screen.queryByRole('alert')).toBeNull();
    const box = await screen.findByRole('checkbox', { name: 'My PC shows the same code' });
    expect(screen.getByTestId('step-scan')).toContainElement(box);
    expect(screen.getByTestId('step-scan')).toHaveTextContent('ABCD-EFGH-JKMN-PQRS');
    await user.click(box);
    expect(field).toHaveFocus();
    expect(pair).not.toHaveBeenCalled();
    await user.keyboard('654321');
    await waitFor(() => expect(onPaired).toHaveBeenCalledWith('m1'));
    expect(pair).toHaveBeenCalledExactlyOnceWith(PAIR_TOTP, 'My Android', '654321');
    for (const id of ['step-scan', 'step-code', 'step-connect']) {
      expect(screen.getByTestId(id)).toHaveAttribute('data-state', 'done');
    }
    expect(screen.getByRole('status')).toHaveTextContent('Paired with Studio.');
  });

  it('a scanned link without 2-step verification pairs as soon as the code is confirmed', async () => {
    const user = userEvent.setup();
    const { native, pair } = fixture();
    native.qrScanner = {
      scan: async () => ({ kind: 'link', uri: PAIR }),
      cancel: async () => {},
      openSettings: async () => {},
    };
    pair.mockResolvedValue(MACHINE);
    const onPaired = renderView(native);
    await user.click(await screen.findByRole('button', { name: /Scan QR code/ }));
    const box = await screen.findByRole('checkbox', { name: 'My PC shows the same code' });
    expect(screen.queryByLabelText(CODE_LABEL)).toBeNull();
    expect(pair).not.toHaveBeenCalled();
    await user.click(box);
    await waitFor(() => expect(onPaired).toHaveBeenCalledWith('m1'));
    expect(pair).toHaveBeenCalledExactlyOnceWith(PAIR, 'My Android', null);
  });

  it('a pasted link never pairs by itself, even when everything is checked', async () => {
    const user = userEvent.setup();
    const { native, pair } = fixture();
    pair.mockResolvedValue(MACHINE);
    renderView(native);
    await paste(user, screen.getByLabelText('Pairing link'), PAIR_TOTP);
    await user.type(screen.getByLabelText(CODE_LABEL), '123456');
    expect(pair).not.toHaveBeenCalled();
    expect(screen.getByTestId('step-connect')).toHaveAttribute('data-state', 'active');
    expect(screen.getByTestId('step-connect')).toHaveTextContent('Tap Pair to connect');
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    await waitFor(() => expect(pair).toHaveBeenCalledOnce());
  });

  it('puts each failure on its own step', async () => {
    const user = userEvent.setup();
    const cases: [NativeError, string][] = [
      [new NativeError('unreachable', 'down'), 'step-connect'],
      [new NativeError('invalid_pairing', 'used'), 'step-scan'],
      [new NativeError('pairing_attempts_exhausted', 'x'), 'step-scan'],
    ];
    for (const [error, step] of cases) {
      const { native, pair } = fixture();
      pair.mockRejectedValue(error);
      const view = render(
        <I18nProvider locale="en">
          <NativeProvider value={native}>
            <PairingView />
          </NativeProvider>
        </I18nProvider>,
      );
      await paste(user, screen.getByLabelText('Pairing link'), PAIR);
      await user.click(screen.getByRole('button', { name: 'Pair' }));
      const alert = await screen.findByRole('alert');
      expect(screen.getByTestId(step)).toContainElement(alert);
      expect(screen.getByTestId(step)).toHaveAttribute('data-state', 'error');
      // Only one error, and no success next to it.
      expect(screen.getAllByRole('alert')).toHaveLength(1);
      expect(screen.queryByText(/Paired with/)).toBeNull();
      view.unmount();
    }
  });

  it('the scanner overlay takes the focus, keeps it and cancels on Escape', async () => {
    const user = userEvent.setup();
    const { native } = fixture();
    let finish: (r: Awaited<ReturnType<QrScanner['scan']>>) => void = () => {};
    const cancel = vi.fn(async () => finish({ kind: 'cancelled' }));
    native.qrScanner = {
      scan: () => new Promise((resolve) => (finish = resolve)),
      cancel,
      openSettings: async () => {},
    };
    renderView(native);
    const scan = await screen.findByRole('button', { name: /Scan QR code/ });
    await user.click(scan);
    const dialog = await screen.findByRole('dialog');
    const close = screen.getByRole('button', { name: 'Cancel' });
    expect(dialog).toContainElement(close);
    expect(close).toHaveFocus();
    await user.keyboard('{Tab}');
    expect(close).toHaveFocus();
    await user.keyboard('{Escape}');
    expect(cancel).toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(scan).toHaveFocus();
  });

  describe('the confirm code', () => {
    const bare = async (
      user: ReturnType<typeof userEvent.setup>,
      field: HTMLElement,
      l: string,
    ) => {
      await user.click(field);
      await user.paste(l);
    };

    it('must be checked before anything goes to the PC', async () => {
      const user = userEvent.setup();
      const { native, pair } = fixture();
      pair.mockResolvedValue(MACHINE);
      const onPaired = renderView(native);
      await bare(user, screen.getByLabelText('Pairing link'), PAIR);
      expect(await screen.findByTestId('confirm-code')).toHaveTextContent('ABCD-EFGH-JKMN-PQRS');
      await user.click(screen.getByRole('button', { name: 'Pair' }));
      expect(await screen.findByRole('alert')).toHaveTextContent(/Check the confirm code first/);
      expect(pair).not.toHaveBeenCalled();
      await user.click(screen.getByRole('checkbox', { name: 'My PC shows the same code' }));
      await user.click(screen.getByRole('button', { name: 'Pair' }));
      await waitFor(() => expect(onPaired).toHaveBeenCalledWith('m1'));
    });

    it('fails closed while there is no code to compare', async () => {
      const user = userEvent.setup();
      const { native, pair } = fixture();
      native.pairingCode = () => new Promise<string>(() => {});
      renderView(native);
      await bare(user, screen.getByLabelText('Pairing link'), PAIR);
      await user.click(screen.getByRole('button', { name: 'Pair' }));
      expect(await screen.findByRole('alert')).toHaveTextContent(/Check the confirm code first/);
      expect(pair).not.toHaveBeenCalled();
    });

    it('is asked again for another link', async () => {
      const user = userEvent.setup();
      const { native } = fixture();
      renderView(native);
      const field = screen.getByLabelText('Pairing link');
      await bare(user, field, PAIR);
      await user.click(await screen.findByRole('checkbox'));
      await user.clear(field);
      await bare(user, field, `${PAIR}x`);
      expect(await screen.findByRole('checkbox')).not.toBeChecked();
    });

    it('holds the sixth digit of a scanned link until it is checked', async () => {
      const user = userEvent.setup();
      const { native, pair } = fixture();
      pair.mockResolvedValue(MACHINE);
      native.qrScanner = {
        scan: async () => ({ kind: 'link', uri: PAIR_TOTP }),
        cancel: async () => {},
        openSettings: async () => {},
      };
      const onPaired = renderView(native);
      await user.click(await screen.findByRole('button', { name: /Scan QR code/ }));
      await screen.findByTestId('confirm-code');
      const field = screen.getByLabelText(CODE_LABEL);
      await user.type(field, '654321');
      expect(pair).not.toHaveBeenCalled();
      // The hint goes to step 1, where the check is.
      const hint = await screen.findByRole('alert');
      expect(hint).toHaveTextContent(/Check the confirm code first/);
      expect(screen.getByTestId('step-scan')).toContainElement(hint);
      // Confirming is the go-ahead: the code typed already is sent.
      await user.click(screen.getByRole('checkbox'));
      await waitFor(() => expect(onPaired).toHaveBeenCalledWith('m1'));
      expect(pair).toHaveBeenCalledExactlyOnceWith(PAIR_TOTP, 'My Android', '654321');
    });

    it('unchecking stops the auto pairing', async () => {
      const user = userEvent.setup();
      const { native, pair } = fixture();
      native.qrScanner = {
        scan: async () => ({ kind: 'link', uri: PAIR_TOTP }),
        cancel: async () => {},
        openSettings: async () => {},
      };
      renderView(native);
      await user.click(await screen.findByRole('button', { name: /Scan QR code/ }));
      const box = await screen.findByRole('checkbox');
      await user.click(box);
      await user.click(box);
      await user.type(screen.getByLabelText(CODE_LABEL), '654321');
      expect(pair).not.toHaveBeenCalled();
    });
  });

  it('says a used link needs a new QR code, with the raw code small', async () => {
    const user = userEvent.setup();
    const { native, pair } = fixture();
    pair.mockRejectedValue(
      new NativeError('invalid_pairing', 'authentication failed: invalid pairing secret'),
    );
    renderView(native);
    await paste(user, screen.getByLabelText('Pairing link'), PAIR);
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(/already used or has expired.*new pairing QR code/);
    expect(alert.querySelector('small')).toHaveTextContent('Error code: invalid_pairing');
    // A new QR code is the fix: the error sits on the scan step.
    expect(screen.getByTestId('step-scan')).toContainElement(alert);
    expect(screen.queryByLabelText(CODE_LABEL)).toBeNull();
  });
});
