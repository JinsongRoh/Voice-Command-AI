import { useEffect, useMemo, useState } from 'react';

import { useI18n } from '../i18n/context';
import { isLocalMachine } from '../native/host';
import type {
  ConductorAgent,
  ConductorSettings as Settings,
  ConductorState,
  SessionInfo,
} from '../protocol/types';
import { RequestError } from '../state/core';
import { useActions, useAppState, useConductor, useMachines } from '../state/store';
import styles from './Conductor.module.css';
import { isStepUpCanceled } from '../native/security';
import { useStepUp } from './StepUpPrompt';

const AGENTS: ConductorAgent[] = ['claude_code', 'codex', 'gemini_cli', 'builtin'];
const ALLOW_MINUTES = [15, 60, 240];

/** A session the conductor may target: on this PC, not a shell, not itself. */
function targetable(s: SessionInfo, state: ConductorState): boolean {
  return (
    s.origin.type === 'local' &&
    s.origin.agent.type !== 'shell' &&
    s.id !== state.session_id &&
    !s.conductor
  );
}

/**
 * Settings › 지휘소 (Command Center): setup only (the opt-in, the agent or
 * LLM, wake-ups, the hourly cap and auto-allow). The desktop app of the PC
 * only; day-to-day use is in 지휘소 and the header control.
 */
export function ConductorSettings({ onOpenCommand }: { onOpenCommand?: () => void }) {
  const { t } = useI18n();
  const actions = useActions();
  const stepUp = useStepUp();
  const state = useConductor();
  const { machine } = useMachines();
  const connected = useAppState((s) => s.connection === 'connected');
  const local = isLocalMachine(machine.id);
  const [draft, setDraft] = useState<Settings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);
  const [unsupported, setUnsupported] = useState(false);

  useEffect(() => {
    if (!connected || !local) return;
    let live = true;
    actions.conductor({ type: 'conductor_get' }).catch((err: unknown) => {
      if (live && err instanceof RequestError && err.code === 'unsupported') setUnsupported(true);
    });
    return () => {
      live = false;
    };
  }, [actions, connected, local]);

  if (!local) return <p className="muted">{t('conductor.settings.localOnly')}</p>;
  if (unsupported) return <p className="muted">{t('conductor.unsupported')}</p>;
  if (!state) return null;

  const current = draft ?? state.settings;
  const update = (patch: Partial<Settings>) => {
    setSaved(false);
    setDraft({ ...current, ...patch });
  };
  const save = async (next: Settings) => {
    setBusy(true);
    setError(null);
    try {
      await stepUp.run('conductor_setup', () =>
        actions.conductor({ type: 'conductor_configure', settings: next }),
      );
      setDraft(null);
      setSaved(true);
    } catch (err) {
      if (!isStepUpCanceled(err)) setError(t('error.request', { error: (err as Error).message }));
    } finally {
      setBusy(false);
    }
  };
  const info = (agent: ConductorAgent) => state.agents.find((a) => a.agent === agent);

  return (
    <div className={styles.section} data-testid="conductor-settings">
      <p className={styles.help}>{t('conductor.settings.help')}</p>
      <label className="check">
        <input
          type="checkbox"
          checked={current.enabled}
          disabled={busy}
          // The opt-in applies at once; other changes wait for Save.
          onChange={(e) => void save({ ...current, enabled: e.target.checked })}
        />
        {t('conductor.settings.enable')}
      </label>
      {current.enabled && (
        <>
          <fieldset className={styles.agents}>
            <legend className="label">{t('conductor.settings.agent')}</legend>
            {AGENTS.map((agent) => {
              const a = info(agent);
              return (
                <label key={agent} className="check">
                  <input
                    type="radio"
                    name="conductor-agent"
                    value={agent}
                    checked={current.agent === agent}
                    onChange={() => update({ agent })}
                  />
                  {t(`conductor.agent.${agent}`)}
                  {a?.experimental && (
                    <span className={styles.tag}>{t('conductor.experimental')}</span>
                  )}
                  {a && !a.available && (
                    <span className={styles.tag}>{t('conductor.notInstalled')}</span>
                  )}
                </label>
              );
            })}
          </fieldset>
          {current.agent === 'builtin' && (
            <div className="field">
              <label htmlFor="conductor-llm">{t('conductor.settings.llm')}</label>
              {state.llms.length === 0 ? (
                <p className={styles.help}>{t('conductor.settings.noLlms')}</p>
              ) : (
                <select
                  id="conductor-llm"
                  value={current.builtin_llm ?? ''}
                  onChange={(e) => update({ builtin_llm: e.target.value || null })}
                >
                  <option value="">{t('conductor.settings.llmNone')}</option>
                  {state.llms.map((l) => (
                    <option key={l.id} value={l.id}>
                      {l.model ? `${l.label} · ${l.model}` : l.label}
                    </option>
                  ))}
                </select>
              )}
              <p className={styles.help}>{t('conductor.settings.llmHelp')}</p>
            </div>
          )}
          <p className={styles.note}>{t('conductor.qualityNote')}</p>
          <label className="check">
            <input
              type="checkbox"
              checked={current.wake_events}
              onChange={(e) => update({ wake_events: e.target.checked })}
            />
            {t('conductor.settings.wake')}
          </label>
          <p className={styles.help}>
            {t('conductor.settings.wakeHelp', { count: current.max_wakes_per_hour })}
          </p>
          <div className="field">
            <label htmlFor="conductor-max-actions">{t('conductor.settings.maxActions')}</label>
            <input
              id="conductor-max-actions"
              type="number"
              min={1}
              max={200}
              value={current.max_actions_per_hour}
              onChange={(e) =>
                update({
                  max_actions_per_hour: Math.min(200, Math.max(1, Number(e.target.value) || 1)),
                })
              }
            />
          </div>
          <div className={styles.buttons}>
            <button
              type="button"
              className="btn btn-sm btn-primary"
              disabled={busy || !draft}
              onClick={() => void save(current)}
            >
              {t('conductor.settings.save')}
            </button>
            {onOpenCommand && (
              <button type="button" className="btn btn-sm" onClick={onOpenCommand}>
                {t('conductor.goToCommand')}
              </button>
            )}
          </div>
        </>
      )}
      {current.enabled && state.settings.enabled && <AllowlistPanel state={state} local={local} />}
      {saved && !draft && (
        <p role="status" className={styles.help}>
          {t('conductor.settings.saved')}
        </p>
      )}
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
    </div>
  );
}

/**
 * 자동 허용: sends to these sessions skip the confirmation until the time
 * runs out. Set on the PC (step-up); phones see the list.
 */
function AllowlistPanel({ state, local }: { state: ConductorState; local: boolean }) {
  const { t, locale } = useI18n();
  const actions = useActions();
  const stepUp = useStepUp();
  const sessions = useAppState((s) => s.sessions);
  const order = useAppState((s) => s.order);
  const [target, setTarget] = useState('');
  const [minutes, setMinutes] = useState(ALLOW_MINUTES[0]!);
  const [error, setError] = useState<string | null>(null);
  const time = new Intl.DateTimeFormat(locale, { timeStyle: 'short' });
  const candidates = useMemo(
    () =>
      order.map((id) => sessions[id]).filter((s): s is SessionInfo => !!s && targetable(s, state)),
    [order, sessions, state],
  );
  const set = async (sessionId: string, mins: number) => {
    setError(null);
    try {
      await stepUp.run('conductor_setup', () =>
        actions.conductor({
          type: 'conductor_allow_session',
          session_id: sessionId,
          minutes: mins,
        }),
      );
    } catch (err) {
      if (!isStepUpCanceled(err)) setError(t('error.request', { error: (err as Error).message }));
    }
  };
  const name = (id: string) => sessions[id]?.display_name ?? id;

  return (
    <section className={`panel ${styles.section}`} aria-labelledby="conductor-allow">
      <h3 id="conductor-allow">{t('conductor.allowlist')}</h3>
      <p className={styles.help}>{t('conductor.allowlistHelp')}</p>
      {state.allowlist.length === 0 ? (
        <p className="muted">{t('conductor.allowlistEmpty')}</p>
      ) : (
        <ul className={styles.allowlist}>
          {state.allowlist.map((a) => (
            <li key={a.session_id} className={styles.allowItem}>
              <span>
                {name(a.session_id)} · {t('conductor.until', { time: time.format(a.expires_at) })}
              </span>
              {local && (
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() => void set(a.session_id, 0)}
                >
                  {t('conductor.allowRemove')}
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
      {local && candidates.length > 0 && (
        <form
          className={styles.allowForm}
          onSubmit={(e) => {
            e.preventDefault();
            const id = target || candidates[0]!.id;
            void set(id, minutes);
          }}
        >
          <div className="field">
            <label htmlFor="conductor-allow-session">{t('conductor.allowSession')}</label>
            <select
              id="conductor-allow-session"
              value={target || candidates[0]!.id}
              onChange={(e) => setTarget(e.target.value)}
            >
              {candidates.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.display_name}
                </option>
              ))}
            </select>
          </div>
          <div className="field">
            <label htmlFor="conductor-allow-minutes">{t('conductor.allowFor')}</label>
            <select
              id="conductor-allow-minutes"
              value={minutes}
              onChange={(e) => setMinutes(Number(e.target.value))}
            >
              {ALLOW_MINUTES.map((m) => (
                <option key={m} value={m}>
                  {t('conductor.minutes', { count: m })}
                </option>
              ))}
            </select>
          </div>
          <button type="submit" className="btn btn-sm">
            {t('conductor.allowAdd')}
          </button>
        </form>
      )}
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
    </section>
  );
}
