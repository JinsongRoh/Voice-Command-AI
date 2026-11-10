import { useEffect, useRef, useState } from 'react';

import { useI18n } from '../i18n/context';
import { describeNativeError } from '../native/errors';
import type { HostServices, HostStatus } from '../native/host';
import { Icon } from './Icon';
import styles from './ConnectPhoneView.module.css';

/**
 * Desktop setup without any machine: this PC's own VCA server comes first.
 * Running, its sessions appear by themselves (the machine "local"); stopped,
 * it starts here. Pairing below is only for another computer.
 */
export function LocalServerCard({ host }: { host: HostServices }) {
  const { t } = useI18n();
  const [status, setStatus] = useState<HostStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    host.status().then(
      (s) => alive.current && setStatus(s),
      () => {},
    );
    return () => {
      alive.current = false;
    };
  }, [host]);

  if (!status) return null;

  const start = async () => {
    setBusy(true);
    setError(null);
    try {
      const next = await host.start();
      if (alive.current) setStatus(next);
    } catch (err) {
      if (alive.current) setError(describeNativeError(t, err));
    } finally {
      if (alive.current) setBusy(false);
    }
  };

  return (
    <section
      className={`panel ${styles.section}`}
      aria-labelledby="setup-this-pc"
      data-testid="setup-this-pc"
    >
      <h2 id="setup-this-pc" className={styles.cardTitle}>
        {t('setup.thisPc.title')}
      </h2>
      {status.running ? (
        <p className={styles.help} role="status">
          {t('setup.thisPc.connecting')}
        </p>
      ) : (
        <>
          <p className={styles.help}>{t('setup.thisPc.help')}</p>
          {status.can_start ? (
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy}
              onClick={() => void start()}
            >
              <Icon name="power" size={18} />
              {busy ? t('host.starting') : t('setup.thisPc.start')}
            </button>
          ) : (
            <p className={styles.help}>{t('setup.thisPc.cannotStart')}</p>
          )}
        </>
      )}
      {error && (
        <p role="alert" className={styles.help}>
          {error}
        </p>
      )}
    </section>
  );
}
