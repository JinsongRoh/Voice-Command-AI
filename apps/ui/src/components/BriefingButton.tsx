import { useI18n } from '../i18n/context';
import { useBriefing } from '../speech/useBriefing';
import { useAppState } from '../state/store';
import { Icon } from './Icon';

/**
 * "Listen to status": the PC's short spoken status of its sessions. `compact`
 * shows the icon only (the header); the label stays for screen readers.
 */
export function BriefingButton({ compact = false }: { compact?: boolean }) {
  const { t } = useI18n();
  const briefing = useBriefing();
  const connected = useAppState((s) => s.connection === 'connected');
  const label = t('briefing.listen');
  return (
    <button
      type="button"
      className={compact ? 'icon-btn' : 'btn btn-sm'}
      aria-label={compact ? label : undefined}
      title={compact ? t('briefing.hint') : undefined}
      aria-busy={briefing.busy}
      disabled={!connected || briefing.busy}
      onClick={() => void briefing.run()}
    >
      <Icon name="briefing" size={compact ? 20 : 14} />
      {!compact && ` ${label}`}
    </button>
  );
}
