import { Fragment } from 'react';

import styles from './ConfirmCode.module.css';

/**
 * A pairing confirm code (`XXXX-XXXX-XXXX-XXXX`, older servers
 * `XXXX-XXXX`), as the PC and the phone both show it for comparison: in
 * monospace so look-alike characters line up, and on a narrow screen it
 * wraps only after a dash, so each group stays whole.
 */
export function ConfirmCode({ code }: { code: string }) {
  const groups = code.split('-');
  return (
    <strong className={styles.code} data-testid="confirm-code">
      {groups.map((group, i) => (
        <Fragment key={i}>
          {i > 0 && (
            <>
              -<wbr />
            </>
          )}
          <span className={styles.group}>{group}</span>
        </Fragment>
      ))}
    </strong>
  );
}
