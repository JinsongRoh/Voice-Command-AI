/**
 * "Start VCA when I sign in" (desktop shell only; mirrors
 * `apps/shell/src/autostart.rs`). The shell reads the sign-in entry from the
 * OS each time, so the setting stays right when it is removed elsewhere
 * (Task Manager, Login Items). The phone apps and the browser have no such
 * entry.
 */
import { call } from './bridge';

export interface LaunchAtLoginStatus {
  /** The sign-in entry is present and not turned off elsewhere. */
  enabled: boolean;
  /** This run was started by it. */
  launched_at_login: boolean;
}

export interface LaunchAtLoginServices {
  status(): Promise<LaunchAtLoginStatus>;
  /** Turns it on or off; resolves with the state read back from the OS. */
  set(enabled: boolean): Promise<LaunchAtLoginStatus>;
}

export const launchAtLoginBridge: LaunchAtLoginServices = {
  status: () => call<LaunchAtLoginStatus>('vca_autostart_status'),
  set: (enabled) => call<LaunchAtLoginStatus>('vca_autostart_set', { enabled }),
};
