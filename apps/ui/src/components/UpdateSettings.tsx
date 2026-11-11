import { useEffect, useRef, useState } from 'react';

import { useI18n } from '../i18n/context';
import type { TranslateFn } from '../i18n/i18n';
import { useNative } from '../native/context';
import {
  startAutoCheck,
  updateControllerFor,
  useUpdateState,
  type UpdateController,
  type UpdatePreflight,
  type UpdateStatus,
} from '../native/updater';
import { useActions } from '../state/store';
import { Modal } from './Modal';
import { openSettingsAt } from './settingsNav';
import styles from './UpdateSettings.module.css';

/** Error codes of `apps/shell/src/updater.rs` with their own message. */
const ERROR_CODES = [
  'update_offline',
  'update_signature',
  'update_no_release',
  'update_no_platform',
  'update_bad_package',
  'update_unsupported',
  'update_busy',
] as const;

function describeError(t: TranslateFn, status: Extract<UpdateStatus, { kind: 'error' }>): string {
  const code = ERROR_CODES.find((c) => c === status.code);
  if (code) return t(`update.error.${code}`);
  if (status.code === 'update_install')
    return t('update.error.update_install', { error: status.message });
  return t('update.error.generic', { error: status.message });
}

/** Release notes as plain text (never HTML). */
function Notes({ notes }: { notes: string | null }) {
  const { t } = useI18n();
  if (!notes) return null;
  return (
    <details className={styles.notes} open>
      <summary>{t('update.notes')}</summary>
      <p data-testid="update-notes">{notes}</p>
    </details>
  );
}

/**
 * Settings → About on the desktop app: check, download, then restart to
 * install, plus the automatic check. Hidden where the shell has no updater
 * (the phone apps and the browser).
 */
export function UpdateSettings() {
  const updater = useNative()?.host?.updater;
  if (!updater) return null;
  return <UpdatePanel controller={updateControllerFor(updater)} />;
}

function UpdatePanel({ controller }: { controller: UpdateController }) {
  const { t, bytes } = useI18n();
  const { status, currentVersion, autoCheck } = useUpdateState(controller);
  const [confirm, setConfirm] = useState<UpdatePreflight | 'unknown' | null>(null);

  const openConfirm = () => {
    controller
      .preflight()
      .then((p) => setConfirm(p))
      .catch(() => setConfirm('unknown'));
  };

  const busy =
    status.kind === 'checking' || status.kind === 'downloading' || status.kind === 'installing';
  const update =
    status.kind === 'available' ||
    status.kind === 'downloading' ||
    status.kind === 'ready' ||
    status.kind === 'installing' ||
    status.kind === 'error'
      ? status.update
      : null;

  let body = null;
  switch (status.kind) {
    case 'checking':
      body = <p role="status">{t('update.checking')}</p>;
      break;
    case 'latest':
      body = <p role="status">{t('update.latest', { version: currentVersion ?? '' })}</p>;
      break;
    case 'available':
      body = (
        <>
          <p role="status">
            <strong>{t('update.available', { version: status.update.version })}</strong>
          </p>
          <Notes notes={status.update.notes} />
          {status.update.install === 'auto' ? (
            <div className={styles.row}>
              <button
                type="button"
                className="btn btn-sm btn-primary"
                onClick={() => void controller.download()}
              >
                {t('update.download')}
              </button>
            </div>
          ) : (
            <>
              <p className="muted">{t('update.manual')}</p>
              <a
                className={styles.link}
                href={status.update.release_url}
                target="_blank"
                rel="noreferrer"
              >
                {t('update.releasePage')}
              </a>
            </>
          )}
        </>
      );
      break;
    case 'downloading': {
      const percent =
        status.total && status.total > 0
          ? Math.min(100, Math.floor((status.downloaded / status.total) * 100))
          : null;
      body = (
        <>
          <p role="status">
            {percent !== null
              ? t('update.downloading', { percent })
              : t('update.downloadingSize', { size: bytes(status.downloaded) })}
          </p>
          <progress
            className={styles.progress}
            max={100}
            value={percent ?? undefined}
            aria-label={t('update.download')}
          />
        </>
      );
      break;
    }
    case 'ready':
      body = (
        <>
          <p role="status">{t('update.ready', { version: status.update.version })}</p>
          <Notes notes={status.update.notes} />
          <div className={styles.row}>
            <button type="button" className="btn btn-sm btn-primary" onClick={openConfirm}>
              {t('update.restart')}
            </button>
          </div>
        </>
      );
      break;
    case 'installing':
      body = <p role="status">{t('update.installing')}</p>;
      break;
    case 'error':
      body = (
        <>
          <p role="alert" className={styles.error}>
            {describeError(t, status)}
          </p>
          <div className={styles.row}>
            <button
              type="button"
              className="btn btn-sm"
              onClick={() => {
                if (status.step === 'download') void controller.download();
                else if (status.step === 'install') controller.backToReady();
                else void controller.check();
              }}
            >
              {t('update.retry')}
            </button>
          </div>
        </>
      );
      break;
  }

  return (
    <div id="set-updates" className={styles.box} data-testid="update-settings">
      <h4 className={styles.heading}>{t('update.heading')}</h4>
      {body}
      <div className={styles.row}>
        <button
          type="button"
          className="btn btn-sm"
          disabled={busy || status.kind === 'ready'}
          onClick={() => void controller.check()}
        >
          {t('update.check')}
        </button>
      </div>
      <label className="check">
        <input
          type="checkbox"
          checked={autoCheck}
          onChange={(e) => controller.setAutoCheck(e.target.checked)}
        />
        {t('update.autoCheck')}
      </label>
      <p className={`muted ${styles.note}`}>{t('update.autoCheckHelp')}</p>

      {confirm !== null && update && (
        <Modal
          title={t('update.confirmTitle')}
          onClose={() => setConfirm(null)}
          footer={
            <>
              <button type="button" className="btn" onClick={() => setConfirm(null)}>
                {t('common.cancel')}
              </button>
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => {
                  setConfirm(null);
                  void controller.install();
                }}
              >
                {t('update.confirmAction')}
              </button>
            </>
          }
        >
          <div className={styles.confirm} data-testid="update-confirm">
            <p>{t('update.confirmBody', { version: update.version })}</p>
            {confirm === 'unknown' ? (
              <p className={styles.warning}>{t('update.confirmSessionsMaybe')}</p>
            ) : (
              <>
                {confirm.managed_sessions > 0 && (
                  <p className={styles.warning}>
                    {t('update.confirmSessions', { count: confirm.managed_sessions })}
                  </p>
                )}
                {confirm.managed_server && <p>{t('update.confirmServer')}</p>}
              </>
            )}
            <p className="muted">{t('update.confirmObserved')}</p>
          </div>
        </Modal>
      )}
    </div>
  );
}

/**
 * Desktop app: the automatic update check, announced with a small toast
 * that leads to Settings. It never downloads or installs anything.
 */
export function UpdateNotifier({ onOpenSettings }: { onOpenSettings: () => void }) {
  const updater = useNative()?.host?.updater;
  const { t } = useI18n();
  const actions = useActions();
  const latest = useRef({ t, actions, onOpenSettings });
  useEffect(() => {
    latest.current = { t, actions, onOpenSettings };
  });
  useEffect(() => {
    if (!updater) return;
    return startAutoCheck(updateControllerFor(updater), (update) => {
      const { t, actions } = latest.current;
      actions.showToast({
        id: `update-${update.version}`,
        category: 'activity',
        sessionId: null,
        title: t('update.toast', { version: update.version }),
        body: '',
        action: {
          label: t('update.toastOpen'),
          run: () => {
            // Settings → About → Updates.
            openSettingsAt('set-updates');
            latest.current.onOpenSettings();
          },
        },
      });
    });
  }, [updater]);
  return null;
}
