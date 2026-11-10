/**
 * "Send to session…": text from one session (a chat message, a code block,
 * a terminal selection) goes to another without leaving the screen.
 *
 * The picker chooses the target; then the text is put into that session's
 * message box (its draft, nothing is sent) or, after a confirmation that
 * names the target, sent right away through the same input path and guards
 * as its composer (see crossSend.ts). The target's name is always on screen
 * before anything goes out.
 */
import { createContext, useContext, useId, useMemo, useState } from 'react';

import { useI18n } from '../i18n/context';
import { folderName, originLabel } from '../protocol/helpers';
import type { SessionId, SessionInfo } from '../protocol/types';
import { workOrder } from '../state/reducer';
import { usePins } from '../state/sessionPins';
import { useActions, useAppState } from '../state/store';
import { insertIntoDraft } from './composerDrafts';
import { crossSendBlock, describeCrossSendBlock, useCrossSend } from './crossSend';
import { Icon } from './Icon';
import { Modal } from './Modal';
import { SessionPicker } from './SessionPicker';
import { useShell } from './ShellProvider';
import styles from './SendToSession.module.css';

/** Opens "Send to session…" for `text`; `null` outside a session view. */
const SendToContext = createContext<((text: string) => void) | null>(null);

export const SendToProvider = SendToContext.Provider;

/** The opener of "Send to session…" (absent: no such button is shown). */
// eslint-disable-next-line react-refresh/only-export-components
export function useSendTo(): ((text: string) => void) | null {
  return useContext(SendToContext);
}

/** Longest preview of the text in the dialog. */
const PREVIEW_CHARS = 400;

function preview(text: string): string {
  const trimmed = text.trim();
  return trimmed.length > PREVIEW_CHARS ? `${trimmed.slice(0, PREVIEW_CHARS)}…` : trimmed;
}

/** A button that opens "Send to session…" for `text` (hidden outside a session view). */
export function SendToButton({ text, className }: { text: string; className?: string }) {
  const { t } = useI18n();
  const open = useSendTo();
  if (!open || !text.trim()) return null;
  return (
    <button
      type="button"
      className={className}
      aria-label={t('sendTo.open')}
      title={t('sendTo.open')}
      onClick={() => open(text)}
    >
      <Icon name="send" size={14} />
    </button>
  );
}

type Step =
  { kind: 'pick' } | { kind: 'target'; id: SessionId } | { kind: 'confirm'; id: SessionId };

export function SendToSessionDialog({
  text,
  sourceId,
  onClose,
}: {
  text: string;
  /** The session the text comes from (not offered as a target). */
  sourceId: SessionId;
  onClose: () => void;
}) {
  const { t } = useI18n();
  const actions = useActions();
  const shell = useShell();
  const send = useCrossSend();
  const order = useAppState(workOrder);
  const map = useAppState((s) => s.sessions);
  const pins = usePins();
  const [step, setStep] = useState<Step>({ kind: 'pick' });
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const whyId = useId();
  const candidates = useMemo(
    () =>
      order
        .filter((id) => id !== sourceId)
        .map((id) => map[id])
        .filter((s): s is SessionInfo => !!s),
    [order, map, sourceId],
  );
  const target = step.kind === 'pick' ? undefined : map[step.id];
  const shown = preview(text);

  const putInDraft = (session: SessionInfo) => {
    insertIntoDraft(session.id, text);
    actions.showToast({
      id: `sendto-draft:${session.id}:${Date.now()}`,
      category: 'activity',
      sessionId: session.id,
      title: t('sendTo.draftToast', { name: session.display_name }),
      body: '',
      security: true,
    });
    onClose();
  };

  const sendNow = async (session: SessionInfo) => {
    setBusy(true);
    setError(null);
    const result = await send(session.id, text);
    setBusy(false);
    if (result.status === 'sent' || result.status === 'shell') onClose();
    else if (result.status === 'blocked')
      setError(describeCrossSendBlock(t, result.reason, session.display_name));
    else setError(t('sendTo.failed', { name: session.display_name, error: result.error.message }));
  };

  if (step.kind === 'pick' || !target) {
    return (
      <Modal key="pick" title={t('sendTo.title')} onClose={onClose}>
        <figure className={styles.preview}>
          <figcaption className="visually-hidden">{t('sendTo.textLabel')}</figcaption>
          <pre>{shown}</pre>
        </figure>
        {step.kind !== 'pick' && <p role="alert">{t('sendTo.blocked.gone')}</p>}
        <SessionPicker
          sessions={candidates}
          pins={pins}
          searchLabel={t('sendTo.search')}
          onPick={(s) => {
            setError(null);
            setStep({ kind: 'target', id: s.id });
          }}
        />
      </Modal>
    );
  }

  const where =
    target.origin.type === 'local'
      ? folderName(target.origin.project_dir)
      : originLabel(target.origin);
  const block = crossSendBlock(target, text, !!shell?.allowed);
  const canDraft = block !== 'gone';

  if (step.kind === 'confirm') {
    return (
      <Modal
        key="confirm"
        title={t('sendTo.confirmTitle', { name: target.display_name })}
        onClose={() => setStep({ kind: 'target', id: target.id })}
        footer={
          <>
            <button
              type="button"
              className="btn"
              onClick={() => setStep({ kind: 'target', id: target.id })}
            >
              {t('common.cancel')}
            </button>
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || block !== null}
              onClick={() => void sendNow(target)}
            >
              <Icon name="send" size={16} />
              {t('sendTo.confirmSend', { name: target.display_name })}
            </button>
          </>
        }
      >
        <p className={styles.target} data-testid="sendto-confirm-target">
          <span className={styles.toLabel}>{t('composer.to')}</span>
          <strong>{target.display_name}</strong>
          <span className={styles.where}>{where}</span>
        </p>
        <figure className={styles.preview}>
          <figcaption className="visually-hidden">{t('sendTo.textLabel')}</figcaption>
          <pre>{shown}</pre>
        </figure>
        <p className="muted">{t('sendTo.confirmBody', { name: target.display_name })}</p>
        {(error || block) && (
          <p role="alert" className={styles.error}>
            {error ?? describeCrossSendBlock(t, block!, target.display_name)}
          </p>
        )}
      </Modal>
    );
  }

  return (
    <Modal
      key="target"
      title={t('sendTo.title')}
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn" onClick={() => setStep({ kind: 'pick' })}>
            {t('sendTo.changeTarget')}
          </button>
          <button
            type="button"
            className="btn"
            disabled={block !== null}
            aria-describedby={block ? whyId : undefined}
            onClick={() => setStep({ kind: 'confirm', id: target.id })}
          >
            <Icon name="send" size={16} />
            {t('sendTo.sendNow')}
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={!canDraft}
            onClick={() => putInDraft(target)}
          >
            {t('sendTo.putInDraft')}
          </button>
        </>
      }
    >
      <p className={styles.target} data-testid="sendto-target">
        <span className={styles.toLabel}>{t('composer.to')}</span>
        <strong>{target.display_name}</strong>
        <span className={styles.where}>{where}</span>
      </p>
      <figure className={styles.preview}>
        <figcaption className="visually-hidden">{t('sendTo.textLabel')}</figcaption>
        <pre>{shown}</pre>
      </figure>
      <p className="muted">{t('sendTo.draftHint', { name: target.display_name })}</p>
      {block && block !== 'empty' && (
        <p id={whyId} className={styles.why}>
          {describeCrossSendBlock(t, block, target.display_name)}
        </p>
      )}
    </Modal>
  );
}
