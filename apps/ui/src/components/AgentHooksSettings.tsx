import { useCallback, useEffect, useState } from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import {
  HOOK_AGENTS,
  type AgentHooks,
  type AgentHooksServices,
  type AgentHooksStatus,
  type HookAgent,
} from '../native/agentHooks';
import { toNativeError } from '../native/bridge';
import { Modal } from './Modal';
import styles from './AgentHooksSettings.module.css';

const AGENT_NAMES: Record<HookAgent, string> = {
  claude: 'Claude Code',
  codex: 'Codex',
};

type Action = 'install' | 'repair' | 'uninstall';

interface Pending {
  agent: HookAgent;
  action: Action;
}

/**
 * Settings > "Agent connection (exact approval alerts)", desktop only:
 * installs, repairs and removes the Claude Code and Codex hooks of this PC.
 * Nothing is installed until the user confirms.
 */
export function AgentHooksSettings({ services }: { services: AgentHooksServices }) {
  const { t } = useI18n();
  const [status, setStatus] = useState<AgentHooksStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [forbidden, setForbidden] = useState(false);
  const [busy, setBusy] = useState<HookAgent | null>(null);
  const [pending, setPending] = useState<Pending | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const fail = useCallback(
    (err: unknown) => {
      const e = toNativeError(err);
      if (e.code === 'forbidden') {
        setForbidden(true);
        return;
      }
      setError(
        e.code === 'not_bundled'
          ? t('agentHooks.notBundled')
          : t('agentHooks.error', { error: e.message }),
      );
    },
    [t],
  );

  const refresh = useCallback(() => {
    services
      .status()
      .then((s) => setStatus(s))
      .catch(fail);
  }, [services, fail]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  if (forbidden) return <AgentHooksHint />;

  const run = (agent: HookAgent, action: Action) => {
    setPending(null);
    setError(null);
    setNotice(null);
    setBusy(agent);
    const name = AGENT_NAMES[agent];
    const request = action === 'uninstall' ? services.uninstall(agent) : services.install(agent);
    request
      .then((result) => {
        setStatus((s) =>
          s ? { ...s, agents: s.agents.map((a) => (a.agent === agent ? result : a)) } : s,
        );
        const done: MessageKey =
          action === 'uninstall' ? 'agentHooks.done.uninstall' : 'agentHooks.done.install';
        setNotice(
          result.backup_path
            ? `${t(done, { agent: name })} ${t('agentHooks.backup', { path: result.backup_path })}`
            : t(done, { agent: name }),
        );
      })
      .catch(fail)
      .finally(() => setBusy(null));
  };

  const pendingAgent = pending && status?.agents.find((a) => a.agent === pending.agent);

  return (
    <div className={styles.box} data-testid="agent-hooks">
      <p className="muted">{t('agentHooks.intro')}</p>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
      {notice && (
        <p role="status" className={styles.notice}>
          {notice}
        </p>
      )}
      {status === null && !error && <p className="muted">{t('agentHooks.loading')}</p>}
      {status && status.cli_path === null && (
        <p className="muted" data-testid="agent-hooks-not-bundled">
          {t('agentHooks.notBundled')}
        </p>
      )}
      {status && (
        <ul className={styles.agents}>
          {HOOK_AGENTS.map((agent) => {
            const entry = status.agents.find((a) => a.agent === agent);
            if (!entry) return null;
            return (
              <AgentRow
                key={agent}
                entry={entry}
                cliPath={status.cli_path}
                busy={busy === agent}
                disabled={busy !== null}
                onAction={(action) => setPending({ agent, action })}
              />
            );
          })}
        </ul>
      )}
      <p className={`muted ${styles.note}`}>{t('agentHooks.projectNote')}</p>
      {pending && pendingAgent && (
        <ConfirmHooks
          pending={pending}
          entry={pendingAgent}
          onCancel={() => setPending(null)}
          onConfirm={() => run(pending.agent, pending.action)}
        />
      )}
    </div>
  );
}

function AgentRow({
  entry,
  cliPath,
  busy,
  disabled,
  onAction,
}: {
  entry: AgentHooks;
  cliPath: string | null;
  busy: boolean;
  disabled: boolean;
  onAction: (action: Action) => void;
}) {
  const { t } = useI18n();
  const name = AGENT_NAMES[entry.agent];
  const canInstall = cliPath !== null;
  const otherPrograms = entry.programs.filter((p) => p !== cliPath);
  return (
    <li className={styles.agent} data-testid={`agent-hooks-${entry.agent}`}>
      <div className={styles.head}>
        <span className={styles.name}>{name}</span>
        <span className={`${styles.state} ${styles[entry.state] ?? ''}`} data-state={entry.state}>
          {t(`agentHooks.state.${entry.state}`)}
        </span>
      </div>
      <p className={`muted ${styles.path}`}>
        {t('agentHooks.config', { path: entry.config_path })}
      </p>
      {entry.state === 'other_path' && otherPrograms.length > 0 && (
        <p className={styles.help}>
          {t('agentHooks.otherPathHelp', { path: otherPrograms.join(', ') })}
        </p>
      )}
      {entry.state === 'outdated' && <p className={styles.help}>{t('agentHooks.outdatedHelp')}</p>}
      {entry.state === 'error' && entry.error && (
        <p className={styles.error}>{t('agentHooks.readError', { error: entry.error })}</p>
      )}
      <div className={styles.actions}>
        {entry.state === 'missing' && (
          <button
            type="button"
            className="btn btn-sm btn-primary"
            disabled={disabled || !canInstall}
            onClick={() => onAction('install')}
          >
            {busy ? t('agentHooks.working') : t('agentHooks.install')}
          </button>
        )}
        {(entry.state === 'outdated' || entry.state === 'other_path') && (
          <button
            type="button"
            className="btn btn-sm btn-primary"
            disabled={disabled || !canInstall}
            onClick={() => onAction('repair')}
          >
            {busy ? t('agentHooks.working') : t('agentHooks.repair')}
          </button>
        )}
        {entry.state !== 'missing' && entry.state !== 'error' && (
          <button
            type="button"
            className="btn btn-sm"
            disabled={disabled}
            onClick={() => onAction('uninstall')}
          >
            {t('agentHooks.uninstall')}
          </button>
        )}
      </div>
    </li>
  );
}

function ConfirmHooks({
  pending,
  entry,
  onCancel,
  onConfirm,
}: {
  pending: Pending;
  entry: AgentHooks;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const { t } = useI18n();
  const agent = AGENT_NAMES[pending.agent];
  const uninstall = pending.action === 'uninstall';
  return (
    <Modal
      title={t(`agentHooks.confirmTitle.${pending.action}`, { agent })}
      onClose={onCancel}
      footer={
        <>
          <button type="button" className="btn" onClick={onCancel}>
            {t('common.cancel')}
          </button>
          <button
            type="button"
            className={`btn ${uninstall ? 'btn-danger' : 'btn-primary'}`}
            onClick={onConfirm}
          >
            {t(`agentHooks.${pending.action}`)}
          </button>
        </>
      }
    >
      {uninstall ? (
        <ul className={styles.explain} data-testid="agent-hooks-confirm">
          <li>{t('agentHooks.confirm.remove', { agent, path: entry.config_path })}</li>
          <li>
            {t(
              pending.agent === 'codex'
                ? 'agentHooks.confirm.removeKeep.codex'
                : 'agentHooks.confirm.removeKeep.claude',
            )}
          </li>
          <li>{t('agentHooks.confirm.backup')}</li>
        </ul>
      ) : (
        <ul className={styles.explain} data-testid="agent-hooks-confirm">
          <li>{t('agentHooks.confirm.what', { agent, path: entry.config_path })}</li>
          <li>
            {t(
              pending.agent === 'codex'
                ? 'agentHooks.confirm.keep.codex'
                : 'agentHooks.confirm.keep.claude',
            )}
          </li>
          <li>{t('agentHooks.confirm.backup')}</li>
          <li>{t('agentHooks.confirm.failOpen')}</li>
          <li>{t('agentHooks.confirm.restart')}</li>
        </ul>
      )}
    </Modal>
  );
}

/** Phones and paired devices: hooks are turned on in the PC app. */
export function AgentHooksHint() {
  const { t } = useI18n();
  return (
    <p className="muted" data-testid="agent-hooks-hint">
      {t('agentHooks.phoneHint')}
    </p>
  );
}
