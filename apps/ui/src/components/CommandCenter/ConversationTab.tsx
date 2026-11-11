import { useEffect, useRef, useState, type FormEvent } from 'react';

import { useI18n } from '../../i18n/context';
import { isStepUpCanceled } from '../../native/security';
import { shellLabel } from '../../protocol/helpers';
import type { ConductorState, SessionInfo } from '../../protocol/types';
import { useSpeech } from '../../speech/context';
import type { Recorder } from '../../speech/recorder';
import {
  CONDUCTOR_CHAT_DRAFT,
  conductorStopped,
  conductorUsable,
  turnsToEvents,
} from '../../state/conductor';
import { speechLanguageParam } from '../../state/settings';
import { useActions, useAppState, useAppStateGetter, useSettings } from '../../state/store';
import { ChatView } from '../ChatView';
import { loadDraft, onDraftAppended, saveDraft } from '../composerDrafts';
import { Icon } from '../Icon';
import { LiveView, type FolderShellTab } from '../LiveView';
import { useShell } from '../ShellProvider';
import { useStepUp } from '../StepUpPrompt';
import { CommandShell } from './CommandShell';
import { commandShellName } from './shellFolders';
import styles from './CommandCenter.module.css';

/**
 * The conversation with the conductor. A CLI conductor's own session is
 * shown exactly like a work session (chat, paging, search, composer, Voco,
 * its decisions); the built-in agent, which has no session, gets its chat
 * in the same chat view with a plain instruction box.
 */
export function ConversationTab({
  state,
  local,
  onOpenSession,
  terminalAsk = 0,
  shellFolder = null,
  onShellFolder,
}: {
  state: ConductorState;
  local: boolean;
  onOpenSession?: (sessionId: string, terminal: boolean) => void;
  /** Changes on each "Show terminal" for the conductor's session. */
  terminalAsk?: number;
  /** The folder picked for the shell tab (`null`: the most recent project folder). */
  shellFolder?: string | null;
  onShellFolder?: (folder: string) => void;
}) {
  const id = state.session_id;
  const session = useAppState((s) => (id ? s.sessions[id] : undefined));
  const stopped = conductorStopped(state);
  const shellTab = useConductorShellTab(id, shellFolder, onShellFolder);
  return (
    <div className={styles.conversation} data-testid="cc-conversation">
      {stopped && <StoppedNotice state={state} local={local} />}
      {session && session.terminal_prompt === 'trust_folder' && !stopped && (
        <TrustCard session={session} onOpenSession={onOpenSession} />
      )}
      {session && !stopped ? (
        <ConductorSession
          key={session.id}
          session={session}
          terminalAsk={terminalAsk}
          shellTab={shellTab}
        />
      ) : (
        <BuiltinConversation state={state} shellTab={shellTab} />
      )}
    </div>
  );
}

/**
 * The conversation's shell tab (next to Chat and Terminal): a remote shell
 * (PowerShell on Windows) in a project folder. Only where remote shells are
 * available to this device (the shell flows and admin permission).
 */
function useConductorShellTab(
  conductorSessionId: string | null | undefined,
  folder: string | null,
  onFolder: ((folder: string) => void) | undefined,
): FolderShellTab | undefined {
  const { t } = useI18n();
  const shell = useShell();
  const [ownFolder, setOwnFolder] = useState<string | null>(null);
  if (!shell?.allowed) return undefined;
  const name = commandShellName(shell.os ?? null);
  return {
    label: name === 'default' ? t('shellTab.tab') : shellLabel(name),
    hint: t('cc.shell.hint'),
    render: () => (
      <CommandShell
        conductorSessionId={conductorSessionId}
        folder={onFolder ? folder : ownFolder}
        onFolder={onFolder ?? setOwnFolder}
      />
    ),
  };
}

/**
 * The conductor's session in the session view. Only the selected session
 * is streamed, so it is selected while shown; the person's own selection
 * comes back afterwards unless they opened another session meanwhile.
 */
function ConductorSession({
  session,
  terminalAsk,
  shellTab,
}: {
  session: SessionInfo;
  terminalAsk: number;
  shellTab: FolderShellTab | undefined;
}) {
  const actions = useActions();
  const getState = useAppStateGetter();
  const id = session.id;
  useEffect(() => {
    const before = getState().selectedId;
    actions.select(id);
    return () => {
      if (getState().selectedId === id) actions.select(before === id ? null : before);
    };
  }, [actions, getState, id]);
  return (
    <div className={styles.sessionPane}>
      {/* "Show terminal" opens the view again, on its terminal tab. */}
      <LiveView key={terminalAsk} session={session} folderShell={shellTab} />
    </div>
  );
}

/** Claude Code's first-run "trust this folder" question, above the chat. */
function TrustCard({
  session,
  onOpenSession,
}: {
  session: SessionInfo;
  onOpenSession?: (sessionId: string, terminal: boolean) => void;
}) {
  const { t } = useI18n();
  const actions = useActions();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const trust = async () => {
    setBusy(true);
    setError(null);
    try {
      await actions.conductor({ type: 'conductor_trust_folder' });
    } catch (err) {
      setError(t('error.request', { error: (err as Error).message }));
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className={`panel ${styles.section}`} data-testid="conductor-trust">
      <p className={styles.strong}>{t('conductor.trustTitle')}</p>
      <p className={styles.help}>{t('conductor.trustHelp')}</p>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
      <div className={styles.buttons}>
        <button
          type="button"
          className="btn btn-sm btn-primary"
          disabled={busy}
          onClick={() => void trust()}
        >
          {t('conductor.trustStart')}
        </button>
        {onOpenSession && (
          <button
            type="button"
            className="btn btn-sm"
            onClick={() => onOpenSession(session.id, true)}
          >
            {t('live.showTerminal')}
          </button>
        )}
      </div>
    </section>
  );
}

/** The conductor is stopped: say so, and offer to start it on the PC. */
function StoppedNotice({ state, local }: { state: ConductorState; local: boolean }) {
  const { t } = useI18n();
  const actions = useActions();
  const stepUp = useStepUp();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const revoked = state.status === 'revoked';
  const start = async () => {
    setBusy(true);
    setError(null);
    try {
      await stepUp.run('conductor_setup', () => actions.conductor({ type: 'conductor_start' }));
    } catch (err) {
      if (!isStepUpCanceled(err)) setError(t('error.request', { error: (err as Error).message }));
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className={`panel ${styles.section}`} data-testid="cc-stopped">
      <p>{t(revoked ? 'cc.stopped.revoked' : 'cc.stopped.off')}</p>
      {!local && <p className={styles.help}>{t('cc.stopped.remote')}</p>}
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
      {local && (
        <div className={styles.buttons}>
          <button
            type="button"
            className="btn btn-sm btn-primary"
            disabled={busy}
            onClick={() => void start()}
          >
            <Icon name="play" size={16} />
            {t('conductor.start')}
          </button>
        </div>
      )}
    </section>
  );
}

/**
 * The built-in conductor (no session): its chat in the chat view and an
 * instruction box with dictation. A transcript lands in the box for
 * review, never sent on its own.
 */
function BuiltinConversation({
  state,
  shellTab,
}: {
  state: ConductorState;
  shellTab: FolderShellTab | undefined;
}) {
  const { t } = useI18n();
  const [mode, setMode] = useState<'chat' | 'shell'>('chat');
  if (!shellTab) return <BuiltinChat state={state} />;
  return (
    <div className={styles.builtin}>
      <div className={`segmented ${styles.viewModes}`} role="group" aria-label={t('live.viewMode')}>
        <button type="button" aria-pressed={mode === 'chat'} onClick={() => setMode('chat')}>
          <Icon name="chat" size={14} /> {t('live.chat')}
        </button>
        <button
          type="button"
          aria-pressed={mode === 'shell'}
          onClick={() => setMode('shell')}
          title={shellTab.hint}
          data-testid="shell-tab-button"
        >
          <Icon name="terminal" size={14} /> {shellTab.label}
        </button>
      </div>
      {mode === 'shell' ? (
        <div className={styles.sessionPane}>{shellTab.render(false)}</div>
      ) : (
        <BuiltinChat state={state} />
      )}
    </div>
  );
}

/** The built-in conductor's chat and its instruction box. */
function BuiltinChat({ state }: { state: ConductorState }) {
  const { t, locale } = useI18n();
  const actions = useActions();
  const speech = useSpeech();
  const { settings } = useSettings();
  const [text, setTextState] = useState(() => loadDraft(CONDUCTOR_CHAT_DRAFT)?.text ?? '');
  const [error, setError] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const [mic, setMic] = useState<'idle' | 'recording' | 'transcribing'>('idle');
  const recorderRef = useRef<Recorder | null>(null);
  const usable = conductorUsable(state);
  const events = turnsToEvents(state.chat);
  const setText = (next: string) => {
    setTextState(next);
    saveDraft(CONDUCTOR_CHAT_DRAFT, { text: next, corrections: [] });
  };

  useEffect(() => () => recorderRef.current?.cancel(), []);
  useEffect(
    () =>
      onDraftAppended(CONDUCTOR_CHAT_DRAFT, () =>
        setTextState(loadDraft(CONDUCTOR_CHAT_DRAFT)?.text ?? ''),
      ),
    [],
  );

  const submit = async (e?: FormEvent) => {
    e?.preventDefault();
    const value = text.trim();
    if (!value || sending) return;
    setSending(true);
    setError(null);
    try {
      await actions.conductor({ type: 'conductor_ask', text: value });
      setText('');
    } catch (err) {
      setError(t('error.request', { error: (err as Error).message }));
    } finally {
      setSending(false);
    }
  };

  const toggleMic = async () => {
    setError(null);
    if (mic === 'recording') {
      const recorder = recorderRef.current;
      recorderRef.current = null;
      if (!recorder) return;
      setMic('transcribing');
      try {
        const audio = await recorder.stop();
        const result = await speech.transcribe(audio, {
          locale,
          language: speechLanguageParam(settings.speechLanguage),
          sessionId: null,
          final: true,
        });
        const before = (loadDraft(CONDUCTOR_CHAT_DRAFT)?.text ?? text).trim();
        setText(before ? `${before} ${result.text}` : result.text);
      } catch (err) {
        setError(t('conductor.micError', { error: (err as Error).message }));
      } finally {
        setMic('idle');
      }
      return;
    }
    if (mic !== 'idle') return;
    const recorder = speech.createRecorder();
    try {
      await recorder.start();
      recorderRef.current = recorder;
      setMic('recording');
    } catch (err) {
      setError(t('conductor.micError', { error: (err as Error).message }));
    }
  };

  return (
    <div className={styles.builtin}>
      <div className={styles.builtinChat}>
        {events.length === 0 ? (
          <p className={`${styles.help} ${styles.empty}`}>{t('conductor.chatEmpty')}</p>
        ) : (
          <ChatView events={events} gapFrom={null} more={false} />
        )}
      </div>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
      <form className={styles.compose} onSubmit={(e) => void submit(e)}>
        <textarea
          aria-label={t('conductor.placeholder')}
          placeholder={t('conductor.placeholder')}
          value={text}
          rows={2}
          maxLength={4000}
          disabled={!usable}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              void submit();
            }
          }}
        />
        <button
          type="button"
          className="btn btn-sm"
          disabled={!usable || mic === 'transcribing'}
          aria-pressed={mic === 'recording'}
          aria-label={
            mic === 'recording'
              ? t('conductor.stopDictation')
              : mic === 'transcribing'
                ? t('conductor.transcribing')
                : t('conductor.dictate')
          }
          onClick={() => void toggleMic()}
        >
          <Icon name={mic === 'recording' ? 'stop' : 'mic'} size={16} />
        </button>
        <button
          type="submit"
          className="btn btn-sm btn-primary"
          disabled={!usable || sending || !text.trim()}
          aria-label={t('conductor.send')}
        >
          <Icon name="send" size={16} />
        </button>
      </form>
    </div>
  );
}
