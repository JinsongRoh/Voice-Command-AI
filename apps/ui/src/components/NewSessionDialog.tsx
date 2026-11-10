import { useId, useState, type FormEvent } from 'react';

import { useI18n } from '../i18n/context';
import {
  agentLabel,
  CLOUD_PROVIDERS,
  folderName,
  LOCAL_AGENT_TYPES,
  providerLabel,
  shellLabel,
} from '../protocol/helpers';
import { shellChoices } from '../protocol/shell';
import type { AgentKind, CloudProvider, SessionInfo } from '../protocol/types';
import { useActions, useAppState, useMachines } from '../state/store';
import { FolderPicker, type FolderSelection } from './FolderPicker';
import { Modal } from './Modal';
import { useShell } from './ShellProvider';
import { describeShellError } from './shellErrors';

const PROFILES = ['personal', 'work', 'api'];

/** Servers that can start an agent under a named account profile. */
export const FEATURE_ACCOUNT_PROFILES = 'account_profiles';

type Kind = 'local' | 'cloud' | 'shell';

/** SES-04 (local), CLD-03 (cloud) and remote shell session creation. */
export function NewSessionDialog({
  projectDirs,
  onClose,
  onLaunched,
}: {
  projectDirs: string[];
  onClose: () => void;
  onLaunched: (session: SessionInfo) => void;
}) {
  const { t } = useI18n();
  const actions = useActions();
  const id = useId();
  const shell = useShell();
  const [kind, setKind] = useState<Kind>('local');
  const shells = shellChoices(shell?.os ?? null);
  const [shellName, setShellName] = useState(shells[0] ?? 'default');
  const [agentType, setAgentType] = useState<(typeof LOCAL_AGENT_TYPES)[number]>('claude_code');
  const [folderMode, setFolderMode] = useState<'existing' | 'new'>(
    projectDirs.length ? 'existing' : 'new',
  );
  const [existingDir, setExistingDir] = useState(projectDirs[0] ?? '');
  const [newDir, setNewDir] = useState('');
  const [profile, setProfile] = useState('');
  // Account profiles need server support; until a server announces it the
  // picker stays hidden (a chosen profile would only fail the launch).
  const profilesSupported = useAppState((s) => s.serverFeatures.includes(FEATURE_ACCOUNT_PROFILES));
  const [displayName, setDisplayName] = useState('');
  /** Claude Code continues the folder's latest conversation unless set. */
  const [fresh, setFresh] = useState(false);
  const [provider, setProvider] = useState<CloudProvider>('jules');
  const [repository, setRepository] = useState('');
  const [prompt, setPrompt] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { machine } = useMachines();
  /** The folder picker, or the typed path of older servers (`legacy`). */
  const [folderUi, setFolderUi] = useState<'picker' | 'legacy'>('picker');
  const [pickerNote, setPickerNote] = useState<string | null>(null);
  const [picked, setPicked] = useState<FolderSelection | null>(null);

  const projectDir =
    folderUi === 'picker'
      ? (picked?.path ?? '')
      : folderMode === 'existing'
        ? existingDir
        : newDir.trim();
  const createDir = folderUi === 'picker' ? !!picked?.create : folderMode === 'new';
  const valid =
    kind === 'cloud'
      ? repository.trim().length > 0 && prompt.trim().length > 0
      : kind === 'shell'
        ? projectDir.length > 0 && !!shell?.allowed
        : projectDir.length > 0;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!valid || busy) return;
    setBusy(true);
    setError(null);
    try {
      if (kind === 'shell') {
        // Biometrics (phones), then the authenticator code, then open_shell.
        const opened = await shell?.openShell({
          shell: shellName,
          project_dir: projectDir,
          display_name: displayName.trim() || null,
        });
        if (opened) onLaunched(opened);
        else setBusy(false);
        return;
      }
      const session =
        kind === 'local'
          ? await actions.launch({
              agent: { type: agentType } as AgentKind,
              project_dir: projectDir,
              create_dir: createDir,
              profile: profilesSupported ? profile || null : null,
              args: [],
              display_name: displayName.trim() || null,
              ...(agentType === 'claude_code' && fresh ? { fresh: true } : {}),
            })
          : await actions.launchCloud({
              provider,
              repository: repository.trim(),
              prompt: prompt.trim(),
              display_name: displayName.trim() || null,
            });
      onLaunched(session);
    } catch (err) {
      setError(
        kind === 'shell'
          ? describeShellError(t, err)
          : t('newSession.error', { error: (err as Error).message }),
      );
      setBusy(false);
    }
  };

  return (
    <Modal
      title={t('newSession.title')}
      onClose={onClose}
      dismissOnBackdrop={false}
      fullScreen
      footer={
        <>
          <button type="button" className="btn" onClick={onClose}>
            {t('common.cancel')}
          </button>
          <button
            type="submit"
            form={`${id}-form`}
            className="btn btn-primary"
            disabled={!valid || busy}
          >
            {busy
              ? t('newSession.launching')
              : kind === 'shell'
                ? t('shell.open')
                : t('newSession.launch')}
          </button>
        </>
      }
    >
      <form id={`${id}-form`} onSubmit={submit}>
        <div className="field">
          <span className="label" id={`${id}-kind`}>
            {t('newSession.kind')}
          </span>
          <div className="segmented" role="group" aria-labelledby={`${id}-kind`}>
            <button type="button" aria-pressed={kind === 'local'} onClick={() => setKind('local')}>
              {t('newSession.local')}
            </button>
            <button type="button" aria-pressed={kind === 'cloud'} onClick={() => setKind('cloud')}>
              {t('newSession.cloud')}
            </button>
            {shell && (
              <button
                type="button"
                aria-pressed={kind === 'shell'}
                disabled={!shell.allowed}
                aria-describedby={!shell.allowed ? `${id}-shell-hint` : undefined}
                onClick={() => {
                  setKind('shell');
                  if (folderMode === 'new' && projectDirs.length) setFolderMode('existing');
                }}
              >
                {t('newSession.shellKind')}
              </button>
            )}
          </div>
          {shell && !shell.allowed && (
            <p
              id={`${id}-shell-hint`}
              className="muted"
              style={{ margin: '6px 0 0', fontSize: 13 }}
            >
              {t('newSession.shellNeedsAdmin')}
            </p>
          )}
        </div>

        {kind !== 'cloud' ? (
          <>
            {kind === 'local' ? (
              <div className="field">
                <label htmlFor={`${id}-agent`}>{t('newSession.agent')}</label>
                <select
                  id={`${id}-agent`}
                  value={agentType}
                  onChange={(e) => setAgentType(e.target.value as typeof agentType)}
                >
                  {LOCAL_AGENT_TYPES.map((a) => (
                    <option key={a} value={a}>
                      {agentLabel({ type: a } as AgentKind)}
                    </option>
                  ))}
                </select>
              </div>
            ) : (
              <>
                <p className="muted" style={{ margin: '0 0 12px', fontSize: 14 }}>
                  {t('newSession.shellIntro')}
                </p>
                <div className="field">
                  <label htmlFor={`${id}-shell`}>{t('newSession.shell')}</label>
                  <select
                    id={`${id}-shell`}
                    value={shellName}
                    onChange={(e) => setShellName(e.target.value)}
                  >
                    {shells.map((name) => (
                      <option key={name} value={name}>
                        {name === 'default' ? t('newSession.shellDefault') : shellLabel(name)}
                      </option>
                    ))}
                  </select>
                </div>
              </>
            )}
            <fieldset className="field" style={{ border: 0, padding: 0, margin: '0 0 14px' }}>
              <legend className="label" style={{ fontWeight: 600, marginBottom: 6 }}>
                {t('newSession.project')}
              </legend>
              {folderUi === 'picker' ? (
                <FolderPicker
                  machineId={machine.id}
                  allowCreate={kind !== 'shell'}
                  onChange={setPicked}
                  onUnavailable={(reason) => {
                    setPickerNote(reason);
                    setFolderUi('legacy');
                  }}
                />
              ) : (
                <>
                  {pickerNote && (
                    <p className="muted" style={{ margin: '0 0 6px', fontSize: 13 }}>
                      {t('folderPicker.unavailable', { error: pickerNote })}
                    </p>
                  )}
                  <label className="check">
                    <input
                      type="radio"
                      name={`${id}-folder`}
                      checked={folderMode === 'existing'}
                      disabled={projectDirs.length === 0}
                      onChange={() => setFolderMode('existing')}
                    />
                    {t('newSession.existing')}
                  </label>
                  {folderMode === 'existing' && (
                    <select
                      aria-label={t('newSession.existing')}
                      value={existingDir}
                      onChange={(e) => setExistingDir(e.target.value)}
                    >
                      {projectDirs.map((d) => (
                        <option key={d} value={d}>
                          {folderName(d)} — {d}
                        </option>
                      ))}
                    </select>
                  )}
                  <label className="check">
                    <input
                      type="radio"
                      name={`${id}-folder`}
                      checked={folderMode === 'new'}
                      onChange={() => setFolderMode('new')}
                    />
                    {kind === 'shell' ? t('newSession.otherFolder') : t('newSession.newFolder')}
                  </label>
                  {folderMode === 'new' && (
                    <input
                      type="text"
                      aria-label={
                        kind === 'shell'
                          ? t('newSession.otherFolder')
                          : t('newSession.newFolderPath')
                      }
                      placeholder="~/code/new-project"
                      value={newDir}
                      onChange={(e) => setNewDir(e.target.value)}
                      autoCapitalize="off"
                      spellCheck={false}
                    />
                  )}
                </>
              )}
            </fieldset>
            {kind === 'local' && agentType === 'claude_code' && !createDir && (
              <div className="field">
                <label className="check">
                  <input
                    type="checkbox"
                    checked={fresh}
                    onChange={(e) => setFresh(e.target.checked)}
                    aria-describedby={`${id}-fresh-hint`}
                  />
                  {t('newSession.fresh')}
                </label>
                <p
                  id={`${id}-fresh-hint`}
                  className="muted"
                  style={{ margin: '4px 0 0', fontSize: 13 }}
                >
                  {fresh ? t('newSession.freshOn') : t('newSession.freshOff')}
                </p>
              </div>
            )}
            {kind === 'local' && profilesSupported && (
              <div className="field">
                <label htmlFor={`${id}-profile`}>{t('newSession.profile')}</label>
                <select
                  id={`${id}-profile`}
                  value={profile}
                  onChange={(e) => setProfile(e.target.value)}
                >
                  <option value="">{t('newSession.profileDefault')}</option>
                  {PROFILES.map((p) => (
                    <option key={p} value={p}>
                      {p}
                    </option>
                  ))}
                </select>
              </div>
            )}
          </>
        ) : (
          <>
            <div className="field">
              <label htmlFor={`${id}-provider`}>{t('newSession.provider')}</label>
              <select
                id={`${id}-provider`}
                value={provider}
                onChange={(e) => setProvider(e.target.value as CloudProvider)}
              >
                {CLOUD_PROVIDERS.map((p) => (
                  <option key={p} value={p}>
                    {providerLabel(p)}
                  </option>
                ))}
              </select>
            </div>
            <div className="field">
              <label htmlFor={`${id}-repo`}>{t('newSession.repository')}</label>
              <input
                id={`${id}-repo`}
                type="text"
                placeholder="owner/repository"
                value={repository}
                onChange={(e) => setRepository(e.target.value)}
                autoCapitalize="off"
                spellCheck={false}
              />
            </div>
            <div className="field">
              <label htmlFor={`${id}-prompt`}>{t('newSession.prompt')}</label>
              <textarea
                id={`${id}-prompt`}
                rows={3}
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
              />
            </div>
          </>
        )}
        <div className="field">
          <label htmlFor={`${id}-name`}>{t('newSession.displayName')}</label>
          <input
            id={`${id}-name`}
            type="text"
            value={displayName}
            onChange={(e) => setDisplayName(e.target.value)}
          />
        </div>
        {error && (
          <p role="alert" style={{ color: 'var(--danger)' }}>
            {error}
          </p>
        )}
      </form>
    </Modal>
  );
}
