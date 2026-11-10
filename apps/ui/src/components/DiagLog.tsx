import { useRef, useState, useSyncExternalStore } from 'react';

import {
  clearDiag,
  diag,
  diagEntries,
  diagReport,
  errorInfo,
  formatDiag,
  setDiagEnabled,
  subscribeDiag,
} from '../diag/log';
import { useI18n } from '../i18n/context';
import { bridge, isNative } from '../native/bridge';
import { APP_VERSION, BUILD_STAMP } from '../state/core';
import { useSettings } from '../state/store';
import styles from './SettingsView.module.css';

/**
 * Settings -> About -> "Diagnostic log": the speech diagnostics ring buffer
 * (see `diag/log.ts`). It stays on this device; Copy puts a redacted text
 * on the clipboard for the person to send to support. The switch stops
 * recording and clears the log; the setting is applied at start in `main.tsx`.
 */
export function DiagLog() {
  const { t } = useI18n();
  const { settings, updateSettings } = useSettings();
  const [open, setOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  const area = useRef<HTMLTextAreaElement>(null);
  const entries = useSyncExternalStore(subscribeDiag, diagEntries);
  const text = open ? formatDiag(entries) : '';

  const copy = async () => {
    let platform = isNative() ? 'app' : 'browser';
    if (isNative()) {
      try {
        platform = await bridge.platform();
      } catch {
        // Keep the generic label.
      }
    }
    const report = diagReport({
      app: 'Voice Command AI',
      version: `${APP_VERSION} (${BUILD_STAMP})`,
      platform,
      timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
      entries: entries.length,
    });
    try {
      await navigator.clipboard.writeText(report);
      setCopied(true);
    } catch {
      // No clipboard permission: select the text so the user can copy it.
      area.current?.focus();
      area.current?.select();
    }
  };

  const snapshot = async () => {
    diag('snapshot', {
      visibility: document.visibilityState,
      online: typeof navigator === 'undefined' ? undefined : navigator.onLine,
    });
    if (!isNative()) return;
    try {
      diag('snapshot.mic', { json: await bridge.micDiag() });
    } catch (err) {
      diag('snapshot.mic.error', errorInfo(err));
    }
  };

  return (
    <div>
      <label className="check">
        <input
          type="checkbox"
          checked={settings.diagnostics}
          aria-describedby="set-diag-privacy"
          onChange={(e) => {
            const on = e.target.checked;
            setDiagEnabled(on);
            updateSettings({ diagnostics: on, diagnosticsChosen: true });
            if (!on) {
              setOpen(false);
              setCopied(false);
            }
          }}
        />
        {t('settings.diag.toggle')}
      </label>
      <p id="set-diag-privacy" className="muted" style={{ margin: '6px 0 0', fontSize: 13 }}>
        {t('settings.diag.privacy')}
      </p>
      {settings.diagnostics && (
        <button
          type="button"
          className={styles.linkButton}
          aria-expanded={open}
          onClick={() => setOpen((v) => !v)}
        >
          {t('settings.diag.title')} ({entries.length})
        </button>
      )}
      {open && (
        <div>
          <p className="muted">{t('settings.diag.help')}</p>
          <textarea
            ref={area}
            readOnly
            rows={14}
            value={text}
            aria-label={t('settings.diag.title')}
            style={{ width: '100%', fontFamily: 'monospace', fontSize: '11px' }}
          />
          <div className={styles.aboutLinks}>
            <button type="button" className="btn btn-sm" onClick={() => void copy()}>
              {copied ? t('settings.diag.copied') : t('settings.diag.copy')}
            </button>
            <button type="button" className="btn btn-sm" onClick={() => void snapshot()}>
              {t('settings.diag.snapshot')}
            </button>
            <button
              type="button"
              className="btn btn-sm"
              onClick={() => {
                clearDiag();
                setCopied(false);
              }}
            >
              {t('settings.diag.clear')}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
