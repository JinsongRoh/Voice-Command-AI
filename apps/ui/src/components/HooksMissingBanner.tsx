import { useState } from 'react';

import { useI18n } from '../i18n/context';
import type { AgentHooksServices } from '../native/agentHooks';
import { toNativeError } from '../native/bridge';
import { useNative } from '../native/context';
import { isLocalMachine } from '../native/host';
import { SESSION_PROBLEM_HOOKS_MISSING, type SessionInfo } from '../protocol/types';
import { useMachines } from '../state/store';
import { Icon } from './Icon';
import styles from './HooksMissingBanner.module.css';

type Status =
  | { kind: 'idle' }
  | { kind: 'busy' }
  | { kind: 'done' }
  | { kind: 'failed'; message: string }
  | { kind: 'dismissed' };

/**
 * "The Claude connection is missing, so progress shows late": the session's
 * agent should report through VCA hooks but they are missing from its
 * settings (another tool may have rewritten them). In the PC app, viewing
 * this PC, "Repair" installs them with the same command as Settings ›
 * Alerts › Agent connection; nothing is written until it is pressed.
 * Elsewhere the banner says where to repair them.
 */
export function HooksMissingBanner({
  session,
  services: injected,
}: {
  session: SessionInfo;
  /** Replaces the desktop app's hook commands (tests). */
  services?: AgentHooksServices | null;
}) {
  const { t } = useI18n();
  const native = useNative();
  const { machine } = useMachines();
  const [status, setStatus] = useState<Status>({ kind: 'idle' });
  const services =
    injected !== undefined
      ? injected
      : isLocalMachine(machine.id)
        ? (native?.host?.agentHooks ?? null)
        : null;
  const missing =
    session.problem === SESSION_PROBLEM_HOOKS_MISSING && session.state.type !== 'exited';
  if (status.kind === 'dismissed') return null;
  if (!missing && status.kind !== 'done') return null;

  const repair = () => {
    if (!services) return;
    setStatus({ kind: 'busy' });
    services
      .install('claude')
      .then(() => setStatus({ kind: 'done' }))
      .catch((err: unknown) => {
        const e = toNativeError(err);
        setStatus({
          kind: 'failed',
          message:
            e.code === 'not_bundled'
              ? t('agentHooks.notBundled')
              : e.code === 'forbidden'
                ? t('session.hooksMissing.onPc')
                : t('session.hooksMissing.failed', { error: e.message }),
        });
      });
  };

  if (status.kind === 'done') {
    return (
      <p role="status" className={styles.banner} data-state="done" data-testid="hooks-missing">
        <Icon name="check" size={14} />
        <span className={styles.text}>{t('session.hooksMissing.done')}</span>
        <button
          type="button"
          className={`icon-btn ${styles.close}`}
          aria-label={t('common.close')}
          title={t('common.close')}
          onClick={() => setStatus({ kind: 'dismissed' })}
        >
          <Icon name="close" size={14} />
        </button>
      </p>
    );
  }
  return (
    <div role="status" className={styles.banner} data-testid="hooks-missing">
      <Icon name="info" size={14} />
      <span className={styles.text}>
        {t('session.hooksMissing.text')}
        {!services && <span className={styles.hint}> {t('session.hooksMissing.onPc')}</span>}
        {status.kind === 'failed' && (
          <span className={styles.error} role="alert">
            {' '}
            {status.message}
          </span>
        )}
      </span>
      {services && (
        <button
          type="button"
          className="btn btn-sm btn-primary"
          disabled={status.kind === 'busy'}
          onClick={repair}
          data-testid="hooks-repair"
        >
          {status.kind === 'busy'
            ? t('session.hooksMissing.repairing')
            : t('session.hooksMissing.repair')}
        </button>
      )}
      <button
        type="button"
        className={`icon-btn ${styles.close}`}
        aria-label={t('common.close')}
        title={t('common.close')}
        onClick={() => setStatus({ kind: 'dismissed' })}
      >
        <Icon name="close" size={14} />
      </button>
    </div>
  );
}
