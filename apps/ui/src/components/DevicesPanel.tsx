import { useEffect, useMemo, useState } from 'react';

import { useI18n } from '../i18n/context';
import type { DeviceScope, PairedDevice } from '../protocol/types';
import { isStepUpCanceled } from '../native/security';
import { useActions, useAppState } from '../state/store';
import {
  DEVICE_SCOPES,
  DEVICES_REFRESH_MS,
  platformIcon,
  platformKind,
  sortDevices,
  type PlatformKind,
} from './deviceInfo';
import { Icon } from './Icon';
import { ConfirmDialog, Modal } from './Modal';
import { useStepUp } from './StepUpPrompt';
import styles from './DevicesPanel.module.css';

/** The platform filter appears above this many devices. */
const FILTER_THRESHOLD = 5;
const NAME_MAX = 64;

type Dialog =
  | { kind: 'actions'; device: PairedDevice }
  | { kind: 'rename'; device: PairedDevice }
  | { kind: 'scope'; device: PairedDevice }
  | { kind: 'revoke'; device: PairedDevice };

/** Mirrors the server's rule: trimmed, 1-64 characters, no control characters. */
function validName(name: string): boolean {
  const trimmed = name.trim();
  const len = [...trimmed].length;
  // eslint-disable-next-line no-control-regex
  return len >= 1 && len <= NAME_MAX && !/[\u0000-\u001f\u007f-\u009f]/.test(trimmed);
}

/**
 * Settings → "Paired devices" of the server the app is connected to: list,
 * rename, permission changes and revocation.
 */
export function DevicesPanel({ onPair }: { onPair: () => void }) {
  const { t, relative, locale } = useI18n();
  const actions = useActions();
  const devices = useAppState((s) => s.devices);
  const [filter, setFilter] = useState<PlatformKind | 'all'>('all');
  const [expanded, setExpanded] = useState<string | null>(null);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [error, setError] = useState<string | null>(null);
  const supported = devices !== null;

  // Keep presence and names fresh while the panel is open.
  useEffect(() => {
    if (!supported) return;
    const refresh = () => void actions.refreshDevices().catch(() => {});
    refresh();
    const timer = setInterval(refresh, DEVICES_REFRESH_MS);
    return () => clearInterval(timer);
  }, [actions, supported]);

  const sorted = useMemo(() => sortDevices(devices ?? []), [devices]);
  const kinds = useMemo(() => [...new Set(sorted.map((d) => platformKind(d.platform)))], [sorted]);
  const showFilter = sorted.length > FILTER_THRESHOLD;
  const shown =
    showFilter && filter !== 'all'
      ? sorted.filter((d) => platformKind(d.platform) === filter)
      : sorted;
  // Older servers do not report scopes; they still enforce them.
  const myScope = sorted.find((d) => d.current)?.scope ?? null;
  const isAdmin = myScope === null || myScope === 'admin';
  const date = (ms: number) => new Date(ms).toLocaleDateString(locale);

  const stepUp = useStepUp();

  /**
   * Widening needs local verification (the server says so, for this exact
   * change); a paired device is refused outright.
   */
  const setScope = async (device: PairedDevice, scope: DeviceScope) => {
    try {
      await stepUp.run('device_approve', () => actions.setDeviceScope(device.id, scope));
    } catch (err) {
      if (!isStepUpCanceled(err)) throw err;
    }
  };

  const run = (p: Promise<void>) => {
    setError(null);
    p.catch((err: Error) => setError(t('error.request', { error: err.message })));
  };

  return (
    <section className={`panel ${styles.panel}`} aria-labelledby="set-devices">
      <div className={styles.head}>
        <h3 id="set-devices">
          {t('settings.devices')}
          {supported && <span className={styles.count}>{sorted.length}</span>}
        </h3>
        <button type="button" className="btn btn-sm" onClick={onPair}>
          <Icon name="qr" size={16} />
          {t('devices.pair')}
        </button>
      </div>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
      {!supported ? (
        <p className="muted">{t('settings.devicesUnsupported')}</p>
      ) : (
        <>
          {!isAdmin && <p className={`muted ${styles.hint}`}>{t('devices.needsAdminHint')}</p>}
          {showFilter && (
            <div className="segmented" role="group" aria-label={t('devices.filter')}>
              {(['all', ...kinds] as const).map((k) => (
                <button
                  key={k}
                  type="button"
                  aria-pressed={filter === k}
                  onClick={() => setFilter(k)}
                >
                  {k === 'all' ? t('devices.filterAll') : t(`devices.platform.${k}`)}
                </button>
              ))}
            </div>
          )}
          {shown.length === 0 ? (
            <p className="muted">{t('devices.empty')}</p>
          ) : (
            <ul className={styles.list}>
              {shown.map((d) => {
                const open = expanded === d.id;
                const detailsId = `device-details-${d.id}`;
                return (
                  <li key={d.id} className={styles.item} data-testid={`device-${d.id}`}>
                    <div className={styles.row}>
                      <span className={styles.icon}>
                        <Icon name={platformIcon(platformKind(d.platform))} />
                      </span>
                      <button
                        type="button"
                        className={styles.main}
                        aria-expanded={open}
                        aria-controls={detailsId}
                        onClick={() => setExpanded(open ? null : d.id)}
                      >
                        <span className={styles.name}>
                          {d.name}
                          {d.current && (
                            <span className={styles.current}>{t('settings.thisDevice')}</span>
                          )}
                        </span>
                        <span className={styles.meta}>
                          <span className={styles.presence} data-online={!!d.online}>
                            <span className={styles.dot} aria-hidden="true" />
                            {d.online
                              ? t('devices.online', { count: d.connections ?? 1 })
                              : t('devices.offline')}
                          </span>
                          {d.scope && (
                            <span className={styles.scope} data-scope={d.scope}>
                              {t(`devices.scope.${d.scope}`)}
                            </span>
                          )}
                          <span className="muted">
                            {d.online
                              ? t('devices.pairedOn', { date: date(d.paired_at) })
                              : t('settings.lastSeen', { time: relative(d.last_seen) })}
                          </span>
                        </span>
                      </button>
                      <button
                        type="button"
                        className="icon-btn"
                        aria-label={t('devices.actions', { name: d.name })}
                        onClick={() => setDialog({ kind: 'actions', device: d })}
                      >
                        <Icon name="more" />
                      </button>
                    </div>
                    {open && (
                      <dl id={detailsId} className={styles.details}>
                        <dt>{t('devices.platformLabel')}</dt>
                        <dd>{d.platform || '-'}</dd>
                        <dt>{t('devices.paired')}</dt>
                        <dd>{date(d.paired_at)}</dd>
                        <dt>{t('devices.lastSeenLabel')}</dt>
                        <dd>{d.online ? t('devices.now') : relative(d.last_seen)}</dd>
                        <dt>{t('devices.lastIp')}</dt>
                        <dd>{d.last_ip || '-'}</dd>
                        {d.app_version && (
                          <>
                            <dt>{t('devices.appVersion')}</dt>
                            <dd>{d.app_version}</dd>
                          </>
                        )}
                      </dl>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
        </>
      )}

      {dialog?.kind === 'actions' && (
        <DeviceActions
          device={dialog.device}
          isAdmin={isAdmin}
          onClose={() => setDialog(null)}
          onPick={(kind) => setDialog({ kind, device: dialog.device })}
        />
      )}
      {dialog?.kind === 'rename' && (
        <RenameDialog
          device={dialog.device}
          onCancel={() => setDialog(null)}
          onSave={(name) => {
            setDialog(null);
            run(actions.renameDevice(dialog.device.id, name.trim()));
          }}
        />
      )}
      {dialog?.kind === 'scope' && (
        <ScopeDialog
          device={dialog.device}
          onCancel={() => setDialog(null)}
          onSave={(scope) => {
            setDialog(null);
            run(setScope(dialog.device, scope));
          }}
        />
      )}
      {dialog?.kind === 'revoke' && (
        <ConfirmDialog
          title={t('settings.revoke')}
          message={t('settings.revokeConfirm', { name: dialog.device.name })}
          confirmLabel={t('settings.revoke')}
          danger
          onCancel={() => setDialog(null)}
          onConfirm={() => {
            setDialog(null);
            run(actions.revokeDevice(dialog.device.id));
          }}
        />
      )}
    </section>
  );
}

/** The per-device overflow menu, as a sheet (touch friendly). */
function DeviceActions({
  device,
  isAdmin,
  onClose,
  onPick,
}: {
  device: PairedDevice;
  isAdmin: boolean;
  onClose: () => void;
  onPick: (kind: 'rename' | 'scope' | 'revoke') => void;
}) {
  const { t } = useI18n();
  const items: { kind: 'rename' | 'scope' | 'revoke'; label: string; why: string | null }[] = [
    { kind: 'rename', label: t('devices.rename'), why: isAdmin ? null : t('devices.needsAdmin') },
    {
      kind: 'scope',
      label: t('devices.changeScope'),
      why: !isAdmin ? t('devices.needsAdmin') : device.current ? t('devices.ownScope') : null,
    },
  ];
  // The current device is never revoked from here (it would cut itself off).
  if (!device.current)
    items.push({
      kind: 'revoke',
      label: t('settings.revoke'),
      why: isAdmin ? null : t('devices.needsAdmin'),
    });
  return (
    <Modal title={device.name} onClose={onClose}>
      <ul
        className={styles.menu}
        role="menu"
        aria-label={t('devices.actions', { name: device.name })}
      >
        {items.map((item) => (
          <li key={item.kind} role="none">
            <button
              type="button"
              role="menuitem"
              className={`${styles.menuItem} ${item.kind === 'revoke' ? styles.danger : ''}`}
              disabled={item.why !== null}
              aria-describedby={item.why ? `why-${item.kind}` : undefined}
              onClick={() => onPick(item.kind)}
            >
              <Icon
                name={
                  item.kind === 'rename' ? 'edit' : item.kind === 'scope' ? 'settings' : 'close'
                }
                size={18}
              />
              <span className={styles.menuText}>
                {item.label}
                {item.why && (
                  <span id={`why-${item.kind}`} className={styles.why}>
                    {item.why}
                  </span>
                )}
              </span>
            </button>
          </li>
        ))}
      </ul>
    </Modal>
  );
}

function RenameDialog({
  device,
  onCancel,
  onSave,
}: {
  device: PairedDevice;
  onCancel: () => void;
  onSave: (name: string) => void;
}) {
  const { t } = useI18n();
  const [name, setName] = useState(device.name);
  const ok = validName(name) && name.trim() !== device.name;
  return (
    <Modal
      title={t('devices.rename')}
      onClose={onCancel}
      footer={
        <>
          <button type="button" className="btn" onClick={onCancel}>
            {t('common.cancel')}
          </button>
          <button type="submit" form="device-rename" className="btn btn-primary" disabled={!ok}>
            {t('common.save')}
          </button>
        </>
      }
    >
      <form
        id="device-rename"
        onSubmit={(e) => {
          e.preventDefault();
          if (ok) onSave(name);
        }}
      >
        <div className="field">
          <label htmlFor="device-name">{t('devices.nameLabel')}</label>
          <input
            id="device-name"
            type="text"
            value={name}
            maxLength={NAME_MAX + 8}
            autoComplete="off"
            aria-invalid={!validName(name)}
            aria-describedby="device-name-help"
            onChange={(e) => setName(e.target.value)}
          />
          <span id="device-name-help" className="muted">
            {t('devices.nameHelp')}
          </span>
        </div>
      </form>
    </Modal>
  );
}

function ScopeDialog({
  device,
  onCancel,
  onSave,
}: {
  device: PairedDevice;
  onCancel: () => void;
  onSave: (scope: DeviceScope) => void;
}) {
  const { t } = useI18n();
  const [scope, setScope] = useState<DeviceScope | null>(device.scope ?? null);
  return (
    <Modal
      title={t('devices.changeScopeFor', { name: device.name })}
      onClose={onCancel}
      footer={
        <>
          <button type="button" className="btn" onClick={onCancel}>
            {t('common.cancel')}
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={!scope || scope === device.scope}
            onClick={() => scope && onSave(scope)}
          >
            {t('common.save')}
          </button>
        </>
      }
    >
      <fieldset className={styles.scopes}>
        <legend className="visually-hidden">{t('devices.changeScope')}</legend>
        {DEVICE_SCOPES.map((s) => (
          <label key={s} className={styles.scopeOption}>
            <input
              type="radio"
              name="device-scope"
              value={s}
              checked={scope === s}
              onChange={() => setScope(s)}
            />
            <span>
              <strong>{t(`devices.scope.${s}`)}</strong>
              <span className="muted">{t(`devices.scopeHelp.${s}`)}</span>
            </span>
          </label>
        ))}
      </fieldset>
      <p className="muted">{t('devices.scopeLive')}</p>
    </Modal>
  );
}
