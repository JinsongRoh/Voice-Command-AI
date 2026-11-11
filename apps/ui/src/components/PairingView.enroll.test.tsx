import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { I18nProvider } from '../i18n/context';
import { NativeError, type DnsPilotEnrollment, type DnsPilotService } from '../native/bridge';
import { NativeProvider, type NativeServices } from '../native/context';
import type { QrScanResult, QrScanner } from '../native/qrScanner';
import { PairingView } from './PairingView';

vi.mock('@tauri-apps/plugin-deep-link', () => ({
  getCurrent: async () => null,
  onOpenUrl: async () => () => {},
}));
vi.mock('@tauri-apps/plugin-barcode-scanner', () => ({ Format: { QRCode: 'QR_CODE' } }));

const CODE = `dpt_inv_${'a'.repeat(43)}`;
const PAIR = 'vca://pair?v=1&k=abc&s=secret';
const PAIR_INVITE = `${PAIR}&h=vca.example.com&t=12&c=${CODE}`;
const CONSOLE = `https://dns.dnspilot.kr/enroll#v=1&h=vca.example.com&t=12&c=${CODE}`;
const UNREACHABLE_TEXT = /could not be reached/;
const WAIT_TEXT = /Connecting… up to \d+ s/;
const NEXT_SCAN = 'Now scan the pairing QR code on your PC';
const CODE_LABEL = 'VCA 2-step verification code (6 digits)';

const enrollment = (readyWithinSec: number): DnsPilotEnrollment => ({
  hostname: 'vca.example.com',
  tunnelId: 12,
  clientId: 41,
  readyWithinSec,
});

const MACHINE = {
  id: 'm1',
  name: 'Studio',
  endpoints: [],
  deviceId: 'd',
  scope: 'command' as const,
  pairedAt: 0,
};

function fixture(ready = 1, platform = 'ios') {
  const pair = vi.fn<NativeServices['pair']>();
  const enroll = vi.fn(async (): Promise<DnsPilotEnrollment | null> => enrollment(ready));
  const native: NativeServices = {
    pair,
    pairingCode: async () => 'ABCD-EFGH',
    forget: async () => {},
    platform: async () => platform,
    dnspilot: { enroll } as unknown as DnsPilotService,
  };
  return { native, pair, enroll };
}

function renderView(native: NativeServices, onPaired = vi.fn()) {
  render(
    <I18nProvider locale="en">
      <NativeProvider value={native}>
        <PairingView enrollRetryMs={40} onPaired={onPaired} />
      </NativeProvider>
    </I18nProvider>,
  );
  return onPaired;
}

function scannerFor(result: QrScanResult): QrScanner {
  return { scan: async () => result, cancel: async () => {}, openSettings: async () => {} };
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

const unreachable = () => new NativeError('unreachable', 'server unreachable');

describe('pairing with a DNSPilot invite', () => {
  it('enrolls, keeps retrying behind the wait message, then pairs', async () => {
    const user = userEvent.setup();
    const { native, pair, enroll } = fixture();
    pair
      .mockRejectedValueOnce(unreachable())
      .mockRejectedValueOnce(unreachable())
      .mockResolvedValue(MACHINE);
    const onPaired = renderView(native);
    await paste(user, screen.getByLabelText('Pairing link'), PAIR_INVITE);
    await user.click(screen.getByRole('button', { name: 'Pair' }));

    expect(await screen.findByText(WAIT_TEXT)).toBeInTheDocument();
    expect(screen.queryByText(UNREACHABLE_TEXT)).not.toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    await waitFor(() => expect(onPaired).toHaveBeenCalledWith('m1'));
    expect(enroll).toHaveBeenCalledExactlyOnceWith(PAIR_INVITE);
    expect(pair).toHaveBeenCalledTimes(3);
    expect(screen.getByRole('status')).toHaveTextContent('Studio');
    expect(screen.queryByText(WAIT_TEXT)).not.toBeInTheDocument();
    expect(screen.getByTestId('step-remote')).toHaveAttribute('data-state', 'done');
    expect(screen.getByTestId('step-connect')).toHaveAttribute('data-state', 'done');
  });

  it('shows the real error only after the window ends', async () => {
    const user = userEvent.setup();
    const { native, pair } = fixture();
    pair.mockRejectedValue(unreachable());
    renderView(native);
    await paste(user, screen.getByLabelText('Pairing link'), PAIR_INVITE);
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    await screen.findByText(WAIT_TEXT);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();

    const alert = await screen.findByRole('alert', {}, { timeout: 3000 });
    expect(alert).toHaveTextContent(UNREACHABLE_TEXT);
    expect(screen.getByTestId('step-connect')).toContainElement(alert);
    // The registration itself worked: it stays done.
    expect(screen.getByTestId('step-remote')).toHaveAttribute('data-state', 'done');
    expect(pair.mock.calls.length).toBeGreaterThan(1);
    expect(screen.queryByText(WAIT_TEXT)).not.toBeInTheDocument();
  });

  it('connects at once, with no wait message, when the key was registered', async () => {
    const user = userEvent.setup();
    const { native, pair } = fixture(0);
    pair.mockRejectedValue(unreachable());
    renderView(native);
    await paste(user, screen.getByLabelText('Pairing link'), PAIR_INVITE);
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(pair).toHaveBeenCalledOnce();
    expect(screen.queryByText(WAIT_TEXT)).not.toBeInTheDocument();
  });

  it('enrolls a link once, even when pairing needs another try', async () => {
    const user = userEvent.setup();
    const { native, pair, enroll } = fixture(0);
    pair
      .mockRejectedValueOnce(new NativeError('totp_required', 'code needed'))
      .mockResolvedValue(MACHINE);
    renderView(native);
    await paste(user, screen.getByLabelText('Pairing link'), PAIR_INVITE);
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    await screen.findByRole('alert');
    await user.type(screen.getByLabelText(/2-step verification code/), '123456');
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    await waitFor(() => expect(pair).toHaveBeenCalledTimes(2));
    expect(enroll).toHaveBeenCalledOnce();
  });

  it('a scan registers at once but pairs only after the confirm code check', async () => {
    const user = userEvent.setup();
    const { native, pair, enroll } = fixture(30);
    native.qrScanner = scannerFor({ kind: 'link', uri: PAIR_INVITE });
    const onPaired = renderView(native);
    await user.click(await screen.findByRole('button', { name: /Scan QR code/ }));
    await waitFor(() => expect(enroll).toHaveBeenCalledOnce());
    await waitFor(() =>
      expect(screen.getByTestId('step-remote')).toHaveAttribute('data-state', 'done'),
    );
    expect(screen.getByTestId('step-remote')).toHaveTextContent('Registered for remote access');
    expect(screen.getByTestId('step-scan')).toHaveAttribute('data-state', 'active');
    expect(pair).not.toHaveBeenCalled();
    expect(screen.getByLabelText('Pairing link')).toHaveValue(PAIR_INVITE);

    pair.mockRejectedValueOnce(unreachable()).mockResolvedValue(MACHINE);
    await user.click(await screen.findByRole('checkbox'));
    // While the PC learns the key, the connect step says how long it may take.
    expect(await screen.findByText(WAIT_TEXT)).toBeInTheDocument();
    expect(screen.getByTestId('step-connect')).toContainElement(screen.getByText(WAIT_TEXT));
    await waitFor(() => expect(onPaired).toHaveBeenCalledWith('m1'));
    expect(pair.mock.calls[0]?.[2]).toBeNull();
    expect(enroll).toHaveBeenCalledOnce();
  });

  it('one QR with an invite and 2-step verification: scan, confirm, six digits, paired', async () => {
    const user = userEvent.setup();
    const { native, pair, enroll } = fixture(0);
    native.qrScanner = scannerFor({ kind: 'link', uri: `${PAIR_INVITE}&tf=1` });
    pair.mockResolvedValue(MACHINE);
    const onPaired = renderView(native);
    await user.click(await screen.findByRole('button', { name: /Scan QR code/ }));
    const field = await screen.findByLabelText(CODE_LABEL);
    await waitFor(() => expect(field).toHaveFocus());
    await waitFor(() => expect(enroll).toHaveBeenCalledOnce());
    await user.click(await screen.findByRole('checkbox'));
    expect(field).toHaveFocus();
    expect(pair).not.toHaveBeenCalled();
    await user.keyboard('123456');
    await waitFor(() => expect(onPaired).toHaveBeenCalledWith('m1'));
    expect(pair).toHaveBeenCalledExactlyOnceWith(`${PAIR_INVITE}&tf=1`, 'My iPhone', '123456');
    for (const id of ['step-scan', 'step-remote', 'step-code', 'step-connect']) {
      expect(screen.getByTestId(id)).toHaveAttribute('data-state', 'done');
    }
  });

  it('a confirm given while the invite registers pairs once it is registered', async () => {
    const user = userEvent.setup();
    const { native, pair, enroll } = fixture(0);
    let finish: (e: DnsPilotEnrollment) => void = () => {};
    enroll.mockImplementation(() => new Promise((resolve) => (finish = resolve)));
    native.qrScanner = scannerFor({ kind: 'link', uri: PAIR_INVITE });
    pair.mockResolvedValue(MACHINE);
    const onPaired = renderView(native);
    await user.click(await screen.findByRole('button', { name: /Scan QR code/ }));
    await waitFor(() => expect(enroll).toHaveBeenCalledOnce());
    expect(screen.getByTestId('step-remote')).toHaveTextContent('Registering for remote access');
    await user.click(await screen.findByRole('checkbox'));
    expect(pair).not.toHaveBeenCalled();
    await act(async () => finish(enrollment(0)));
    await waitFor(() => expect(onPaired).toHaveBeenCalledWith('m1'));
    expect(pair).toHaveBeenCalledOnce();
  });

  it('a scan with the code typed first waits for the confirm code', async () => {
    const user = userEvent.setup();
    const { native, pair } = fixture(0);
    native.qrScanner = scannerFor({ kind: 'link', uri: `${PAIR_INVITE}&tf=1` });
    pair.mockResolvedValue(MACHINE);
    const onPaired = renderView(native);
    await user.click(await screen.findByRole('button', { name: /Scan QR code/ }));
    await screen.findByTestId('confirm-code');
    await user.type(screen.getByLabelText(CODE_LABEL), '123456');
    expect(pair).not.toHaveBeenCalled();
    expect(within(screen.getByTestId('step-scan')).getByRole('alert')).toHaveTextContent(
      /Check the confirm code first/,
    );
    await user.click(screen.getByRole('checkbox'));
    await waitFor(() => expect(onPaired).toHaveBeenCalledWith('m1'));
  });

  it('a plain pairing QR still only fills the form', async () => {
    const user = userEvent.setup();
    const { native, pair, enroll } = fixture();
    native.qrScanner = scannerFor({ kind: 'link', uri: PAIR });
    renderView(native);
    await user.click(await screen.findByRole('button', { name: /Scan QR code/ }));
    expect(await screen.findByDisplayValue(PAIR)).toBeInTheDocument();
    expect(enroll).not.toHaveBeenCalled();
    expect(pair).not.toHaveBeenCalled();
  });

  it('the console QR enrolls only, then asks for the pairing QR', async () => {
    const user = userEvent.setup();
    const { native, pair, enroll } = fixture(30);
    native.qrScanner = scannerFor({ kind: 'link', uri: CONSOLE });
    renderView(native);
    await user.click(await screen.findByRole('button', { name: /Scan QR code/ }));
    expect(await screen.findByText(NEXT_SCAN)).toBeInTheDocument();
    expect(enroll).toHaveBeenCalledExactlyOnceWith(CONSOLE);
    expect(pair).not.toHaveBeenCalled();
    expect(screen.getByLabelText('Pairing link')).toHaveValue('');
    // Registered: step 2 is done, scanning the PC's QR (step 1) is next.
    expect(screen.getByTestId('step-remote')).toHaveAttribute('data-state', 'done');
    expect(screen.getByTestId('step-scan')).toHaveAttribute('data-state', 'active');
    expect(screen.getByTestId('step-scan')).toHaveAttribute('aria-current', 'step');
    expect(screen.getByTestId('step-scan')).toHaveTextContent(NEXT_SCAN);
    expect(screen.getByRole('button', { name: 'Scan QR code' })).toHaveClass('btn-primary');
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('after the console QR, Pair with an empty field asks for the pairing QR, not "invalid"', async () => {
    const user = userEvent.setup();
    const { native, pair } = fixture(30);
    native.qrScanner = scannerFor({ kind: 'link', uri: CONSOLE });
    renderView(native);
    await user.click(await screen.findByRole('button', { name: /Scan QR code/ }));
    await screen.findByText(NEXT_SCAN);
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    expect(screen.queryByText(/not a valid vca:\/\/pair link/i)).not.toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    expect(pair).not.toHaveBeenCalled();
  });

  it('the wait also covers the pairing QR scanned after the console QR', async () => {
    const user = userEvent.setup();
    const { native, pair } = fixture(1);
    pair.mockRejectedValueOnce(unreachable()).mockResolvedValue(MACHINE);
    const onPaired = renderView(native);
    const field = screen.getByLabelText('Pairing link');
    await paste(user, field, CONSOLE);
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    await screen.findByText(NEXT_SCAN);

    await paste(user, field, PAIR);
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    await waitFor(() => expect(onPaired).toHaveBeenCalledWith('m1'));
    expect(pair).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('refuses a look-alike console link without calling anything', async () => {
    const user = userEvent.setup();
    const { native, pair, enroll } = fixture();
    renderView(native);
    await user.type(
      screen.getByLabelText('Pairing link'),
      CONSOLE.replace('dns.dnspilot.kr', 'dns.dnspilot.kr.evil.example'),
    );
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(enroll).not.toHaveBeenCalled();
    expect(pair).not.toHaveBeenCalled();
  });

  it('explains a used invite and keeps the code out of the message', async () => {
    const user = userEvent.setup();
    const { native, enroll } = fixture();
    enroll.mockRejectedValue(new NativeError('dnspilot_invite_invalid', 'bad'));
    renderView(native);
    await paste(user, screen.getByLabelText('Pairing link'), PAIR_INVITE);
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('expired or was already used');
    expect(screen.getByTestId('step-remote')).toContainElement(alert);
    expect(alert).not.toHaveTextContent(CODE);
  });

  it('says how long to wait when DNSPilot rate limits the phone', async () => {
    const user = userEvent.setup();
    const { native, enroll } = fixture();
    enroll.mockRejectedValue(new NativeError('dnspilot_rate_limited', 'limited', 120));
    renderView(native);
    await paste(user, screen.getByLabelText('Pairing link'), CONSOLE);
    await user.click(screen.getByRole('button', { name: 'Pair' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Try again in 120 seconds');
  });
});
