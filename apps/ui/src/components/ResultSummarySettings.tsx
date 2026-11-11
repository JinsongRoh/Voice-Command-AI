import { useId } from 'react';

import { useI18n } from '../i18n/context';
import { useSettings } from '../state/store';

/**
 * "Read work results aloud" (per device, off by default) and its "read
 * details" sub-setting. Turning it on lets the PC summarize the agent's
 * answers with its LLM, so the note says that conversation content is read.
 */
export function ResultSummarySettings() {
  const { t } = useI18n();
  const { settings, updateSettings } = useSettings();
  const noteId = useId();
  return (
    <>
      <label className="check">
        <input
          type="checkbox"
          checked={settings.resultSummary}
          aria-describedby={noteId}
          onChange={(e) => updateSettings({ resultSummary: e.target.checked })}
        />
        {t('summary.setting')}
      </label>
      <p id={noteId} className="muted">
        {t('summary.note')}
      </p>
      {settings.resultSummary && (
        <label className="check">
          <input
            type="checkbox"
            checked={settings.resultSummaryDetails}
            onChange={(e) => updateSettings({ resultSummaryDetails: e.target.checked })}
          />
          {t('summary.details')}
        </label>
      )}
    </>
  );
}
