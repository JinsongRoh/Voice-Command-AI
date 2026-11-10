import { useCallback, useState, useSyncExternalStore } from 'react';

import { useI18n } from '../i18n/context';
import { useNative, type NativeServices } from '../native/context';
import { describeNativeError } from '../native/errors';
import { useLinkStatus } from '../native/useLinkStatus';
import { useMachines, type Machine } from '../state/store';
import { endpointHost, endpointKind } from './deviceInfo';
import { Icon } from './Icon';
import { ConfirmDialog, Modal } from './Modal';
import styles from './MachinesPanel.module.css';

const ALIAS_MAX = 64;

type MachineState = 'connected' | 'connecting' | 'reconnecting' | 'disconnected' | 'login_required';

/** Native link state when known, else the transport's connection state. */
function useMachineState(machine: Machine): MachineState {
  const link = useLinkStatus(machine);
  const transport = machine.transport;
  const subscribe = useCallback(
    (onChange: () => void) => transport.onStateChange(onChange),
    [transport],
  );
  const state = useSyncExternalStore(subscribe, () => transport.state);
  return link?.state ?? state;
}

function useStateLabel() {
  const { t } = useI18n();
  return (state: MachineState) =>
    state === 'login_required' ? t('machines.loginRequired') : t(`conn.${state}`);
}

/** Can this shell edit local machine metadata? */
function canManage(
  native: NativeServices | null,
): native is NativeServices &
  Required<Pick<NativeServices, 'renameMachine' | 'setDefaultMachine' | 'moveMachine'>> {
  return !!native?.renameMachine && !!native.setDefaultMachine && !!native.moveMachine;
}

/**
 * The computers (servers) this app is paired with: alias, default on launch,
 * order, details, re-pair and forget. Inline in Settings; `MachinesSheet`
 * shows it full screen on phones.
 */
export function MachinesPanel({
  onPair,
  heading = true,
}: {
  onPair: () => void;
  heading?: boolean;
}) {
  const { t } = useI18n();
  const { machines, machine: selected } = useMachines();
  const native = useNative();
  const manage = canManage(native);
  const [editing, setEditing] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const editMachine = machines.find((m) => m.id === editing);

  const run = (p: Promise<void> | undefined) => {
    setError(null);
    p?.catch((err: unknown) => setError(describeNativeError(t, err)));
  };

  return (
    <section
      className={heading ? `panel ${styles.panel}` : styles.bare}
      aria-labelledby="set-machines"
    >
      <div className={styles.head}>
        <h3 id="set-machines" className={heading ? undefined : 'visually-hidden'}>
          {t('machines.title')}
          <span className={styles.count}>{machines.length}</span>
        </h3>
        <button type="button" className="btn btn-sm" onClick={onPair}>
          <Icon name="plus" size={16} />
          {t('machines.addShort')}
        </button>
      </div>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
      {!manage && <p className={`muted ${styles.hint}`}>{t('machines.nativeOnly')}</p>}
      <ul className={styles.list}>
        {machines.map((m, i) => (
          <MachineRow
            key={m.id}
            machine={m}
            selected={m.id === selected.id}
            first={i === 0}
            last={i === machines.length - 1}
            manage={manage}
            onEdit={() => setEditing(m.id)}
            onMove={(offset) => manage && run(native.moveMachine(m.id, offset))}
          />
        ))}
      </ul>
      {editMachine && (
        <MachineEditSheet
          machine={editMachine}
          index={machines.indexOf(editMachine)}
          count={machines.length}
          onClose={() => setEditing(null)}
          onPair={() => {
            setEditing(null);
            onPair();
          }}
        />
      )}
    </section>
  );
}

function MachineRow({
  machine,
  selected,
  first,
  last,
  manage,
  onEdit,
  onMove,
}: {
  machine: Machine;
  selected: boolean;
  first: boolean;
  last: boolean;
  manage: boolean;
  onEdit: () => void;
  onMove: (offset: number) => void;
}) {
  const { t, relative } = useI18n();
  const state = useMachineState(machine);
  const label = useStateLabel();
  const info = machine.native?.info;
  const endpoint = info?.endpoints[0];
  return (
    <li className={styles.item} data-testid={`machine-${machine.id}`}>
      <button
        type="button"
        className={styles.main}
        onClick={onEdit}
        aria-label={t('machines.edit', { name: machine.name })}
      >
        <span className={styles.name}>
          {machine.name}
          {info?.isDefault && <span className={styles.badge}>{t('machines.default')}</span>}
          {selected && <span className={styles.selected}>{t('machines.selected')}</span>}
        </span>
        <span className={styles.meta}>
          <span className={styles.state} data-state={state}>
            <span className={styles.dot} aria-hidden="true" />
            {label(state)}
          </span>
          <span>
            {info
              ? `${t(`machines.kind.${endpointKind(endpoint)}`)} · ${endpointHost(endpoint)}`
              : machine.connectivity}
          </span>
          {info?.lastConnected ? (
            <span className="muted">
              {t('machines.lastConnectedAgo', { time: relative(info.lastConnected) })}
            </span>
          ) : null}
        </span>
      </button>
      {manage && (
        <span className={styles.order}>
          <button
            type="button"
            className="icon-btn"
            disabled={first}
            aria-label={t('machines.moveUpName', { name: machine.name })}
            onClick={() => onMove(-1)}
          >
            <Icon name="up" />
          </button>
          <button
            type="button"
            className="icon-btn"
            disabled={last}
            aria-label={t('machines.moveDownName', { name: machine.name })}
            onClick={() => onMove(1)}
          >
            <Icon name="down" />
          </button>
        </span>
      )}
    </li>
  );
}

/** Per-machine edit sheet: alias, default, order, details, re-pair, forget. */
function MachineEditSheet({
  machine,
  index,
  count,
  onClose,
  onPair,
}: {
  machine: Machine;
  index: number;
  count: number;
  onClose: () => void;
  onPair: () => void;
}) {
  const { t, relative, locale } = useI18n();
  const native = useNative();
  const manage = canManage(native);
  const { machine: selected, switchMachine } = useMachines();
  const state = useMachineState(machine);
  const label = useStateLabel();
  const info = machine.native?.info;
  const [alias, setAlias] = useState(info?.alias ?? '');
  const [confirmForget, setConfirmForget] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const endpoint = info?.endpoints[0];
  const aliasChanged = alias.trim() !== (info?.alias ?? '');
  const aliasValid = [...alias.trim()].length <= ALIAS_MAX;
  const date = (ms: number) => new Date(ms).toLocaleDateString(locale);

  const run = (p: Promise<void>) => {
    setError(null);
    setBusy(true);
    p.catch((err: unknown) => setError(describeNativeError(t, err))).finally(() => setBusy(false));
  };

  return (
    <Modal title={machine.name} onClose={onClose} fullScreen>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
      {manage && info && (
        <>
          <form
            className={styles.aliasForm}
            onSubmit={(e) => {
              e.preventDefault();
              if (aliasChanged && aliasValid)
                run(native.renameMachine(machine.id, alias.trim() || null));
            }}
          >
            <div className="field">
              <label htmlFor="machine-alias">{t('machines.alias')}</label>
              <input
                id="machine-alias"
                type="text"
                value={alias}
                placeholder={info.name}
                maxLength={ALIAS_MAX}
                autoComplete="off"
                aria-describedby="machine-alias-help"
                onChange={(e) => setAlias(e.target.value)}
              />
              <span id="machine-alias-help" className="muted">
                {t('machines.aliasHelp', { name: info.name })}
              </span>
            </div>
            <button
              type="submit"
              className="btn btn-sm btn-primary"
              disabled={busy || !aliasChanged || !aliasValid}
            >
              {t('common.save')}
            </button>
          </form>
          <label className="check">
            <input
              type="checkbox"
              checked={!!info.isDefault}
              disabled={busy}
              onChange={(e) => run(native.setDefaultMachine(e.target.checked ? machine.id : null))}
            />
            {t('machines.setDefault')}
          </label>
          <div className={styles.actions}>
            <button
              type="button"
              className="btn btn-sm"
              disabled={busy || index <= 0}
              onClick={() => run(native.moveMachine(machine.id, -1))}
            >
              <Icon name="up" size={16} />
              {t('machines.moveUp')}
            </button>
            <button
              type="button"
              className="btn btn-sm"
              disabled={busy || index >= count - 1}
              onClick={() => run(native.moveMachine(machine.id, 1))}
            >
              <Icon name="down" size={16} />
              {t('machines.moveDown')}
            </button>
          </div>
        </>
      )}

      <h3 className={styles.subhead}>{t('machines.details')}</h3>
      <dl className={styles.details}>
        <dt>{t('machines.connection')}</dt>
        <dd>{label(state)}</dd>
        {info ? (
          <>
            <dt>{t('machines.serverName')}</dt>
            <dd>{info.name}</dd>
            <dt>{t('machines.kindLabel')}</dt>
            <dd>{t(`machines.kind.${endpointKind(endpoint)}`)}</dd>
            <dt>{t('machines.address')}</dt>
            <dd>{endpointHost(endpoint)}</dd>
            {info.serverFingerprint && (
              <>
                <dt>{t('machines.fingerprint')}</dt>
                <dd className={styles.mono}>{info.serverFingerprint}</dd>
              </>
            )}
            <dt>{t('machines.paired')}</dt>
            <dd>{date(info.pairedAt)}</dd>
            <dt>{t('machines.lastConnected')}</dt>
            <dd>{info.lastConnected ? relative(info.lastConnected) : t('machines.never')}</dd>
            <dt>{t('machines.permission')}</dt>
            <dd>{t(`devices.scope.${info.scope}`)}</dd>
          </>
        ) : (
          <>
            <dt>{t('settings.provider')}</dt>
            <dd>{machine.connectivity}</dd>
          </>
        )}
      </dl>

      <div className={styles.actions}>
        {machine.id !== selected.id && (
          <button
            type="button"
            className="btn btn-sm btn-primary"
            onClick={() => {
              switchMachine(machine.id);
              onClose();
            }}
          >
            {t('machines.switch')}
          </button>
        )}
        {native && (
          <button type="button" className="btn btn-sm" onClick={onPair}>
            <Icon name="qr" size={16} />
            {t('machines.repair')}
          </button>
        )}
        {native && machine.native && (
          <button
            type="button"
            className="btn btn-sm btn-danger"
            onClick={() => setConfirmForget(true)}
          >
            {t('settings.forget')}
          </button>
        )}
      </div>
      {native && <p className="muted">{t('machines.repairHelp')}</p>}

      {confirmForget && native && (
        <ConfirmDialog
          title={t('settings.forget')}
          message={t('settings.forgetConfirm', { machine: machine.name })}
          confirmLabel={t('settings.forgetConfirmAction')}
          danger
          onCancel={() => setConfirmForget(false)}
          onConfirm={() => {
            setConfirmForget(false);
            setError(null);
            native
              .forget(machine.id)
              .then(onClose)
              .catch((err: unknown) => setError(describeNativeError(t, err)));
          }}
        />
      )}
    </Modal>
  );
}

/** "Manage computers…" from the machine selector: full screen on phones. */
export function MachinesSheet({ onClose, onPair }: { onClose: () => void; onPair: () => void }) {
  const { t } = useI18n();
  return (
    <Modal title={t('machines.title')} onClose={onClose} fullScreen>
      <MachinesPanel onPair={onPair} heading={false} />
    </Modal>
  );
}
