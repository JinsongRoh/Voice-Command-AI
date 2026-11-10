import { useI18n } from '../i18n/context';
import { Icon } from './Icon';
import { useSendTo } from './SendToSession';
import styles from './TerminalView.module.css';

/**
 * The strip above the terminal: "Send to session…" for the selected text,
 * and on phones the zoom buttons (pinch zoom works too).
 */
export function TerminalToolbar({
  selection,
  zoomable,
  zoom,
  onZoom,
  onFit,
}: {
  /** Text selected in the terminal (empty: none). */
  selection: string;
  /** Phones: the font scales to the width; show zoom buttons. */
  zoomable: boolean;
  zoom: number;
  onZoom: (direction: 1 | -1) => void;
  onFit: () => void;
}) {
  const { t } = useI18n();
  const sendTo = useSendTo();
  const hasSelection = selection.trim() !== '';
  if (!zoomable && !(sendTo && hasSelection)) return null;
  return (
    <div className={styles.toolbar} role="toolbar" aria-label={t('terminal.toolbar')}>
      {sendTo && hasSelection && (
        <button
          type="button"
          className="btn btn-sm"
          onClick={() => sendTo(selection)}
          data-testid="terminal-send-selection"
        >
          <Icon name="send" size={14} />
          {t('terminal.sendSelection')}
        </button>
      )}
      {zoomable && (
        <span className={styles.zoom}>
          <button
            type="button"
            className="btn btn-sm"
            aria-label={t('terminal.zoomOut')}
            title={t('terminal.zoomOut')}
            onClick={() => onZoom(-1)}
          >
            <Icon name="minus" size={14} />
          </button>
          <button
            type="button"
            className="btn btn-sm"
            aria-pressed={zoom === 1}
            title={t('terminal.fitHint')}
            onClick={onFit}
          >
            {t('terminal.fit')}
          </button>
          <button
            type="button"
            className="btn btn-sm"
            aria-label={t('terminal.zoomIn')}
            title={t('terminal.zoomIn')}
            onClick={() => onZoom(1)}
          >
            <Icon name="plus" size={14} />
          </button>
        </span>
      )}
    </div>
  );
}
