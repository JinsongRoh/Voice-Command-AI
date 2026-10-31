import { useId, useState, type FormEvent } from 'react';

import { useI18n } from '../i18n/context';
import {
  agentLabel,
  CLOUD_PROVIDERS,
  folderName,
  LOCAL_AGENT_TYPES,
  providerLabel,
} from '../protocol/helpers';
import type { AgentKind, CloudProvider, SessionInfo } from '../protocol/types';
import { useActions } from '../state/store';
import { Modal } from './Modal';

const PROFILES = ['personal', 'work', 'api'];

/** SES-04 (local) and CLD-03 (cloud) session creation. */
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
  const [kind, setKind] = useState<'local' | 'cloud'>('local');
  const [agentType, setAgentType] = useState<(typeof LOCAL_AGENT_TYPES)[number]>('claude_code');
  const [folderMode, setFolderMode] = useState<'existing' | 'new'>(
    projectDirs.length ? 'existing' : 'new',
  );
  const [existingDir, setExistingDir] = useState(projectDirs[0] ?? '');
  const [newDir, setNewDir] = useState('');
  const [profile, setProfile] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [provider, setProvider] = useState<CloudProvider>('jules');
  const [repository, setRepository] = useState('');
  const [prompt, setPrompt] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const projectDir = folderMode === 'existing' ? existingDir : newDir.trim();
  const valid =
    kind === 'local'
      ? projectDir.length > 0
      : repository.trim().length > 0 && prompt.trim().length > 0;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!valid || busy) return;
    setBusy(true);
    setError(null);
    try {
      const session =
        kind === 'local'
          ? await actions.launch({
              agent: { type: agentType } as AgentKind,
              project_dir: projectDir,
              create_dir: folderMode === 'new',
              profile: profile || null,
              args: [],
              display_name: displayName.trim() || null,
            })
          : await actions.launchCloud({
              provider,
              repository: repository.trim(),
              prompt: prompt.trim(),
              display_name: displayName.trim() || null,
            });
      onLaunched(session);
    } catch (err) {
      setError(t('newSession.error', { error: (err as Error).message }));
      setBusy(false);
    }
  };

  return (
    <Modal
      title={t('newSession.title')}
      onClose={onClose}
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
            {busy ? t('newSession.launching') : t('newSession.launch')}
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
          </div>
        </div>

        {kind === 'local' ? (
          <>
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
            <fieldset className="field" style={{ border: 0, padding: 0, margin: '0 0 14px' }}>
              <legend className="label" style={{ fontWeight: 600, marginBottom: 6 }}>
                {t('newSession.project')}
              </legend>
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
                {t('newSession.newFolder')}
              </label>
              {folderMode === 'new' && (
                <input
                  type="text"
                  aria-label={t('newSession.newFolderPath')}
                  placeholder="~/code/new-project"
                  value={newDir}
                  onChange={(e) => setNewDir(e.target.value)}
                  autoCapitalize="off"
                  spellCheck={false}
                />
              )}
            </fieldset>
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
