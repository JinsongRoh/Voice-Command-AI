import { useCallback, useEffect, useId, useState, type ReactNode } from 'react';

import { useI18n } from '../i18n/context';
import type { MessageKey, TranslateFn } from '../i18n/i18n';
import { toNativeError } from '../native/bridge';
import { isStepUpCanceled } from '../native/security';
import { useStepUp } from './StepUpPrompt';
import {
  ENGINE_KINDS,
  ENGINE_TYPES,
  TTS_FORMATS,
  emptyForm,
  isValidProfileId,
  parseSpeakers,
  speakersToText,
  supportsGateway,
  type EngineEntry,
  type EngineForm,
  type EngineServices,
  type EngineSetup as Setup,
  type EngineTestResult,
  type EngineType,
} from '../native/engines';
import type { HostServices } from '../native/host';
import { ENGINE_PRESETS, presetForm } from './enginePresets';
import { ConfirmDialog } from './Modal';
import styles from './EngineSetup.module.css';

/** The step-by-step guide for public users. */
export const ENGINE_GUIDE_URL =
  'https://github.com/JinsongRoh/Voice-Command-AI/blob/main/docs/speech-engines.md';

type TestState = { running: true } | { running: false; result: EngineTestResult };

interface Draft {
  kind: EngineType;
  /** The saved id being edited; null for a new profile. */
  originalId: string | null;
  form: EngineForm;
  /** Typed key: sent once on save or test, never filled in from the server. */
  key: string;
  speakers: string;
  makeDefault: boolean;
  /** The edited profile has a stored key. */
  keyStored: boolean;
  /** Typed gateway token: sent once on save or test, never filled in. */
  gatewayToken: string;
  /** The edited profile has a stored gateway token. */
  gatewayStored: boolean;
  /** Delete the stored gateway token on save. */
  clearGateway: boolean;
}

const DRAFT_TEST = 'draft';

/** A profile's engine kind from the server; a kind this app has no label for shows as-is. */
function kindLabel(t: TranslateFn, kind: string): string {
  const key = `engines.kind.${kind}`;
  const label = t(key as MessageKey);
  return label === key ? kind : label;
}

function testKey(kind: EngineType, id: string) {
  return `${kind}:${id}`;
}

function withoutDraftTest(s: Record<string, TestState>): Record<string, TestState> {
  const next = { ...s };
  delete next[DRAFT_TEST];
  return next;
}

function errorText(err: unknown): string {
  return toNativeError(err).message;
}

/** `null` for blank input. */
function opt(value: string): string | null {
  const v = value.trim();
  return v === '' ? null : v;
}

/**
 * Speech engines of this PC's own VCA server (desktop app only): add,
 * edit, test and remove the speech-to-text engines, correction LLMs and
 * TTS engines, and pick the defaults. Keys are typed once and stored on the
 * PC; they are never shown again.
 */
/**
 * Plain http to a private network address: the server refuses a key there
 * unless the engine allows it (the server's rule; this only offers the box).
 */
function isPrivatePlainHttp(url: string | null): boolean {
  const m = /^http:\/\/(\[[^\]]+\]|[^/:]+)/i.exec(url?.trim() ?? '');
  const host = m?.[1]?.toLowerCase() ?? '';
  return (
    /^(10\.|192\.168\.|169\.254\.|172\.(1[6-9]|2\d|3[01])\.)/.test(host) ||
    /^\[(f[cd]|fe[89ab])/.test(host)
  );
}

export function EngineSetup({
  services,
  host,
}: {
  services: EngineServices;
  /** Restarts the server after engine changes (when this app runs it). */
  host?: HostServices;
}) {
  const { t } = useI18n();
  const [setup, setSetup] = useState<Setup | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [saving, setSaving] = useState(false);
  const [tests, setTests] = useState<Record<string, TestState>>({});
  const [removing, setRemoving] = useState<{ kind: EngineType; id: string } | null>(null);
  const [restarting, setRestarting] = useState<'confirm' | 'running' | 'done' | null>(null);

  const load = useCallback(
    () =>
      services.get().then(
        (s) => {
          setLoadError(null);
          setSetup(s);
        },
        (err: unknown) => setLoadError(errorText(err)),
      ),
    [services],
  );

  useEffect(() => {
    void load();
  }, [load]);

  const stepUp = useStepUp();
  const fail = (err: unknown) => {
    if (!isStepUpCanceled(err)) setError(t('engines.error', { error: errorText(err) }));
  };

  const runTest = (key: string, run: () => Promise<EngineTestResult>) => {
    setTests((s) => ({ ...s, [key]: { running: true } }));
    run().then(
      (result) => setTests((s) => ({ ...s, [key]: { running: false, result } })),
      (err: unknown) =>
        setTests((s) => ({
          ...s,
          [key]: {
            running: false,
            result: { ok: false, latency_ms: 0, message: errorText(err) },
          },
        })),
    );
  };

  const startAdd = (kind: EngineType) => {
    setError(null);
    setTests(withoutDraftTest);
    const first = ENGINE_PRESETS[kind][0];
    const form = first ? (presetForm(kind, first.id) ?? emptyForm()) : emptyForm();
    setDraft({
      kind,
      originalId: null,
      form,
      key: '',
      speakers: speakersToText(form.speakers),
      makeDefault: (setup?.[kind].length ?? 0) === 0,
      keyStored: false,
      gatewayToken: '',
      gatewayStored: false,
      clearGateway: false,
    });
  };

  const startEdit = (kind: EngineType, entry: EngineEntry) => {
    setError(null);
    setTests(withoutDraftTest);
    setDraft({
      kind,
      originalId: entry.profile.id,
      form: { ...entry.profile, speakers: [...entry.profile.speakers] },
      key: '',
      speakers: speakersToText(entry.profile.speakers),
      makeDefault: entry.default,
      keyStored: entry.key === 'stored',
      gatewayToken: '',
      gatewayStored: entry.gateway === 'stored',
      clearGateway: false,
    });
  };

  const draftForm = (d: Draft): EngineForm => ({
    ...d.form,
    id: d.form.id.trim(),
    speakers: d.kind === 'tts' ? parseSpeakers(d.speakers) : [],
  });

  const save = () => {
    if (!draft) return;
    const form = draftForm(draft);
    if (!isValidProfileId(form.id)) {
      setError(t('engines.error.id'));
      return;
    }
    setSaving(true);
    setError(null);
    // A saved key that would go to a new address needs the person.
    stepUp
      .run('engine_url', () =>
        services.save(draft.kind, draft.originalId, form, opt(draft.key), draft.makeDefault, {
          token: opt(draft.gatewayToken),
          clear: draft.clearGateway,
        }),
      )
      .then((s) => {
        setSetup(s);
        // The key is gone from the page once it is stored.
        setDraft(null);
        setRestarting(null);
      }, fail)
      .finally(() => setSaving(false));
  };

  const confirmRemove = () => {
    if (!removing) return;
    const { kind, id } = removing;
    setRemoving(null);
    setError(null);
    services.remove(kind, id, true).then((s) => {
      setSetup(s);
      if (draft?.originalId === id && draft.kind === kind) setDraft(null);
    }, fail);
  };

  const makeDefault = (kind: EngineType, id: string | null) => {
    setError(null);
    services.setDefault(kind, id).then(setSetup, fail);
  };

  const restart = async () => {
    if (!host) return;
    setRestarting('running');
    setError(null);
    try {
      await host.stop();
      await host.start();
      setRestarting('done');
      load();
    } catch (err) {
      setRestarting(null);
      fail(err);
    }
  };

  if (loadError) {
    return (
      <div className={styles.engines}>
        <p role="alert" className={styles.error}>
          {t('engines.loadError', { error: loadError })}
        </p>
        <button type="button" className="btn btn-sm" onClick={load}>
          {t('engines.retry')}
        </button>
      </div>
    );
  }
  if (!setup) {
    return (
      <p className="muted" role="status">
        {t('engines.loading')}
      </p>
    );
  }

  return (
    <div className={styles.engines} data-testid="engine-setup">
      <p className="muted">{t('engines.intro')}</p>
      <a className={styles.guide} href={ENGINE_GUIDE_URL} target="_blank" rel="noreferrer">
        {t('engines.guide')}
      </a>

      {setup.restart_required && (
        <div className={styles.restart} role="status">
          <p>{restarting === 'done' ? t('engines.restartDone') : t('engines.restart')}</p>
          {host && restarting !== 'done' && (
            <button
              type="button"
              className="btn btn-sm"
              disabled={restarting === 'running'}
              onClick={() => setRestarting('confirm')}
            >
              {restarting === 'running' ? t('engines.restarting') : t('engines.restartNow')}
            </button>
          )}
        </div>
      )}

      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}

      {ENGINE_TYPES.map((kind) => (
        <EngineList
          key={kind}
          kind={kind}
          entries={setup[kind]}
          tests={tests}
          onAdd={() => startAdd(kind)}
          onEdit={(entry) => startEdit(kind, entry)}
          onRemove={(id) => setRemoving({ kind, id })}
          onDefault={(id) => makeDefault(kind, id)}
          onTest={(id) =>
            runTest(testKey(kind, id), () => services.test(kind, { profileId: id }, null))
          }
        >
          {draft?.kind === kind && (
            <EngineEditor
              draft={draft}
              saving={saving}
              test={tests[DRAFT_TEST]}
              onChange={setDraft}
              onCancel={() => setDraft(null)}
              onSave={save}
              onTest={() =>
                runTest(DRAFT_TEST, () =>
                  services.test(
                    kind,
                    { profile: draftForm(draft) },
                    opt(draft.key),
                    opt(draft.gatewayToken),
                  ),
                )
              }
            />
          )}
        </EngineList>
      ))}

      <p className={`muted ${styles.path}`}>
        {t('engines.configPath', { path: setup.config_path })}
      </p>

      {removing && (
        <ConfirmDialog
          title={t('engines.remove')}
          message={t('engines.removeConfirm', { name: removing.id })}
          confirmLabel={t('engines.remove')}
          danger
          onCancel={() => setRemoving(null)}
          onConfirm={confirmRemove}
        />
      )}
      {restarting === 'confirm' && (
        <ConfirmDialog
          title={t('engines.restartNow')}
          message={t('engines.restartConfirm')}
          confirmLabel={t('engines.restartNow')}
          onCancel={() => setRestarting(null)}
          onConfirm={() => void restart()}
        />
      )}
    </div>
  );
}

function TestOutcome({ state }: { state: TestState | undefined }) {
  const { t } = useI18n();
  if (!state) return null;
  if (state.running) {
    return (
      <span role="status" className="muted">
        {t('engines.testing')}
      </span>
    );
  }
  const { result } = state;
  return (
    <span role="status" className={result.ok ? styles.ok : styles.fail}>
      {result.ok
        ? t('engines.result.ok', { ms: result.latency_ms })
        : t('engines.result.fail', { message: result.message })}
    </span>
  );
}

function EngineList({
  kind,
  entries,
  tests,
  onAdd,
  onEdit,
  onRemove,
  onDefault,
  onTest,
  children,
}: {
  kind: EngineType;
  entries: EngineEntry[];
  tests: Record<string, TestState>;
  onAdd: () => void;
  onEdit: (entry: EngineEntry) => void;
  onRemove: (id: string) => void;
  onDefault: (id: string | null) => void;
  onTest: (id: string) => void;
  children?: ReactNode;
}) {
  const { t } = useI18n();
  const headingId = useId();
  return (
    <section className={styles.group} aria-labelledby={headingId}>
      <div className={styles.groupHead}>
        <h4 id={headingId}>{t(`engines.type.${kind}`)}</h4>
        <button
          type="button"
          className="btn btn-sm"
          onClick={onAdd}
          aria-label={t('engines.addType', { type: t(`engines.type.${kind}`) })}
        >
          {t('engines.add')}
        </button>
      </div>
      {entries.length === 0 && <p className="muted">{t('engines.none')}</p>}
      <ul className={styles.list}>
        {entries.map((entry) => {
          const p = entry.profile;
          const name = p.label ?? p.id;
          return (
            <li key={p.id} className={styles.item} data-testid={`engine-${kind}-${p.id}`}>
              <div className={styles.itemText}>
                <span className={styles.name}>
                  {name}
                  {entry.default && <span className={styles.chip}>{t('engines.default')}</span>}
                </span>
                <span className="muted">
                  {[kindLabel(t, p.kind), p.base_url, p.model].filter(Boolean).join(' · ')}
                </span>
                <span className="muted">
                  {entry.key === 'stored'
                    ? t('engines.key.stored')
                    : entry.key === 'env'
                      ? t('engines.key.env', { env: p.api_key_env ?? '' })
                      : entry.ready
                        ? t('engines.key.none')
                        : t('engines.key.missing')}
                  {entry.gateway === 'stored' && ` · ${t('engines.gateway.stored')}`}
                  {entry.gateway === 'env' &&
                    ` · ${t('engines.gateway.env', { env: p.gateway_token_env ?? '' })}`}
                  {entry.read_only && ` · ${t('engines.readOnly')}`}
                </span>
                <TestOutcome state={tests[testKey(kind, p.id)]} />
              </div>
              <div className={styles.actions}>
                <button
                  type="button"
                  className="btn btn-sm"
                  aria-label={`${t('engines.test')} ${name}`}
                  disabled={tests[testKey(kind, p.id)]?.running === true}
                  onClick={() => onTest(p.id)}
                >
                  {t('engines.test')}
                </button>
                {!entry.read_only && (
                  <>
                    <button
                      type="button"
                      className="btn btn-sm"
                      aria-label={`${t('engines.edit')} ${name}`}
                      onClick={() => onEdit(entry)}
                    >
                      {t('engines.edit')}
                    </button>
                    <button
                      type="button"
                      className="btn btn-sm btn-ghost"
                      aria-label={`${t('engines.remove')} ${name}`}
                      onClick={() => onRemove(p.id)}
                    >
                      {t('engines.remove')}
                    </button>
                  </>
                )}
                {!entry.default && (
                  <button
                    type="button"
                    className="btn btn-sm btn-ghost"
                    aria-label={`${t('engines.makeDefault')} ${name}`}
                    onClick={() => onDefault(p.id)}
                  >
                    {t('engines.makeDefault')}
                  </button>
                )}
                {entry.default && kind === 'llm' && (
                  <button
                    type="button"
                    className="btn btn-sm btn-ghost"
                    onClick={() => onDefault(null)}
                  >
                    {t('engines.unsetDefault')}
                  </button>
                )}
              </div>
            </li>
          );
        })}
      </ul>
      {children}
    </section>
  );
}

function EngineEditor({
  draft,
  saving,
  test,
  onChange,
  onCancel,
  onSave,
  onTest,
}: {
  draft: Draft;
  saving: boolean;
  test: TestState | undefined;
  onChange: (d: Draft) => void;
  onCancel: () => void;
  onSave: () => void;
  onTest: () => void;
}) {
  const { t } = useI18n();
  const uid = useId();
  const { kind, form } = draft;
  const set = (patch: Partial<EngineForm>) => onChange({ ...draft, form: { ...form, ...patch } });
  const field = (name: string) => `${uid}-${name}`;
  const isNew = draft.originalId === null;
  const typeName = t(`engines.type.${kind}`);

  const applyPreset = (presetId: string) => {
    const next = presetForm(kind, presetId);
    if (!next) return;
    onChange({ ...draft, form: next, speakers: speakersToText(next.speakers) });
  };

  const text = (
    name: keyof EngineForm,
    label: string,
    opts: { help?: string; placeholder?: string; type?: string } = {},
  ) => (
    <div className="field">
      <label htmlFor={field(name)}>{label}</label>
      <input
        id={field(name)}
        type={opts.type ?? 'text'}
        className={styles.input}
        value={(form[name] as string | null) ?? ''}
        placeholder={opts.placeholder}
        spellCheck={false}
        autoComplete="off"
        onChange={(e) =>
          set({
            [name]: name === 'id' ? e.target.value : opt(e.target.value),
          } as Partial<EngineForm>)
        }
        aria-describedby={opts.help ? field(`${name}-help`) : undefined}
      />
      {opts.help && (
        <p id={field(`${name}-help`)} className={`muted ${styles.help}`}>
          {opts.help}
        </p>
      )}
    </div>
  );

  return (
    <form
      className={styles.editor}
      aria-label={
        isNew
          ? t('engines.addTitle', { type: typeName })
          : t('engines.editTitle', { name: draft.originalId ?? '' })
      }
      onSubmit={(e) => {
        e.preventDefault();
        onSave();
      }}
    >
      <h5>
        {isNew
          ? t('engines.addTitle', { type: typeName })
          : t('engines.editTitle', { name: draft.originalId ?? '' })}
      </h5>
      {isNew && (
        <div className="field">
          <label htmlFor={field('preset')}>{t('engines.preset')}</label>
          <select
            id={field('preset')}
            defaultValue={ENGINE_PRESETS[kind][0]?.id ?? ''}
            onChange={(e) => applyPreset(e.target.value)}
          >
            {ENGINE_PRESETS[kind].map((p) => (
              <option key={p.id} value={p.id}>
                {t(`engines.preset.${p.id}` as MessageKey)}
              </option>
            ))}
          </select>
        </div>
      )}
      {text('id', t('engines.field.id'), { help: t('engines.field.idHelp') })}
      {kind !== 'tts' && text('label', t('engines.field.label'))}
      <div className="field">
        <label htmlFor={field('kind')}>{t('engines.field.kind')}</label>
        <select
          id={field('kind')}
          value={form.kind}
          onChange={(e) => set({ kind: e.target.value })}
        >
          {ENGINE_KINDS[kind].map((k) => (
            <option key={k} value={k}>
              {t(`engines.kind.${k}` as MessageKey)}
            </option>
          ))}
        </select>
      </div>
      {text('base_url', t('engines.field.baseUrl'), {
        type: 'url',
        placeholder: 'http://127.0.0.1:8000/v1',
        help: t('engines.field.baseUrlHelp'),
      })}
      {isPrivatePlainHttp(form.base_url) && (
        <div className="field">
          <label>
            <input
              type="checkbox"
              checked={form.allow_plain_http}
              onChange={(e) => set({ allow_plain_http: e.target.checked })}
            />{' '}
            {t('engines.field.allowPlainHttp')}
          </label>
          <p className={`muted ${styles.help}`}>{t('engines.field.allowPlainHttpHelp')}</p>
        </div>
      )}
      {text('model', t('engines.field.model'))}
      {kind === 'stt' && form.kind === 'google' && (
        <>
          {text('project_id', t('engines.field.projectId'))}
          {text('location', t('engines.field.location'), { placeholder: 'global' })}
        </>
      )}
      {kind === 'tts' && (
        <>
          {text('voice', t('engines.field.voice'))}
          <div className="field">
            <label htmlFor={field('speakers')}>{t('engines.field.speakers')}</label>
            <textarea
              id={field('speakers')}
              className={styles.input}
              rows={3}
              spellCheck={false}
              value={draft.speakers}
              aria-describedby={field('speakers-help')}
              onChange={(e) => onChange({ ...draft, speakers: e.target.value })}
            />
            <p id={field('speakers-help')} className={`muted ${styles.help}`}>
              {t('engines.field.speakersHelp')}
            </p>
          </div>
          <div className="field">
            <label htmlFor={field('format')}>{t('engines.field.format')}</label>
            <select
              id={field('format')}
              value={form.format ?? 'mp3'}
              onChange={(e) => set({ format: e.target.value })}
            >
              {TTS_FORMATS.map((f) => (
                <option key={f} value={f}>
                  {f}
                </option>
              ))}
            </select>
          </div>
        </>
      )}
      <div className="field">
        <label htmlFor={field('key')}>{t('engines.field.key')}</label>
        <input
          id={field('key')}
          type="password"
          className={styles.input}
          value={draft.key}
          autoComplete="new-password"
          spellCheck={false}
          placeholder={draft.keyStored ? t('engines.field.keyStoredPlaceholder') : ''}
          aria-describedby={field('key-help')}
          onChange={(e) => onChange({ ...draft, key: e.target.value })}
        />
        <p id={field('key-help')} className={`muted ${styles.help}`}>
          {draft.keyStored ? t('engines.field.keyHelpStored') : t('engines.field.keyHelpNew')}
        </p>
      </div>
      <details className={styles.advanced}>
        <summary>{t('engines.field.advanced')}</summary>
        {text('credential', t('engines.field.credential'), {
          help: t('engines.field.credentialHelp'),
          placeholder: form.id,
        })}
        {text('api_key_env', t('engines.field.apiKeyEnv'))}
        {supportsGateway(kind, form.kind) && (
          <>
            {text('api_key_header', t('engines.field.keyHeader'), {
              help: t('engines.field.keyHeaderHelp'),
              placeholder: 'X-VCA-Key',
            })}
            <div className="field">
              <label htmlFor={field('gateway')}>{t('engines.field.gatewayToken')}</label>
              <input
                id={field('gateway')}
                type="password"
                className={styles.input}
                value={draft.gatewayToken}
                autoComplete="new-password"
                spellCheck={false}
                placeholder={draft.gatewayStored ? t('engines.field.keyStoredPlaceholder') : ''}
                aria-describedby={field('gateway-help')}
                onChange={(e) => onChange({ ...draft, gatewayToken: e.target.value })}
              />
              <p id={field('gateway-help')} className={`muted ${styles.help}`}>
                {draft.gatewayStored
                  ? t('engines.field.gatewayTokenHelpStored')
                  : t('engines.field.gatewayTokenHelpNew')}
              </p>
            </div>
            {draft.gatewayStored && (
              <label className="check">
                <input
                  type="checkbox"
                  checked={draft.clearGateway}
                  onChange={(e) => onChange({ ...draft, clearGateway: e.target.checked })}
                />
                {t('engines.field.gatewayTokenClear')}
              </label>
            )}
          </>
        )}
      </details>
      <label className="check">
        <input
          type="checkbox"
          checked={draft.makeDefault}
          onChange={(e) => onChange({ ...draft, makeDefault: e.target.checked })}
        />
        {t('engines.field.makeDefault')}
      </label>
      <div className={styles.editorActions}>
        <button type="submit" className="btn btn-sm btn-primary" disabled={saving}>
          {saving ? t('engines.saving') : t('engines.save')}
        </button>
        <button
          type="button"
          className="btn btn-sm"
          disabled={test?.running === true}
          onClick={onTest}
        >
          {t('engines.test')}
        </button>
        <button type="button" className="btn btn-sm btn-ghost" onClick={onCancel}>
          {t('engines.cancel')}
        </button>
        <TestOutcome state={test} />
      </div>
    </form>
  );
}

/**
 * Phones and other paired devices: engines are listed by name in the speech
 * and notification settings, and set up on the PC only.
 */
export function EngineSetupHint() {
  const { t } = useI18n();
  return (
    <p className="muted" data-testid="engine-setup-hint">
      {t('engines.phoneHint')}
    </p>
  );
}
