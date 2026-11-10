import { useEffect, useState } from 'react';

import { useI18n } from '../i18n/context';
import { toNativeError } from '../native/bridge';
import type { LaunchAtLoginServices } from '../native/launchAtLogin';
import styles from './LaunchAtLoginSettings.module.css';

/**
 * Settings > General > Startup, desktop app only: "Start VCA when I sign
 * in". The state comes from the OS each time the section opens; nothing is
 * stored in the app.
 */
export function LaunchAtLoginSettings({ services }: { services: LaunchAtLoginServices }) {
  const { t } = useI18n();
  /** `null` until the OS answered. */
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    services
      .status()
      .then((s) => {
        if (live) setEnabled(s.enabled);
      })
      .catch((err: unknown) => {
        if (live) setError(t('launchAtLogin.error', { error: toNativeError(err).message }));
      });
    return () => {
      live = false;
    };
  }, [services, t]);

  const change = (on: boolean) => {
    setBusy(true);
    setError(null);
    services
      .set(on)
      .then((s) => {
        setEnabled(s.enabled);
        // Read back on but asked off (or the reverse): the OS kept its own state.
        if (s.enabled !== on) setError(t('launchAtLogin.notApplied'));
      })
      .catch((err: unknown) =>
        setError(t('launchAtLogin.error', { error: toNativeError(err).message })),
      )
      .finally(() => setBusy(false));
  };

  return (
    <>
      <label className="check">
        <input
          type="checkbox"
          checked={enabled ?? false}
          disabled={enabled === null || busy}
          onChange={(e) => change(e.target.checked)}
          data-testid="launch-at-login"
        />
        {t('launchAtLogin.toggle')}
      </label>
      <p className={`muted ${styles.note}`}>{t('launchAtLogin.help')}</p>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
    </>
  );
}
