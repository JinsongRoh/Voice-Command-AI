import { useId } from 'react';

import { useMediaQuery } from '../hooks/useMediaQuery';
import { useI18n } from '../i18n/context';
import { isLocalMachine } from '../native/host';
import { useActions, useAppState, useMachines } from '../state/store';
import { BrandLogo } from './BrandLogo';
import { BriefingButton } from './BriefingButton';
import { CommandControl } from './CommandCenter/CommandControl';
import type { CommandTab } from './CommandCenter/commandTabs';
import { OverflowMenu } from './OverflowMenu';
import { SpeakingIndicator, VoiceToggle } from './VoiceControls';
import styles from './Header.module.css';

// Selector entries that open a screen instead of switching machines.
const MANAGE = '__manage__';
const ADD = '__add__';
/** Below this width the speaker actions move into the "more" menu. */
const TIGHT_QUERY = '(max-width: 339px)';

export function Header({
  onManageMachines,
  onAddMachine,
  onOpenCommand,
}: {
  /** "Manage computers…": opens the machines sheet. */
  onManageMachines?: () => void;
  /** "Add computer…": opens the pairing flow. */
  onAddMachine?: () => void;
  /** Opens the Command Center (on a tab): the header's Command Center control. */
  onOpenCommand?: (tab?: CommandTab) => void;
} = {}) {
  const { t } = useI18n();
  const connection = useAppState((s) => s.connection);
  const actions = useActions();
  // Under ~340 px the two speaker actions fold into a "more" menu.
  const tight = useMediaQuery(TIGHT_QUERY);

  return (
    <header className={styles.header}>
      <h1 className={styles.brand}>
        <span className="visually-hidden">{t('app.title')}</span>
        {/* Horizontal lockup on wide screens, the symbol alone on narrow ones. */}
        <BrandLogo variant="horizontal" className={styles.lockup} />
        <BrandLogo variant="symbol" className={styles.symbol} />
        <span className={styles.short} aria-hidden="true">
          VCA
        </span>
      </h1>
      <div className={styles.context}>
        <MachineSelect
          id="machine-switcher"
          className={styles.machine}
          labelClassName="visually-hidden"
          onManageMachines={onManageMachines}
          onAddMachine={onAddMachine}
        />
        <span className={styles.conn} data-state={connection} role="status">
          <span className={styles.dot} aria-hidden="true" />
          <span className={styles.connText}>{t(`conn.${connection}`)}</span>
        </span>
        {connection === 'disconnected' && (
          <button
            type="button"
            className="btn btn-sm"
            onClick={() => void actions.reconnect().catch(() => {})}
          >
            {t('conn.reconnect')}
          </button>
        )}
        <SpeakingIndicator />
        {/* Command Center: status, talk to the conductor, hands-free, pause and kill switch. */}
        <CommandControl onOpen={onOpenCommand} />
        {tight ? (
          <OverflowMenu label={t('header.more')}>
            {() => (
              <>
                <VoiceToggle />
                {connection === 'connected' && <BriefingButton />}
              </>
            )}
          </OverflowMenu>
        ) : (
          <>
            <VoiceToggle compact />
            {connection === 'connected' && <BriefingButton compact />}
          </>
        )}
      </div>
    </header>
  );
}

/** The computer selector, with "Manage computers…" and "Add computer…". */
function MachineSelect({
  id,
  className,
  labelClassName,
  onManageMachines,
  onAddMachine,
}: {
  id: string;
  className: string | undefined;
  labelClassName: string | undefined;
  onManageMachines?: () => void;
  onAddMachine?: () => void;
}) {
  const { t } = useI18n();
  const { machines, machine, switchMachine } = useMachines();
  return (
    <>
      <label className={labelClassName} htmlFor={id}>
        {t('header.machine')}
      </label>
      <select
        id={id}
        className={className}
        value={machine.id}
        onChange={(e) => {
          const value = e.target.value;
          if (value === MANAGE) onManageMachines?.();
          else if (value === ADD) onAddMachine?.();
          else switchMachine(value);
        }}
      >
        {machines.map((m) => (
          <option key={m.id} value={m.id}>
            {isLocalMachine(m.id) ? t('host.thisPcName', { name: m.name }) : m.name}
          </option>
        ))}
        {(onManageMachines || onAddMachine) && (
          <optgroup label={t('machines.title')}>
            {onManageMachines && <option value={MANAGE}>{t('machines.manage')}</option>}
            {onAddMachine && <option value={ADD}>{t('machines.add')}</option>}
          </optgroup>
        )}
      </select>
    </>
  );
}

/**
 * The app header's controls as a column, for the "more" menu of the session
 * top bar on phones (where the app header steps aside for the chat): the
 * computer, the connection (with Reconnect), the Command Center, spoken alerts and the
 * briefing.
 */
export function HeaderMenuItems({
  onManageMachines,
  onAddMachine,
  onOpenCommand,
  inSession = false,
}: {
  onManageMachines?: () => void;
  onAddMachine?: () => void;
  onOpenCommand?: (tab?: CommandTab) => void;
  /** Inside a phone session: the composer's tray has the hands-free switch. */
  inSession?: boolean;
}) {
  const { t } = useI18n();
  const connection = useAppState((s) => s.connection);
  const actions = useActions();
  const id = useId();
  return (
    <div className={styles.menuItems}>
      <div className={styles.menuField}>
        <MachineSelect
          id={`${id}-machine`}
          className={styles.machine}
          labelClassName={styles.menuLabel}
          onManageMachines={onManageMachines}
          onAddMachine={onAddMachine}
        />
      </div>
      <div className={styles.menuConn}>
        <span className={styles.conn} data-state={connection}>
          <span className={styles.dot} aria-hidden="true" />
          <span>{t(`conn.${connection}`)}</span>
        </span>
        {connection === 'disconnected' && (
          <button
            type="button"
            className="btn btn-sm"
            onClick={() => void actions.reconnect().catch(() => {})}
          >
            {t('conn.reconnect')}
          </button>
        )}
      </div>
      <div className={styles.menuRow}>
        <SpeakingIndicator />
        <CommandControl onOpen={onOpenCommand} showHandsFree={!inSession} />
      </div>
      <VoiceToggle />
      {connection === 'connected' && <BriefingButton />}
    </div>
  );
}
