import { useId, useMemo, useState } from 'react';

import { uploadFile } from '../attachments/upload';
import { useI18n } from '../i18n/context';
import {
  TASK_MAX_ATTACHMENTS,
  TASK_MAX_OUTPUT_CHARS,
  TASK_MAX_RESULT_CHARS,
  type HumanTask,
  type TaskResultDraft,
} from '../protocol/tasks';
import { Client } from '../protocol/transport';
import { useActions, useAppState, useMachines } from '../state/store';
import { Modal } from './Modal';
import styles from './TasksView.module.css';

/**
 * "결과 보내기": the result (text, pasted output and files through the
 * normal attachment upload) goes to the session that asked, as a message,
 * after a preview; the task is checked off on success.
 */
export function SendResultDialog({ task, onClose }: { task: HumanTask; onClose: () => void }) {
  const { t } = useI18n();
  const actions = useActions();
  const { machine } = useMachines();
  const client = useMemo(() => new Client(machine.transport), [machine.transport]);
  const session = useAppState((s) => (task.session_id ? s.sessions[task.session_id] : undefined));
  const id = useId();
  const [text, setText] = useState('');
  const [output, setOutput] = useState(task.result?.output ?? '');
  const [files, setFiles] = useState<File[]>([]);
  const [preview, setPreview] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const name = session?.display_name ?? task.session_name ?? task.session_id ?? '';
  const empty = !text.trim() && !output.trim() && files.length === 0;

  const draft = (attachments: string[], mentions: string[]): TaskResultDraft => ({
    text,
    attachments,
    mentions,
    output: output.trim() ? output : null,
  });

  const showPreview = async () => {
    setError(null);
    if (empty) {
      setError(t('tasks.send.needSomething'));
      return;
    }
    try {
      const res = await actions.task({
        type: 'task_send_result',
        task_id: task.id,
        // Files are uploaded on send; the preview names them.
        result: { ...draft([], []), text: text || ' ' },
        preview: true,
      });
      if (res.type === 'task_result_sent') {
        const names = files.map((f) => `Attached file: ${f.name}`);
        setPreview([res.text, ...names].join('\n\n'));
      }
    } catch (err) {
      setError((err as Error).message);
    }
  };

  const send = async () => {
    setError(null);
    if (empty) {
      setError(t('tasks.send.needSomething'));
      return;
    }
    if (!task.session_id) return;
    setBusy(true);
    try {
      const paths: string[] = [];
      const mentions: string[] = [];
      for (const file of files) {
        setStatus(t('tasks.send.uploading', { name: file.name }));
        const done = await uploadFile((body) => client.request(body), file, {
          sessionId: task.session_id,
          submit: false,
        });
        paths.push(done.path);
        if (!done.typed && done.mention) mentions.push(done.mention);
      }
      setStatus(null);
      const res = await actions.task({
        type: 'task_send_result',
        task_id: task.id,
        result: draft(paths, mentions),
        preview: false,
      });
      if (res.type === 'task_result_sent' && res.sent) {
        actions.showToast({
          id: `task-sent:${task.id}`,
          category: 'completed',
          sessionId: null,
          title: t('tasks.send.sent'),
          body: '',
          security: true,
        });
        onClose();
        return;
      }
    } catch (err) {
      setError((err as Error).message);
    }
    setStatus(null);
    setBusy(false);
  };

  return (
    <Modal
      title={t('tasks.send.title')}
      onClose={onClose}
      fullScreen
      footer={
        <>
          <button type="button" className="btn" onClick={() => void showPreview()}>
            {t('tasks.send.preview')}
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={busy}
            onClick={() => void send()}
          >
            {t('tasks.send.send')}
          </button>
        </>
      }
    >
      <p className="muted">{t('tasks.send.to', { name })}</p>
      <p className={styles.sendTitle}>{task.title}</p>
      <div className="field">
        <label htmlFor={`${id}-text`}>{t('tasks.send.text')}</label>
        <textarea
          id={`${id}-text`}
          rows={4}
          maxLength={TASK_MAX_RESULT_CHARS}
          value={text}
          onChange={(e) => setText(e.target.value)}
        />
      </div>
      <div className="field">
        <label htmlFor={`${id}-output`}>{t('tasks.send.output')}</label>
        <textarea
          id={`${id}-output`}
          rows={4}
          spellCheck={false}
          maxLength={TASK_MAX_OUTPUT_CHARS}
          value={output}
          onChange={(e) => setOutput(e.target.value)}
        />
      </div>
      <div className="field">
        <label htmlFor={`${id}-files`}>{t('tasks.send.files')}</label>
        <input
          id={`${id}-files`}
          type="file"
          multiple
          onChange={(e) =>
            setFiles(Array.from(e.target.files ?? []).slice(0, TASK_MAX_ATTACHMENTS))
          }
        />
      </div>
      {preview !== null && (
        <pre className={styles.preview} aria-label={t('tasks.send.preview')}>
          {preview}
        </pre>
      )}
      {status && (
        <p className="muted" role="status">
          {status}
        </p>
      )}
      {error && (
        <p className={styles.error} role="alert">
          {error}
        </p>
      )}
    </Modal>
  );
}
