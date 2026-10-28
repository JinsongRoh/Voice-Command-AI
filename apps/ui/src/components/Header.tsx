import { useI18n } from '../i18n/context';
import { useActions, useAppState, useMachines } from '../state/store';
import styles from './Header.module.css';

export function Header() {
  const { t } = useI18n();
  const { machines, machine, switchMachine } = useMachines();
  const connection = useAppState((s) => s.connection);
  const actions = useActions();

  return (
    <header className={styles.header}>
      <h1 className={styles.brand}>
        <span className={styles.logo} aria-hidden="true">
          VCA
        </span>
        <span className={styles.brandText}>{t('app.title')}</span>
      </h1>
      <label className="visually-hidden" htmlFor="machine-switcher">
        {t('header.machine')}
      </label>
      <select
        id="machine-switcher"
        className={styles.machine}
        value={machine.id}
        onChange={(e) => switchMachine(e.target.value)}
      >
        {machines.map((m) => (
          <option key={m.id} value={m.id}>
            {m.name}
          </option>
        ))}
      </select>
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
    </header>
  );
}
