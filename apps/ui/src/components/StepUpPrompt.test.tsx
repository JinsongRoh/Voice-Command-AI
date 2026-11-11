import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { describe, expect, it, vi } from 'vitest';

import { I18nProvider } from '../i18n/context';
import { NativeError } from '../native/bridge';
import {
  isStepUpCanceled,
  withStepUp,
  type GatedAction,
  type SecurityServices,
} from '../native/security';
import { fakeSecurity, securityStatus } from '../test/fakeSecurity';
import { onSettingsViewRequest, peekSettingsTarget } from './settingsNav';
import { StepUpProvider, useStepUp } from './StepUpPrompt';

function Harness({ call }: { call: () => Promise<string> }) {
  const stepUp = useStepUp();
  const [result, setResult] = useState('none');
  return (
    <>
      <button
        type="button"
        onClick={() =>
          void stepUp.run('mail_settings', call).then(
            (v) => setResult(v),
            (err: unknown) => setResult(isStepUpCanceled(err) ? 'canceled' : 'failed'),
          )
        }
      >
        Go
      </button>
      <output data-testid="result">{result}</output>
    </>
  );
}

function gatedCall(services: ReturnType<typeof fakeSecurity>['state']) {
  return vi.fn(async () => {
    if (!services.verified) throw new NativeError('step_up_required', 'verify first');
    return 'ran';
  });
}

function setup(services: SecurityServices, call: () => Promise<string>) {
  render(
    <I18nProvider locale="en">
      <StepUpProvider services={services}>
        <Harness call={call} />
      </StepUpProvider>
    </I18nProvider>,
  );
  return userEvent.setup();
}

describe('withStepUp', () => {
  it('retries once after verification and passes other errors on', async () => {
    const run = vi
      .fn<() => Promise<string>>()
      .mockRejectedValueOnce({ code: 'step_up_required', message: 'x' })
      .mockResolvedValueOnce('ok');
    await expect(withStepUp(run, async () => true)).resolves.toBe('ok');
    expect(run).toHaveBeenCalledTimes(2);

    const verify = vi.fn(async () => true);
    await expect(
      withStepUp(() => Promise.reject(new NativeError('mail_failed', 'no')), verify),
    ).rejects.toMatchObject({ code: 'mail_failed' });
    expect(verify).not.toHaveBeenCalled();
  });

  it('retries only once', async () => {
    const run = vi.fn(() => Promise.reject(new NativeError('step_up_required', 'x')));
    await expect(withStepUp(run, async () => true)).rejects.toMatchObject({
      code: 'step_up_required',
    });
    expect(run).toHaveBeenCalledTimes(2);
  });
});

describe('StepUpPrompt', () => {
  it('verifies with Windows Hello, then retries the call', async () => {
    const { services, state } = fakeSecurity();
    const call = gatedCall(state);
    const user = setup(services, call);
    await user.click(screen.getByRole('button', { name: 'Go' }));

    const dialog = await screen.findByRole('dialog', { name: "Verify it's you" });
    expect(dialog).toHaveTextContent('Change mail settings');
    await user.click(
      await within(dialog).findByRole('button', { name: 'Verify with Windows Hello' }),
    );

    await waitFor(() => expect(screen.getByTestId('result')).toHaveTextContent('ran'));
    expect(services.stepUp).toHaveBeenCalledWith({
      reason: 'Change mail settings',
      action: 'mail_settings',
      language: 'en',
    });
    expect(call).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('labels the button for macOS', async () => {
    const { services, state } = fakeSecurity(
      securityStatus({ verifier: { kind: 'mac_local_auth', available: true, detail: null } }),
    );
    const user = setup(services, gatedCall(state));
    await user.click(screen.getByRole('button', { name: 'Go' }));
    expect(
      await screen.findByRole('button', { name: 'Verify with Touch ID or password' }),
    ).toBeInTheDocument();
  });

  it('uses the authenticator code without an OS verifier, and reports a wrong code', async () => {
    const { services, state } = fakeSecurity(
      securityStatus({ verifier: { kind: 'none', available: false, detail: null } }),
    );
    services.stepUp.mockRejectedValueOnce(new NativeError('totp_invalid', 'bad'));
    const call = gatedCall(state);
    const user = setup(services, call);
    await user.click(screen.getByRole('button', { name: 'Go' }));

    const field = await screen.findByLabelText('6-digit code from your authenticator app');
    await user.type(field, '111111');
    await user.click(screen.getByRole('button', { name: 'Verify' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('That code is not correct');
    expect(field).toHaveValue('');

    await user.type(field, '222 222');
    await user.click(screen.getByRole('button', { name: 'Verify' }));
    await waitFor(() => expect(screen.getByTestId('result')).toHaveTextContent('ran'));
    expect(services.stepUp).toHaveBeenLastCalledWith({
      reason: 'Change mail settings',
      action: 'mail_settings',
      language: 'en',
      totp: '222222',
    });
  });

  it('asks for the code when the shell answers totp_required', async () => {
    const { services, state } = fakeSecurity();
    services.stepUp.mockRejectedValueOnce(new NativeError('totp_required', 'code'));
    const user = setup(services, gatedCall(state));
    await user.click(screen.getByRole('button', { name: 'Go' }));
    await user.click(await screen.findByRole('button', { name: 'Verify with Windows Hello' }));

    await user.type(
      await screen.findByLabelText('6-digit code from your authenticator app'),
      '123456',
    );
    await user.click(screen.getByRole('button', { name: 'Verify' }));
    await waitFor(() => expect(screen.getByTestId('result')).toHaveTextContent('ran'));
  });

  it('stays open after a canceled or failed verification, and closing cancels the action', async () => {
    const { services, state } = fakeSecurity();
    services.stepUp
      .mockRejectedValueOnce(new NativeError('verification_canceled', 'no'))
      .mockRejectedValueOnce(new NativeError('verification_failed', 'no'));
    const call = gatedCall(state);
    const user = setup(services, call);
    await user.click(screen.getByRole('button', { name: 'Go' }));
    const os = await screen.findByRole('button', { name: 'Verify with Windows Hello' });

    await user.click(os);
    expect(await screen.findByRole('alert')).toHaveTextContent('Verification was canceled');
    await user.click(os);
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('Verification failed'));

    await user.click(screen.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.getByTestId('result')).toHaveTextContent('canceled'));
    expect(call).toHaveBeenCalledTimes(1);
  });

  it('explains when this PC has no way to verify yet', async () => {
    const { services, state } = fakeSecurity(
      securityStatus({
        verifier: { kind: 'none', available: false, detail: null },
        totp: {
          enrolled: false,
          enrolled_at: null,
          backup_codes_remaining: 0,
          backup_codes_total: 0,
        },
      }),
    );
    const user = setup(services, gatedCall(state));
    await user.click(screen.getByRole('button', { name: 'Go' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('cannot verify you yet');
    expect(screen.queryByLabelText('6-digit code from your authenticator app')).toBeNull();

    // The way out: close the prompt (the action is canceled) and open Security.
    const opened = vi.fn();
    const stop = onSettingsViewRequest(opened);
    await user.click(screen.getByRole('button', { name: 'Open security settings' }));
    await waitFor(() => expect(screen.getByTestId('result')).toHaveTextContent('canceled'));
    expect(screen.queryByTestId('step-up')).toBeNull();
    expect(opened).toHaveBeenCalledTimes(1);
    expect(peekSettingsTarget()).toBe('set-sec-status');
    stop();
  });

  it('is not closed by a stray tap on the backdrop', async () => {
    const { services, state } = fakeSecurity();
    const user = setup(services, gatedCall(state));
    await user.click(screen.getByRole('button', { name: 'Go' }));
    const dialog = await screen.findByRole('dialog');
    await user.pointer({ keys: '[MouseLeft]', target: dialog.parentElement! });
    expect(screen.getByTestId('step-up')).toBeInTheDocument();
  });

  it('shows verification_unavailable from the shell', async () => {
    const { services, state } = fakeSecurity();
    services.stepUp.mockRejectedValueOnce(new NativeError('verification_unavailable', 'none'));
    const user = setup(services, gatedCall(state));
    await user.click(screen.getByRole('button', { name: 'Go' }));
    await user.click(await screen.findByRole('button', { name: 'Verify with Windows Hello' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('cannot verify you yet');
  });
});

describe('what the verification is for', () => {
  function ActionHarness({ action }: { action: GatedAction | 'test' }) {
    const stepUp = useStepUp();
    return (
      <button type="button" onClick={() => void stepUp.request(action)}>
        Go
      </button>
    );
  }

  const open = async (services: SecurityServices, action: GatedAction | 'test') => {
    render(
      <I18nProvider locale="ko">
        <StepUpProvider services={services}>
          <ActionHarness action={action} />
        </StepUpProvider>
      </I18nProvider>,
    );
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: 'Go' }));
    return user;
  };

  const headless = (enrolled: boolean) =>
    securityStatus({
      verifier: { kind: 'none', available: false, detail: null },
      totp: {
        enrolled,
        enrolled_at: null,
        backup_codes_remaining: 0,
        backup_codes_total: 0,
      },
    });

  it('sends the language of the app for the OS prompt', async () => {
    const { services } = fakeSecurity();
    const user = await open(services, 'mail_settings');
    await user.click(await screen.findByRole('button', { name: /Windows Hello/ }));
    await waitFor(() => expect(services.stepUp).toHaveBeenCalled());
    expect(services.stepUp.mock.calls[0]).toEqual([
      expect.objectContaining({ action: 'mail_settings', language: 'ko' }),
    ]);
  });

  it('a test approves nothing: it is not a verification for an action', async () => {
    const { services } = fakeSecurity();
    const user = await open(services, 'test');
    await user.click(await screen.findByRole('button', { name: /Windows Hello/ }));
    await waitFor(() => expect(services.stepUpTest).toHaveBeenCalled());
    expect(services.stepUp).not.toHaveBeenCalled();
  });

  it('the first setup with nothing to verify against needs a confirmation', async () => {
    const { services } = fakeSecurity(headless(false));
    const user = await open(services, 'totp_setup');
    await user.click(await screen.findByRole('button', { name: /계속/ }));
    await waitFor(() => expect(services.stepUp).toHaveBeenCalled());
    expect(services.stepUp.mock.calls[0]).toEqual([
      expect.objectContaining({ action: 'totp_setup', confirmed: true }),
    ]);
  });

  it('only the first setup can be confirmed that way', async () => {
    const { services } = fakeSecurity(headless(false));
    await open(services, 'mail_settings');
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /계속/ })).not.toBeInTheDocument();
  });

  it('takes a whole backup code for recovery', async () => {
    const { services } = fakeSecurity(headless(true));
    const user = await open(services, 'recovery');
    const field = await screen.findByRole('textbox');
    await user.type(field, 'AAAA-BBBB-CCCC-DDDD');
    expect(field).toHaveValue('AAAA-BBBB-CCCC-DDDD');
  });
});
