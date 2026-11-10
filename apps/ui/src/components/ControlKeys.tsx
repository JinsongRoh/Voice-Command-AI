import { useState, type ReactNode } from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey } from '../i18n/i18n';
import { ctrlChar } from '../protocol/shell';
import type { ControlKey, SessionId } from '../protocol/types';
import { useActions } from '../state/store';
import { Icon, type IconName } from './Icon';
import { describeShellError } from './shellErrors';
import styles from './Composer.module.css';

// eslint-disable-next-line react-refresh/only-export-components
export const CONTROL_KEYS: { key: ControlKey; label: MessageKey; icon?: IconName }[] = [
  { key: 'interrupt', label: 'key.interrupt' },
  { key: 'escape', label: 'key.escape' },
  { key: 'enter', label: 'key.enter' },
  { key: 'tab', label: 'key.tab' },
  { key: 'up', label: 'key.up', icon: 'up' },
  { key: 'down', label: 'key.down', icon: 'down' },
  { key: 'left', label: 'key.left', icon: 'back' },
  { key: 'right', label: 'key.right', icon: 'back' },
];

/** Ctrl combinations of the key bar: Ctrl+C is the interrupt key, the rest are control bytes. */
// eslint-disable-next-line react-refresh/only-export-components
export const CTRL_KEYS = ['C', 'D', 'L', 'R', 'Z'] as const;

/**
 * The terminal key bar: Ctrl (which turns the row into Ctrl combinations),
 * Ctrl+C, Esc, Enter, Tab and the arrows. Shared by the composer and the
 * phone terminal's input row; `children` adds buttons at the end (paste,
 * snippets). Keys go through the same `send_input` as everything else.
 */
export function ControlKeys({
  sessionId,
  disabled,
  onError,
  className,
  children,
}: {
  sessionId: SessionId;
  disabled: boolean;
  /** A key failed (`null`: a new key was pressed). */
  onError: (message: string | null) => void;
  className?: string;
  children?: ReactNode;
}) {
  const { t } = useI18n();
  const actions = useActions();
  const [ctrl, setCtrl] = useState(false);

  const sendCtrl = (letter: (typeof CTRL_KEYS)[number]) => {
    setCtrl(false);
    onError(null);
    const p =
      letter === 'C'
        ? actions.sendKey(sessionId, 'interrupt')
        : actions.sendRaw(sessionId, ctrlChar(letter));
    p.catch((err: Error) => onError(describeShellError(t, err)));
  };

  return (
    <div
      className={`${styles.keys} ${className ?? ''}`}
      role="group"
      aria-label={t('composer.keys')}
    >
      <button
        type="button"
        className={`btn btn-sm ${styles.ctrl}`}
        disabled={disabled}
        aria-pressed={ctrl}
        aria-label={t('keys.ctrl')}
        onClick={() => setCtrl((v) => !v)}
      >
        Ctrl
      </button>
      {ctrl
        ? CTRL_KEYS.map((letter) => (
            <button
              key={letter}
              type="button"
              className={`btn btn-sm ${letter === 'C' ? styles.interrupt : ''}`}
              disabled={disabled}
              aria-label={t('keys.ctrlCombo', { key: letter })}
              title={t(`keys.ctrlHint.${letter}`)}
              onClick={() => sendCtrl(letter)}
            >
              {`^${letter}`}
            </button>
          ))
        : CONTROL_KEYS.map(({ key, label, icon }) => (
            <button
              key={key}
              type="button"
              className={`btn btn-sm ${key === 'interrupt' ? styles.interrupt : ''}`}
              disabled={disabled}
              aria-label={t(label)}
              onClick={() => actions.sendKey(sessionId, key).catch(() => {})}
            >
              {icon ? (
                <Icon name={icon} size={16} className={key === 'right' ? styles.flip : undefined} />
              ) : (
                t(label)
              )}
            </button>
          ))}
      {children}
    </div>
  );
}
