/**
 * Remote shell flows shared by the new-session dialog, the decision card,
 * chat code blocks and the composer: the biometric / passcode check on
 * phones, the authenticator code prompt, and the "Run in shell" sheet.
 *
 * The server is the authority (admin scope, TOTP, step-up window, danger
 * classification); these flows only make sure nothing reaches a shell
 * without the person explicitly confirming it.
 */
import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react';

import { useI18n } from '../i18n/context';
import { checkUserPresence, rememberTotpOnlyNotice, totpOnlyNoticeDue } from '../native/presence';
import { guessMachineOs, type MachineOs } from '../protocol/shell';
import type { DeviceScope, SessionId, SessionInfo, ShellRequest } from '../protocol/types';
import { useActions, useAppState, useMachines } from '../state/store';
import { RunInShellDialog } from './RunInShellDialog';
import { ShellLinesDialog, type ShellLinesRequest } from './ShellLinesDialog';
import { isErrorCode, isTotpNotEnrolled } from './shellErrors';
import { TotpPrompt, type TotpPromptProps } from './TotpPrompt';

export interface RunInShellRequest {
  command: string;
  /** Folder for a new shell (the agent session's project); defaults to the selected session's. */
  projectDir?: string | null;
  /** Run in this shell session without asking for a target. */
  sessionId?: SessionId | null;
}

export interface TotpRequest {
  title: string;
  description: string;
  submitLabel: string;
  /** The server has no authenticator: explain instead of asking for a code. */
  notEnrolled?: boolean;
}

export interface ShellServices {
  /** False when this device is known not to have admin permission. */
  allowed: boolean;
  /** OS of the machine, guessed from its folders (`null`: unknown). */
  os: MachineOs | null;
  /**
   * Biometrics or the device passcode on phones (no-op elsewhere). `false`
   * when the person cancelled. Phones without either pass with a one-time
   * notice that only the authenticator code protects the shell.
   */
  confirmPresence(reason: string): Promise<boolean>;
  /** Asks for a TOTP code and calls `apply` with it; `null` when cancelled. */
  withTotp<T>(request: TotpRequest, apply: (code: string) => Promise<T>): Promise<T | null>;
  /**
   * Asks the server without a code first (so a missing authenticator or
   * permission is explained before anyone looks for a code), then the
   * biometric check, the TOTP prompt and `open_shell`. `null` when cancelled.
   */
  openShell(request: Omit<ShellRequest, 'totp'>): Promise<SessionInfo | null>;
  /** Opens the "Run in shell" sheet (target, classification, explicit confirmation). */
  runInShell(request: RunInShellRequest): void;
  /**
   * Joins a shell another device opened: the biometric check on phones,
   * then an authenticator code (always asked for, once per shell and
   * device). `true` once joined, `false` when cancelled.
   */
  joinShell(sessionId: SessionId): Promise<boolean>;
  /**
   * "Run these N lines?" for input the server held back (`shell_confirm`):
   * shows all of it and resends it with `confirm` only when the person
   * presses Run. `true` once it ran, `false` when cancelled.
   */
  confirmLines(request: ShellLinesRequest): Promise<boolean>;
}

const ShellContext = createContext<ShellServices | null>(null);

/** Shell flows, or `null` outside the app shell (component tests, previews). */
// eslint-disable-next-line react-refresh/only-export-components
export function useShell(): ShellServices | null {
  return useContext(ShellContext);
}

/** This device's permission on the selected machine (`null`: not reported). */
// eslint-disable-next-line react-refresh/only-export-components
export function useDeviceScope(): DeviceScope | null {
  const { machine } = useMachines();
  const reported = useAppState((s) => s.devices?.find((d) => d.current)?.scope ?? null);
  return reported ?? machine.native?.info.scope ?? null;
}

type PromptState = Omit<TotpPromptProps, 'notice'> & { notice: string | null };

export function ShellProvider({ children }: { children: ReactNode }) {
  const { t } = useI18n();
  const actions = useActions();
  const { machine } = useMachines();
  const scope = useDeviceScope();
  const sessions = useAppState((s) => s.sessions);
  const selectedId = useAppState((s) => s.selectedId);
  const [prompt, setPrompt] = useState<PromptState | null>(null);
  const [run, setRun] = useState<RunInShellRequest | null>(null);
  const [lines, setLines] = useState<{
    request: ShellLinesRequest;
    resolve: (ran: boolean) => void;
  } | null>(null);
  /** "TOTP only on this device" notice waiting for the next code prompt. */
  const noticeRef = useRef<string | null>(null);

  const os = useMemo(() => {
    const paths = [...(machine.projectDirs ?? [])];
    for (const s of Object.values(sessions)) {
      if (s.origin.type === 'local') paths.push(s.origin.project_dir);
    }
    return guessMachineOs(paths);
  }, [machine.projectDirs, sessions]);

  const confirmPresence = useCallback(
    async (reason: string) => {
      const result = await checkUserPresence(reason);
      if (result === 'cancelled') return false;
      if (result === 'unavailable' && totpOnlyNoticeDue()) {
        rememberTotpOnlyNotice();
        noticeRef.current = t('shell.totpOnlyNotice');
      }
      return true;
    },
    [t],
  );

  /** Shows a pending notice as a toast when no code prompt follows. */
  const flushNotice = useCallback(() => {
    const notice = noticeRef.current;
    if (!notice) return;
    noticeRef.current = null;
    actions.showToast({
      id: 'shell:totp-only',
      category: 'activity',
      sessionId: null,
      title: t('shell.totpOnlyTitle'),
      body: notice,
      security: true,
    });
  }, [actions, t]);

  const withTotp = useCallback(
    <T,>(request: TotpRequest, apply: (code: string) => Promise<T>) =>
      new Promise<T | null>((resolve, reject) => {
        const notice = noticeRef.current;
        noticeRef.current = null;
        setPrompt({
          ...request,
          notice,
          onSubmit: async (code) => {
            const value = await apply(code);
            setPrompt(null);
            resolve(value);
          },
          onCancel: () => {
            setPrompt(null);
            resolve(null);
          },
          onFail: (err) => {
            setPrompt(null);
            reject(err);
          },
        });
      }),
    [],
  );

  const openShell = useCallback(
    async (request: Omit<ShellRequest, 'totp'>) => {
      // The server never opens a shell without a code: this only learns
      // whether one can be asked for.
      let notEnrolled = false;
      try {
        return await actions.openShell({ ...request, totp: '' });
      } catch (err) {
        if (isTotpNotEnrolled(err)) notEnrolled = true;
        else if (!isErrorCode(err, 'totp_required')) throw err;
      }
      if (!notEnrolled && !(await confirmPresence(t('shell.presence.open')))) return null;
      return withTotp(
        {
          title: t('totp.openTitle'),
          description: t('totp.openDescription', { dir: request.project_dir }),
          submitLabel: t('shell.open'),
          notEnrolled,
        },
        (code) => actions.openShell({ ...request, totp: code }),
      );
    },
    [actions, confirmPresence, t, withTotp],
  );

  const joinShell = useCallback(
    async (sessionId: SessionId) => {
      if (!(await confirmPresence(t('shell.presence.join')))) return false;
      const session = sessions[sessionId];
      const ok = await withTotp(
        {
          title: t('shell.join.title'),
          description: t('shell.join.description', {
            name: session?.display_name ?? sessionId,
            device: session?.shell?.opened_by ?? '',
          }),
          submitLabel: t('shell.join.submit'),
        },
        async (code) => {
          await actions.joinShell(sessionId, code);
          return true;
        },
      );
      return ok === true;
    },
    [actions, confirmPresence, sessions, t, withTotp],
  );

  const confirmLines = useCallback(
    (request: ShellLinesRequest) =>
      new Promise<boolean>((resolve) => {
        setLines((open) => {
          // One at a time: a newer request replaces (cancels) the shown one.
          open?.resolve(false);
          return { request, resolve };
        });
      }),
    [],
  );

  const services = useMemo<ShellServices>(
    () => ({
      allowed: scope === null || scope === 'admin',
      os,
      confirmPresence,
      withTotp,
      openShell,
      runInShell: setRun,
      joinShell,
      confirmLines,
    }),
    [scope, os, confirmPresence, withTotp, openShell, joinShell, confirmLines],
  );

  // Folder for a new shell: the request's, else the selected local session's.
  const selected = selectedId ? sessions[selectedId] : undefined;
  const defaultDir =
    run?.projectDir ??
    (selected?.origin.type === 'local' ? selected.origin.project_dir : null) ??
    machine.projectDirs?.[0] ??
    '';

  return (
    <ShellContext.Provider value={services}>
      {children}
      {run && (
        <RunInShellDialog
          command={run.command}
          projectDir={defaultDir}
          sessionId={run.sessionId ?? null}
          onClose={() => setRun(null)}
          onDone={(session) => {
            setRun(null);
            flushNotice();
            actions.showToast({
              id: `shell:ran:${session.id}:${Date.now()}`,
              category: 'activity',
              sessionId: session.id,
              title: t('shell.sent', { name: session.display_name }),
              body: '',
              security: true,
            });
          }}
        />
      )}
      {lines && (
        <ShellLinesDialog
          key={`${lines.request.sessionId}:${lines.request.command}`}
          request={lines.request}
          onClose={() => {
            lines.resolve(false);
            setLines(null);
          }}
          onDone={() => {
            lines.resolve(true);
            setLines(null);
            flushNotice();
            const session = sessions[lines.request.sessionId];
            actions.showToast({
              id: `shell:ran:${lines.request.sessionId}:${Date.now()}`,
              category: 'activity',
              sessionId: lines.request.sessionId,
              title: t('shell.sent', { name: session?.display_name ?? lines.request.sessionId }),
              body: '',
              security: true,
            });
          }}
        />
      )}
      {prompt && <TotpPrompt {...prompt} />}
    </ShellContext.Provider>
  );
}
