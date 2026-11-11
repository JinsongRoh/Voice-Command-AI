import { useEffect, useId, useMemo, useState } from 'react';

import { useI18n } from '../i18n/context';
import { folderName, isOpenShell, originLabel, shellLabel } from '../protocol/helpers';
import { commandProblem, shellChoices } from '../protocol/shell';
import type { SessionId, SessionInfo } from '../protocol/types';
import { useActions, useAppState, type CommandDanger } from '../state/store';
import { CommandPreview, DangerBadge, DangerReasons } from './CommandPreview';
import { Icon } from './Icon';
import { Modal } from './Modal';
import { useShell } from './ShellProvider';
import { describeShellError, isErrorCode } from './shellErrors';
import styles from './Shell.module.css';

type Target = { kind: 'session'; id: SessionId } | { kind: 'new' };

/**
 * "Run in shell": pick an open shell on this machine (or open one in the
 * project folder), review the command with the server's classification,
 * then confirm. Nothing is sent before the Run button is pressed;
 * dangerous commands also need biometrics (phones) and, outside the
 * step-up window, an authenticator code.
 */
export function RunInShellDialog({
  command,
  projectDir,
  sessionId,
  onClose,
  onDone,
}: {
  command: string;
  /** Folder for a new shell. */
  projectDir: string;
  /** Preselected shell session (skips the target step). */
  sessionId: SessionId | null;
  onClose: () => void;
  onDone: (session: SessionInfo) => void;
}) {
  const { t } = useI18n();
  const id = useId();
  const actions = useActions();
  const shell = useShell();
  const sessionsById = useAppState((s) => s.sessions);
  const order = useAppState((s) => s.order);
  const shells = useMemo(
    () =>
      order.map((sid) => sessionsById[sid]).filter((s): s is SessionInfo => !!s && isOpenShell(s)),
    [order, sessionsById],
  );
  const preset = sessionId && shells.some((s) => s.id === sessionId) ? sessionId : null;
  const [step, setStep] = useState<'target' | 'confirm'>(preset ? 'confirm' : 'target');
  const [target, setTarget] = useState<Target>(() => {
    if (preset) return { kind: 'session', id: preset };
    const sameDir = shells.find(
      (s) => s.origin.type === 'local' && s.origin.project_dir === projectDir,
    );
    const first = sameDir ?? shells[0];
    return first ? { kind: 'session', id: first.id } : { kind: 'new' };
  });
  const choices = shellChoices(shell?.os ?? null);
  const [newShell, setNewShell] = useState(choices[0] ?? 'default');
  const [newDir, setNewDir] = useState(projectDir);
  const [danger, setDanger] = useState<CommandDanger | null>(null);
  const [classifyFailed, setClassifyFailed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const problem = commandProblem(command);

  useEffect(() => {
    let live = true;
    actions.classifyCommand(command).then(
      (res) => live && setDanger(res),
      () => live && setClassifyFailed(true),
    );
    return () => {
      live = false;
    };
  }, [actions, command]);

  const targetSession = target.kind === 'session' ? sessionsById[target.id] : undefined;
  const targetLabel =
    target.kind === 'new'
      ? t('runShell.newTarget', { shell: shellLabel(newShell), folder: folderName(newDir) })
      : (targetSession?.display_name ?? '');

  const run = async () => {
    if (problem || !shell || busy) return;
    setBusy(true);
    setError(null);
    try {
      let session: SessionInfo | undefined;
      let verified = false;
      if (target.kind === 'new') {
        const opened = await shell.openShell({
          shell: newShell,
          project_dir: newDir.trim(),
          display_name: null,
        });
        if (!opened) {
          setBusy(false);
          return;
        }
        session = opened;
        verified = true;
      } else {
        session = sessionsById[target.id];
      }
      if (!session) throw new Error(t('shell.error.closed'));
      // Unknown classification counts as dangerous here; the server decides.
      const dangerous = danger ? danger.level === 'dangerous' : true;
      if (dangerous && !verified && !(await shell.confirmPresence(t('shell.presence.run')))) {
        setBusy(false);
        return;
      }
      const sid = session.id;
      try {
        await actions.shellRun(sid, command, null);
      } catch (err) {
        if (!isErrorCode(err, 'totp_required')) throw err;
        const ok = await shell.withTotp(
          {
            title: t('totp.runTitle'),
            description: t('totp.runDescription'),
            submitLabel: t('runShell.run'),
          },
          async (code) => {
            await actions.shellRun(sid, command, code);
            return true;
          },
        );
        if (!ok) {
          setBusy(false);
          return;
        }
      }
      onDone(session);
    } catch (err) {
      setError(describeShellError(t, err));
      setBusy(false);
    }
  };

  const level = danger?.level ?? null;

  return (
    <Modal
      title={t('runShell.title')}
      onClose={onClose}
      footer={
        step === 'target' ? (
          <>
            <button type="button" className="btn" onClick={onClose}>
              {t('common.cancel')}
            </button>
            <button
              type="button"
              className="btn btn-primary"
              disabled={target.kind === 'new' && !newDir.trim()}
              onClick={() => setStep('confirm')}
            >
              {t('runShell.next')}
            </button>
          </>
        ) : (
          <>
            {!preset && (
              <button
                type="button"
                className="btn"
                disabled={busy}
                onClick={() => setStep('target')}
              >
                {t('runShell.back')}
              </button>
            )}
            <button type="button" className="btn" onClick={onClose}>
              {t('common.cancel')}
            </button>
            <button
              type="button"
              className={`btn ${level === 'dangerous' ? 'btn-danger' : 'btn-primary'}`}
              disabled={busy || problem !== null || (!danger && !classifyFailed)}
              onClick={() => void run()}
            >
              <Icon name="terminal" size={16} />
              {busy
                ? t('runShell.running')
                : level === 'dangerous'
                  ? t('runShell.runDangerous')
                  : t('runShell.run')}
            </button>
          </>
        )
      }
    >
      <div className={styles.run}>
        <CommandPreview
          command={command}
          spans={danger?.spans}
          label={t('runShell.commandLabel')}
        />
        {step === 'target' ? (
          <fieldset className={styles.targets}>
            <legend className="label">{t('runShell.where')}</legend>
            {shells.map((s) => (
              <label key={s.id} className={styles.target}>
                <input
                  type="radio"
                  name={`${id}-target`}
                  checked={target.kind === 'session' && target.id === s.id}
                  onChange={() => setTarget({ kind: 'session', id: s.id })}
                />
                <span className={styles.targetText}>
                  <span className={styles.targetName}>{s.display_name}</span>
                  <span className={styles.targetMeta}>
                    {originLabel(s.origin)}
                    {s.origin.type === 'local' && ` · ${s.origin.project_dir}`}
                  </span>
                </span>
              </label>
            ))}
            <label className={styles.target}>
              <input
                type="radio"
                name={`${id}-target`}
                checked={target.kind === 'new'}
                onChange={() => setTarget({ kind: 'new' })}
              />
              <span className={styles.targetText}>
                <span className={styles.targetName}>{t('runShell.openNew')}</span>
                <span className={styles.targetMeta}>{t('runShell.openNewHint')}</span>
              </span>
            </label>
            {target.kind === 'new' && (
              <div className={styles.newShell}>
                <div className="field">
                  <label htmlFor={`${id}-shell`}>{t('newSession.shell')}</label>
                  <select
                    id={`${id}-shell`}
                    value={newShell}
                    onChange={(e) => setNewShell(e.target.value)}
                  >
                    {choices.map((c) => (
                      <option key={c} value={c}>
                        {c === 'default' ? t('newSession.shellDefault') : shellLabel(c)}
                      </option>
                    ))}
                  </select>
                </div>
                <div className="field">
                  <label htmlFor={`${id}-dir`}>{t('newSession.project')}</label>
                  <input
                    id={`${id}-dir`}
                    type="text"
                    value={newDir}
                    onChange={(e) => setNewDir(e.target.value)}
                    autoCapitalize="off"
                    spellCheck={false}
                  />
                </div>
              </div>
            )}
          </fieldset>
        ) : (
          <>
            <p className={styles.runTarget}>
              <Icon name="terminal" size={16} />
              <span>{t('runShell.runsIn', { target: targetLabel })}</span>
            </p>
            <div className={styles.verdict} aria-live="polite">
              {danger ? (
                <>
                  <DangerBadge level={danger.level} />
                  <DangerReasons reasons={danger.reasons} />
                  {danger.level === 'dangerous' && (
                    <p className={styles.warn}>{t('runShell.dangerousHint')}</p>
                  )}
                </>
              ) : classifyFailed ? (
                <p className={styles.warn}>{t('runShell.classifyFailed')}</p>
              ) : (
                <p className="muted">{t('runShell.checking')}</p>
              )}
            </div>
            {problem && (
              <p role="alert" className={styles.error}>
                {t(`runShell.problem.${problem}`)}
              </p>
            )}
            <p className={styles.fineprint}>{t('runShell.confirmHint')}</p>
          </>
        )}
        {error && (
          <p role="alert" className={styles.error}>
            {error}
          </p>
        )}
      </div>
    </Modal>
  );
}
