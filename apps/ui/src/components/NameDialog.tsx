import { useId, useState } from 'react';

import { useI18n } from '../i18n/context';
import { Modal } from './Modal';
import styles from './WindowsPanel.module.css';

/** Asks for one short name (a profile, a screen layout). */
export function NameDialog({
  title,
  label,
  initial,
  onSave,
  onClose,
}: {
  title: string;
  label: string;
  initial: string;
  onSave: (name: string) => Promise<unknown>;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const id = useId();
  const [name, setName] = useState(initial);
  const [error, setError] = useState<string | null>(null);
  return (
    <Modal
      title={title}
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn" onClick={onClose}>
            {t('common.cancel')}
          </button>
          <button type="submit" form={`${id}-form`} className="btn btn-primary">
            {t('windows.save')}
          </button>
        </>
      }
    >
      <form
        id={`${id}-form`}
        className={styles.form}
        onSubmit={(e) => {
          e.preventDefault();
          onSave(name).then(onClose, (err: Error) => setError(err.message));
        }}
      >
        <div className="field">
          <label htmlFor={`${id}-name`}>{label}</label>
          <input
            id={`${id}-name`}
            value={name}
            maxLength={40}
            required
            onChange={(e) => setName(e.target.value)}
          />
        </div>
        {error && (
          <p role="alert" className={styles.error}>
            {error}
          </p>
        )}
      </form>
    </Modal>
  );
}
